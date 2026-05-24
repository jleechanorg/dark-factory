"""Tests for orch-qhez: token + cost tracking on codergen subprocesses.

These tests cover the metric parser in isolation (banner / no-banner / total-
only) and an end-to-end integration that runs the engine with the echo backend
and verifies the values land in CXDB metadata.
"""

from __future__ import annotations

import json
import pathlib
import sqlite3
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.cxdb import CXDB
from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY, _codergen_metrics
from runner.healer import _clusters
from runner.parser import parse


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


# ---------------------------------------------------------------------------
# Parser unit tests
# ---------------------------------------------------------------------------


def test_metrics_parser_extracts_known_banner():
    stdout = "doing work...\ninput_tokens=1234 output_tokens=567 cost_usd=0.0421\n"
    m = _codergen_metrics(stdout, "", wall_ms=2500)
    assert m["wall_ms"] == 2500
    assert m["tokens_in"] == 1234
    assert m["tokens_out"] == 567
    assert abs(m["cost_usd"] - 0.0421) < 1e-9


def test_metrics_parser_returns_nulls_without_banner():
    m = _codergen_metrics("nothing relevant here\n", "", wall_ms=42)
    assert m["wall_ms"] == 42
    assert m["tokens_in"] is None
    assert m["tokens_out"] is None
    assert m["cost_usd"] is None
    # No total banner either → no tokens_total key
    assert "tokens_total" not in m


def test_metrics_parser_total_only_falls_back_to_tokens_total():
    m = _codergen_metrics("usage report: total_tokens=900\n", "", wall_ms=10)
    assert m["tokens_in"] is None
    assert m["tokens_out"] is None
    assert m["tokens_total"] == 900


def test_metrics_parser_prefers_final_banner_over_progress():
    stdout = (
        "progress: input_tokens=10 output_tokens=5\n"
        "more work...\n"
        "FINAL: input_tokens=1000 output_tokens=2000 cost_usd=0.50\n"
    )
    m = _codergen_metrics(stdout, "", wall_ms=99)
    assert m["tokens_in"] == 1000
    assert m["tokens_out"] == 2000
    assert m["cost_usd"] == 0.50


def test_metrics_parser_tolerates_thousands_separators():
    m = _codergen_metrics("input_tokens=12,345 output_tokens=6_789\n", "", wall_ms=1)
    assert m["tokens_in"] == 12345
    assert m["tokens_out"] == 6789


# ---------------------------------------------------------------------------
# Integration: echo backend records wall_ms; fake codergen records full banner.
# ---------------------------------------------------------------------------


def test_echo_backend_records_wall_ms_in_cxdb(tmp_path, monkeypatch):
    """The echo backend must still emit a wall_ms metric (even if tiny)."""
    db_path = tmp_path / "cxdb.sqlite"

    # Make the holdout green so the pipeline reaches exit cleanly.
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    graph = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo", cxdb_path=db_path)
    history = run(graph, ctx, max_steps=20)
    assert history[-1].outcome == "success"

    # Inspect CXDB rows directly.
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT node, metadata_json FROM steps WHERE run_id = ? ORDER BY seq",
            (ctx.run_id,),
        ).fetchall()
    finally:
        conn.close()

    codergen_rows = [r for r in rows if r["node"] in {"plan", "implement"}]
    assert codergen_rows, "expected at least one codergen row"
    for r in codergen_rows:
        meta = json.loads(r["metadata_json"])
        assert "wall_ms" in meta, meta
        # echo backend never reports tokens.
        assert meta.get("tokens_in") in (None, "", "None")
        assert meta.get("tokens_out") in (None, "", "None")


def test_fake_codergen_with_banner_lands_in_cxdb_metadata(tmp_path, monkeypatch):
    """A handler stub emitting a token banner round-trips through CXDB.

    Simulates the production claude/codex path without invoking the network.
    """
    db_path = tmp_path / "cxdb.sqlite"

    captured = {}

    def fake_codergen(node, ctx):
        # Mimic the production helper to verify shape.
        from runner.handlers import _codergen_metrics

        metrics = _codergen_metrics(
            "input_tokens=42 output_tokens=21 cost_usd=0.0007\n",
            "",
            wall_ms=137,
        )
        meta = {k: ("" if v is None else str(v)) for k, v in metrics.items()}
        captured["meta"] = meta
        return Result(outcome="success", output="ok", metadata=meta)

    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    graph = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo", cxdb_path=db_path)
    history = run(graph, ctx, max_steps=20)
    assert history[-1].outcome == "success"

    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT node, metadata_json FROM steps WHERE run_id = ? AND node = 'plan'",
            (ctx.run_id,),
        ).fetchall()
    finally:
        conn.close()
    assert rows, "expected the plan codergen row"
    meta = json.loads(rows[0]["metadata_json"])
    assert meta["tokens_in"] == "42"
    assert meta["tokens_out"] == "21"
    assert meta["cost_usd"] == "0.0007"
    assert meta["wall_ms"] == "137"


# ---------------------------------------------------------------------------
# Healer aggregate columns
# ---------------------------------------------------------------------------


def test_healer_clusters_surface_aggregate_metrics(tmp_path):
    """When failure rows carry metric metadata, the Healer must sum them."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        run_id = db.start_run(pipeline="p", goal="g")
        # Two identical failure rows with token + cost metadata.
        for seq in range(2):
            db.record_step(
                run_id=run_id,
                seq=seq,
                node="implement",
                outcome="failure",
                ts=0.0,
                output="boom",
                metadata={
                    "tokens_in": "100",
                    "tokens_out": "50",
                    "cost_usd": "0.01",
                    "wall_ms": "1000",
                },
            )
        db.end_run(run_id, final="failure")
    finally:
        db.close()

    clusters = _clusters(db_path)
    assert len(clusters) == 1
    c = clusters[0]
    assert c.total_tokens == 300  # (100+50) * 2
    assert abs(c.total_cost_usd - 0.02) < 1e-9
    assert c.total_wall_ms == 2000
