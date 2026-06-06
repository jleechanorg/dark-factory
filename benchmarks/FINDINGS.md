# When does dynamic fan-out (Mode A+B) earn its complexity?

A consolidated read of the two A-vs-A+B benchmarks in this repo. The question
behind both: **the runner can either walk a static `.dot` middle graph (Mode A)
or let a harness loop the middle chain and dispatch nodes dynamically (Mode A+B).
Mode A+B is more code and more moving parts. When is it actually worth it?**

- **Mode A** — Python engine walks a *statically authored* `.dot`. Node count,
  edges, and prompts are fixed at authoring time. Prompts may still thread state
  via `${state._last_output}` / `${state.<key>}`.
- **Mode A+B** — a harness loops `ir.middle_chain()`, calling `_codergen(Node(...))`
  per node, so it can **discover runtime quantities** and size the graph to them.

Two benchmarks, deliberately different in method:

| | `workflow_graphgen` | `dynamic_fanout` |
|---|---|---|
| Coder | real Sonnet, stochastic | one deterministic Python fn (shared by both modes) |
| n | 10 trials/cell | 5 trials/cell (determinism ⇒ conclusive) |
| Grades | conformance, tokens, wall_ms, graph-quality | conformance, tokens (call-count model) |
| Role | the **measurement** | the **instrument calibration** |
| Aggregator | `scoring.aggregate` (range-non-overlap + `MIN_N_FOR_WINNER=5`) | **the same** `scoring.aggregate` |

---

## Finding 1 — On first-pass tasks, Mode A+B buys nothing (true negative)

`workflow_graphgen` ran real Sonnet at n=10 across both modes and found **no
separation on any axis**: conformance tied (50/50 and 90/90), token ranges
overlap, wall_ms for A+B was ~5–9% slower but the ranges overlap too. The one
apparent token "win" in an early n=1 smoke was noise — the `MIN_N_FOR_WINNER=5`
guard correctly refused to crown it.

Why: in that benchmark both modes execute the **identical** `ir.middle_chain()`
with `${goal}`-only prompts. The dispatch path is the only thing that differs, and
for a single-pass build it is genuinely interchangeable. **Dynamic fan-out adds
latency and code with no quality or cost return on first-pass tasks.**

## Finding 2 — The null is real, not a blind ruler (calibration)

A null only matters if the ruler can see. `dynamic_fanout` feeds the **same
aggregator** three scenarios engineered so the dispatch path is load-bearing, on
real on-disk artifacts. It credits **4 winners**. Same code path, same rule,
opposite outcome ⇒ the workflow_graphgen null is a **true negative**, not an
artifact of an insensitive instrument.

## Finding 3 — Where A+B *does* separate, and what tier each gap is

Three candidate capability gaps were probed. Only one survives an honest Mode A.

| Gap | What it is | Survives honest Mode A? | Tier |
|-----|-----------|--------------------------|------|
| **G1 adjacent** | node 2 needs node 1's output | **No** — works in static `.dot` today via `${state._last_output}` | authoring choice (no engine change) |
| **G1 non-adjacent** | node N needs node N−5's *named* output | partial | **1-line engine gap** (store per-node `.output` in `ctx.state`) |
| **G3** | graph node count = a quantity discovered at runtime (K endpoints) | **Yes** | **paradigm gap** |

- **G1 is mostly a mirage.** The engine already writes `ctx.state["_last_output"]`
  after every node and `_render_prompt` already substitutes it (proven by
  `tests/test_state_threading.py`). The naive `schema_migration` "A+B win" only
  appears because Mode A was handed its *worst* prompt (`${goal}`-only). Give it
  `${state._last_output}` and the win evaporates — the same aggregator crowns **no
  winner** on `schema_migration_threaded`. Only *non-adjacent named* threading
  needs a tiny engine change, and even that is an engine gap, not a paradigm one.

