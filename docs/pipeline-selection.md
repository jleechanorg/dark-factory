# Pipeline selection

**Do not default every run to one `.dot` file.** Pick the graph that matches the
task. If the user passes `--pipeline`, use it. Otherwise classify the goal first
(see `factory-spec` skill Step 0: greenfield vs brownfield).

## Decision table

| Task | Pipeline | Notes |
|------|----------|-------|
| Wiring smoke / install verify | `pipelines/factory/hello.dot` | `--backend echo` |
| New feature, full production loop | `pipelines/slim/minimal_feature.dot` | plan → test → review → holdout → gates |
| New feature, minimal loop | `pipelines/factory/hello.dot` | plan → implement → holdout → fix |
| In-flight PR iteration | `pipelines/slim/minimal_pr.dot` | no holdout; set `--state slim.test_command=...` |
| Validate diff + sealed holdout | `pipelines/factory/gates.dot` | code already implemented |
| Validate in-flight PR (no holdout) | `pipelines/factory/pr_gates.dot` | evidence gates only |
| Spec review (line-aware, slim) | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` | acceptance + codex reviewer |
| Spec review (+ stack smoke) | `benchmarks/attractor-spec-review/pipelines/review_full.dot` | adds fullstack smoke |
| Brownfield replace / delete | **custom goal + often `minimal_feature.dot` or custom `.dot`** | apply factory-spec delete-first rules; never treat as greenfield additive |

## Independent reviewer (adversarial-by-default)

The slim pipelines (`minimal_feature.dot`, `minimal_pr.dot`) pin the `review`
node to `backend="codex"` so the reviewer is an **independent agent on a
different backend than the coder** by default — `--backend claude` (or any coder
backend) does **not** override it, because an explicit node `backend` attr wins
over both the model stylesheet and `--backend`/`ctx.backend`. **`codex` must be
installed** for these pipelines to run end-to-end.

To override the reviewer backend, either edit `backend=` on the review node in
the `.dot`, or supply a `model_stylesheet` whose `.review` rule sets a different
backend (note: a stylesheet rule only applies if the node has no explicit
`backend` attr, so removing the node attr is required for the stylesheet to win).
Coder nodes (plan/implement/fix/test) carry no backend rule and honor `--backend`.

## Short names (expanded by skill)

`gates`, `hello`, `pr_gates`, `minimal_pr`, `minimal_feature`, `review_slim`, `review_full`

## Invocation

Always use the installed binary from the **target repo** cwd:

```bash
export PATH="$HOME/.local/bin:$PATH"
export DARK_FACTORY_HOME=~/projects/dark-factory
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts

cd /path/to/target-repo
dark-factory --pipeline pipelines/slim/minimal_pr.dot --goal "..." --backend claude
```

Pipelines and prompts resolve from `$DARK_FACTORY_HOME`; code changes land in cwd.
