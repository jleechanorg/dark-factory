"""Parametric sweep: when does Mode A+B's dynamic fan-out earn its dispatch cost?

This is a *pure, deterministic, no-LLM* cost-model analysis layered on top of the
``dynamic_fanout`` G3 gap (runtime-determined graph shape). It answers one
architect-facing question rigorously:

    For what K-distribution does Mode A+B's "fan out to exactly K" dispatch beat
    the BEST single static Mode A node count F, and is any static F ever optimal?

Mechanics recap (from ``modes.py``)
-----------------------------------
A feature exposes ``K`` endpoints, known only at runtime. A static Mode A graph
has a node count ``F`` fixed at authoring time, so it covers ``min(F, K)`` of the
``K`` endpoints and spends ``F`` dispatches. Mode A+B discovers ``K`` and fans
out to exactly ``K`` nodes: it covers all ``K`` and spends ``K`` dispatches.

    coverage_A(K, F)  = min(F, K) / K        calls_A(K, F)  = F
    coverage_AB(K)    = 1.0                   calls_AB(K)    = K

Value model (stated explicitly; this is an ASSUMPTION, not a measurement)
-------------------------------------------------------------------------
Each correctly-covered endpoint is worth ``V`` value units. Each coder dispatch
costs ``C`` cost units. A mode's *net value* is::

    net = V * (covered endpoints) - C * (dispatches)

    net_A(K, F) = V * min(F, K) - C * F
    net_AB(K)   = V * K        - C * K   = (V - C) * K

``V`` and ``C`` enter only as the ratio ``r = V / C`` (net scales with C), so the
whole trade collapses to one knob ``r``. This is a deterministic cost model over
*dispatch counts*, NOT a metered-token or billing claim.

Why a single known K is a red herring
-------------------------------------
If K were known at authoring time you would simply set F = K and Mode A would tie
A+B (``net_A(K, K) = (V - C) * K = net_AB(K)``). The whole G3 premise is that one
static F must serve a *distribution* of K while A+B adapts per draw. So the
breakeven is a property of the K-distribution, and the headline claim ("no single
static F is optimal") is only meaningful across a spread of K.
"""

from __future__ import annotations

from dataclasses import dataclass

# Sweep grid (exposed for tests + the SWEEP.md generator).
K_RANGE = tuple(range(1, 13))           # K in 1..12
F_GRID = (1, 2, 3, 5, 8, 12)            # candidate static node counts


# ---- per-cell primitives -------------------------------------------------

def coverage_a(k: int, f: int) -> float:
    """Mode A coverage at (K, F): ``min(F, K) / K``."""
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    return min(f, k) / k


def calls_a(k: int, f: int) -> int:
    """Mode A dispatch count: always ``F``, independent of K."""
    return f


def coverage_ab(k: int) -> float:
    """Mode A+B coverage: always full."""
    return 1.0


def calls_ab(k: int) -> int:
    """Mode A+B dispatch count: exactly ``K``."""
    return k


def net_a(k: int, f: int, v: float, c: float) -> float:
    """Net value of Mode A at (K, F): ``V*min(F,K) - C*F``."""
    return v * min(f, k) - c * f


def net_ab(k: int, v: float, c: float) -> float:
    """Net value of Mode A+B at K: ``(V - C) * K``."""
    return (v - c) * k


# ---- expectations over a K-distribution ----------------------------------

@dataclass(frozen=True)
class KDist:
    """A K-distribution as explicit (K, weight) support. Weights need not sum to 1;
    they are normalized. Default constructor builds a uniform distribution over an
    inclusive [kmin, kmax] range."""

    support: tuple[tuple[int, float], ...]

    @classmethod
    def uniform(cls, kmin: int, kmax: int) -> "KDist":
        if kmin < 1 or kmax < kmin:
            raise ValueError(f"bad uniform range [{kmin}, {kmax}]")
        ks = range(kmin, kmax + 1)
        return cls(tuple((k, 1.0) for k in ks))

    @classmethod
    def point(cls, k: int) -> "KDist":
        """Degenerate (constant-K) distribution — spread 0."""
        return cls(((k, 1.0),))

    def _normalized(self) -> list[tuple[int, float]]:
        total = sum(w for _, w in self.support)
        return [(k, w / total) for k, w in self.support]

    @property
    def ks(self) -> tuple[int, ...]:
        return tuple(k for k, _ in self.support)

    @property
    def spread(self) -> int:
        """max(K) - min(K). Zero iff K is effectively constant."""
        ks = self.ks
        return max(ks) - min(ks)

    def expect(self, fn) -> float:
        """Probability-weighted expectation of ``fn(k)``."""
        return sum(w * fn(k) for k, w in self._normalized())


