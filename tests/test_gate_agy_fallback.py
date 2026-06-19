"""agy reviewer backend + claude infra fallback + no-reviewer-shopping.

Three properties that make "agy reviews, falls back to claude if it doesn't
work" honest rather than a label: (a) actually invoke agy, (b) fall back to
claude only on agy *infrastructure* failure, (c) NEVER reviewer-shop a real
agy fail/partial verdict onto claude.

NOTE: The source `tests/test_gates.py` had two copies of `_agy_gate_node`
and two copies of `test_gate_er_runs_agy_when_backend_agy`. The split
dedupes both — one helper, one test.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import os
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402


def _agy_gate_node():
    return make_node(name="evidence", backend="agy")


def test_gate_er_runs_agy_when_backend_agy(tmp_path, monkeypatch):
    """backend=agy → the reviewer subprocess is `agy --print ...`, not claude."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    # ctx.backend is the run-level CLI backend; the per-node backend=agy must win.
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    fake_sha = "a" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)

    assert result.outcome == "success"
    assert seen, "subprocess.run must have been called"
    assert seen[0][0] == "agy", f"expected agy reviewer argv, got {seen[0][:1]!r}"
    assert result.metadata["reviewer_backend"] == "agy"
    assert result.metadata["fallback_used"] == "false"
    # No reviewer-shopping: a passing agy verdict must not also call claude.
    assert not any("claude" in c[0] for c in seen)


def test_gate_agy_falls_back_to_claude_on_infra_failure(tmp_path, monkeypatch):
    """agy missing (FileNotFoundError) → fall back to claude; result is the claude verdict."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    fake_sha = "c" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        if cmd[0] == "agy":
            raise FileNotFoundError("agy: command not found")
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)

    assert result.outcome == "success"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "agy"
    assert result.metadata["reviewer_backend"] == "claude"
    # agy was tried first, then claude.
    assert seen[0][0] == "agy"
    assert any("claude" in c[0] for c in seen), "claude fallback must have been invoked"


def test_gate_agy_real_fail_verdict_not_retried(tmp_path, monkeypatch):
    """A genuine agy `verdict: fail` (matching SHA) is kept — claude is never called."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    fake_sha = "d" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        # agy returns a real review verdict (rc 0, SHA echoed): this is NOT an
        # infra failure, so the fallback must not fire.
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: fail\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)

    assert result.outcome == "failure"
    assert result.metadata["reviewer_backend"] == "agy"
    assert result.metadata["fallback_used"] == "false"
    # Reviewer-shopping guard: claude must NEVER be consulted for a real verdict.
    assert all(c[0] == "agy" for c in seen), f"claude must not be retried; saw {[c[0] for c in seen]!r}"


def test_gate_er_falls_back_to_claude_on_agy_infra_failure(tmp_path, monkeypatch):
    """agy binary missing / sandbox block → verdict: unknown + head_sha: missing,
    which is an infra failure. It must fall back to claude; the final result
    records the fallback in metadata."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    fake_sha = "b" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        if os.path.basename(cmd[0]) == "agy":
            # agy failed to resolve/run → raise FileNotFoundError
            raise FileNotFoundError("agy not found")
        # claude fallback succeeds
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)
    assert result.outcome == "success"
    assert len(seen) == 2, "must attempt agy, then fall back to claude"
    assert os.path.basename(seen[0][0]) == "agy"
    assert os.path.basename(seen[1][0]) == "claude"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "agy"


def test_gate_er_does_not_fall_back_on_real_agy_verdict(tmp_path, monkeypatch):
    """agy runs successfully and emits 'verdict: fail' → this is a real grading,
    not an infra crash. We must NOT fall back to claude (no reviewer-shopping);
    the fail is returned as-is."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    fake_sha = "a" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: fail\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)
    assert result.outcome == "failure"
    assert result.metadata["fallback_used"] == "false"
    assert len(seen) == 1, "real FAIL verdict must not trigger a second backend"
    assert os.path.basename(seen[0][0]) == "agy"
