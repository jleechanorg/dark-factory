"""Crash-resilience tests for the engine run() loop.

Root cause these guard against: run()'s main loop was wrapped in a bare
try/finally (no except). When a node handler — or the next-node/goal-gate
transition computation — raised, the exception propagated past the loop and
terminated the runner process with NO step recorded, NO graceful exit, and
runs.final left as 'success'. There was also no captured stderr/stdout, so the
crash left no traceback anywhere.

These tests assert the engine instead:
  - records an `error` StepRecord for the node that raised,
  - routes to a fix/retry edge when one exists, else ends gracefully,
  - reports final outcome `error` (and writes runs.final='error' to CXDB),
  - writes a per-run log file to ~/.dark-factory/logs/<run_id>.log.

Run with: source .venv/bin/activate && python -m pytest tests/
"""

from __future__ import annotations

import pathlib
import sqlite3
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


def test_node_exception_is_recorded_not_raised(monkeypatch):
    """A handler that raises must NOT crash run(); it returns with an error step."""

    def boom(node, ctx):
        raise RuntimeError("backend exploded while launching claude")

    # holdout has a `fix` edge in hello.dot — but here implement raises first.
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", boom)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")

    # Must return, not raise.
    history = run(g, ctx, max_steps=50)

    # An error StepRecord was appended for the node that raised (plan/implement).
    error_records = [r for r in history if r.outcome == "error"]
    assert error_records, f"expected an error StepRecord, got {[ (r.node, r.outcome) for r in history]}"
    rec = error_records[0]
    assert rec.node in {"plan", "implement", "fix"}
    # Exception type + message + a traceback fragment land in output_preview.
    assert "RuntimeError" in rec.output_preview
    assert "backend exploded" in rec.output_preview


def test_node_exception_routes_to_fix_edge(monkeypatch):
    """If the raising node has a fix/retry edge, the engine should route there."""
    calls = {"holdout": 0, "fix": 0}

    def exploding_holdout(node, ctx):
        calls["holdout"] += 1
        raise RuntimeError("holdout evaluator crashed")

    def fix_handler(node, ctx):
        calls["fix"] += 1
        return Result(outcome="success", output="fix ran")

    # holdout -> fix [condition="outcome!=success"] exists in hello.dot.
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", exploding_holdout)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fix_handler)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")

    history = run(g, ctx, max_steps=50)

    # holdout raised → error step recorded for holdout, then routed to fix.
    nodes = [r.node for r in history]
    assert "holdout" in nodes
    holdout_idx = nodes.index("holdout")
    assert history[holdout_idx].outcome == "error"
    assert "fix" in nodes[holdout_idx:], f"expected route to fix after holdout crash, got {nodes}"
    assert calls["fix"] >= 1


def test_node_exception_ends_gracefully_without_fix_edge(monkeypatch, tmp_path):
    """A node with no fix/retry edge that raises should end the run with `error`."""
    dot = tmp_path / "linear.dot"
    dot.write_text(
        'digraph linear {\n'
        '  graph [goal="Linear crash test" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  work [type="codergen", label="Work"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> work -> exit\n'
        '}\n'
    )

    def boom(node, ctx):
        raise ValueError("no recovery path")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", boom)
    g = parse(dot)
    ctx = Context(goal="test", workdir=ROOT, backend="echo")

    history = run(g, ctx, max_steps=50)

    assert history[-1].node == "work"
    assert history[-1].outcome == "error"
    assert "ValueError" in history[-1].output_preview


def test_run_final_is_error_in_cxdb_on_crash(monkeypatch, tmp_path):
    """runs.final must be 'error' (never 'success') when a run ends on a crash."""

    def boom(node, ctx):
        raise RuntimeError("engine crash repro")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", boom)
    g = parse(_pipeline("hello.dot"))
    cxdb_path = tmp_path / "crash.sqlite"
    ctx = Context(goal="test", workdir=ROOT, backend="echo", cxdb_path=cxdb_path)

    history = run(g, ctx, max_steps=50)

    assert history[-1].outcome == "error"

    conn = sqlite3.connect(str(cxdb_path))
    try:
        rows = conn.execute(
            "SELECT final FROM runs WHERE run_id = ?", (ctx.run_id,)
        ).fetchall()
    finally:
        conn.close()
    assert rows, "run row missing from CXDB"
    assert rows[0][0] == "error", f"runs.final should be 'error', got {rows[0][0]!r}"


def test_crash_writes_per_run_log_file(monkeypatch, tmp_path):
    """A per-run log file at ~/.dark-factory/logs/<run_id>.log is written."""
    log_root = tmp_path / "logs"
    monkeypatch.setattr("runner.engine._LOG_DIR", log_root)

    def boom(node, ctx):
        raise RuntimeError("traceback should land in the log file")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", boom)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")

    history = run(g, ctx, max_steps=50)

    assert ctx.run_id, "run_id must be set so the log file is addressable"
    log_path = log_root / f"{ctx.run_id}.log"
    assert log_path.exists(), f"expected log at {log_path}"
    contents = log_path.read_text()
    assert "RuntimeError" in contents
    assert "traceback should land in the log file" in contents


def test_normal_run_still_writes_log_file(monkeypatch, tmp_path):
    """Logging is on by default — even a green run leaves a diagnosable log."""
    log_root = tmp_path / "logs"
    monkeypatch.setattr("runner.engine._LOG_DIR", log_root)

    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")

    history = run(g, ctx, max_steps=50)

    assert history[-1].outcome == "success"
    assert ctx.run_id
    log_path = log_root / f"{ctx.run_id}.log"
    assert log_path.exists()
