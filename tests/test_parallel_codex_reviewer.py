"""Parallel reviewer handler tests.

Covers the minimal A8 slice:
  * primary + shadow lanes run and persist distinct artifacts,
  * combined free-form review handoff,
  * conservative merge outcome (both pass => success, any error => error).
"""

from __future__ import annotations

import subprocess

from pathlib import Path

from runner.parser import parse
from runner.handlers import Context, _parallel_reviewer  # noqa: F401
from conftest import make_node

ROOT = Path(__file__).parent.parent


def _node_with_prompt(tmp_path: Path) -> object:
    prompt = tmp_path / "review.md"
    prompt.write_text("parallel review: ${goal}\n", encoding="utf-8")
    return make_node(
        name="review",
        type="parallel_reviewer",
        backend="codex",
        prompt=f"@{prompt}",
    )


def _mock_ctx(tmp_path: Path) -> Context:
    return Context(
        goal="smoke parallel review",
        workdir=tmp_path,
        backend="codex",
        run_id="parallel-review",
        event_log_path=tmp_path / "events.jsonl",
    )


def test_parallel_reviewer_is_registered_as_validation_and_read_only_branch_type():
    """The reviewer lane must be treated like gate/reviewer infrastructure."""
    from runner.engine_branches import _READ_ONLY_BRANCH_TYPES
    from runner.engine_observability import _VALIDATION_TYPES as ENGINE_VALIDATION_TYPES
    from runner.parser import _VALIDATION_TYPES as PARSER_VALIDATION_TYPES
    from runner.handlers import TYPE_REGISTRY

    assert "parallel_reviewer" in TYPE_REGISTRY
    assert "parallel_reviewer" in ENGINE_VALIDATION_TYPES
    assert "parallel_reviewer" in PARSER_VALIDATION_TYPES
    assert "parallel_reviewer" in _READ_ONLY_BRANCH_TYPES


def test_production_pipelines_do_not_use_raw_codex_exec_reviewer_tools():
    """Reviewer CLIs must be first-class typed nodes, not opaque tool commands."""
    offenders: list[str] = []
    for path in sorted((ROOT / "pipelines").rglob("*.dot")):
        graph = parse(path, require_start_exit=False)
        for node in graph.nodes.values():
            command = str(node.attrs.get("command", ""))
            if node.attrs.get("type") == "tool" and "codex exec" in command:
                offenders.append(f"{path.relative_to(ROOT)}:{node.name}")
    assert offenders == []


def test_parallel_reviewer_runs_both_lanes_and_logs_distinct_outputs(tmp_path, monkeypatch):
    """Both lanes execute and leave separate artifacts in metadata/events."""
    node = _node_with_prompt(tmp_path)
    ctx = _mock_ctx(tmp_path)
    ctx.state["_df_shadow_codex_review"] = "true"
    expected_sha = "a" * 40

    call_log: list[str] = []

    class _ShadowPopen:
        pid = 11111

        def __init__(self, args, **kwargs):
            call_log.append(f"popen:{args[0]}")
            self.args = args
            self.returncode = 0

        def communicate(self, timeout=None):
            call_log.append("shadow-communicate")
            return (
                f"head_sha: {expected_sha}\n"
                "## Review Verdict\npass\n\n"
                "verdict: pass\n",
                "",
            )

    def _fake_run(cmd, **kwargs):
        call_log.append(f"run:{cmd[0]}")
        return subprocess.CompletedProcess(
            cmd,
            0,
            stdout=f"head_sha: {expected_sha}\nverdict: pass\n",
            stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda wd: expected_sha)
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handler_dispatch.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _ShadowPopen)

    result = _parallel_reviewer(node, ctx)

    assert result.outcome == "success"
    assert call_log[0].startswith("popen:")
    assert call_log[1].startswith("run:")
    assert "## Parallel Codex Gate Review" in result.output

    assert result.metadata["parallel_reviewer_primary_outcome"] == "success"
    assert result.metadata["shadow_codex_gate_outcome"] == "success"

    primary_output_path = Path(result.metadata["parallel_reviewer_primary_output_path"])
    shadow_output_path = Path(result.metadata["shadow_codex_gate_output_path"])
    assert primary_output_path.exists()
    assert shadow_output_path.exists()
    assert ("head_sha: " + expected_sha) in primary_output_path.read_text()
    assert "## Review Verdict" in shadow_output_path.read_text()

    events = ctx.event_log_path.read_text()
    assert '"event": "node_prompt"' in events
    assert '"event": "parallel_reviewer_primary_result"' in events
    assert '"event": "shadow_gate_prompt"' in events
    assert '"event": "shadow_gate_result"' in events


