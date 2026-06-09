---
name: factory-spec
description: "Dark Factory spec workflow (/factory-spec, /fs): create a reviewed spec via the spec_gen pipeline, review an existing spec, or display the pipeline node graphs. Create mode runs the pipeline; review and show modes are in-session only."
---

# /factory_spec — Dark Factory Spec Workflow & Node Graph Reference

## Install vs source

Dark Factory runs via the **`dark-factory` binary**, not `python -m runner` from
a raw checkout:

```bash
./install.sh   # uv python + venv + ~/.local/bin/dark-factory
export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$(pwd)}"
export PATH="$HOME/.local/bin:$PATH"
```

- **`/fs` / `/factory-spec`** — spec creation (via pipeline), review, or graph reference.
- **`/f` / `/factory`** — run feature/implementation pipelines via `dark-factory`.

Pipelines and prompts resolve from `$DARK_FACTORY_HOME`; implementation work
happens in the caller's cwd (`--workdir` defaults to cwd).

## Pipeline selection (mandatory before `/f` or `/factory`)

Full decision table:
[docs/pipeline-selection.md](../../../docs/pipeline-selection.md)

**Do not default every run to one `.dot`.** If the user did not pass `--pipeline`,
classify the goal (Step 0 below) and pick from this quick guide:

| Task | Pipeline |
|------|----------|
| **Create a reviewed spec** | `pipelines/slim/spec_gen.dot` ← `/fs` create mode |
| Smoke / wiring | `pipelines/factory/hello.dot` |
| New feature (full loop) | `pipelines/slim/minimal_feature.dot` |
| PR iteration (no holdout) | `pipelines/slim/minimal_pr.dot` |
| Validate diff + holdout | `pipelines/factory/gates.dot` |
| PR gates only | `pipelines/factory/pr_gates.dot` |
| Spec review slim | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` |
| Spec review full | `benchmarks/attractor-spec-review/pipelines/review_full.dot` |
| Brownfield replace/delete | custom goal + delete-first rules; often `minimal_feature.dot` or custom `.dot` |

Short names for `--pipeline`: `spec_gen`, `gates`, `hello`, `pr_gates`, `minimal_pr`,
`minimal_feature`, `review_slim`, `review_full`.

Execution command (from target repo cwd):

```bash
export PATH="$HOME/.local/bin:$PATH"
dark-factory --pipeline pipelines/slim/minimal_pr.dot --goal "..." --backend claude
```

## Purpose

Show the factory pipeline graph structure at a glance — node types, edges,
conditions, and handler mappings — without running a pipeline. Use this when
you need to remember what nodes exist, what the wiring looks like, or which
pipeline to pick for a given goal.

## Spec-Review Pipelines (Primary)

These are the Attractor-style spec-validation pipelines under
`benchmarks/attractor-spec-review/pipelines/`. The key innovation: an
**independent cold reviewer** (`codex exec --yolo`) that numbers every spec
line and returns strict JSON line-by-line findings.

### 1. `review_slim.dot` — Line-Aware Spec Review (Slim)

```
start ──▶ plan ──▶ implement ──▶ acceptance ──(success)──▶ review ──(success)──▶ exit
                                    │                       │
                                    └──(fail)──▶ fix ◀──────┘ (max 3 visits)
                                                      │
                                                      └──▶ implement (loop)
```

| Node | Type | Command / Prompt | What it does |
|------|------|-----------------|-------------|
| `start` | built-in | — | Entry point |
| `plan` | `codergen` | `@benchmarks/attractor-spec-review/prompts/plan.md` | Plan from spec |
| `implement` | `codergen` | `@benchmarks/attractor-spec-review/prompts/implement.md` | Write code |
| `acceptance` | `tool` | `python scripts/validate_spec.py --spec spec/feature.md --report spec_review/validation_report.json` | Line-numbered spec validation |
| `review` | `tool` | `benchmarks/attractor-spec-review/scripts/review_with_codex.sh . spec/feature.md spec_review/independent_reviewer.json` | Independent cold reviewer via `codex exec --yolo`; returns JSON verdict |
| `fix` | `codergen` | `@benchmarks/attractor-spec-review/prompts/fix.md`, `max_visits=3` | Fix failures, loops back to implement |
| `exit` | built-in | — | Terminal node |

**Use when:** validating a spec implementation against line-numbered acceptance
+ independent reviewer, without full-stack smoke.

### 2. `review_full.dot` — Line-Aware Spec Review (Full)

```
start ──▶ plan ──▶ implement ──▶ acceptance ──(success)──▶ stack_smoke ──(success)──▶ review ──(success)──▶ exit
                                    │                       │                       │
                                    └──(fail)──▶ fix ◀──────┘                       └──(fail)──▶ fix
                                                      │
                                                      └──▶ implement (loop)
```

Same as slim, plus a `stack_smoke` node between acceptance and review:

| Node | Type | Command | What it does |
|------|------|---------|-------------|
| `stack_smoke` | `tool` | `bash scripts/fullstack_smoke.sh` | Full-stack smoke test before reviewer |

**Use when:** you need end-to-end runtime verification before the independent
reviewer checks spec conformance.

### Key node: Independent Reviewer

The `review` node runs `review_with_codex.sh` which:

1. Numbers every spec line (`0001: ...`, `0002: ...`)
2. Shells out to `codex exec --yolo` — **separate process, cold reviewer**
3. Returns strict JSON: line-by-line findings + verdict (`pass`/`fail`)
4. `goal_gate=true` + `retry_target="fix"` — failure routes to fix loop

This is the Attractor guarantee: the reviewer has never seen the implementation
prompt, only the spec and the code diff.

## Factory Pipelines (Validation Gates)

### 3. `gates.dot` — 4-Gate Validation

```
start ──▶ holdout_eval ──(success)──▶ gate_es ──(success)──▶ gate_er ──(success)──▶ gate_cs ──▶ exit
                 │                      │                      │
                 └──(fail)──▶ exit      └──(fail)──▶ exit     └──(fail)──▶ exit
