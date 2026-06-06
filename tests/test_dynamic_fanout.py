"""Tests for the deterministic dynamic_fanout separation benchmark.

These assert two things:
  1. the coder/evaluator/mode mechanics are correct on real on-disk artifacts;
  2. the SAME aggregator that returned the workflow_graphgen n=10 null now
     credits winners here — the instrument-calibration proof.
"""

import pathlib
import sys
import tempfile

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from benchmarks.dynamic_fanout import (  # noqa: E402
    add_validation,
    consistency,
    count_endpoints,
    coverage,
    make_api_source,
    run_mode_a,
    run_mode_apb,
    FIXED_NODES,
)
from benchmarks.dynamic_fanout.driver import run, FEATURES  # noqa: E402
from benchmarks.workflow_graphgen.scoring import aggregate_axis  # noqa: E402


# ---- scenario + coder + evaluator mechanics ------------------------------

def test_make_api_source_is_deterministic_and_counts():
    with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
        pa = make_api_source(a, 5)
        pb = make_api_source(b, 5)
        assert pa.read_text() == pb.read_text()  # byte-identical for same k
        assert count_endpoints(a) == 5


def test_coverage_starts_at_zero_then_tracks_guards():
    with tempfile.TemporaryDirectory() as d:
        make_api_source(d, 4)
        assert coverage(d) == {"available": True, "pass": 0, "total": 4}
        assert add_validation(d, 0) is True
        assert add_validation(d, 2) is True
        c = coverage(d)
        assert c["pass"] == 2 and c["total"] == 4


def test_add_validation_idempotent_and_noop_for_missing():
    with tempfile.TemporaryDirectory() as d:
        make_api_source(d, 2)
        assert add_validation(d, 0) is True
        assert add_validation(d, 0) is False   # already guarded -> no second insert
        assert add_validation(d, 9) is False    # endpoint 9 does not exist -> no-op
        assert coverage(d)["pass"] == 1


# ---- G3: fixed node count vs runtime fan-out -----------------------------

def test_mode_a_undercovers_when_k_exceeds_fixed():
    with tempfile.TemporaryDirectory() as d:
        res = run_mode_a("validate", d, k=6, fixed=FIXED_NODES)
        # 3 fixed nodes cover 3 of 6 endpoints.
        assert res["conformance"]["pass"] == FIXED_NODES
        assert res["conformance"]["total"] == 6
        assert res["calls"] == FIXED_NODES


def test_mode_a_wastes_dispatch_when_k_below_fixed():
    with tempfile.TemporaryDirectory() as d:
        res = run_mode_a("validate", d, k=2, fixed=FIXED_NODES)
        # full coverage (2/2) but still spends 3 calls -> a wasted dispatch.
        assert res["conformance"]["pass"] == 2 and res["conformance"]["total"] == 2
        assert res["calls"] == FIXED_NODES  # > k: the over-spend


def test_mode_apb_always_full_coverage():
    for k in (1, 2, 3, 6, 8):
        with tempfile.TemporaryDirectory() as d:
            res = run_mode_apb("validate", d, k=k)
            assert res["conformance"]["pass"] == k
            assert res["conformance"]["total"] == k
            assert res["calls"] == k  # fans out to exactly k


# ---- G1: state threading -------------------------------------------------

def test_mode_a_schema_drifts_without_state():
    with tempfile.TemporaryDirectory() as d:
        cols = ["id", "field_X", "updated_at"]  # != Mode A's static default
        res = run_mode_a("schema", d, schema_columns=cols)
        assert res["conformance"]["pass"] == 0  # migration drifted from schema
        assert consistency(d)["pass"] == 0


def test_mode_apb_schema_matches_with_state():
    with tempfile.TemporaryDirectory() as d:
        cols = ["id", "field_X", "updated_at"]
        res = run_mode_apb("schema", d, schema_columns=cols)
        assert res["conformance"]["pass"] == 1  # threaded -> matches
        assert consistency(d)["pass"] == 1


def test_honest_mode_a_threaded_matches_schema():
    # Mode A given its BEST honest config (${state._last_output} threading, proven
    # in tests/test_state_threading.py) matches the schema just like A+B. The G1
    # "win" was an authoring choice, not a paradigm gap.
    with tempfile.TemporaryDirectory() as d:
        cols = ["id", "field_X", "updated_at"]
        res = run_mode_a("schema_threaded", d, schema_columns=cols)
        assert res["conformance"]["pass"] == 1
        assert consistency(d)["pass"] == 1


# ---- the calibration proof: same aggregator credits winners --------------

def _records(trials=5):
    with tempfile.TemporaryDirectory() as wr:
        return run(list(FEATURES), ["A", "A+B"], trials, wr)


def test_aggregator_credits_apb_on_conformance_for_g3_gap():
    recs = _records()
    agg = aggregate_axis(recs, "validate_k6", "conformance")
    assert agg["winner"] == "A+B"       # A+B covers all K; A under-covers
    assert agg["result"] == "separated"


def test_aggregator_credits_a_on_tokens_for_g3_gap():
    # A+B buys coverage at higher dispatch cost -> A wins the token axis. The
    # trade is real and the instrument shows BOTH sides.
    recs = _records()
    agg = aggregate_axis(recs, "validate_k6", "tokens_total")
    assert agg["winner"] == "A"


def test_aggregator_credits_apb_on_tokens_when_k_below_fixed():
    recs = _records()
    agg = aggregate_axis(recs, "validate_k2", "tokens_total")
    assert agg["winner"] == "A+B"       # A wastes a dispatch; A+B is leaner
    # ...and coverage ties (both reach 100%).
    assert aggregate_axis(recs, "validate_k2", "conformance")["winner"] is None


def test_aggregator_credits_apb_on_conformance_for_g1_gap():
    recs = _records()
    agg = aggregate_axis(recs, "schema_migration", "conformance")
    assert agg["winner"] == "A+B"
    # tokens tie for G1 (both modes run 2 nodes) -> the win is purely the gap.
    assert aggregate_axis(recs, "schema_migration", "tokens_total")["winner"] is None


def test_g1_win_evaporates_when_mode_a_is_honest():
    # The fairness proof: the SAME aggregator that credits A+B on the naive G1
    # scenario crowns NO winner once Mode A threads state (schema_migration_threaded).
    # Disjoint ranges vanish -> no separation -> G1 is an authoring gap, not paradigm.
    recs = _records()
    agg = aggregate_axis(recs, "schema_migration_threaded", "conformance")
    assert agg["winner"] is None
    assert aggregate_axis(recs, "schema_migration_threaded", "tokens_total")["winner"] is None


def test_underpowered_below_min_n_even_when_deterministic():
    # Determinism does not bypass the MIN_N guard: at n=4 the disjoint ranges are
    # reported as 'separated but underpowered', no winner crowned. This is the
    # exact guard that refused the workflow_graphgen n=1 token fluke.
    recs = _records(trials=4)
    agg = aggregate_axis(recs, "validate_k6", "conformance")
    assert agg["winner"] is None
    assert agg["apparent_winner"] == "A+B"
    assert "underpowered" in agg["result"]


def test_calibration_meta_the_instrument_is_not_blind():
    # The whole point: the same aggregate that returned NULL on workflow_graphgen
    # credits >= 3 winners here -> the n=10 null was a true negative.
    recs = _records()
    credited = 0
    for feature in FEATURES:
        for axis in ("conformance", "tokens_total"):
            if aggregate_axis(recs, feature, axis).get("winner"):
                credited += 1
    assert credited >= 3
