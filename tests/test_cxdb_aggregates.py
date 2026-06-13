"""Tests for :mod:`runner.cxdb` metric coercion and cluster aggregation."""

from __future__ import annotations

import pathlib

import pytest

from runner.cxdb import CXDB, _coerce_metric


# ---------------------------------------------------------------------------
# _coerce_metric — pure function
# ---------------------------------------------------------------------------


def test_coerce_metric_int_succeeds_on_numeric_string() -> None:
    """A numeric string coerces to int."""
    assert _coerce_metric("10", int) == 10


def test_coerce_metric_int_succeeds_on_actual_int() -> None:
    """An actual int passes through unchanged (int(int) is a no-op)."""
    assert _coerce_metric(10, int) == 10


def test_coerce_metric_int_returns_none_on_garbage() -> None:
    """A non-numeric string returns None — no exception leaks."""
    assert _coerce_metric("bad", int) is None


def test_coerce_metric_int_returns_none_on_empty_string() -> None:
    """Empty string is the canonical 'absent' value and returns None."""
    assert _coerce_metric("", int) is None


def test_coerce_metric_int_returns_none_on_none() -> None:
    """None is the canonical 'absent' value and returns None."""
    assert _coerce_metric(None, int) is None


def test_coerce_metric_float_accepts_decimal_string() -> None:
    """A decimal string coerces to float for the cost_usd key."""
    assert _coerce_metric("0.25", float) == 0.25


def test_coerce_metric_float_returns_none_on_non_numeric() -> None:
    """A non-numeric string returns None — no exception leaks."""
    assert _coerce_metric("x", float) is None


@pytest.mark.parametrize("garbage", [{}, [], object()])
def test_coerce_metric_does_not_raise_on_non_scalar(garbage: object) -> None:
    """Non-scalar input goes through the ``except`` path and returns None.

    Guards against the regression where ``int(dict)`` would raise
    ``TypeError`` and leak out of ``cluster_aggregates`` if a
    future CXDB writer stored a structured value under
    ``tokens_in`` or similar. The current contract is "never
    raise" — pin it down.

    Note: ``True`` / ``False`` are intentionally excluded — Python's
    ``int(True) == 1`` is well-defined behaviour, not garbage.
    """
    assert _coerce_metric(garbage, int) is None


# ---------------------------------------------------------------------------
# cluster_aggregates — integration with CXDB rows
# ---------------------------------------------------------------------------


def _seed_run(db: CXDB, rows: list[dict]) -> str:
    """Insert ``rows`` (list of metadata dicts) as steps of one run.

    All rows share (node, outcome) so a single ``cluster_aggregates``
    call returns the sum of all of them. Each row gets a unique
    ``output`` so ``output_hash`` is per-row, but ``cluster_aggregates``
    groups by hash — so we instead call it once per row and sum the
    results. (See ``test_cluster_aggregates_sums_*`` for that pattern.)
    """
    rid = db.start_run(pipeline="p", goal="g")
    for seq, meta in enumerate(rows):
        db.record_step(
            rid,
            seq,
            "impl",
            "failure",
            float(seq),
            f"out_{seq}",  # unique output → unique output_hash
            meta,
        )
    db.end_run(rid, "failure")
    return rid


def test_cluster_aggregates_sums_valid_metrics_and_skips_garbage(tmp_path: pathlib.Path) -> None:
    """A 3-row cluster with a mix of valid + garbage metrics produces the manual sum.

    Row 0 — all valid.
    Row 1 — empty string + None (the "absent" sentinels).
    Row 2 — garbage (unparseable string).
    Expected totals: tokens = 10 + 0 + 0 = 10 (row 1's None skipped, row 2's
    "bad" skipped), cost = 0.5 + 0 + 0 = 0.5, wall = 100 + 0 + 0 = 100.
    """
    db = CXDB(tmp_path / "cxdb.sqlite")
    try:
        rid = _seed_run(
            db,
            [
                {"tokens_in": "10", "cost_usd": "0.5", "wall_ms": "100"},
                {"tokens_in": None, "cost_usd": "", "wall_ms": ""},
                {"tokens_in": "bad", "cost_usd": "not-a-float", "wall_ms": "x"},
            ],
        )
        # Each row has a unique output_hash, so sum per-row and add.
        conn = db._conn
        hashes = [
            r[0]
            for r in conn.execute(
                "SELECT output_hash FROM steps WHERE run_id = ? ORDER BY seq",
                (rid,),
            ).fetchall()
        ]
        merged: dict = {
            "total_tokens": 0,
            "total_cost_usd": 0.0,
            "total_wall_ms": 0,
        }
        for h in hashes:
            row_agg = db.cluster_aggregates("impl", "failure", h)
            merged["total_tokens"] += row_agg["total_tokens"] or 0
            merged["total_cost_usd"] += row_agg["total_cost_usd"] or 0.0
            merged["total_wall_ms"] += row_agg["total_wall_ms"] or 0
        assert merged == {"total_tokens": 10, "total_cost_usd": 0.5, "total_wall_ms": 100}
    finally:
        db.close()