```

| Node | Type | Handler | What it does |
|------|------|---------|-------------|
| `holdout` | `holdout_eval` | Sealed evaluator at `$DARK_FACTORY_HOLDOUTS/evaluator/run.py` | Runs hidden test scenarios against the diff |
| `gate_es` | `gate_es` | `claude --print /es` | Evidence standards check |
| `gate_er` | `gate_er` | `claude --print /er` | Evidence review check |
| `gate_cs` | `gate_code_standards` | `claude --print /code_standards` | ZFC + leveling + root-cause-first |

**Use when:** already-implemented diff needs Attractor-style 4-gate validation (requires holdout).

### 3.5 `pr_gates.dot` — 3-Gate PR Validation (No Holdout)

```
start ──▶ gate_es ──(success)──▶ gate_er ──(success)──▶ gate_cs ──▶ exit
             │                      │
             └──(fail)──▶ exit      └──(fail)──▶ exit
```

| Node | Type | Handler | What it does |
|------|------|---------|-------------|
| `gate_es` | `gate_es` | `claude --print /es` | Evidence standards check |
| `gate_er` | `gate_er` | `claude --print /er` | Evidence review check |
| `gate_cs` | `gate_code_standards` | `claude --print /code_standards` | ZFC + leveling + root-cause-first |

**Use when:** validating an in-flight PR diff (like gates.dot but bypasses holdout features).

### 4. `hello.dot` — Plan/Implement/Fix Loop

```
start ──▶ plan ──▶ implement ──▶ holdout_eval ──(success)──▶ exit
                                      │
                                      └──(fail)──▶ fix ──▶ holdout_eval (loop, max 3 visits)
```

**Use when:** adding a new feature from scratch with a holdout scenario.

### 5. `minimal_feature.dot` — Full Feature Factory (Slim)

```
start ──▶ explore ──▶ plan ──▶ implement ──▶ test ──(success)──▶ review ──(success)──▶ holdout ──(success)──▶ gate_es ──(success)──▶ gate_er ──(success)──▶ exit
                                    │                  │                  │                  │
                                    └──(fail)──▶ fix ◀─┘                  │                  │
                                                          └──(fail)──▶ fix ┘                  │
                                                                               └──(fail)──▶ fix ┘

fix ──▶ test (loop)
```

**Use when:** full production pipeline from scratch: test → review → holdout → evidence gates.

### 6. `minimal_pr.dot` — Slim PR Iteration Factory (No Holdout)

```
start ──▶ explore ──▶ plan ──▶ implement ──▶ test ──(success)──▶ review ──(success)──▶ gate_es ──(success)──▶ gate_er ──(success)──▶ exit
                                    │                  │                  │                  │
                                    └──(fail)──▶ fix ◀─┘                  │                  │
                                                          └──(fail)──▶ fix ┘                  │
                                                                               └──(fail)──▶ fix ┘

fix ──▶ test (loop)
```

**Use when:** in-flight PR iteration loop with parameterized test commands (`--state slim.test_command="..."`) and evidence checks, bypassing behavioral holdout scenarios.

## Handler type registry

| Node `type` attr | Handler function | Behavior |
|-------------------|-----------------|----------|
| `codergen` | `_codergen` | Render prompt template, dispatch to backend (claude/codex/ao/agy) |
| `tool` | `_tool` | Shell out to `command="..."` attribute |
| `holdout_eval` | `_holdout_eval` | Run sealed evaluator from `$DARK_FACTORY_HOLDOUTS` |
| `gate_es` | `_gate_es` | Shell out to `claude --print /es` |
| `gate_er` | `_gate_er` | Shell out to `claude --print /er` |
| `gate_code_standards` | `_gate_code_standards` | Shell out to `claude --print /code_standards` |
| `human_gate` | `_human_gate` | Block on stdin or use `ctx.state["<node>.outcome"]` |
| `conditional` | `_conditional` | Hexagon decision node; outcome from `ctx.state[decision_key]` |

## Shape-based handler fallback

| DOT shape | Handler |
|-----------|---------|
| `Mdiamond` | `start` |
| `Msquare` | `exit` |
| `hexagon` | `conditional` |

## Edge condition syntax

- `condition="key=value"` — matches `ctx.state[key] == value`
- `condition="key!=value"` — matches `ctx.state[key] != value`
- No condition — unconditional (fallback)
- Conditional edges tried **before** unconditional ones

## Backend routing

| `--backend` flag | Per-node `backend` attr | CLI invoked |
|------------------|------------------------|-------------|
| `echo` | — | Deterministic mock, no LLM |
| `claude` | `claude` | `claude --print --dangerously-skip-permissions` |
| `codex` | `codex` | `codex exec --yolo` |
| `ao` | `ao` | Agent Orchestrator `ao spawn` |
| `agy` | `agy` | `agy --print --dangerously-skip-permissions` |

## Source files

- Spec-review pipelines: `benchmarks/attractor-spec-review/pipelines/`
- Spec-review scripts: `benchmarks/attractor-spec-review/scripts/`
- Factory pipelines: `pipelines/factory/` and `pipelines/slim/`
- Handlers: `runner/handlers.py`
- Engine: `runner/engine.py`
- Parser: `runner/parser.py`
