# Pipeline selection

**Do not default every run to one `.dot` file.** Pick the graph that matches the
task. If the user passes `--pipeline`, use it. Otherwise classify the goal first
(see `factory-spec` skill Step 0: greenfield vs brownfield).

## Decision table

| Task | Pipeline | Notes |
|------|----------|-------|
| Wiring smoke / install verify | `pipelines/factory/hello.dot` | `--backend echo` |
| New feature, full production loop | `pipelines/slim/minimal_feature.dot` | explore → plan → test → review → holdout → gates |
| New feature, minimal loop | `pipelines/factory/hello.dot` | plan → implement → holdout → fix |
| In-flight PR iteration | `pipelines/slim/minimal_pr.dot` | explore → plan → …; no holdout; set `--state slim.test_command=...` |
| Bug fix with red/green discipline | `pipelines/bug_fix.dot` | reproduce (fresh test) → red gate → fix → green gate; max 3 fix visits |
| Validate diff + sealed holdout | `pipelines/factory/gates.dot` | code already implemented |
| Validate in-flight PR (no holdout) | `pipelines/factory/pr_gates.dot` | evidence gates only |
| Spec review (line-aware, slim) | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` | acceptance + codex reviewer |
| Spec review (+ stack smoke) | `benchmarks/attractor-spec-review/pipelines/review_full.dot` | adds fullstack smoke |
| Brownfield replace / delete | **custom goal + often `minimal_feature.dot` or custom `.dot`** | apply factory-spec delete-first rules; never treat as greenfield additive |

## Role-based model routing (slim pipelines)

`minimal_feature.model.css` routes by node class:

| Class | Nodes | Default routing |
|-------|-------|-----------------|
| *(none)* | `explore`, `implement`, `fix` | Honor run-level `--backend` (coder tier) |
| `.plan` | `plan` | `claude` + `claude-opus-4-6` (design/spec tier) |
| `.review` | `review` | `agy` (independent adversarial reviewer) |

Explore uses the **same CLI/model as the coder** (`--backend`). Plan uses a
heavier model for architecture work. Review stays on a separate backend.

To override, set `backend=` / `model_name=` on the node in the `.dot` (explicit
attrs win over the stylesheet). Tests pin `plan` and `review` to `echo` for
offline determinism.

## Short names (expanded by skill)

`gates`, `hello`, `pr_gates`, `minimal_pr`, `minimal_feature`, `bug_fix`, `review_slim`, `review_full`

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
