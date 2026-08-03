"""Universal infra fallback (codex/minimax/etc → claude) + infra_failure tag.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import os
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def test_execute_gate_codex_infra_failure_falls_back_to_claude(tmp_path, monkeypatch):
    """codex and agy missing → claude fallback, recorded in metadata."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "f" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        name = os.path.basename(cmd[0])
        if name in ("codex", "agy"):
            raise FileNotFoundError(f"{name}: command not found")
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "evidence", "codex")

    assert result.outcome == "success"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "codex"
    assert result.metadata["reviewer_backend"] == "claude"
    assert os.path.basename(seen[0][0]) == "codex"
    assert any(os.path.basename(c[0]) == "agy" for c in seen), (
        "agy fallback must have been invoked after codex infra failure"
    )
    assert any(os.path.basename(c[0]) == "claude" for c in seen), (
        "claude fallback must have been invoked after agy infra failure"
    )


def test_execute_gate_codex_infra_failure_falls_back_to_agy(tmp_path, monkeypatch):
    """codex missing (FileNotFoundError) → agy fallback succeeds, recorded in metadata."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "f" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        if os.path.basename(cmd[0]) == "codex":
            raise FileNotFoundError("codex: command not found")
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "evidence", "codex")

    assert result.outcome == "success"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "codex"
    assert result.metadata["reviewer_backend"] == "agy"
    assert os.path.basename(seen[0][0]) == "codex"
    assert any(os.path.basename(c[0]) == "agy" for c in seen), (
        "agy fallback must have been invoked after codex infra failure"
    )
    assert not any(os.path.basename(c[0]) == "claude" for c in seen), (
        "claude fallback must not have been invoked since agy succeeded"
    )


def test_execute_gate_codex_real_fail_not_retried(tmp_path, monkeypatch):
    """A genuine codex `verdict: fail` (matching SHA) is kept — no reviewer-shopping."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "a" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        return _sp.CompletedProcess(
            cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: fail\n", stderr=""
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "evidence", "codex")

    assert result.outcome == "failure"
    assert result.metadata["fallback_used"] == "false"
    assert len(seen) == 1, "real FAIL verdict must not trigger a second backend"
    assert os.path.basename(seen[0][0]) == "codex"


def test_execute_gate_controller_codex_infra_failure_is_returned_unchanged(
    tmp_path, monkeypatch,
):
    """Cold-review Codex transport failures never enter generic fallback."""
    from runner.handler_core import Result
    from runner.handlers import _execute_gate, Context as HCtx

    calls: list[str] = []
    original = Result(
        outcome="error",
        output="codex transport unavailable",
        metadata={"verdict": "unknown", "backend_missing": "true"},
    )

    def fake_run_gate_once(backend, *_args, **_kwargs):
        calls.append(backend)
        return original

    monkeypatch.setattr(
        "runner.handler_dispatch._run_gate_once", fake_run_gate_once,
    )
    ctx = HCtx(goal="test controller fallback", workdir=tmp_path, backend="codex")
    ctx.state["_df_controller_review_json"] = "true"

    result = _execute_gate("PROMPT", "c" * 40, 300, ctx, "cold_reviewer", "codex")

    assert result is original
    assert calls == ["codex"]
    assert result.metadata == {"verdict": "unknown", "backend_missing": "true"}


def test_execute_gate_tags_infra_failure_when_all_backends_die(tmp_path, monkeypatch):
    """codex times out AND the claude fallback times out → verdict: infra_failure,
    so the operator can tell 'no reviewer ever graded the diff' from a real FAIL."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "b" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        raise _sp.TimeoutExpired(cmd, 300, output=b"partial", stderr=None)

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "evidence", "codex")

    assert result.outcome == "failure"
    assert result.metadata["verdict"] == "infra_failure"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "codex"
    assert os.path.basename(seen[0][0]) == "codex"
    assert any(os.path.basename(c[0]) == "claude" for c in seen), (
        "claude fallback must have been invoked after codex timeout"
    )
