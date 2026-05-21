"""Regression tests for iteration-2 hardening fixes.

Covers:
  - _parse_verdict marker discipline + standalone fallback
  - CXDB WAL pragma + concurrent-writer survival
  - engine._attr_int defensive default on bad attrs
  - engine.run records runs.ended_ts via try/finally even on stuck pipelines
"""

from __future__ import annotations

import pathlib
import sqlite3
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.cxdb import CXDB  # noqa: E402
from runner.engine import _attr_int, run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY, _parse_verdict  # noqa: E402
from runner.parser import Node, parse  # noqa: E402


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


# ---------------------------------------------------------------------------
# _parse_verdict
# ---------------------------------------------------------------------------

def test_parse_verdict_ignores_compound_text():
    """A marker line whose value is not a verdict token must not collapse via
    the fallback into the embedded word ('fail' inside 'not a fail')."""
    raw, norm = _parse_verdict("verdict: not a fail")
    assert (raw, norm) != ("fail", "failure"), (
        "marker regex should require verdict:<TOKEN> and the fallback should "
        "not lift the embedded 'fail' out of compound prose"
    )

    raw2, norm2 = _parse_verdict("passes warnings cleanly")
    assert (raw2, norm2) != ("pass", "success"), (
        "no marker present; 'passes' is not a standalone PASS token"
    )


def test_parse_verdict_picks_last_marker():
    """If multiple VERDICT: lines appear, the last one wins."""
    text = "VERDICT: FAIL\nstuff happens\nVERDICT: PASS\n"
    raw, norm = _parse_verdict(text)
    assert raw == "pass"
    assert norm == "success"


def test_parse_verdict_standalone_fallback():
    """No explicit marker but tail contains a bare verdict token on its own line → success."""
    body = "noise line 1\nnoise line 2\nPASS\n"
    raw, norm = _parse_verdict(body)
    assert norm == "success"
    assert raw == "pass"


def test_tool_handler_tolerates_bad_timeout(tmp_path, monkeypatch):
    """`_tool` must not crash when a .dot author writes `timeout="abc"`.

    Also verifies the command actually ran and its output flowed through:
    the previous version of this test only checked the outcome was in a
    set that included both success and failure, so a no-op would have
    passed it silently.
    """
    from runner.handlers import _tool, Context, Result
    from runner.parser import Node

    node = Node(name="t", attrs={"command": "echo hi", "timeout": "not-a-number"})
    ctx = Context(goal="t", workdir=tmp_path, backend="echo")
    result = _tool(node, ctx)
    assert isinstance(result, Result)
    assert result.outcome == "success", f"unexpected outcome: {result.outcome}"
    assert "hi" in result.output, f"command output missing: {result.output!r}"
    assert result.metadata.get("returncode") == "0", result.metadata


def test_parse_verdict_marker_invalid_token_does_not_fall_back():
    """If a `verdict:` marker exists with an invalid token, refuse to guess.

    Prevents "verdict: not a fail" from being misclassified as a fail verdict
    via the standalone fallback grabbing "fail" out of "not a fail".
    """
    raw, norm = _parse_verdict("verdict: not a fail")
    assert raw == "unknown"
    assert norm == "failure"


# ---------------------------------------------------------------------------
# CXDB pragmas + concurrent writes
# ---------------------------------------------------------------------------

def test_cxdb_wal_pragma(tmp_path):
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        mode = db._conn.execute("PRAGMA journal_mode").fetchone()[0]
    finally:
        db.close()
    assert mode.lower() == "wal", f"expected WAL journal_mode, got {mode!r}"


