# Benchmark prompt-shape asymmetry

**Status:** intentional, per-benchmark, NOT a canonical contract.

The two production-shape benchmarks in this repo, `benchmarks/airbnb-clone/`
and `benchmarks/amazon-clone/`, use **structurally different prompt and
pipeline shapes** by design. This file documents what each looks like and why
the asymmetry is not a refactor target.

## Why this exists

A read-only audit (2026-06-27) found that `prompts/slim/*` and
`prompts/catalog/*` are visibly biased toward world-architect / D&D-style
example phrasing in unrelated benchmarks, and the runner code carries
benchmark-specific assumptions (Firebase emulator hardcoded in
`runner/handler_holdout.py`, iOS lint pattern global in
`runner/pre_review_lint.py`, Gemini-named evidence defaults in
`runner/handler_audit.py`). That class of bias — application code baked for
one benchmark and quietly assumed everywhere — is the real defect.

The **pipeline shape itself** (sprint × N vs. slice × M + parallel) is the
opposite: it is **intentional per-benchmark**, lives in the benchmark's own
`pipelines/*.dot` and `prompts/*.md`, and is **not** a shared contract that
needs harmonization. Future benchmarks may adopt either shape or invent a
third; the runner does not require any single shape.

## The two shapes

### airbnb-clone: `sprint × 3` (sequential sprints, one verify/fix triple each)

`pipelines/airbnb-clone/airbnb-clone.dot` decomposes the spec into three
sequential sprints (Data → Backend → Frontend). Each sprint is a
`plan → implement → holdout_eval → [fix] → next sprint` loop, and only the
last sprint can route to `exit`. Files:

- `pipelines/sprint-{1,2,3}-*.dot` (one per sprint, callable individually)
- `prompts/sprint-{1,2,3}-{plan,implement,fix}.md` (nine prompt files, three
  per sprint)
- `pipelines/airbnb-clone.dot` (master graph that wires the three sprints in
  series with `outcome=success` edges advancing to the next sprint)

Why sprint × 3: the airbnb spec is naturally layered (Data must exist before
Backend can validate against it; Backend contracts must exist before
Frontend can consume them). The shape makes the inter-sprint dependency
explicit in the graph itself.

### amazon-clone: `slice × parallel × fix` (multiple slices, optionally concurrent, then merged)

`pipelines/amazon-clone/` ships several slice graphs that target disjoint
product surfaces:

- `slim.dot` — single-agent minimal loop over the whole spec
- `dark_factory.dot`, `kilroy.dot`, `mammoth.dot`, `smasher.dot`,
  `tracker.dot` — multi-attractor references
- `slices_{foundation,cart,catalog,checkout}.dot` — slice-level pipelines
  that an operator can run in parallel against separate worktrees, each with
  its own public acceptance gate and `fix` loop
- `slices_cart.dot` / `slices_catalog.dot` / `slices_checkout.dot` are
  designed to be dispatchable concurrently under `pipelines/amazon-clone/`

Why slice × parallel × fix: the amazon spec is broader and the surfaces are
more independent — the cart can be implemented and verified without the
catalog being done. Parallel slices compress wall time on multi-attractor
lanes, and per-slice public acceptance + fix keeps each surface's failure
isolated from the others.

## What this means for new benchmarks

A new benchmark in `benchmarks/<name>/` picks **the shape that fits its
spec**, not the shape of the largest existing benchmark. Concretely:

- A spec that decomposes into **ordered layers** (data → backend →
  frontend) → consider `sprint × N`. Useful when each layer is a hard
  prerequisite for the next.
- A spec that decomposes into **independent surfaces** (catalog, cart,
  checkout, account, …) → consider `slice × M`. Useful when surfaces can be
  implemented concurrently and merged without a strict layer order.
- A spec that is a single self-contained artifact (CLI, library, function)
  → use one of the smoke shapes (`pipelines/factory/hello.dot`,
  `fibonacci/pipelines/slim.dot`, or the roman-style harness under
  `benchmarks/workflow_graphgen/`).

The runner's only invariant is `start` + `exit` present; everything else is
benchmark-owned. See `docs/pipeline-selection.md` for the pipeline-selection
table and `runner/parser.py` for the parser contract.

## Anti-pattern (do not copy)

Do **not** create a benchmark-shaped prompt template under `prompts/slim/`
or `prompts/catalog/` and assume every benchmark inherits it. Those prompts
are deliberately thin and stack-agnostic (see Lane A–D fixes in the
2026-06-27 audit for the world-architect term-strip work). Per-benchmark
prompt files live under the benchmark directory (`prompts/`, sibling to
`pipelines/`, `spec.md`, `visible_acceptance.md`) and are written for that
benchmark only.