def expected_coverage_a(dist: KDist, f: int) -> float:
    return dist.expect(lambda k: coverage_a(k, f))


def expected_calls_a(dist: KDist, f: int) -> float:
    # calls_a is constant in K, but compute via expectation for symmetry.
    return dist.expect(lambda k: float(calls_a(k, f)))


def expected_coverage_ab(dist: KDist) -> float:
    return dist.expect(coverage_ab)


def expected_calls_ab(dist: KDist) -> float:
    return dist.expect(lambda k: float(calls_ab(k)))


def expected_net_a(dist: KDist, f: int, v: float, c: float) -> float:
    return dist.expect(lambda k: net_a(k, f, v, c))


def expected_net_ab(dist: KDist, v: float, c: float) -> float:
    return dist.expect(lambda k: net_ab(k, v, c))


# ---- the central comparison ----------------------------------------------

def _full_f_range(dist: KDist) -> range:
    """The authoring choices a static graph could genuinely make: any integer node
    count from 1 to max(K). (F beyond max K only adds wasted dispatches, never
    coverage, so it can never be optimal — capping there is lossless.)"""
    return range(1, max(dist.ks) + 1)


def best_static_f(dist: KDist, v: float, c: float, f_grid=None) -> tuple[int, float]:
    """Return (F*, net) for the static node count that maximizes expected net
    value over ``dist`` at ratio V/C. Ties broken toward the SMALLER F (cheaper).

    ``f_grid`` defaults to the FULL integer range ``1..max(K)`` — the fair "Mode A
    given its best static config" test. Pass an explicit grid only to restrict the
    architect to a sparse menu (used by the display matrix, not the breakeven).
    """
    candidates = f_grid if f_grid is not None else _full_f_range(dist)
    best_f = None
    best_net = None
    for f in candidates:
        net = expected_net_a(dist, f, v, c)
        if best_net is None or net > best_net + 1e-12:
            best_f, best_net = f, net
    return best_f, best_net


def apb_beats_best_static(dist: KDist, v: float, c: float, f_grid=None) -> bool:
    """True iff Mode A+B's expected net strictly exceeds the best static F.

    ``f_grid=None`` (default) gives Mode A its best config over the FULL integer
    range — the fair test.
    """
    _, best = best_static_f(dist, v, c, f_grid)
    return expected_net_ab(dist, v, c) > best + 1e-12


def breakeven_ratio(dist: KDist, f_grid=None, *, hi: float = 1000.0,
                    tol: float = 1e-6) -> float | None:
    """Smallest ratio ``r = V/C`` at which A+B beats the best static F over
    ``dist``. ``f_grid=None`` (default) gives Mode A its best config over the FULL
    integer range ``1..max(K)`` — the fair "best static config" test.

    Returns ``None`` if A+B never *strictly* wins for any r in (1, hi]. The
    canonical None case is a **single-point** (spread-0) distribution: there the
    best static F equals K exactly, so ``net_A(K,K) = (V-C)·K = net_AB`` for ALL
    r — a permanent tie, never a strict A+B win.

    Closed-form intuition: net is homogeneous in C, so fix ``C = 1`` and solve in
    ``V = r``. Both ``net_AB`` and every ``net_A(·, F)`` are *linear* in V, so
    ``Δ(V) = net_AB - max_F net_A`` is piecewise-linear and non-decreasing once
    A+B's slope (E[K]) exceeds the best static slope (E[min(F,K)]); we bisect for
    the crossing rather than do algebra so the ``max`` over F stays honest.
    """
    c = 1.0
    # A+B must beat the best static F by more than this margin to count as a
    # *strict* win. Without it, the spread-0 identity net_A(K,K) == net_AB(K) can
    # register a spurious ~1e-16 "win" from floating-point rounding.
    eps = 1e-9

    def delta(r: float) -> float:
        _, best = best_static_f(dist, r, c, f_grid)
        return expected_net_ab(dist, r, c) - best

    lo = 1.0 + tol
    if delta(lo) > eps:
        return lo  # A+B wins even at the smallest meaningful ratio
    if delta(hi) <= eps:
        return None  # never wins in range -> a static F is always at least as good
    # monotone crossing: delta is non-decreasing in r once A+B's slope (K) exceeds
    # the best static slope (min(F,K)); bisect.
    for _ in range(200):
        mid = (lo + hi) / 2
        if delta(mid) > eps:
            hi = mid
        else:
            lo = mid
        if hi - lo < tol:
            break
    return hi


