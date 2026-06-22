---
description: "/f — full Dark Factory loop; auto-routes to PR-mode when a PR is open on the current branch, otherwise feature-mode. DEFAULT: invoke the .claude/workflows/dark-factory.md workflow (multi-phase orchestration). Pass --legacy to bypass and run a single-phase .dot pipeline directly."
type: quality
execution_mode: immediate
aliases: [f]
---

# /f — Full Dark Factory Loop (auto-routes PR vs feature)

Shortcut for `/factory` oriented toward **full production loops**. **Default
invocation path** is the multi-phase Claude Workflow at
`$DARK_FACTORY_HOME/.claude/workflows/dark-factory.md` (the G3 closure
artifact). The workflow orchestrates `spec_validation → explore → plan →
implement → gate → bounded fix loop` as separate `dark-factory` invocations
sharing one CXDB, so the LLM decides fan-out at workflow-execution time, not
parse time.

Pass `--legacy` (or `--pipeline <name>.dot`) to bypass the workflow and run
a single-phase `.dot` pipeline directly against the `dark-factory` binary.
Use `--legacy` when you want to iterate one phase in isolation (e.g., a
quick `gates.dot` re-run after a fix).

**Prerequisite:** `./install.sh` once; `dark-factory` on PATH via `~/.local/bin`.

**Usage**:

```
/f <goal description>                          # DEFAULT: workflow (6-phase orchestration)
/f --legacy <goal>                             # single .dot pipeline (skip workflow)
/f --legacy --pipeline gates <goal>            # explicit legacy pipeline
/f --backend echo <goal>                       # wiring smoke (no LLM)
/f --feature <name> <goal>                     # override holdout feature key
/f --phase spec_validation <goal>              # workflow, but stop after Phase 1
```

## Action

1. Parse `$ARGUMENTS`. If `--legacy` or `--pipeline` is present, the user is
   being explicit — **skip the workflow** and fall through to the **Legacy
   dispatch** section below.
2. If neither flag is present, this is a **workflow invocation** — invoke
   the dark-factory Claude Workflow (Step 0: detect context first, then
   `Skill("dark-factory", args="…")`).

### Step 0: detect context (auto-routing — applies to BOTH paths)

The auto-detect (PR-mode vs feature-mode) is **identical** whether the user
gets the workflow or a legacy single-phase pipeline. Run once, before
either dispatch:

```bash
# Is there an open PR for the current branch?
gh pr list --head "$(git rev-parse --abbrev-ref HEAD)" \
  --json number,title,state,isDraft,additions,deletions,changedFiles,baseRefName,headRefName,labels

# What does the user want? (already in $ARGUMENTS)
echo "$ARGUMENTS"
```

**Reasoning prompts** (LLM, not rules):

- **No open PR** → feature-mode (new work).
- **Open PR exists** AND goal relates to it → PR-mode (drive the existing
  PR to green).
- **Open PR exists** AND goal is unrelated → **ask the user** which mode
  they meant. Do not silently route.

### Default path: invoke the dark-factory workflow

After Step 0 resolves PR-vs-feature, invoke the workflow:

```
Skill("dark-factory", args="--mode <pr|feature> --backend <echo|claude|...> --feature <name> --goal <…>")
```

The workflow owns phase ordering, shared CXDB, fix-loop bounding, and the
final summary. It dispatches each phase as a separate `dark-factory`
invocation against a phase-specific `.dot` graph (no engine changes
required — the `.dot` engine stays a single-phase runner).

If the user passed `--phase <name>`, forward it to the workflow as
`--phase-until <name>` so the workflow halts after that phase (useful for
iterating one phase in isolation without running the full 6-phase loop).

**Why this is the default:** the workflow is the **G3 closure artifact** —
runtime-determined fan-out (the original gap the user's prior `type="dynamic"`
custom code was meant to close) is handled by the workflow itself, not by
hand-rolled engine code. Default nodes (`codergen`, `tool`, `holdout_eval`,
`gate_*`) + a workflow orchestrator is the architectural pattern the user
pinned on 2026-06-22 (see
`~/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-06-22_user_pivot_default_nodes_over_custom.md`).

### Legacy dispatch (only when `--legacy` or `--pipeline` is present)

Read the PR context (PR-mode only) and pick from the pipeline menu in the
same way `/f` did before the workflow existed:

