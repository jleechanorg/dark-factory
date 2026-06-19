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