# ---- Pareto frontier over (expected coverage, expected calls) ------------

def _dominates(a: tuple[float, float], b: tuple[float, float]) -> bool:
    """Strategy ``a`` dominates ``b`` iff a has >= coverage AND <= calls, with at
    least one strict. Point = (coverage, calls); higher coverage + lower calls
    is better."""
    cov_a, calls_a_ = a
    cov_b, calls_b_ = b
    no_worse = cov_a >= cov_b - 1e-12 and calls_a_ <= calls_b_ + 1e-12
    strictly_better = cov_a > cov_b + 1e-12 or calls_a_ < calls_b_ - 1e-12
    return no_worse and strictly_better


def pareto_frontier(dist: KDist, f_grid=F_GRID) -> dict:
    """Compute the Pareto frontier over (expected coverage ↑, expected calls ↓)
    across all static F and Mode A+B.

    Returns a dict with each strategy's (coverage, calls), the set of
    non-dominated strategies, and the set of dominated static F values.
    """
    strategies: dict[str, tuple[float, float]] = {}
    for f in f_grid:
        strategies[f"F={f}"] = (expected_coverage_a(dist, f), expected_calls_a(dist, f))
    strategies["A+B"] = (expected_coverage_ab(dist), expected_calls_ab(dist))

    non_dominated = []
    dominated = []
    for name, pt in strategies.items():
        if any(_dominates(other, pt) for o_name, other in strategies.items() if o_name != name):
            dominated.append(name)
        else:
            non_dominated.append(name)
    dominated_static = sorted(
        (name for name in dominated if name.startswith("F=")),
        key=lambda s: int(s[2:]),
    )
    return {
        "strategies": strategies,
        "non_dominated": non_dominated,
        "dominated": dominated,
        "dominated_static_f": dominated_static,
        "apb_on_frontier": "A+B" in non_dominated,
    }


# ---- matrices for the report ---------------------------------------------

def coverage_cost_matrix(k_range=K_RANGE, f_grid=F_GRID) -> dict:
    """Build the K-rows × F-cols coverage/cost matrix plus the A+B row.

    Each static cell is ``coverage_a(K,F) / calls_a(K,F)``; the A+B row is
    ``1.00 / K``.
    """
    rows = []
    for k in k_range:
        cells = [(coverage_a(k, f), calls_a(k, f)) for f in f_grid]
        rows.append({"k": k, "cells": cells, "apb": (1.0, k)})
    return {"k_range": tuple(k_range), "f_grid": tuple(f_grid), "rows": rows}


# ---- SWEEP.md emitter ----------------------------------------------------

# A small, named set of K-distributions the report characterizes. Each is a plain
# assumption an architect can map to their feature.
REPORT_DISTS = {
    "constant K=4 (spread 0)": KDist.point(4),
    "narrow uniform K∈[3,4]": KDist.uniform(3, 4),
    "uniform K∈[1,12] (max spread)": KDist.uniform(1, 12),
    "uniform K∈[2,8]": KDist.uniform(2, 8),
}

# Reference ratios for the breakeven table.
REPORT_RATIOS = (1.5, 2.0, 4.0, 8.0)


def _fmt_cell(cov: float, calls: float) -> str:
    return f"{cov:.2f}/{int(round(calls))}c"


