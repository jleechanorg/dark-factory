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


# ─── qw5: N-shadow fan-out (Beads: jleechan-qw5) ──────────────────────────
#
# Pilot scope: extend parallel_reviewer from 1 primary + 1 shadow to
# 1 primary + N shadows (configurable via node attribute `n_shadows="N"`).
# Coalesce is conservative: any error→error, any failure→failure, all-pass only.
# Per the /advice 2026-06-27 ruling, NO reviewer-shopping — a reviewer
# disagreement must surface, never be averaged out.

def _n_node_with_prompt(tmp_path: Path, n_shadows: int) -> object:
    prompt = tmp_path / "review.md"
    prompt.write_text("parallel review: ${goal}\n", encoding="utf-8")
    return make_node(
        name="review",
        type="parallel_reviewer",
        backend="codex",
        prompt=f"@{prompt}",
        n_shadows=str(n_shadows),
    )


class _MultiShadowPopen:
    """Popen stub that returns a distinct verdict per shadow slot.

    `verdicts` is a list, one entry per shadow. To exercise partial failure
    or error, inject `("failure", rc)` or `("error", 1)` entries.
    """

    pid = 33333

    def __init__(self, args, **kwargs):
        self.args = args
        # Each Popen instance picks the next verdict in FIFO order; tests
        # drive sequencing by counting Popen calls.
        idx = _MultiShadowPopen.calls
        _MultiShadowPopen.calls += 1
        try:
            verdict, rc = _MultiShadowPopen.verdicts[idx]
        except IndexError:
            verdict, rc = ("success", 0)
        self.returncode = rc
        self.verdict = verdict

    def communicate(self, timeout=None):
        sha = _MultiShadowPopen.expected_sha
        if self.returncode != 0:
            return ("shadow infra failure\n", "")
        # Map internal verdict tokens onto the parser's recognized set
        # (pass | warn | approved | fail | partial | ...). The internal label
        # "success" maps to "pass" so _parse_verdict returns success; "failure"
        # maps to "fail" so it returns failure; "warn" stays warn.
        token_map = {"success": "pass", "warn": "warn", "failure": "fail"}
        token = token_map.get(self.verdict, self.verdict)
        return (
            f"head_sha: {sha}\n## Review Verdict\n{token}\n\nverdict: {token}\n",
            "",
        )


