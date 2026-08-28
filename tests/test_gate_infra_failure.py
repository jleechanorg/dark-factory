"""Reviewer infrastructure-failure handling and ``infra_failure`` tagging.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import os
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def test_codex_infra_fallback_docs_do_not_claim_personal_claude() -> None:
    """The fail-closed codex→agy contract must not drift back to Claude prose."""
    module = sys.modules[__name__]
    stale_names = [
        name
        for name, value in vars(module).items()
        if name.startswith("test_")
        and callable(value)
        and "falls_back_to_claude" in name
    ]
    assert not stale_names, f"stale Claude-fallback test aliases: {stale_names!r}"

    stale_docs = []
    for name, value in vars(module).items():
        if not name.startswith("test_") or not callable(value):
            continue
        doc = (getattr(value, "__doc__", "") or "").lower()
        if "claude fallback" in doc or "-> claude" in doc:
            stale_docs.append(name)
    assert not stale_docs, f"stale Claude-fallback test documentation: {stale_docs!r}"


def test_execute_gate_codex_infra_failure_stops_after_agy(tmp_path, monkeypatch):
    """codex and agy missing → no implicit personal-Claude transport."""
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

    assert result.outcome == "error"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "codex"
    assert result.metadata["reviewer_backend"] == "agy"
    assert os.path.basename(seen[0][0]) == "codex"
    assert any(os.path.basename(c[0]) == "agy" for c in seen), (
        "agy fallback must have been invoked after codex infra failure"
    )
    assert not any(os.path.basename(c[0]) == "claude" for c in seen)


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
        "personal Claude transport must not be invoked since agy succeeded"
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


def test_execute_gate_tags_infra_failure_when_all_backends_die(tmp_path, monkeypatch):
    """codex and its explicit agy fallback time out → ``infra_failure``.

    This distinguishes "no reviewer ever graded the diff" from a real FAIL
    without introducing an implicit personal-Claude transport.
    """
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
    assert not any(os.path.basename(c[0]) == "claude" for c in seen)