def render_markdown(k_range=K_RANGE, f_grid=F_GRID) -> str:
    out: list[str] = []
    A = out.append

    A("# dynamic_fanout — parametric sweep: does dynamic fan-out earn its cost?\n")
    A("**Run:** pure deterministic cost model (no LLM, no tokens metered).  ")
    A("**Module:** `benchmarks/dynamic_fanout/sweep.py`  ")
    A("**Question:** for what K-distribution does Mode A+B's fan-out beat the BEST "
      "single static F, and is any static F ever optimal?\n")

    # --- assumptions ---
    A("## Assumptions (stated, not measured)\n")
    A("- **Value model:** each correctly-covered endpoint is worth `V`; each coder "
      "dispatch costs `C`. `net = V·covered − C·calls`.")
    A("  - `net_A(K,F) = V·min(F,K) − C·F`")
    A("  - `net_AB(K)  = (V−C)·K`  (covers all K, spends K calls)")
    A("- Only the **ratio `r = V/C`** matters (net is homogeneous in `C`).")
    A("- **K-distribution** is the load-bearing assumption. Where a single K is "
      "known at authoring time, `F=K` ties A+B — so the interesting cases are "
      "*spreads* of K. Reported distributions are **uniform** unless noted.")
    A("- This is a **dispatch-count** cost model, **not** a metered-token or "
      "billing claim.\n")

    # --- coverage/cost matrix ---
    A("## Coverage / cost matrix  (cell = `coverage / calls`)\n")
    header = "| K \\ F | " + " | ".join(f"F={f}" for f in f_grid) + " | **A+B** |"
    sep = "|" + "---|" * (len(f_grid) + 2)
    A(header)
    A(sep)
    mat = coverage_cost_matrix(k_range, f_grid)
    for row in mat["rows"]:
        cells = " | ".join(_fmt_cell(cov, calls) for cov, calls in row["cells"])
        apb = _fmt_cell(*row["apb"])
        A(f"| K={row['k']} | {cells} | **{apb}** |")
    A("")
    A("Read each cell as *coverage / dispatch-count*. A+B is always `1.00`; its "
      "cost rises with K. No static column is `1.00` for every row, and no static "
      "column is cheapest for every row — that tension is the whole result.\n")

    # --- Pareto frontier (max-spread uniform) ---
    A("## Pareto frontier over (expected coverage ↑, expected calls ↓)\n")
    A("Strategies are dominated if some other strategy has **≥ coverage AND ≤ "
      "calls** (one strict). Computed per K-distribution:\n")
    A("| K-distribution | spread | A+B on frontier? | dominated static F |")
    A("|---|---|---|---|")
    for label, dist in REPORT_DISTS.items():
        pf = pareto_frontier(dist, f_grid)
        dom = ", ".join(pf["dominated_static_f"]) or "—"
        on = "yes" if pf["apb_on_frontier"] else "no"
        A(f"| {label} | {dist.spread} | {on} | {dom} |")
    A("")
    A("A+B sits on the frontier in every non-degenerate distribution: it is the "
      "only strategy with full expected coverage, so nothing can dominate it on "
      "the coverage axis. Static F values get dominated when a smaller F achieves "
      "the same expected coverage more cheaply, or a larger F is strictly worse on "
      "both axes for that distribution.\n")

    # --- breakeven table ---
    A("## Breakeven: smallest `V/C` at which A+B beats the best static F\n")
    A("For each distribution, the smallest ratio `r = V/C` at which A+B's expected "
      "net strictly exceeds the best static F (chosen with full knowledge of the "
      "distribution). `—` means A+B never *strictly* wins — at spread 0 the best "
      "static `F = K` ties A+B for every `r`.\n")
    A("(Mode A is given its **best static config over all integer F ∈ 1..max(K)** — "
      "not the sparse display grid — so this is the strongest static baseline.)\n")
    A("| K-distribution | spread | breakeven V/C | best static F @ r=2 | A+B beats best @ r=2? |")
    A("|---|---|---|---|---|")
    for label, dist in REPORT_DISTS.items():
        be = breakeven_ratio(dist)          # full-integer best static config
        if be is None:
            be_s = "— (never; permanent tie)"
        else:
            be_s = "1.0 (any V>C)" if be < 1.01 else f"{be:.3g}"
        bf, _ = best_static_f(dist, 2.0, 1.0)
        beats = "yes" if apb_beats_best_static(dist, 2.0, 1.0) else "no"
        A(f"| {label} | {dist.spread} | {be_s} | F={bf} | {beats} |")
    A("")
    A("**The clean result:** under this value model the breakeven is `V/C = 1` for "
      "*every* spread-`>0` distribution, and **undefined (a permanent tie)** for "
      "spread 0. Derivation: `net_AB` has slope `E[K]` in `V`; the best static F "
      "has slope `E[min(F,K)] ≤ E[K]`, with equality only at `F = max K` — which "
      "then carries a strictly worse intercept (`−C·max K < −C·E[K]`). So for any "
      "spread `> 0`, A+B's net exceeds the best static F for **all** `V > C`, ties "
      "at `V = C`, and loses below it. The *spread*, not the ratio, is the real "
      "knob: the ratio breakeven collapses to the trivial `V/C > 1` (each covered "
      "endpoint must merely be worth more than one dispatch).\n")

    # --- decision rule ---
    A("## DECISION RULE\n")
    A(_decision_rule())
    A("")
    return "\n".join(out)