| Pipeline | When the LLM might pick it |
|----------|----------------------------|
| `pipelines/factory/hello.dot` | Wiring smoke only — verify mechanics first with `--backend echo` |
| `pipelines/factory/gates.dot` | Code is on the branch and the LLM wants holdout + the 3 evidence gates |
| `pipelines/factory/pr_gates.dot` | Code is on the branch and the LLM wants the 3 evidence gates; `--feature <name>` for the holdout to resolve |
| `pipelines/slim/minimal_pr.dot` | The LLM thinks the PR needs more implement/test work, not just gates |
| `pipelines/bug_fix.dot` | The PR is a TDD bug fix with red/green discipline |
| `pipelines/slim/spec_gen.dot` | The LLM has decided /fs is needed first — STOP and recommend running it before any /f pipeline |
| `pipelines/factory/level5_feature.dot` | Full Level-5 (dark-factory) reference pipeline with hard-tier gates wired in |
| **no /f pipeline** | Docs-only / test-only / config-only PRs have no behavioral surface for the holdout to grade. The LLM should say so and stop |

Then construct:

```bash
export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
export PATH="$HOME/.local/bin:$PATH"
cd <target repo>   # the repo the work lands in
dark-factory \
  --pipeline <CHOSEN> \
  --goal "<1-line LLM rationale + scope>" \
  --backend <echo for smoke | claude/codex/agy/minimax for real> \
  [--feature <holdout-feature-if-applicable>] \
  [--state <KEY>=<VALUE> if minimal_pr] \
  --cxdb ~/.dark-factory/cxdb.sqlite
```

### Report the verdict

**Workflow path:** the workflow emits its own final summary block (see
`.claude/workflows/dark-factory.md` § "Final summary"). Quote it verbatim.
If `Final outcome` is anything but `PASS`, run `df-healer --cxdb <CXDB>`.

**Legacy path:** quote the runner's final JSON line (`final_outcome`,
`pipeline`, `steps`) and the per-node trace. If `final_outcome !=
"success"`, run `df-healer --cxdb <path>` and surface the report.

## Honesty rules (LLM, do not skip)

- Quote the **actual** command or `Skill()` call you ran. No paraphrasing.
- If the LLM decided /fs is needed first, **say so and stop** — do not
  silently fall through to `gates.dot` and pretend the PR is green.
- If the LLM decided no /f pipeline fits (e.g. docs-only PR), **say so and
  stop** — do not silently fall through to a holdout-bearing pipeline.
- If `--backend echo` was used, label the run as a wiring smoke, not a
  real validation. The 3-gate verdicts from echo are not real LLM verdicts.
- The `gate_er` priority queue is `codex > minimax > agy > claude-sonnet`
  by default. If the LLM passes `--backend claude` for a real run, the
  `gate_er` reviewer is still resolved via the priority queue.
- Do not invent `--feature` values. If you cannot find a holdout directory
  at `~/projects/dark-factory-holdouts/holdouts/<feature>/`, do not pass
  `--feature`.
- When the goal is unrelated to the open PR, **ask the user** which mode
  they meant. Do not silently route to PR-mode for unrelated work.
- When the workflow exhausts its fix loop (3 attempts), surface the
  diagnosis verbatim and **stop** — do not auto-merge.

## Why the workflow is the default

The user's 2026-06-22 architectural pivot (memory:
`feedback_2026-06-22_user_pivot_default_nodes_over_custom`):

> "default nodes plus majority of this graph/pipeline managed by claude
> workflow instead of custom code"

Translation: prefer the framework's built-in primitive (Claude Workflow)
over hand-rolled extension (custom `type="dynamic"` Python handler). The
single workflow file at `.claude/workflows/dark-factory.md` is the G3
closure — it handles runtime-determined fan-out via re-dispatch across
phases, without engine-level changes.

Use **legacy** dispatch only when:
- The user explicitly opts in (`--legacy` / `--pipeline`)
- The user is iterating one phase in isolation
- The user wants a wiring smoke (`hello.dot` with `--backend echo`)

## See also

- `/f-pr` — explicit PR-mode entry point (legacy-only by default).
- `/factory` — alias for `/f` with identical behavior.
- `/fs` — spec-generation entry point; dispatches Phase 1 of the workflow
  when called without `--legacy`.
- `~/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-06-22_user_pivot_default_nodes_over_custom.md`
  — the architectural pivot that motivated this change.