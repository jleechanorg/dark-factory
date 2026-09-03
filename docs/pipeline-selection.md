# Pipeline selection

**The slim two-node graph is the new default for `/f` and `/factory`
(set 2026-08-02).** The runner's `--pipeline` argparse default is
`two_node.dot`, so a bare `dark-factory …` invocation (no `--pipeline`)
dispatches `pipelines/slim/two_node.dot` — a generic worker plus a fresh,
fully tooled Codex reviewer. Operators opt into richer pipelines by
passing `--pipeline <name>` explicitly. Custom dot graphs are always
expressible via `--pipeline /absolute/path/to/your.dot`.

The "auto-select from the goal" rule that previously lived in the
factory skill is retired. The slim two-node shape is the new default
across the board.

Explicit cold-review-v1 graphs retain their controller-specific resume rules;
the default two-node graph does not select that contract.

## Decision table

| Task | Pipeline | Notes |
|------|----------|-------|
| **Default `/f` / `/factory` invocation** | **`pipelines/slim/two_node.dot`** | Generic worker + fresh fully tooled Codex reviewer + verbatim feedback loop. The user-stated default since 2026-08-02. |
| Two-node with holdouts (opt-in) | `pipelines/slim/two_node_holdout.dot` | Optional opt-in: generic worker + fresh Codex reviewer + behavioral holdout eval. Does not alter the default `/f` / `/factory` pipeline. |
| Wiring smoke / install verify | `pipelines/factory/hello.dot` | `--backend echo`; explore → plan → implement → holdout → fix (max 3) → exit |
| New feature, full production loop | `pipelines/slim/minimal_feature.dot` | explore → plan → test → review → holdout → gates |
| New feature, minimal loop | `pipelines/factory/hello.dot` | plan → implement → holdout → fix |
| In-flight PR iteration | `pipelines/slim/minimal_pr.dot` | explore → research → plan → … → review → holdout → gates; set `--state slim.test_command=...`; requires `$DARK_FACTORY_HOLDOUTS` |
| PR `/ready` enforcement + fix iteration | `pipelines/slim/ready.dot` | test → /es → /er → /advice → holdout → fix (max 3 visits); enforces all /ready gates |
| Bug fix with red/green discipline | `pipelines/bug_fix.dot` | reproduce (fresh test) → red gate → fix → green gate → holdout → adversarial evidence; max 3 fix visits |
| Validate diff + sealed holdout | `pipelines/factory/gates.dot` | code already implemented (no fix loop; explore rollout intentionally skipped — see PR for rationale) |
| Validate in-flight PR | `pipelines/factory/pr_gates.dot` | holdout + evidence gates (no fix loop) |
| Spec review (line-aware, slim) | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` | acceptance + codex reviewer |
| Spec review (+ stack smoke) | `benchmarks/attractor-spec-review/pipelines/review_full.dot` | adds fullstack smoke |
| Produce a reviewed spec only (no implementation) | `pipelines/slim/spec_gen.dot` | explore → plan → cold spec review → fix loop; no implement node; rejects specs with parallel lanes lacking a file-ownership matrix |
| Research report only (no implementation) | `pipelines/slim/minimal_research.dot` | explore fanout → research → exit; writes `.dark-factory/research-findings.md`; no implement node |
| Brownfield replace / delete | **custom goal + often `minimal_feature.dot` or custom `.dot`** | apply factory-spec delete-first rules; never treat as greenfield additive |
| Repo-specific pipeline (in the target repo itself) | `<target_repo>/dark-factory/pipelines/<name>.dot` | dark-factory's runner looks up `--pipeline <name>` (bare filename only) under `<workdir>/dark-factory/pipelines/` **before** delegating to `resolve_factory_path(<name>)`, which itself checks cwd → `$DARK_FACTORY_HOME/<name>`. Use this for graphs that hardcode the target repo's slash commands (e.g. worldarchitect's `pr_gates_split_cs.dot` uses `/zfc`, `/zfclevel`, `/thermo`). The `<workdir>/dark-factory/pipelines/` lookup is the target-repo subdir convention; see `runner/paths.py::resolve_pipeline_path` for the canonical order. |

## Holdout-always policy

**Every implement-bearing lane runs the sealed behavioral holdouts**
(`holdout_eval` node) before its evidence gates. Operator standing order:
green tests + a fresh-eyes review prove the diff matches its own intent, not
that it preserved the feature's sealed behavioral contract. Lanes with an
`implement`/`fix` node and no holdout are a policy gap, not a variant.

Current coverage: `gates.dot`, `pr_gates.dot`, `hello.dot`,
`minimal_feature.dot`, `minimal_pr.dot`, `bug_fix.dot` all carry holdout.
Deliberate exceptions:

- `pipelines/slim/spec_gen.dot` — no implement node; nothing to evaluate.
- `pipelines/slim/minimal_research.dot` — no implement node; research report
  only.
- `pipelines/slim/review_pr.dot` — holdouts test feature scenarios, not PR
  diffs (see its header comment).

## Loop bounds (`max_visits`)

Every `fix` loop in the feature lanes is bounded by a `max_visits="N"`
attribute on the `fix` node. The engine emits a synthetic `exhausted` step
when a node is visited more than `max_visits` times, then terminates the
run. This is **graph-level** enforcement (per-node per-pipeline), distinct
from `max_retries` which is **handler-level** (per-codergen attempt
re-execution on a single visit).

Current bounds:

| Pipeline | fix node | max_visits |
|----------|----------|------------|
| `pipelines/factory/hello.dot` | `fix` | `3` |
| `pipelines/factory/gates.dot` | n/a (no fix loop) | n/a |
| `pipelines/slim/minimal_feature.dot` | `fix` | `3` |
| `pipelines/slim/minimal_pr.dot` | `fix` | `3` |
| `pipelines/slim/ready.dot` | `fix` | `3` |
| `pipelines/bug_fix.dot` | `fix` | `3` |

`hello.dot` also carries an explicit `fix -> exit [condition="outcome=exhausted"]`
edge as a safety net for any future engine revision that routes on
`exhausted` rather than terminating.

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

## Adversarial-review priority queue (`gate_er` and `gate_es`)

For real-LLM reviews (not webhook pings), reviewer-gate nodes (`gate_er`,
`gate_es`) accept a **`backend_priority=...`** attribute (comma-separated
list) that picks the **first installed** entry via `which <name> +
<name> --version`. The default dark-factory priority is
`codex > minimax > agy > claude-sonnet`; set
`DARK_FACTORY_ADVERSARIAL_PRIORITY` (comma-separated) to override per-run.
Setting **`prefer_adversarial: true`** drops the run-level coder backend
from the queue so a `claude` coder run never gets a `claude` reviewer
(guarantees the reviewer is a different vendor). The priority queue is the
**first** adversarial pass selector — it is NOT a retry cascade. A real
`fail|partial` from the chosen backend is kept (no-reviewer-shopping rule).

```dot
evidence [
    type="gate_er",
    backend_priority="codex,minimax,agy,claude-sonnet",
    prefer_adversarial="true"
]
```

Resolver audit metadata lands in the gate's `Result.metadata`:
`adversarial_priority`, `adversarial_resolved`, `adversarial_skipped`,
`prefer_adversarial`, `reviewer_backend_resolution` — so the operator/CXDB
can see exactly which backend graded the diff and why.

## Benchmark prompt shape (intentionally per-benchmark)

`airbnb-clone` uses a `sprint × 3` shape (sequential
plan→implement→verify→fix loops, one per sprint), while `amazon-clone` uses
a `slice × parallel × fix` shape (multiple disjoint slice graphs that can
be dispatched concurrently, each with its own fix loop). The two shapes are
**intentional and benchmark-owned**, not a canonical contract that needs
harmonization. See [docs/benchmark-shape.md](benchmark-shape.md) for the
rationale and what to copy when authoring a new benchmark.

## Short names (expanded by skill)

`gates`, `hello`, `pr_gates`, `minimal_pr`, `minimal_feature`, `minimal_research`, `ready`, `bug_fix`, `review_slim`, `review_full`, `spec_gen`

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
