"""Measurement-instrument tests for the workflow_graphgen A-vs-A+B benchmark.

Covers the three pieces that make the benchmark a *real* measurement rather than
an n=1 point estimate:
  * the self-contained conformance evaluator (lights up the conformance axis for
    the public hello/roman acceptance criteria),
  * the aggregator's minimum-n guard (refuses to crown a winner on too few
    trials) plus per-mode variance stats and the wall_ms axis,
  * the prompt-parity invariant (plan/implement templates interpolate only
    ${goal}, so the coder prompt is byte-identical across modes -> the token axis
    measures the model, not the dispatch path).
"""

from __future__ import annotations

import pathlib

ROOT = pathlib.Path(__file__).parent.parent

from benchmarks.workflow_graphgen.conformance_local import evaluate, feature_total
from benchmarks.workflow_graphgen.scoring import (
    MIN_N_FOR_WINNER,
    aggregate,
    aggregate_axis,
)


# ---------------------------------------------------------------------------
# Conformance evaluator
# ---------------------------------------------------------------------------

_GOOD_HELLO = (
    "import sys\n"
    "def hello(name):\n"
    "    if not name or not name.strip():\n"
    "        return 'Hello, World!'\n"
    "    return f'Hello, {name}!'\n"
)

_GOOD_ROMAN = (
    "def to_roman(n):\n"
    "    if not isinstance(n, int) or isinstance(n, bool) or not (1 <= n <= 3999):\n"
    "        raise ValueError('out of range')\n"
    "    table = [(1000,'M'),(900,'CM'),(500,'D'),(400,'CD'),(100,'C'),(90,'XC'),\n"
    "             (50,'L'),(40,'XL'),(10,'X'),(9,'IX'),(5,'V'),(4,'IV'),(1,'I')]\n"
    "    out = ''\n"
    "    for v, s in table:\n"
    "        while n >= v:\n"
    "            out += s; n -= v\n"
    "    return out\n"
)


def test_conformance_all_pass_on_correct_hello(tmp_path):
    (tmp_path / "hello.py").write_text(_GOOD_HELLO)
    res = evaluate(tmp_path, "hello")
    assert res["total"] == feature_total("hello")
    assert res["pass"] == res["total"]


def test_conformance_all_pass_on_correct_roman(tmp_path):
    (tmp_path / "roman.py").write_text(_GOOD_ROMAN)
    res = evaluate(tmp_path, "roman")
    assert res["total"] == feature_total("roman")
    assert res["pass"] == res["total"]


def test_conformance_partial_on_wrong_hello(tmp_path):
    # Always returns the same string -> fails the named-greeting cases, passes the
    # World-default cases. Must be strictly between 0 and total (real grading).
    (tmp_path / "hello.py").write_text("def hello(name):\n    return 'Hello, World!'\n")
    res = evaluate(tmp_path, "hello")
    assert 0 < res["pass"] < res["total"]


def test_conformance_zero_on_missing_module(tmp_path):
    res = evaluate(tmp_path, "roman")  # no roman.py written
    assert res["pass"] == 0
    assert res["total"] == feature_total("roman")


def test_conformance_unsupported_feature_raises(tmp_path):
    import pytest

    with pytest.raises(ValueError):
        evaluate(tmp_path, "nonexistent-feature")


# ---------------------------------------------------------------------------
# Aggregator: minimum-n guard, variance, wall_ms axis
# ---------------------------------------------------------------------------

def _rec(feature, mode, trial, *, tokens_in, tokens_out, wall_ms):
    return {
        "feature": feature, "mode": mode, "trial": trial,
        "tokens": {"tokens_in": tokens_in, "tokens_out": tokens_out, "wall_ms": wall_ms},
        "wall_ms": wall_ms,
        "graph_quality": {"score": None},
        "zero_touch": True,
        "conformance": {"available": False},
    }


