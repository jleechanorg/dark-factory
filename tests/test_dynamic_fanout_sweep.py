"""Tests for the dynamic_fanout parametric sweep (sweep.py).

Pure deterministic cost-model checks — no LLM, no token metering. They assert:
  1. the per-cell coverage/cost primitives are exact (``coverage_a == min(F,K)/K``);
  2. the value-model net arithmetic is right;
  3. the dominance / Pareto-frontier claims the report makes are true;
  4. at least one breakeven value computed by the module matches a hand-checked
     number (spread-0 ties forever; spread-`>0` breaks even at V/C = 1).
"""

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from benchmarks.dynamic_fanout.sweep import (  # noqa: E402
    KDist,
    coverage_a,
    coverage_ab,
    calls_a,
    calls_ab,
    net_a,
    net_ab,
    best_static_f,
    apb_beats_best_static,
    breakeven_ratio,
    pareto_frontier,
    coverage_cost_matrix,
    summarize,
    render_markdown,
    K_RANGE,
    F_GRID,
)


# ---- per-cell primitives -------------------------------------------------

def test_coverage_a_is_min_f_k_over_k():
    # The defining identity, exhaustively over the sweep grid.
    for k in K_RANGE:
        for f in F_GRID:
            assert coverage_a(k, f) == min(f, k) / k
    # spot hand-checks
    assert coverage_a(6, 3) == 0.5          # 3 of 6
    assert coverage_a(2, 3) == 1.0          # min(3,2)/2 = 1
    assert coverage_a(12, 8) == 8 / 12


def test_calls_and_coverage_ab_are_exact_k():
    for k in K_RANGE:
        assert calls_a(k, 5) == 5           # static cost is F, independent of K
        assert coverage_ab(k) == 1.0        # A+B always full coverage
        assert calls_ab(k) == k             # A+B spends exactly K


def test_net_value_arithmetic():
    # net_A(K,F) = V*min(F,K) - C*F ; net_AB(K) = (V-C)*K
    assert net_a(6, 3, v=2.0, c=1.0) == 2.0 * 3 - 1.0 * 3   # = 3
    assert net_ab(6, v=2.0, c=1.0) == (2.0 - 1.0) * 6        # = 6
    # over-provision past K covers no more but still costs:
    assert net_a(2, 5, v=2.0, c=1.0) == 2.0 * 2 - 1.0 * 5    # = -1
    # at F == K the static net equals A+B net (the spread-0 tie identity):
    for k in (1, 4, 7):
        assert net_a(k, k, v=3.0, c=1.0) == net_ab(k, v=3.0, c=1.0)


# ---- dominance / Pareto frontier -----------------------------------------

def test_apb_on_frontier_for_every_reported_distribution():
    for dist in (KDist.point(4), KDist.uniform(3, 4),
                 KDist.uniform(1, 12), KDist.uniform(2, 8)):
        pf = pareto_frontier(dist)
        assert pf["apb_on_frontier"] is True


def test_large_static_f_dominated_on_wide_distribution():
    # Over-provisioning (F=12) on a wide uniform K∈[1,12] is dominated: A+B reaches
    # full coverage at lower expected calls.
    pf = pareto_frontier(KDist.uniform(1, 12))
    assert "F=12" in pf["dominated_static_f"]
    assert "F=8" in pf["dominated_static_f"]


def test_no_static_f_dominates_apb_across_a_spread():
    # The headline claim: no single static F is on the frontier *to the exclusion*
    # of A+B — A+B is never dominated.
    pf = pareto_frontier(KDist.uniform(2, 8))
    assert "A+B" in pf["non_dominated"]
    assert "A+B" not in pf["dominated"]


def test_apb_beats_best_static_above_breakeven_and_loses_below():
    dist = KDist.uniform(2, 8)
    assert apb_beats_best_static(dist, v=2.0, c=1.0) is True    # r=2 > 1
    assert apb_beats_best_static(dist, v=0.5, c=1.0) is False   # r<1, dispatches lose


def test_best_static_f_equals_k_for_point_distribution():
    # Given its best static config, Mode A picks exactly F=K for a known K.
    f, _ = best_static_f(KDist.point(7), v=2.0, c=1.0)
    assert f == 7


# ---- breakeven (hand-checked) --------------------------------------------

def test_breakeven_is_none_for_constant_k():
    # Spread 0: best static F=K ties A+B for ALL r -> never a strict win.
    assert breakeven_ratio(KDist.point(4)) is None
    assert breakeven_ratio(KDist.point(9)) is None


def test_breakeven_is_one_for_any_spread():
    # Hand derivation (uniform K∈{3,4}, C=1):
    #   net_AB   = E[(V-1)K] = 3.5V - 3.5
    #   best static over F∈1..4 at r just above 1 is F=3: net = 3V - 3
    #   net_AB - net_F3 = 0.5V - 0.5 = 0.5(V-1) > 0  <=>  V > 1
    # so the breakeven ratio is exactly 1 (the solver returns ~1 + tol).
    be = breakeven_ratio(KDist.uniform(3, 4))
    assert be is not None
    assert abs(be - 1.0) < 1e-3

    # Same trivial-breakeven result on a very wide and a skewed distribution.
    assert abs(breakeven_ratio(KDist.uniform(1, 12)) - 1.0) < 1e-3
    skewed = KDist(((2, 100.0), (12, 1.0)))   # mostly K=2, rare K=12, spread 10
    assert abs(breakeven_ratio(skewed) - 1.0) < 1e-3


def test_breakeven_crossing_is_exactly_at_r_equals_one():
    # Direct check of the V=C tie point: A+B does NOT strictly beat best static at
    # r=1, but does at r=1+eps.
    dist = KDist.uniform(2, 8)
    assert apb_beats_best_static(dist, v=1.0, c=1.0) is False     # tie at r=1
    assert apb_beats_best_static(dist, v=1.0001, c=1.0) is True   # win just above


# ---- matrix + summary plumbing -------------------------------------------

def test_coverage_cost_matrix_shape_and_apb_row():
    mat = coverage_cost_matrix()
    assert mat["k_range"] == tuple(K_RANGE)
    assert mat["f_grid"] == tuple(F_GRID)
    assert len(mat["rows"]) == len(K_RANGE)
    for row in mat["rows"]:
        assert len(row["cells"]) == len(F_GRID)
        cov, calls = row["apb"]
        assert cov == 1.0 and calls == row["k"]      # A+B row is 1.00 / K


def test_summarize_matches_report_claims():
    s = summarize()["distributions"]
    # spread-0 -> permanent tie (no breakeven), A+B still on frontier
    assert s["constant K=4 (spread 0)"]["breakeven_vc"] is None
    assert s["constant K=4 (spread 0)"]["apb_on_frontier"] is True
    assert s["constant K=4 (spread 0)"]["apb_beats_best_at_r2"] is False
    # every spread>0 distribution: A+B beats best static at r=2 and is on frontier
    for label, info in s.items():
        if info["spread"] > 0:
            assert info["apb_beats_best_at_r2"] is True
            assert info["apb_on_frontier"] is True


def test_render_markdown_contains_decision_rule_and_matrix():
    md = render_markdown()
    assert "## DECISION RULE" in md
    assert "Coverage / cost matrix" in md
    assert "Pareto frontier" in md
    assert "Breakeven" in md
    # the spread-0 tie must be visible in the rendered table
    assert "permanent tie" in md
