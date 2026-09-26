"""Tests for _gate_slash generic single-lane reviewer gate.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402


def _slash_node(command: str | None):
    attrs = {} if command is None else {"command": command}
    return make_node(name="lane", **attrs)


def test_gate_slash_missing_command_errors(tmp_path):
    from runner.handlers import _gate_slash, Context as HCtx

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _gate_slash(_slash_node(None), ctx)
    assert result.outcome == "error"
    assert "missing required command attr" in result.output


def test_gate_slash_unknown_command_errors(tmp_path, monkeypatch):
    """Command absent from BOTH the target repo and user scope → error,
    not a free-associated review."""
    import pathlib as _pl
    from runner.handlers import _gate_slash, Context as HCtx

    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setattr(_pl.Path, "home", staticmethod(lambda: fake_home))

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _gate_slash(_slash_node("zfc"), ctx)
    assert result.outcome == "error"
    assert "refusing to run an undefined review lane" in result.output


def test_gate_slash_materializes_user_scope_command(tmp_path, monkeypatch):
    """Command in ~/.claude/commands/ but not the repo → copied into the
    workdir so every reviewer backend (incl. codex) resolves it repo-local."""
    import pathlib as _pl
    import subprocess as _sp
    from runner.handlers import _gate_slash, Context as HCtx

    fake_home = tmp_path / "home"
    user_cmds = fake_home / ".claude" / "commands"
    user_cmds.mkdir(parents=True)
    (user_cmds / "zfc.md").write_text("# /zfc user-scope review")
    monkeypatch.setattr(_pl.Path, "home", staticmethod(lambda: fake_home))

    workdir = tmp_path / "repo"
    workdir.mkdir()
    fake_sha = "d" * 40

    def _fake_run(cmd, **kwargs):
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    ctx = HCtx(goal="test", workdir=workdir, backend="claude")
    result = _gate_slash(_slash_node("zfc"), ctx)

    assert result.outcome == "success"
    materialized = workdir / ".claude" / "commands" / "zfc.md"
    assert materialized.exists(), "user-scope command must be copied into the workdir"
    assert materialized.read_text() == "# /zfc user-scope review"


def test_gate_slash_runs_named_command(tmp_path, monkeypatch):
    """With .claude/commands/<cmd>.md present, the gate shells out `/cmd` with
    SHA binding, identical to the named gates."""
    import subprocess as _sp
    from runner.handlers import _gate_slash, Context as HCtx

    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "zfc.md").write_text("# /zfc review")

    fake_sha = "c" * 40
    seen_prompts: list[str] = []

    def _fake_run(cmd, **kwargs):
        seen_prompts.append(cmd[-1])
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _gate_slash(_slash_node("zfc"), ctx)

    assert result.outcome == "success"
    assert result.metadata["slash_command"] == "zfc"
    # The command file content must be INLINED into the prompt — a literal
    # "/zfc" prompt is backend-dependent (claude vs codex resolve slash
    # commands from different namespaces).
    assert seen_prompts and "--- /zfc instructions ---" in seen_prompts[0]
    assert "# /zfc review" in seen_prompts[0]
    assert f"head_sha: {fake_sha}" in seen_prompts[0]
    assert "verdict: <pass|warn|fail|partial>" in seen_prompts[0]


def test_gate_slash_warn_verdict_strict_forwarding(tmp_path, monkeypatch):
    """Real `_gate_slash` (not a TYPE_REGISTRY fake) must actually forward
    node.attrs["gate_strict"] through `_execute_gate` -> `_run_gate_once` ->
    `_parse_verdict`, so a `verdict: warn` reviewer response normalizes to
    `failure` when gate_strict="true" and to `success` when it is absent.

    tests/test_pipeline_ready.py::test_ready_pipeline_advice_warn_fails_gate
    exercises `_gate_strict_flag`/`_parse_verdict` directly via a fake
    TYPE_REGISTRY["gate_slash"] handler — it would still pass even if the
    real `_gate_slash` stopped forwarding `gate_strict` into `_execute_gate`.
    This test closes that gap by calling the real `_gate_slash` function
    (mocking only `subprocess.run`, per the pattern above)."""
    import subprocess as _sp
    from runner.handlers import _gate_slash, Context as HCtx

    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "advice.md").write_text("# /advice review")

    fake_sha = "e" * 40

    def _fake_run(cmd, **kwargs):
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: warn\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    strict_node = make_node(name="lane", command="advice", gate_strict="true")
    strict_result = _gate_slash(strict_node, ctx)
    assert strict_result.outcome == "failure", (
        "real _gate_slash with gate_strict='true' must normalize a warn "
        "verdict to failure, not silently pass it through as success"
    )

    lenient_node = make_node(name="lane", command="advice")
    lenient_result = _gate_slash(lenient_node, ctx)
    assert lenient_result.outcome == "success", (
        "sanity check: without gate_strict, the legacy warn->success mapping "
        "still applies via the real _gate_slash path"
    )
