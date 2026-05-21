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