def _setup_n_shadow(monkeypatch, tmp_path, expected_sha):
    _MultiShadowPopen.calls = 0
    _MultiShadowPopen.expected_sha = expected_sha
    _MultiShadowPopen.verdicts = []

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda wd: expected_sha)
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")

    def _fake_run(cmd, **kwargs):
        return subprocess.CompletedProcess(
            cmd,
            0,
            stdout=f"head_sha: {expected_sha}\nverdict: pass\n",
            stderr="",
        )

    monkeypatch.setattr("runner.handler_dispatch.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _MultiShadowPopen)


def test_parallel_reviewer_n_shadows_default_is_one_shadow(tmp_path, monkeypatch):
    """n_shadows absent or '1' ⇒ exactly 1 shadow runs (back-compat)."""
    node = _n_node_with_prompt(tmp_path, n_shadows=1)
    ctx = _mock_ctx(tmp_path)
    expected_sha = "c" * 40
    _setup_n_shadow(monkeypatch, tmp_path, expected_sha)
    _MultiShadowPopen.verdicts = [("success", 0)]

    result = _parallel_reviewer(node, ctx)

    assert _MultiShadowPopen.calls == 1
    assert result.outcome == "success"


def test_parallel_reviewer_n_shadows_three_all_success(tmp_path, monkeypatch):
    """n_shadows=3 with all-success verdicts ⇒ overall success, 3 distinct artifacts."""
    node = _n_node_with_prompt(tmp_path, n_shadows=3)
    ctx = _mock_ctx(tmp_path)
    expected_sha = "d" * 40
    _setup_n_shadow(monkeypatch, tmp_path, expected_sha)
    _MultiShadowPopen.verdicts = [("success", 0), ("success", 0), ("success", 0)]

    result = _parallel_reviewer(node, ctx)

    assert _MultiShadowPopen.calls == 3
    assert result.outcome == "success"
    # Primary path is unchanged; per-shadow metadata keys exist for each slot.
    for i in (1, 2, 3):
        assert result.metadata[f"shadow_codex_gate_outcome_{i}"] == "success", (
            f"shadow {i} outcome missing or wrong: "
            f"{result.metadata.get(f'shadow_codex_gate_outcome_{i}')!r}"
        )


def test_parallel_reviewer_n_shadows_one_failure_dominates(tmp_path, monkeypatch):
    """A single shadow failure in N=3 ⇒ overall failure (no majority-vote)."""
    node = _n_node_with_prompt(tmp_path, n_shadows=3)
    ctx = _mock_ctx(tmp_path)
    expected_sha = "e" * 40
    _setup_n_shadow(monkeypatch, tmp_path, expected_sha)
    _MultiShadowPopen.verdicts = [
        ("success", 0),
        ("success", 0),
        ("warn", 0),  # warn counts as failure under conservative coalesce
    ]

    # Mute the warn→failure mapping: the conservative rules in this pilot
    # treat ANY non-success, non-error verdict as failure. Override the
    # implementation's verdict parsing via a simple success/anything-else
    # binning; warn maps to "warn" inside _parse_verdict which the handler
    # then normalizes to "failure" via _is_success_outcome().
    result = _parallel_reviewer(node, ctx)

    assert _MultiShadowPopen.calls == 3
    assert result.outcome == "failure", (
        f"expected overall failure when one shadow returned warn; got {result.outcome}"
    )


def test_parallel_reviewer_n_shadows_one_error_dominates(tmp_path, monkeypatch):
    """A single shadow error in N=3 ⇒ overall error (infra failure surfaces)."""
    node = _n_node_with_prompt(tmp_path, n_shadows=3)
    ctx = _mock_ctx(tmp_path)
    expected_sha = "f" * 40
    _setup_n_shadow(monkeypatch, tmp_path, expected_sha)
    _MultiShadowPopen.verdicts = [
        ("success", 0),
        ("success", 0),
        ("success", 1),  # rc=1 → shadow_outcome="error"
    ]

    result = _parallel_reviewer(node, ctx)

    assert _MultiShadowPopen.calls == 3
    assert result.outcome == "error", (
        f"expected overall error when one shadow returned rc=1; got {result.outcome}"
    )


def test_parallel_reviewer_n_shadows_distinct_artifacts(tmp_path, monkeypatch):
    """Each N shadow writes its own output path; primary path unchanged."""
    node = _n_node_with_prompt(tmp_path, n_shadows=3)
    ctx = _mock_ctx(tmp_path)
    expected_sha = "1" * 40
    _setup_n_shadow(monkeypatch, tmp_path, expected_sha)
    _MultiShadowPopen.verdicts = [("success", 0)] * 3

    result = _parallel_reviewer(node, ctx)

    primary_path = Path(result.metadata["parallel_reviewer_primary_output_path"])
    assert primary_path.exists(), "primary output path must exist"
    for i in (1, 2, 3):
        shadow_key = f"shadow_codex_gate_output_path_{i}"
        assert shadow_key in result.metadata, f"missing shadow output path {shadow_key}"
        shadow_path = Path(result.metadata[shadow_key])
        assert shadow_path.exists(), f"shadow {i} output path must exist"
        # Each sidecar carries the expected head_sha echo and a verdict.
        body = shadow_path.read_text()
        assert expected_sha in body, f"shadow {i} missing head_sha echo"
    events = ctx.event_log_path.read_text()
    assert '"event": "parallel_reviewer_primary_result"' in events
    for i in (1, 2, 3):
        assert f'"shadow_index": {i}' in events, f"missing shadow_index {i} event"