def test_min_n_guard_blocks_winner_at_n1():
    # A clearly cheaper than A+B on tokens, but only 1 trial each -> no winner.
    records = [
        _rec("hello", "A", 1, tokens_in=100, tokens_out=10, wall_ms=1000),
        _rec("hello", "A+B", 1, tokens_in=900, tokens_out=10, wall_ms=2000),
    ]
    row = aggregate_axis(records, "hello", "tokens_total")
    assert row["winner"] is None, "must not crown a winner at n=1"
    assert row["apparent_winner"] == "A"
    assert "underpowered" in row["result"]


def test_min_n_guard_credits_winner_at_threshold():
    # Non-overlapping ranges with n == MIN_N_FOR_WINNER on each side -> credited.
    records = []
    for t in range(1, MIN_N_FOR_WINNER + 1):
        records.append(_rec("hello", "A", t, tokens_in=100 + t, tokens_out=10, wall_ms=1000))
        records.append(_rec("hello", "A+B", t, tokens_in=900 + t, tokens_out=10, wall_ms=2000))
    row = aggregate_axis(records, "hello", "tokens_total")
    assert row["n"] == MIN_N_FOR_WINNER
    assert row["winner"] == "A", "lower tokens_total should win when adequately powered"
    assert row["result"] == "separated"


def test_overlapping_ranges_report_no_separation():
    records = []
    for t in range(1, MIN_N_FOR_WINNER + 1):
        records.append(_rec("hello", "A", t, tokens_in=100 + 50 * t, tokens_out=10, wall_ms=1000))
        records.append(_rec("hello", "A+B", t, tokens_in=120 + 50 * t, tokens_out=10, wall_ms=1000))
    row = aggregate_axis(records, "hello", "tokens_total")
    assert row["winner"] is None
    assert "no separation" in row["result"]


def test_aggregate_reports_variance_stats():
    records = [
        _rec("hello", "A", 1, tokens_in=100, tokens_out=10, wall_ms=1000),
        _rec("hello", "A", 2, tokens_in=200, tokens_out=10, wall_ms=1200),
        _rec("hello", "A+B", 1, tokens_in=150, tokens_out=10, wall_ms=1100),
        _rec("hello", "A+B", 2, tokens_in=160, tokens_out=10, wall_ms=1150),
    ]
    row = aggregate_axis(records, "hello", "tokens_total")
    assert row["stats_A"]["n"] == 2
    assert row["stats_A"]["mean"] == 160.0  # (110 + 210) / 2
    assert row["stats_A"]["stdev"] > 0
    assert row["stats_AB"]["mean"] == 165.0  # (160 + 170) / 2


def test_wall_ms_is_an_aggregated_axis():
    records = [
        _rec("hello", "A", 1, tokens_in=100, tokens_out=10, wall_ms=1000),
        _rec("hello", "A+B", 1, tokens_in=100, tokens_out=10, wall_ms=2000),
    ]
    agg = aggregate(records)
    assert "wall_ms" in agg["axes"]
    row = next(r for r in agg["results"] if r["axis"] == "wall_ms")
    assert row["stats_A"]["mean"] == 1000


# ---------------------------------------------------------------------------
# Token-axis parity invariant
# ---------------------------------------------------------------------------

def test_plan_implement_prompts_interpolate_only_goal():
    """The coder prompt must be byte-identical across modes, which holds iff the
    plan/implement templates reference only ${goal} (never ${state.*}); otherwise
    the engine's per-node state threading would diverge from the A+B direct loop
    and the token axis would measure the harness, not the model."""
    import json

    catalog = json.loads((ROOT / "prompts" / "catalog.json").read_text())["prompts"]
    for vocab in ("plan", "implement"):
        text = (ROOT / catalog[vocab]).read_text()
        assert "${goal}" in text, f"{vocab} template should interpolate ${{goal}}"
        assert "${state" not in text, (
            f"{vocab} template references ${{state.*}} — breaks cross-mode prompt "
            "parity and confounds the token axis"
        )
