# Amazon Clone MVP Benchmark

Attractor-style e-commerce benchmark for comparing orchestration methods under
one visible product contract and one sealed validation layer.

This benchmark currently has two execution scopes:

- **Deterministic orchestration smoke**: runs every DOT pipeline with mock
  coder/holdout/tool behavior to prove the graph shape is executable.
- **Sealed candidate evaluation**: runs real coding agents and the sibling
  sealed evaluator. This requires external agent setup and normalized budgets
  before results can be called a fair method comparison.

## Revealed vs. sealed

### Revealed to coder agents

- `spec.md`: full visible product requirements.
- `visible_acceptance.md`: public scoring and acceptance checklist.
- `starter/`: starting application files.
- `prompts/`: public task prompts.
- `pipelines/*.dot`: workflow graph for the selected method.
- Redacted evaluator summaries: aggregate verdicts, counts, and high-level
  failure categories only.

### Sealed from coder agents

- Exact holdout scenario names and IDs.
- Concrete input values used by the evaluator.
- Playwright selectors and assertion code.
- Raw per-scenario failure records.
- Hidden evaluator source and sibling-repo paths.
- Other candidates' private run artifacts.

The visible spec must be complete enough to implement the product fairly. The
sealed layer hides adversarial examples and scoring internals, not material
user-story requirements.

## Methods

| Method | Pipeline | Current scope |
|---|---|---|
| `dark-factory` | `pipelines/dark_factory.dot` | DOT adapter for full Dark Factory loop |
| `df-slim` | `pipelines/slim.dot` | Minimal implement/verify/fix loop |
| `kilroy` | `pipelines/kilroy.dot` | DOT adapter shaped like the Dan Shapiro/Kilroy loop |
| `smasher` | `pipelines/smasher.dot` | DOT adapter for the Smasher lane |
| `mammoth` | `pipelines/mammoth.dot` | DOT adapter for the Mammoth lane |
| `tracker` | `pipelines/tracker.dot` | DOT adapter shaped like Harper/Tracker refinement |

The Kilroy, Mammoth, Tracker, and Smasher entries here are local DOT adapters,
not direct invocations of their upstream binaries. Direct upstream adapters are
a separate fairness task.

## Deterministic matrix smoke

Run all five method pipelines without real LLM calls or sealed evaluator access:

```bash
benchmarks/amazon-clone/scripts/run_matrix_deterministic.sh
```

Optional output directory:

```bash
benchmarks/amazon-clone/scripts/run_matrix_deterministic.sh /tmp/amazon-matrix
```

Output:

- `<output>/summary.json`
- `<output>/<method>/run.json`
- `<output>/<method>/checkpoint.json`

This is the right script for checking that Dark Factory, DF-Slim, Kilroy,
Tracker, and Smasher pipeline shapes all execute deterministically. It is not a
product-quality score.

## Sealed candidate run

Prepare a candidate workdir:

```bash
benchmarks/amazon-clone/scripts/prepare_candidate.sh <candidate> <workdir>
```

Run one method against the sealed evaluator:

```bash
export DARK_FACTORY_HOLDOUTS=<sealed-holdouts-repo>
export AO_AGENT=antigravity
export AO_PROJECT=amazon-clone
benchmarks/amazon-clone/scripts/run_candidate.sh <method> <run_id> <workdir>
```

Run the whole real-agent matrix:

```bash
export DARK_FACTORY_HOLDOUTS=<sealed-holdouts-repo>
export AO_AGENT=antigravity
export AO_PROJECT=amazon-clone
benchmarks/amazon-clone/scripts/run_all.sh <workdir-base>
```

Real-agent runs are not deterministic unless the outer harness also pins model
access, runtime budget, token budget, retry budget, starter state, environment,
and sealed evaluator version.

AO's `antigravity` agent plugin launches the `agy` binary. It only adds
`--dangerously-skip-permissions` when the AO project config passes
`agentConfig.permissions: skip`, `permissionless`, or `auto-edit`.

## Scoring

See `SCORING.md`.

`score_candidate.py` grades one produced artifact. It does not by itself prove
a fair method bakeoff.
