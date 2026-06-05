# dynamic_fanout — deterministic A-vs-A+B separation (instrument calibration)

**Run:** deterministic (no LLM) · **n:** 5 trials/cell · 30 records
**Grader:** `benchmarks.workflow_graphgen.scoring.aggregate` — the *same* aggregator
(range-non-overlap + `MIN_N_FOR_WINNER=5`) that returned the
[workflow_graphgen n=10 null](../workflow_graphgen/RESULTS.md).

## Why this exists

The workflow_graphgen measurement found **no separation on any axis at n=10**. A
null is only meaningful if the instrument can detect a real effect when one is
present — otherwise the null could just mean "blind ruler." `dynamic_fanout` is
that calibration: three scenarios engineered so the dispatch path is
*load-bearing*, graded on **real on-disk artifacts** by the **same** aggregator.
If it now credits winners, the workflow_graphgen null was a **true negative**.

The coder is a single deterministic function shared by both modes (`add_validation`,
`write_migration`) — the coder *capability* is identical; only the **dispatch**
differs. That isolates the variable: any separation is attributable to
orchestration, not coder skill. Determinism is intentional — it removes model
variance so a single trial is conclusive, in deliberate contrast to the
stochastic Sonnet n=10.

## Headline result: the same aggregator now credits 4 winners

| Feature | Gap | Axis | A | A+B | Verdict |
|---------|-----|------|----|-----|---------|
| `validate_k6` | **G3** | conformance | 0.50 | **1.00** | **WINNER A+B** (A covers 3 of 6) |
| `validate_k6` | **G3** | tokens_total | **4 050** | 8 100 | **WINNER A** (A+B pays for coverage) |
| `validate_k2` | **G3** | conformance | 1.00 | 1.00 | tie (both reach 100%) |
| `validate_k2` | **G3** | tokens_total | 4 050 | **2 700** | **WINNER A+B** (A wastes a dispatch) |
| `schema_migration` | **G1** | conformance | 0.00 | **1.00** | **WINNER A+B** (state threaded) |
| `schema_migration` | **G1** | tokens_total | 2 700 | 2 700 | tie (both run 2 nodes) |

Contrast: workflow_graphgen credited **0** winners across 5 axes × 2 features.
Same code path, same rule, opposite outcome → **the ruler is not blind**, so the
n=10 null was a genuine equivalence for first-pass tasks.

## What each scenario isolates

### G3 — runtime-determined graph shape (`validate_*`)
A feature exposes **K** endpoints, known only at runtime. Mode A's `.dot` is
authored once with a **fixed** node count `F=3`, so it covers `min(F,K)/K`. Mode
A+B reads the source, **discovers K**, and fans out to exactly K nodes.

- `K=6 > F`: A under-covers (0.50) — **A+B wins conformance**, but spends K calls
  vs F, so **A wins tokens**. The trade is real and the instrument shows both faces.
- `K=2 < F`: A reaches full coverage but **wastes** its 3rd dispatch on a
  nonexistent endpoint — **A+B wins tokens**, conformance ties.

A static graph cannot make its node count depend on a runtime count. This is the
genuine **paradigm gap** — not an engine setting.

### G1 — inter-node data flow (`schema_migration`)
Node 2 (migration) must agree with node 1 (schema) on a column set. Mode A's
prompts read only `${goal}`, so node 2 is blind to node 1's output and falls back
to a static default → **drift (0.00)**. Mode A+B threads `state["schema.columns"]`
→ **match (1.00)**. Tokens tie (both run 2 nodes), so the win is *purely* the gap.
Because A **structurally cannot** thread state today, the win is 100% attributable.

> **Tier honesty:** G1 is an **engine-fixable** gap — wiring `ctx.state["node.output"]`
> + `${state.*}` interpolation into the runner would let a static Mode A graph thread
> state and close it. G3 is a **paradigm** gap — no engine setting lets a fixed `.dot`
> size itself to a runtime K. Label any future "A+B wins" result by which tier it is.

## Fairness: Mode A is given its best static config and is still dominated

The G3 win is not a strawman against a badly-chosen `F`. Coverage `min(F,K)/K`
and call cost for every static `F` across the K distribution (A+B always
`1.00 / Kc`):

| F \ K | K=2 | K=4 | K=6 | K=8 |
|-------|-----|-----|-----|-----|
| F=2   | 1.00/2c | 0.50/2c | 0.33/2c | 0.25/2c |
| F=3   | 1.00/3c | 0.75/3c | 0.50/3c | 0.38/3c |
| F=4   | 1.00/4c | 1.00/4c | 0.67/4c | 0.50/4c |
| F=8   | 1.00/8c | 1.00/8c | 1.00/8c | 1.00/8c |
| **A+B** | **1.00/2c** | **1.00/4c** | **1.00/6c** | **1.00/8c** |

No single static `F` is on the Pareto frontier across K: small `F` under-covers
at large K; large `F` (e.g. `F=8` "over-provision to be safe") reaches coverage
but burns 4× the calls at `K=2`. Mode A+B's "exactly K" dispatch dominates every
fixed choice. That is the paradigm gap stated rigorously.

## What this proves — and does not

- **Proves:** the workflow_graphgen instrument (same aggregator) detects a real
  mechanism difference when one exists; therefore its n=10 null is a true
  negative for first-pass tasks, not an artifact of a blind ruler.
- **Proves:** the separation lives exactly where the brainstorm predicted —
  runtime-determined shape (G3) and inter-node data flow (G1) — the two places
  the dispatch path stops being interchangeable.
- **Does NOT prove** a real-model token delta: token cost here is a deterministic
  call-count model, not metered Sonnet usage. It models *number of dispatches*,
  which is the quantity that actually differs; it is not a billing claim.
- **Does NOT prove** Mode A+B is better in general. `validate_k2` shows A+B can
  also win on *cost* where A over-provisions, and `validate_k6` shows A+B *loses*
  tokens when buying coverage. "Better" is axis- and K-dependent.

## Reproduce

```bash
export DARK_FACTORY_HOME=$PWD
.venv/bin/python -m benchmarks.dynamic_fanout --trials 5 --out /tmp/dynfan/records.jsonl
.venv/bin/python -m pytest tests/test_dynamic_fanout.py -q
```
