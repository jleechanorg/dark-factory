"""Engine-level integration smoke: run + cxdb + healer + echo-seeded outcomes.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402

from runner.cxdb import CXDB  # noqa: E402
from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.healer import report  # noqa: E402
from runner.parser import parse  # noqa: E402


def test_holdout_missing_feature_handoff_reaches_fix_prompt(tmp_path):
    fix_prompt = tmp_path / "fix.md"
    fix_prompt.write_text(
        "fix sees:\n${state._last_output}\n",
        encoding="utf-8",
    )
    dot = tmp_path / "missing_feature_handoff.dot"
    dot.write_text(
        "digraph missing_feature_handoff {\n"
        "  start [shape=Mdiamond]\n"
        "  holdout [type=\"holdout_eval\"]\n"
        f"  fix [type=\"codergen\", backend=\"echo\", prompt=\"@{fix_prompt}\", max_visits=\"1\"]\n"
        "  exit [shape=Msquare]\n"
        "  start -> holdout\n"
        "  holdout -> fix [condition=\"outcome!=success\"]\n"
        "  fix -> exit\n"
        "}\n",
        encoding="utf-8",
    )

    history = run(parse(dot), Context(goal="validate evidence", workdir=tmp_path, backend="echo"), max_steps=6)
    fix = next(step for step in history if step.node == "fix")

    assert "holdout_eval is missing required feature metadata" in fix.output_preview
    assert "no feature attribute or state" not in fix.output_preview


def test_gate_echo_seeded_outcome(monkeypatch):
    """Gate handlers in echo mode pull outcome from ctx.state."""
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("gates.dot"))
    g.nodes["adversarial_reviewer"].attrs["test_fixture"] = "true"
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state.update(
        {
            "_df_controller_fixture": "cold-review-v1",
            "gate_skeptic.outcome": "success",
            "adversarial_reviewer.outcome": "success",
            "gate_es.outcome": "success",
            "gate_er.outcome": "success",
            "gate_cs.outcome": "success",
        }
    )

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    # `__run_end__` is a synthetic CXDB step (P-B) that surfaces the
    # uncommitted-state snapshot; it is NOT appended to the in-memory
    # `history` list, so the assertion here stays on the original 8.
    assert nodes == [
        "start",
        "holdout",
        "gate_skeptic",
        "adversarial_reviewer",
        "gate_es",
        "gate_er",
        "gate_cs",
        "web_advice",
        "exit",
    ]
    assert history[-1].outcome == "success"


def test_cxdb_records_steps(tmp_path, monkeypatch):
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    ctx.state.update(
        {
            "gate_skeptic.outcome": "success",
            "adversarial_reviewer.outcome": "success",
            "gate_es.outcome": "success",
            "gate_er.outcome": "success",
            "gate_cs.outcome": "success",
        }
    )
    run(g, ctx, max_steps=20)
    assert db_path.exists()

    db = CXDB(db_path)
    rows = list(db._conn.execute("SELECT node FROM steps ORDER BY seq").fetchall())
    db.close()
    # P-B adds a synthetic `__run_end__` step carrying the uncommitted-state
    # metadata so the Healer (P-C) can sub-cluster exhausted runs with vs
    # without work sitting in the worktree. Real .dot nodes always come first.
    assert [r[0] for r in rows] == [
        "start",
        "holdout",
        "gate_skeptic",
        "adversarial_reviewer",
        "gate_es",
        "gate_er",
        "gate_cs",
        "web_advice",
        "exit",
        "__run_end__",
    ]


def test_healer_reports_failures(tmp_path, monkeypatch):
    def fake_holdout(node, ctx):
        return Result(outcome="fail", output="boom")
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
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=tmp_path / "cxdb.sqlite")
    ctx.state["gate_skeptic.outcome"] = "success"
    ctx.state["adversarial_reviewer.outcome"] = "success"
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "error"

    history = run(g, ctx, max_steps=20)
    assert any(r.outcome == "error" for r in history)

    text = report(ctx.cxdb_path)
    assert "gate_er" in text
    assert "error" in text.lower()


def test_healer_no_failures(tmp_path, monkeypatch):
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    ctx.state.update(
        {
            "gate_skeptic.outcome": "success",
            "adversarial_reviewer.outcome": "success",
            "gate_es.outcome": "success",
            "gate_er.outcome": "success",
            "gate_cs.outcome": "success",
        }
    )
    run(g, ctx, max_steps=20)

    text = report(db_path)
    assert "Nothing to diagnose" in text
