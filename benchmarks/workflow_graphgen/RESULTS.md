# workflow_graphgen — real A-vs-A+B measurement (n=10)

**Run date:** 2026-06-05 · **Backend:** `claude` · **Model:** `claude-sonnet-4-6`
**Design:** middle-only (`--no-spine`), `--conformance local`, 10 trials × 2 features × 2 modes = **40 real Sonnet runs**.
**Raw evidence:** [`results/n10_records.jsonl`](results/n10_records.jsonl) · [`results/n10_aggregate.json`](results/n10_aggregate.json)
**Instrument SHA at run:** `2c1e48b` (+ uncommitted measurement instrument, this change).

## What is being measured

The single independent variable is **who executes the dynamic middle graph**:

- **Mode A** — the Python runner walks `render_middle_only_dot` (engine-dispatched).
- **Mode A+B** — the harness loops `ir.middle_chain()` and calls `_codergen(Node(...))` per node directly.

Everything else is held fixed: shared graph-IR, identical `baseline_ref` handoff, identical
coder backend/model, and a coder prompt that is **byte-identical across modes** (the
`plan`/`implement` templates interpolate only `${goal}`, never `${state.*}` — enforced by
`test_plan_implement_prompts_interpolate_only_goal`).

## Headline result: no separation on any axis at n=10

| Feature | Axis | A (mean ± sd) | A+B (mean ± sd) | Verdict |
|--------|------|---------------|-----------------|---------|
| hello | conformance | 1.00 ± 0.00 | 1.00 ± 0.00 | **tie** (50/50 vs 50/50) |
| hello | tokens_total | 281 910 ± 27 175 | 281 398 ± 38 755 | no separation (ranges overlap) |
| hello | wall_ms | 41 373 ± 3 873 | 43 550 ± 6 030 | no separation (A+B +5.3%, overlaps) |
| hello | zero_touch | 1.00 | 1.00 | tie |
| roman | conformance | 1.00 ± 0.00 | 1.00 ± 0.00 | **tie** (90/90 vs 90/90) |
| roman | tokens_total | 304 748 ± 39 946 | 313 719 ± 38 618 | no separation (ranges overlap) |
| roman | wall_ms | 48 700 ± 3 637 | 53 190 ± 4 868 | no separation (A+B +9.2%, overlaps) |
| roman | zero_touch | 1.00 | 1.00 | tie |

`graph_quality` is **insufficient data by construction** — it grades the shared IR (mode-invariant)
and no live `fit` reviewer score was wired for this run, so it is recorded unscored, never a silent zero.

No axis crosses the §11.4 non-overlap bar, so **zero winners are credited** at n=10.

## Findings

1. **The dispatch path does not change output quality or cost for self-contained coding tasks.**
   Both modes produce fully correct `hello.py`/`roman.py` (perfect conformance both sides) at
   statistically indistinguishable token cost. For these tasks, "who runs the middle" is a wash.

2. **The n=1 token signal was model variance, not structure — and the min-n guard caught it.**
   The n=1 pilot showed Mode A ~40k tokens cheaper on hello, which *looked* like a dispatch-call-count
   signal. At n=10 the per-call input variance is sd ≈ 27k–40k and the ranges overlap almost
   completely (A+B is actually *slightly lower* on hello). The earlier "A is cheaper" read was noise.
   `MIN_N_FOR_WINNER = 5` plus range-non-overlap is exactly what prevented that false win from being
   crowned — this is the measurement working as designed.

3. **The only consistent directional hint is wall-clock.** A+B is slower on *both* features
   (+5.3% hello, +9.2% roman), consistent with the extra Python dispatch overhead of the per-node
   `_codergen` loop. But sd is large (machine/network), ranges overlap, so it is **not credited**.
   This is the one axis worth re-running at higher n if a latency claim is ever needed.

## What this measurement can and cannot prove

- **Can prove:** conformance (self-contained public acceptance criteria — no sealed repo, no
  isolation break), token cost (unconfounded — byte-identical prompt), wall-clock, zero-touch.
- **Cannot prove (structural):** `graph_quality` can *never* separate A from A+B — both consume the
  same IR. Any graph-quality claim is mode-invariant and must not be cited as an A-vs-A+B difference.
- **Honest scope:** hello/roman are easy, self-contained tasks. A null result here means the dispatch
  path is irrelevant *for tasks the model solves on the first pass*. It does **not** generalize to
  tasks with fix-loops, where A+B's per-node control could diverge — that is the next experiment.

## Reproduce

```bash
export DARK_FACTORY_HOME=$PWD; export PATH="$HOME/.local/bin:$PATH"
.venv/bin/python -m benchmarks.workflow_graphgen \
  --features hello,roman --modes A,A+B --trials 10 \
  --backend claude --model claude-sonnet-4-6 \
  --conformance local --no-spine \
  --workroot /tmp/wfgg-real --out /tmp/wfgg-real/records.jsonl
```