def test_parallel_reviewer_maps_shadow_errors_to_error(tmp_path, monkeypatch):
    """A shadow infra error flips final outcome to error."""
    node = _node_with_prompt(tmp_path)
    ctx = _mock_ctx(tmp_path)
    ctx.state["_df_shadow_codex_review"] = "true"
    expected_sha = "b" * 40

    class _ShadowPopen:
        pid = 22222

        def __init__(self, args, **kwargs):
            self.args = args
            self.returncode = 1

        def communicate(self, timeout=None):
            return ("shadow infra failure\n", "")

    def _fake_run(cmd, **kwargs):
        return subprocess.CompletedProcess(
            cmd,
            0,
            stdout=f"head_sha: {expected_sha}\nverdict: pass\n",
            stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda wd: expected_sha)
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handler_dispatch.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _ShadowPopen)

    result = _parallel_reviewer(node, ctx)

    assert result.outcome == "error"
    assert result.metadata["parallel_reviewer_primary_outcome"] == "success"
    assert result.metadata["shadow_codex_gate_outcome"] == "error"
    assert result.metadata.get("parallel_reviewer_outcome") == "error"


def test_parallel_reviewer_launches_all_shadows_before_awaiting(tmp_path, monkeypatch):
    """N shadow lanes are Popen-launched before any communicate() (true concurrency)."""
    node = _node_with_prompt(tmp_path)
    ctx = _mock_ctx(tmp_path)
    ctx.state["_df_shadow_backends"] = "codex,minimax"
    expected_sha = "c" * 40

    call_log: list[str] = []

    class _ShadowPopen:
        _pid = 30000

        def __init__(self, args, **kwargs):
            call_log.append(f"popen:{args[0]}")
            self.args = args
            self.returncode = 0

        def communicate(self, timeout=None):
            call_log.append("communicate")
            return (
                f"head_sha: {expected_sha}\n## Review Verdict\npass\n\nverdict: pass\n",
                "",
            )

    def _fake_run(cmd, **kwargs):
        call_log.append(f"run:{cmd[0]}")
        return subprocess.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {expected_sha}\nverdict: pass\n", stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda wd: expected_sha)
    monkeypatch.setattr("runner.handlers._get_claude_executable", lambda: "claude")
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/" + str(name))
    monkeypatch.setattr("runner.handler_dispatch.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _ShadowPopen)

    result = _parallel_reviewer(node, ctx)

    popen_idxs = [i for i, e in enumerate(call_log) if e.startswith("popen:")]
    communicate_idxs = [i for i, e in enumerate(call_log) if e == "communicate"]
    assert len(popen_idxs) == 2, call_log
    assert len(communicate_idxs) == 2, call_log
    # Both launches happen before EITHER await -> concurrent launch.
    assert max(popen_idxs) < min(communicate_idxs), call_log
    assert result.outcome == "success"
    assert result.metadata["shadow_codex_gate_outcome"] == "success"
    assert result.metadata["shadow_minimax_gate_outcome"] == "success"
    assert "codex" in result.metadata["parallel_reviewer_shadow_backends"]
    assert "minimax" in result.metadata["parallel_reviewer_shadow_backends"]


def test_parallel_reviewer_one_shadow_real_fail_maps_to_failure_not_error(tmp_path, monkeypatch):
    """One shadow passing + one shadow real-fail (not infra) => failure, not error."""
    node = _node_with_prompt(tmp_path)
    ctx = _mock_ctx(tmp_path)
    ctx.state["_df_shadow_backends"] = "codex,minimax"
    expected_sha = "d" * 40

    class _ShadowPopen:
        def __init__(self, args, **kwargs):
            self.args = args
            self.returncode = 0  # clean exit -> real verdict, not infra error
            # codex passes, minimax fails, keyed off the executable name in args
            joined = " ".join(str(a) for a in args)
            self._fail = "claude" in joined  # minimax routes through the claude binary

        def communicate(self, timeout=None):
            verdict = "fail" if self._fail else "pass"
            return (
                f"head_sha: {expected_sha}\n## Review Verdict\n{verdict}\n\nverdict: {verdict}\n",
                "",
            )

    def _fake_run(cmd, **kwargs):
        return subprocess.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {expected_sha}\nverdict: pass\n", stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda wd: expected_sha)
    monkeypatch.setattr("runner.handlers._get_claude_executable", lambda: "claude")
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/" + str(name))
    monkeypatch.setattr("runner.handler_dispatch.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _ShadowPopen)

    result = _parallel_reviewer(node, ctx)

    assert result.metadata["shadow_codex_gate_outcome"] == "success"
    assert result.metadata["shadow_minimax_gate_outcome"] == "failure"
    assert result.outcome == "failure"
    assert result.metadata.get("parallel_reviewer_outcome") == "failure"