- **G3 is the real thing.** A statically authored `.dot` has a fixed node count.
  If a feature exposes **K** endpoints discoverable only at runtime, Mode A covers
  `min(F,K)/K` for its authored `F`; Mode A+B reads the source, discovers K, and
  fans out to exactly K nodes. No prompt trick or engine setting lets a fixed graph
  size itself to K. This is the one place dynamic fan-out is *structurally*
  required.

## Finding 4 — Even at G3, "better" is axis- and spread-dependent

The G3 win is not unconditional, and Mode A is given its **best** static config
(full `1..max K` range), not a strawman. From `sweep.py`'s value model
`net = V·covered − C·calls` over arbitrary K-distributions:

1. **Constant K (spread 0):** best static `F = K` *ties* A+B exactly for all V/C.
   If the endpoint count is known at authoring time, dynamic fan-out earns nothing.
2. **K varies at runtime (spread ≥ 1):** no single static `F` is Pareto-optimal —
   small `F` under-covers the large-K tail, large `F` ("over-provision to be safe")
   burns calls at small K. A+B's exact-K dispatch is the only point on the frontier.
3. **The knob is spread, not a high value/cost ratio.** For *any* spread > 0 the
   breakeven collapses to the trivial **V/C > 1**: as long as a covered endpoint is
   worth more than one coder dispatch, A+B wins. Below V/C = 1, dispatching costs
   more than it returns and the cheapest static `F=1` wins.
4. And A+B can win on **cost** too, not just coverage: when `K < F` Mode A wastes
   its surplus dispatches (`validate_k2`), so A+B is leaner.

## The decision rule

> **Author a static `.dot` (Mode A) by default.** It ties dynamic fan-out on
> first-pass tasks (Finding 1) and on any graph whose shape is known at authoring
> time (Finding 4.1), with less code and lower latency. Thread adjacent state with
> `${state._last_output}` when a node needs its predecessor's output — that is free
> today (Finding 3).
>
> **Reach for dynamic fan-out (Mode A+B) only when the graph's node count is
> determined by a quantity discovered at runtime (G3) and V/C > 1** — i.e. the work
> per discovered item is worth more than one coder dispatch. That is the single
> condition under which A+B is structurally required rather than merely different.
>
> Everything else A+B appears to buy (inter-node data flow) is an authoring choice
> or a 1-line engine change, and should be labeled as such, never as a paradigm win.

## Caveats (what is NOT claimed)

- `dynamic_fanout` token figures are a **call-count model**, not metered Sonnet
  billing. They model number-of-dispatches — the quantity that actually differs —
  not dollars.
- `workflow_graphgen`'s null is scoped to **first-pass build tasks** with
  `${goal}`-only prompts. It does not claim A and A+B are equivalent on
  multi-pass, repair-loop, or runtime-shape tasks — Finding 3/4 are exactly the
  cases where they diverge.
- These are mechanism benchmarks. They isolate *dispatch* by holding the coder
  constant; they do not measure real-model quality variance.

## Artifacts

| File | What |
|------|------|
| `benchmarks/workflow_graphgen/RESULTS.md` | the n=10 real-Sonnet null |
| `benchmarks/dynamic_fanout/RESULTS.md` | calibration: same aggregator credits winners |
| `benchmarks/dynamic_fanout/SWEEP.md` | K×F parametric sweep + analytic decision rule |
| `tests/test_state_threading.py` | proves `${state._last_output}` threading works in the runner today |
| `tests/test_dynamic_fanout.py` | scenario/coder/evaluator + aggregator-credits-winners + honest-A tie |
| `tests/test_dynamic_fanout_sweep.py` | locks the spread-0-tie / spread>0-breakeven theorems |

## Reproduce

```bash
export DARK_FACTORY_HOME=$PWD
.venv/bin/python -m benchmarks.dynamic_fanout --trials 5 --out /tmp/dynfan/records.jsonl
.venv/bin/python -m benchmarks.dynamic_fanout.sweep          # regenerates SWEEP.md
.venv/bin/python -m pytest tests/test_dynamic_fanout.py tests/test_dynamic_fanout_sweep.py tests/test_state_threading.py -q
```
