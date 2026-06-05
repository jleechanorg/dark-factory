# workflow_graphgen — A vs A+B benchmark

Measures one question: **does it matter *who* executes the dynamic middle of a
generated pipeline graph?**

A per-goal graph has two parts:

- **guaranteed tail (the "spine")** — pinned terminal reviewer nodes
  (`code_reviewer` → `gate_es` → `gate_er`) that always run, in order, to `exit`.
- **dynamic middle** — a generated `plan → implement …` chain that does the
  actual coding work.

The benchmark holds the graph, the coder backend/model, the diff handoff, and the
reviewer spine **identical** across two modes, varying only the middle's executor:

| Mode | Who runs the dynamic middle | Who runs the spine |
|------|-----------------------------|--------------------|
| **A**   | the Python **runner** walks a middle-only graph (`start → middle → exit`) | the runner |
| **A+B** | the **harness** calls the same `_codergen` per middle node directly | the runner |

Because both modes call the *same* `_codergen` claude/Sonnet path on the *same*
prompts and then commit onto the *same* `baseline_ref` for the *same* spine to
grade, the single independent variable is the middle's executor.

## Fairness invariants (enforced in code)

- **Shared IR.** The graph-IR is generated **once per goal** and drives both
  modes (`run_benchmark` generates before the mode loop). `graph_quality` is
  therefore mode-invariant by construction.
- **Diff handoff (spec §7a).** Each run records `baseline_ref = HEAD` before the
  middle, commits the combined middle diff on that ref, and runs the spine
  against the committed diff. An empty diff vs `baseline_ref` ⇒ `diff_empty=True`
  ⇒ not zero-touch.
- **Identical spine.** Both modes render and run the same `render_spine_dot`
  graph through the runner.
- **No retry wiring.** `assert_no_retry_wiring` raises if the rendered middle or
  spine enables `goal_gate` or declares a `retry_target` — otherwise Mode A could
  self-repair while the A+B spine (no `fix` node) could not, confounding the
  conformance / zero-touch axes.
- **Token parity (spec §9b).** Both modes read the **identical** coder-execution
  fields (`TOKEN_FIELDS = tokens_in, tokens_out, wall_ms`) for the **dynamic
  middle only**. Reviewers, generator, and orchestration are excluded (equal by
  construction). Generator cost is recorded separately under `gen_tokens`.

## Scored axes

| Axis | Source | Better |
|------|--------|--------|
| **conformance** | sealed evaluator aggregate (`{pass,total,available}`) | higher |
| **cost (tokens)** | `tokens_in + tokens_out` over the middle | lower |
| **graph_quality** | shared IR: presence 35% + edge_validity 35% + fit 30% | higher |
| **zero_touch** | middle ok ∧ spine reached exit ∧ non-empty diff | higher |

> Coder tokens are only observable because the claude codergen branch runs with
> `--output-format json`; the envelope's `usage`/`total_cost_usd` are parsed by
> `runner.handlers._claude_json_result`. Plain `--print` emits no usage banner.

## Aggregation (spec §11.4)

`scoring.aggregate` declares a per-axis winner **only** when the two modes' trial
ranges do not overlap (`separated`); overlapping ranges report
`no separation at n=<k>`; missing data on either side reports
`insufficient data`. No winner is asserted from a single trial unless the ranges
are already disjoint.

## Running

From the dark-factory checkout (needs `DARK_FACTORY_HOME` set):

```bash
python -m benchmarks.workflow_graphgen \
  --features hello,roman --modes A,A+B --trials 1 \
  --backend claude --model claude-sonnet-4-6 \
  --workroot /tmp/wfgg-smoke --out /tmp/wfgg-smoke/records.jsonl
```

Outputs:

- `--out` — one JSON record per run (JSONL), 4 axes each.
- `<out>.aggregate.json` — the §11.4 directional signal.
- stdout — `{"records": [...], "aggregate": {...}}` for piping.

Feature goals are holdout-free generic coding tasks defined in `harness.SMOKE_GOALS`
(`hello`, `roman`). The smoke conformance axis is `available: False` (no sealed
evaluator wired into the smoke lane); wire a `holdout_evaluator` callable into
`run_benchmark` to populate it.

## Layout

| File | Role |
|------|------|
| `graph_ir.py` | `GraphIR` + renderers (`render_full_dot` / `render_spine_dot` / `render_middle_only_dot`); `GUARANTEED_NODES` |
| `generator.py` | `deterministic_generator(goal, *, backend, model_name)` → `GraphIR` |
| `harness.py` | Mode A / A+B executors, `run_one`, `run_benchmark`, `SMOKE_GOALS` |
| `scoring.py` | `graph_quality`, token accounting, `assemble_record`, `aggregate` |
| `__main__.py` | CLI entrypoint |

Governing spec: [`specs/workflow_graphgen.md`](../../specs/workflow_graphgen.md).
