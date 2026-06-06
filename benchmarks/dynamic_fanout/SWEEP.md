# dynamic_fanout — parametric sweep: does dynamic fan-out earn its cost?

**Run:** pure deterministic cost model (no LLM, no tokens metered).  
**Module:** `benchmarks/dynamic_fanout/sweep.py`  
**Question:** for what K-distribution does Mode A+B's fan-out beat the BEST single static F, and is any static F ever optimal?

## Assumptions (stated, not measured)

- **Value model:** each correctly-covered endpoint is worth `V`; each coder dispatch costs `C`. `net = V·covered − C·calls`.
  - `net_A(K,F) = V·min(F,K) − C·F`
  - `net_AB(K)  = (V−C)·K`  (covers all K, spends K calls)
- Only the **ratio `r = V/C`** matters (net is homogeneous in `C`).
- **K-distribution** is the load-bearing assumption. Where a single K is known at authoring time, `F=K` ties A+B — so the interesting cases are *spreads* of K. Reported distributions are **uniform** unless noted.
- This is a **dispatch-count** cost model, **not** a metered-token or billing claim.

## Coverage / cost matrix  (cell = `coverage / calls`)

| K \ F | F=1 | F=2 | F=3 | F=5 | F=8 | F=12 | **A+B** |
|---|---|---|---|---|---|---|---|
| K=1 | 1.00/1c | 1.00/2c | 1.00/3c | 1.00/5c | 1.00/8c | 1.00/12c | **1.00/1c** |
| K=2 | 0.50/1c | 1.00/2c | 1.00/3c | 1.00/5c | 1.00/8c | 1.00/12c | **1.00/2c** |
| K=3 | 0.33/1c | 0.67/2c | 1.00/3c | 1.00/5c | 1.00/8c | 1.00/12c | **1.00/3c** |
| K=4 | 0.25/1c | 0.50/2c | 0.75/3c | 1.00/5c | 1.00/8c | 1.00/12c | **1.00/4c** |
| K=5 | 0.20/1c | 0.40/2c | 0.60/3c | 1.00/5c | 1.00/8c | 1.00/12c | **1.00/5c** |
| K=6 | 0.17/1c | 0.33/2c | 0.50/3c | 0.83/5c | 1.00/8c | 1.00/12c | **1.00/6c** |
| K=7 | 0.14/1c | 0.29/2c | 0.43/3c | 0.71/5c | 1.00/8c | 1.00/12c | **1.00/7c** |
| K=8 | 0.12/1c | 0.25/2c | 0.38/3c | 0.62/5c | 1.00/8c | 1.00/12c | **1.00/8c** |
| K=9 | 0.11/1c | 0.22/2c | 0.33/3c | 0.56/5c | 0.89/8c | 1.00/12c | **1.00/9c** |
| K=10 | 0.10/1c | 0.20/2c | 0.30/3c | 0.50/5c | 0.80/8c | 1.00/12c | **1.00/10c** |
| K=11 | 0.09/1c | 0.18/2c | 0.27/3c | 0.45/5c | 0.73/8c | 1.00/12c | **1.00/11c** |
| K=12 | 0.08/1c | 0.17/2c | 0.25/3c | 0.42/5c | 0.67/8c | 1.00/12c | **1.00/12c** |

Read each cell as *coverage / dispatch-count*. A+B is always `1.00`; its cost rises with K. No static column is `1.00` for every row, and no static column is cheapest for every row — that tension is the whole result.

## Pareto frontier over (expected coverage ↑, expected calls ↓)

Strategies are dominated if some other strategy has **≥ coverage AND ≤ calls** (one strict). Computed per K-distribution:

| K-distribution | spread | A+B on frontier? | dominated static F |
|---|---|---|---|
| constant K=4 (spread 0) | 0 | yes | F=5, F=8, F=12 |
| narrow uniform K∈[3,4] | 1 | yes | F=5, F=8, F=12 |
| uniform K∈[1,12] (max spread) | 11 | yes | F=8, F=12 |
| uniform K∈[2,8] | 6 | yes | F=5, F=8, F=12 |

A+B sits on the frontier in every non-degenerate distribution: it is the only strategy with full expected coverage, so nothing can dominate it on the coverage axis. Static F values get dominated when a smaller F achieves the same expected coverage more cheaply, or a larger F is strictly worse on both axes for that distribution.

## Breakeven: smallest `V/C` at which A+B beats the best static F

For each distribution, the smallest ratio `r = V/C` at which A+B's expected net strictly exceeds the best static F (chosen with full knowledge of the distribution). `—` means A+B never *strictly* wins — at spread 0 the best static `F = K` ties A+B for every `r`.

(Mode A is given its **best static config over all integer F ∈ 1..max(K)** — not the sparse display grid — so this is the strongest static baseline.)

| K-distribution | spread | breakeven V/C | best static F @ r=2 | A+B beats best @ r=2? |
|---|---|---|---|---|
| constant K=4 (spread 0) | 0 | — (never; permanent tie) | F=4 | no |
| narrow uniform K∈[3,4] | 1 | 1.0 (any V>C) | F=3 | yes |
| uniform K∈[1,12] (max spread) | 11 | 1.0 (any V>C) | F=6 | yes |
| uniform K∈[2,8] | 6 | 1.0 (any V>C) | F=5 | yes |

**The clean result:** under this value model the breakeven is `V/C = 1` for *every* spread-`>0` distribution, and **undefined (a permanent tie)** for spread 0. Derivation: `net_AB` has slope `E[K]` in `V`; the best static F has slope `E[min(F,K)] ≤ E[K]`, with equality only at `F = max K` — which then carries a strictly worse intercept (`−C·max K < −C·E[K]`). So for any spread `> 0`, A+B's net exceeds the best static F for **all** `V > C`, ties at `V = C`, and loses below it. The *spread*, not the ratio, is the real knob: the ratio breakeven collapses to the trivial `V/C > 1` (each covered endpoint must merely be worth more than one dispatch).

## DECISION RULE

1. If K is effectively **constant** (spread 0 — the endpoint count is known at authoring time), the best static `F = K` *ties* A+B exactly (`net_A(K,K) = (V−C)·K = net_AB`). Dynamic fan-out earns nothing; stay static.
2. The moment K **varies at runtime** (spread ≥ 1), no single static F is Pareto-optimal: A+B is always on the (expected-coverage, expected-calls) frontier, and at least one static F is dominated. Over-provisioning (`F = max K`) buys full coverage but is strictly dominated by A+B on cost; under-provisioning leaves the large-K tail uncovered.
3. The breakeven is governed by **spread, not by a large V/C threshold**. For any spread `> 0` the ratio breakeven is the trivial `V/C > 1`: as long as a covered endpoint is worth more than one coder dispatch, A+B's exact-K fan-out beats the best static F. Below `V/C = 1` dispatches cost more than they return, and the cheapest static `F = 1` wins.
4. Concretely: **use Mode A+B whenever K is runtime-determined (spread ≥ 1) and V/C > 1. Use static Mode A only when K is fixed/known at authoring time, or when V/C ≤ 1 (a covered endpoint is not worth even one dispatch).**