def test_cxdb_concurrent_writes(tmp_path):
    """Two CXDB instances on the same file must both write without
    'database is locked' — the busy_timeout PRAGMA absorbs brief contention."""
    db_path = tmp_path / "cxdb.sqlite"
    db_a = CXDB(db_path)
    db_b = CXDB(db_path)
    try:
        run_a = db_a.start_run(pipeline="p", goal="g")
        run_b = db_b.start_run(pipeline="p", goal="g")
        db_a.record_step(
            run_id=run_a, seq=0, node="n", outcome="success",
            ts=0.0, output="hello", metadata={},
        )
        # If WAL+busy_timeout is missing this raises sqlite3.OperationalError.
        db_b.record_step(
            run_id=run_b, seq=0, node="n", outcome="success",
            ts=0.0, output="world", metadata={},
        )
    finally:
        db_a.close()
        db_b.close()

    # Verify both rows landed.
    conn = sqlite3.connect(str(db_path))
    try:
        n = conn.execute("SELECT COUNT(*) FROM steps").fetchone()[0]
    finally:
        conn.close()
    assert n == 2


# ---------------------------------------------------------------------------
# engine._attr_int defensive default
# ---------------------------------------------------------------------------

def test_attr_int_fallback():
    node = Node(name="x", attrs={"max_visits": "bad"})
    assert _attr_int(node, "max_visits", 0) == 0
    # Empty string also falls back.
    node2 = Node(name="x", attrs={"max_visits": ""})
    assert _attr_int(node2, "max_visits", 7) == 7
    # Missing key falls back.
    assert _attr_int(Node(name="x", attrs={}), "max_visits", 5) == 5
    # Valid int parses normally.
    assert _attr_int(Node(name="x", attrs={"max_visits": "3"}), "max_visits", 0) == 3


# ---------------------------------------------------------------------------
# engine.run finally-block CXDB closure on stuck pipelines
# ---------------------------------------------------------------------------

def test_engine_records_finally_on_stuck(monkeypatch, tmp_path):
    """When the engine hits a 'stuck' state (no outgoing edge matches),
    the run must still be closed: runs.ended_ts non-null."""
    # Holdout returns an outcome that neither edge condition matches —
    # hello.dot only has condition=outcome=success and outcome!=success,
    # so to force "stuck" we patch _pick_next via TYPE_REGISTRY ... actually
    # outcome!=success covers everything-not-success. Easier: emit an outcome
    # that matches *no* edge by making holdout the terminal and removing edges.
    #
    # Cheapest path: use a stand-in handler whose result has no matching edge
    # by patching the registry to mark the FIRST node 'plan' as a type that
    # returns outcome="weird", and craft a pipeline where 'plan' has only a
    # conditional edge that does not match.
    #
    # Use hello.dot but force `implement` to return outcome="weird"; the only
    # outgoing edge implement->holdout is unconditional so it would still match.
    # Trick: hello.dot's holdout has two conditional edges — both branches are
    # covered. So we synthesize a tiny pipeline in tmp_path that has only a
    # conditional outgoing edge from 'plan' that requires outcome=success, and
    # have plan return outcome="weird".

    dot = tmp_path / "stuck.dot"
    dot.write_text(
        'digraph stuck {\n'
        '  graph [goal="stuck" label="stuck"]\n'
        '  start [shape=Mdiamond label="Start"]\n'
        '  exit  [shape=Msquare label="Exit"]\n'
        '  plan  [type="codergen" label="Plan" prompt="@nope.md"]\n'
        '  start -> plan\n'
        '  plan  -> exit [condition="outcome=success"]\n'
        '}\n'
    )

    def weird_plan(node, ctx):
        return Result(outcome="weird", output="no edge matches me")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", weird_plan)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(dot)
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    history = run(g, ctx, max_steps=10)

    assert any(r.outcome == "stuck" for r in history), \
        f"expected a 'stuck' step, got {[r.outcome for r in history]}"

    # The finally block must have flushed runs.ended_ts even though the loop
    # broke via the stuck branch (not via 'exit').
    conn = sqlite3.connect(str(db_path))
    try:
        row = conn.execute(
            "SELECT ended_ts, final FROM runs"
        ).fetchone()
    finally:
        conn.close()
    assert row is not None, "no run row recorded"
    ended_ts, final = row
    assert ended_ts is not None, "runs.ended_ts is NULL — finally block did not fire"
    assert final == "stuck", f"expected final='stuck', got {final!r}"
