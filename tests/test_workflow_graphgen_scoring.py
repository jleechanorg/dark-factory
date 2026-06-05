"""Unit tests for workflow_graphgen scoring: token parity + §11.4 aggregation."""

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from benchmarks.workflow_graphgen.scoring import (  # noqa: E402
    TOKEN_FIELDS,
    aggregate_axis,
    read_token_record,
    sum_token_records,
)


def _rec(feature, mode, *, tok_in=None, tok_out=None, gq=None, zt=None, conf=None):
    return {
        "feature": feature,
        "mode": mode,
        "tokens": {"tokens_in": tok_in, "tokens_out": tok_out, "wall_ms": 1},
        "graph_quality": {"score": gq},
        "zero_touch": zt,
        "conformance": conf or {"available": False},
    }


def test_token_fields_are_the_identical_triple():
    assert TOKEN_FIELDS == ("tokens_in", "tokens_out", "wall_ms")
    # Both modes read these exact fields (parity guard).
    md = {"tokens_in": "100", "tokens_out": "20", "wall_ms": 5, "cost_usd": "0.1"}
    rec = read_token_record(md)
    assert set(rec) == set(TOKEN_FIELDS)
    assert rec == {"tokens_in": 100, "tokens_out": 20, "wall_ms": 5}


def test_sum_token_records_none_vs_zero():
    summed = sum_token_records([
        {"tokens_in": 10, "tokens_out": None, "wall_ms": 1},
        {"tokens_in": 5, "tokens_out": None, "wall_ms": 2},
    ])
    assert summed["tokens_in"] == 15
    assert summed["tokens_out"] is None  # all None -> None, not 0
    assert summed["wall_ms"] == 3


def test_aggregate_tokens_separated_lower_is_better():
    # A's middle is consistently cheaper than A+B's, ranges disjoint, and BOTH
    # modes have >= MIN_N_FOR_WINNER trials -> A is credited the winner.
    records = []
    for i in range(5):
        records.append(_rec("roman", "A", tok_in=100 + i, tok_out=10))
        records.append(_rec("roman", "A+B", tok_in=300 + i, tok_out=50))
    agg = aggregate_axis(records, "roman", "tokens_total")
    assert agg["result"] == "separated"
    assert agg["winner"] == "A"


def test_aggregate_separated_but_underpowered_at_low_n():
    # Disjoint ranges but only 2 trials each -> apparent direction reported, NO
    # winner credited (guards against n=1/n=2 noise crowning a false winner).
    records = [
        _rec("roman", "A", tok_in=100, tok_out=10),
        _rec("roman", "A", tok_in=110, tok_out=10),
        _rec("roman", "A+B", tok_in=300, tok_out=50),
        _rec("roman", "A+B", tok_in=320, tok_out=50),
    ]
    agg = aggregate_axis(records, "roman", "tokens_total")
    assert agg["winner"] is None
    assert agg["apparent_winner"] == "A"
    assert "underpowered" in agg["result"]


def test_aggregate_overlap_reports_no_separation():
    records = [
        _rec("hello", "A", gq=80.0),
        _rec("hello", "A", gq=95.0),
        _rec("hello", "A+B", gq=85.0),  # overlaps A's [80,95]
        _rec("hello", "A+B", gq=90.0),
    ]
    agg = aggregate_axis(records, "hello", "graph_quality")
    assert agg["winner"] is None
    assert agg["result"].startswith("no separation at n=")


def test_aggregate_graph_quality_higher_is_better_separated():
    records = []
    for i in range(5):
        records.append(_rec("hello", "A", gq=60.0 + i))
        records.append(_rec("hello", "A+B", gq=90.0 + i))
    agg = aggregate_axis(records, "hello", "graph_quality")
    assert agg["result"] == "separated"
    assert agg["winner"] == "A+B"  # higher score wins


def test_aggregate_conformance_unavailable_is_insufficient():
    records = [_rec("hello", "A"), _rec("hello", "A+B")]
    agg = aggregate_axis(records, "hello", "conformance")
    assert agg["winner"] is None
    assert agg["result"] == "insufficient data"