def test_cluster_aggregates_returns_none_for_total_when_all_garbage(tmp_path: pathlib.Path) -> None:
    """When every row's metric is unparseable, the total is ``None`` (saw_any=False)."""
    db = CXDB(tmp_path / "cxdb.sqlite")
    try:
        rid = _seed_run(
            db,
            [
                {"tokens_in": "bad", "cost_usd": "x", "wall_ms": "y"},
                {"tokens_in": None, "cost_usd": "", "wall_ms": ""},
            ],
        )
        conn = db._conn
        hashes = [
            r[0]
            for r in conn.execute(
                "SELECT output_hash FROM steps WHERE run_id = ? ORDER BY seq",
                (rid,),
            ).fetchall()
        ]
        for h in hashes:
            agg = db.cluster_aggregates("impl", "failure", h)
            assert agg == {"total_tokens": None, "total_cost_usd": None, "total_wall_ms": None}
    finally:
        db.close()


def test_cluster_aggregates_sums_all_three_token_keys(tmp_path: pathlib.Path) -> None:
    """All three token keys (in/out/total) contribute to ``total_tokens``."""
    db = CXDB(tmp_path / "cxdb.sqlite")
    try:
        rid = db.start_run(pipeline="p", goal="g")
        db.record_step(
            rid,
            0,
            "impl",
            "failure",
            0.0,
            "out",
            {"tokens_in": "3", "tokens_out": "7", "tokens_total": "10"},
        )
        db.end_run(rid, "failure")
        h = db._conn.execute("SELECT output_hash FROM steps LIMIT 1").fetchone()[0]
        agg = db.cluster_aggregates("impl", "failure", h)
        # 3 (in) + 7 (out) + 10 (total) = 20
        assert agg["total_tokens"] == 20
    finally:
        db.close()


def test_cluster_aggregates_keeps_distinct_metric_kinds_independent(tmp_path: pathlib.Path) -> None:
    """A garbage ``cost_usd`` does not poison the integer sums and vice versa.

    Guards against a future regression where one metric's coercion
    failure would taint another metric's accumulator.

    Note: ``total_cost_usd`` is the per-key accumulator (initialised
    to ``0.0``). The function returns ``total_cost_usd`` whenever
    ``saw_any`` is True (which any valid metric in any row
    triggers). So a row with valid tokens + valid wall but garbage
    cost returns ``0.0`` for cost, not ``None`` — the "any saw" gate
    is per-run, not per-key. This is pre-existing behaviour
    inherited from the original implementation; pin it down so a
    future per-key gate is a conscious decision.
    """
    db = CXDB(tmp_path / "cxdb.sqlite")
    try:
        rid = db.start_run(pipeline="p", goal="g")
        db.record_step(
            rid,
            0,
            "impl",
            "failure",
            0.0,
            "out",
            {"tokens_in": "10", "cost_usd": "bad", "wall_ms": "100"},
        )
        db.end_run(rid, "failure")
        h = db._conn.execute("SELECT output_hash FROM steps LIMIT 1").fetchone()[0]
        agg = db.cluster_aggregates("impl", "failure", h)
        assert agg == {"total_tokens": 10, "total_cost_usd": 0.0, "total_wall_ms": 100}
    finally:
        db.close()