def _decision_rule() -> str:
    """The architect-facing rule, returned verbatim by the CLI and the report."""
    return (
        "1. If K is effectively **constant** (spread 0 — the endpoint count is "
        "known at authoring time), the best static `F = K` *ties* A+B exactly "
        "(`net_A(K,K) = (V−C)·K = net_AB`). Dynamic fan-out earns nothing; stay "
        "static.\n"
        "2. The moment K **varies at runtime** (spread ≥ 1), no single static F is "
        "Pareto-optimal: A+B is always on the (expected-coverage, expected-calls) "
        "frontier, and at least one static F is dominated. Over-provisioning "
        "(`F = max K`) buys full coverage but is strictly dominated by A+B on "
        "cost; under-provisioning leaves the large-K tail uncovered.\n"
        "3. The breakeven is governed by **spread, not by a large V/C threshold**. "
        "For any spread `> 0` the ratio breakeven is the trivial `V/C > 1`: as "
        "long as a covered endpoint is worth more than one coder dispatch, A+B's "
        "exact-K fan-out beats the best static F. Below `V/C = 1` dispatches cost "
        "more than they return, and the cheapest static `F = 1` wins.\n"
        "4. Concretely: **use Mode A+B whenever K is runtime-determined (spread ≥ "
        "1) and V/C > 1. Use static Mode A only when K is fixed/known at authoring "
        "time, or when V/C ≤ 1 (a covered endpoint is not worth even one "
        "dispatch).**"
    )


# ---- programmatic summary (used by tests + CLI) --------------------------

def summarize(k_range=K_RANGE, f_grid=F_GRID) -> dict:
    """Machine-readable rollup of the sweep for tests and the CLI."""
    out = {"distributions": {}}
    for label, dist in REPORT_DISTS.items():
        pf = pareto_frontier(dist, f_grid)
        out["distributions"][label] = {
            "spread": dist.spread,
            # breakeven / best-static / beats use the FULL integer F range (fairest
            # static baseline); the Pareto frontier uses the sparse display grid
            # (the architect's realistic authoring menu).
            "breakeven_vc": breakeven_ratio(dist),
            "best_static_f_at_r2": best_static_f(dist, 2.0, 1.0)[0],
            "apb_beats_best_at_r2": apb_beats_best_static(dist, 2.0, 1.0),
            "apb_on_frontier": pf["apb_on_frontier"],
            "dominated_static_f": pf["dominated_static_f"],
        }
    return out


def main(argv=None) -> int:
    import argparse
    import pathlib

    ap = argparse.ArgumentParser(prog="benchmarks.dynamic_fanout.sweep")
    ap.add_argument("--out", default=None,
                    help="path to write SWEEP.md (default: alongside this module)")
    args = ap.parse_args(argv)

    md = render_markdown()
    out = (pathlib.Path(args.out) if args.out
           else pathlib.Path(__file__).with_name("SWEEP.md"))
    out.write_text(md)

    print(f"wrote {out}")
    print()
    summary = summarize()
    for label, info in summary["distributions"].items():
        be = info["breakeven_vc"]
        be_s = f"{be:.3g}" if be is not None else "—"
        print(f"  {label:34s} spread={info['spread']:>2d}  "
              f"breakeven V/C={be_s:>5s}  "
              f"A+B on frontier={info['apb_on_frontier']}  "
              f"dominated F={info['dominated_static_f']}")
    print()
    print("DECISION RULE:")
    print(_decision_rule())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
