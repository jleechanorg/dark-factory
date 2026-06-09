"""Gate handler + CXDB + Healer smoke tests."""

from __future__ import annotations

import pathlib
import stat
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.cxdb import CXDB  # noqa: E402
from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.handlers import _parse_verdict  # noqa: E402
from runner.healer import report  # noqa: E402
from runner.parser import parse  # noqa: E402


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


def test_parse_verdict_pass_warn_fail():
    assert _parse_verdict("blah\nVERDICT: PASS\n")[1] == "success"
    assert _parse_verdict("Overall: WARN — minor")[1] == "success"
    assert _parse_verdict("verdict: FAIL")[1] == "failure"
    assert _parse_verdict("Verdict: PARTIAL")[1] == "failure"
    assert _parse_verdict("verdict: INCONCLUSIVE")[1] == "failure"
    # Standalone-line fallback fires when no marker is present.
    assert _parse_verdict("everything is fine\nPASS\n")[1] == "success"
    # Prose that contains the word "pass" inside another phrase is NOT a verdict.
    assert _parse_verdict("everything is fine\nresult: pass needed")[1] == "failure"


def test_gate_echo_seeded_outcome(monkeypatch):
    """Gate handlers in echo mode pull outcome from ctx.state."""
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "success"
    ctx.state["gate_cs.outcome"] = "success"

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    assert nodes == ["start", "holdout", "gate_es", "gate_er", "gate_cs", "exit"]
    assert history[-1].outcome == "success"


def test_gate_failure_short_circuits(monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "failure"  # fail at /er
    ctx.state["gate_cs.outcome"] = "success"

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    assert nodes == ["start", "holdout", "gate_es", "gate_er", "exit"]
    assert history[-1].outcome == "failure"


def test_gate_nonzero_returncode_cannot_spoof_pass(monkeypatch, tmp_path):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    claude = bin_dir / "claude"
    claude.write_text("#!/bin/sh\nprintf 'VERDICT: PASS\\n'\nexit 19\n")
    claude.chmod(claude.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("PATH", f"{bin_dir}:{pathlib.Path('/usr/bin')}")

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="claude")

    history = run(g, ctx, max_steps=20)

    assert history[-1].outcome != "success"
    assert any(r.node == "gate_es" and r.outcome == "error" for r in history)


def test_cxdb_records_steps(tmp_path, monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    ctx.state.update(
        {"gate_es.outcome": "success", "gate_er.outcome": "success", "gate_cs.outcome": "success"}
    )
    run(g, ctx, max_steps=20)
    assert db_path.exists()

    db = CXDB(db_path)
    rows = list(db._conn.execute("SELECT node FROM steps ORDER BY seq").fetchall())
    db.close()
    assert [r[0] for r in rows] == [
        "start",
        "holdout",
        "gate_es",
        "gate_er",
        "gate_cs",
        "exit",
    ]


def test_healer_reports_failures(tmp_path, monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="fail", output="boom")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    run(g, ctx, max_steps=20)

    text = report(db_path)
    assert "holdout" in text
    assert "fail" in text.lower()
    assert "Prescription" in text or "prescription" in text.lower()


def test_healer_reports_gate_infra_errors(tmp_path, monkeypatch):
    """Gate infra errors are terminal failures and must be diagnosable."""
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=tmp_path / "cxdb.sqlite")
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "error"

    history = run(g, ctx, max_steps=20)
    assert any(r.outcome == "error" for r in history)

    text = report(ctx.cxdb_path)
    assert "gate_er" in text
    assert "error" in text.lower()


def test_healer_no_failures(tmp_path, monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    ctx.state.update(
        {"gate_es.outcome": "success", "gate_er.outcome": "success", "gate_cs.outcome": "success"}
    )
    run(g, ctx, max_steps=20)

    text = report(db_path)
    assert "Nothing to diagnose" in text


# ---------------------------------------------------------------------------
# Bug fix: gate handlers must fall back to universal prompt when local command
# file is absent from the workdir's .claude/commands/ directory.
# ---------------------------------------------------------------------------

def test_gate_es_uses_universal_prompt_when_local_es_md_absent(tmp_path, monkeypatch):
    """_gate_es must fall back to the embedded universal prompt when
    .claude/commands/es.md is absent from the workdir.

    RED: current code is `_gate_es = _slash_gate("es")` which always builds
    a "/es ..." prompt regardless of whether es.md exists locally.

    GREEN: _gate_es checks for local es.md; when absent it calls
    _run_universal_prompt_gate with UNIVERSAL_EVIDENCE_REVIEW_PROMPT.
    """
    import subprocess as _sp
    from runner.handlers import _gate_es, Context as HCtx

    node = type("Node", (), {"name": "gate_es", "attrs": {}, "shape": ""})()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    # tmp_path has no .claude/commands/es.md
    assert not (tmp_path / ".claude" / "commands" / "es.md").exists()

    called_prompts: list[str] = []

    fake_sha = "a" * 40

    def _fake_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_es(node, ctx)
    assert result.outcome == "success"
    assert called_prompts, "subprocess.run must have been called"

    prompt_used = called_prompts[0]
    # Universal prompt path: starts with "You are performing..." not "/es "
    assert not prompt_used.startswith("/es "), (
        f"When es.md is absent, _gate_es must use universal prompt, not /es slash. "
        f"Got prompt starting with: {prompt_used[:60]!r}"
    )


def test_gate_code_standards_uses_universal_prompt_when_local_file_absent(tmp_path, monkeypatch):
    """_gate_code_standards must fall back to embedded prompt when
    .claude/commands/code-standards.md is absent from workdir.

    RED: current code is `_gate_code_standards = _slash_gate("code-standards")`
    which always invokes /code-standards regardless of file presence.

    GREEN: _gate_code_standards checks for local code-standards.md and falls
    back to UNIVERSAL_CODE_STANDARDS_PROMPT when absent.
    """
    import subprocess as _sp
    from runner.handlers import _gate_code_standards, Context as HCtx

    node = type("Node", (), {"name": "gate_cs", "attrs": {}, "shape": ""})()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    assert not (tmp_path / ".claude" / "commands" / "code-standards.md").exists()

    called_prompts: list[str] = []
    fake_sha = "b" * 40

    def _fake_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_code_standards(node, ctx)
    assert result.outcome == "success"
    assert called_prompts

    prompt_used = called_prompts[0]
    assert not prompt_used.startswith("/code-standards "), (
        f"When code-standards.md is absent, _gate_code_standards must use "
        f"universal prompt. Got: {prompt_used[:60]!r}"
    )


# ---------------------------------------------------------------------------
# agy reviewer backend + claude fallback (review_pr.dot evidence gate).
#
# A reviewer gate node with backend="agy" (explicit or via a .review model
# stylesheet) must (a) actually invoke agy, (b) fall back to claude only on agy
# *infrastructure* failure, and (c) NEVER reviewer-shop a real agy fail/partial
# verdict onto claude. These three properties are what make "agy reviews, falls
# back to claude if it doesn't work" honest rather than a label.
# ---------------------------------------------------------------------------

def _agy_gate_node():
    return type("Node", (), {"name": "evidence", "attrs": {"backend": "agy"}, "shape": ""})()


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
