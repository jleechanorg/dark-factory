---
description: "/f — full Dark Factory loop; auto-routes to PR-mode when a PR is open on the current branch, otherwise feature-mode"
type: quality
execution_mode: immediate
aliases: [f]
---

# /f — Full Dark Factory Loop (auto-routes PR vs feature)

Shortcut for `/factory` oriented toward **full production loops**, but the
pipeline is **chosen for the task** — not hardcoded to `minimal_feature.dot`.

Uses the installed **`dark-factory` binary** (`./install.sh` from this repo).

**Prerequisite:** `./install.sh` once; `dark-factory` on PATH via `~/.local/bin`.

**Usage**:

```
/f <goal description>               # auto-detect: PR-mode or feature-mode
/f --backend echo <goal>            # dry-run wiring smoke (no LLM)
/f --feature <name> <goal>          # override holdout feature key
/f --pipeline <name> <goal>         # explicit pipeline (skips auto-detect)
```

## Action

1. Parse `$ARGUMENTS`. If `--pipeline` is present, the user is being
   explicit — pass through to the dark-factory skill with that pipeline.
   No detection needed.
2. If `--pipeline` is **missing**, do **Step 0: detect context** below,
   then either run **PR-mode** (an open PR exists for the current branch)
   or **feature-mode** (no open PR). Both modes are described in this file
   so the LLM has the full menu in one place.

### Step 0: detect context (auto-routing)

This is what the user invokes when they don't remember to use `/f-pr`.
The LLM is given the facts and reasons; there are no hardcoded if/else
rules. Two facts to gather:

```bash
# Is there an open PR for the current branch?
gh pr list --head "$(git rev-parse --abbrev-ref HEAD)" \
  --json number,title,state,isDraft,additions,deletions,changedFiles,baseRefName,headRefName,labels

# What does the user want? (already in $ARGUMENTS)
echo "$ARGUMENTS"
```

**Reasoning prompts** (LLM, not rules):

- If **no open PR** → the user wants new work. Proceed to **feature-mode**.
- If **an open PR exists** AND the goal `$ARGUMENTS` clearly relates to
  the PR's work (e.g. the goal mentions the PR, the PR's title, the
  files, or describes fixing / improving / validating that diff) → the
  user wants PR-mode. Proceed to **PR-mode**.
- If **an open PR exists** AND the goal is **unrelated** to the PR's
  work (e.g. a brand-new feature description that has nothing to do with
  the open diff) → the LLM should **ask the user** which mode they
  meant, and surface both options. Do not silently route.

### PR-mode (open PR detected for the current branch)

Read the PR context:

```bash
gh pr view <N> --json number,title,state,isDraft,additions,deletions,changedFiles,baseRefName,headRefName,body,files,labels,statusCheckRollup
gh pr diff <N> --name-only
ls specs/ 2>/dev/null
ls road*/  docs/design/ 2>/dev/null
```

Let the LLM reason about:

- **What kind of work is this?** New feature / bug fix / refactor /
  docs / test-only / infra. The diff stat + body + file list inform
  this. Do NOT pre-bucket by label.
- **Is the spec already present?** If `specs/<feature>.md` or a
  `roadmap/*.md` link in the PR body covers the change, /fs is
  skippable. Otherwise the LLM may decide spec_gen is a prerequisite
  and stop here with a recommendation (this is a valid terminal state).
- **Holdout eligibility.** If the PR's diff touches a feature that has
  a sealed holdout at `~/projects/dark-factory-holdouts/holdouts/<feature>/`,
  pass `--feature <name>`. Do not invent `--feature` if no such
  directory exists for this feature.
- **What evidence do we need?** /es + /er + /code_standards are the
  minimum; holdout_eval is the behavior-grade. Pick the pipeline that
  delivers the right evidence mix without over-running.

Pipeline menu (the LLM picks based on the reasoning above; not a
deterministic rule table):

| Pipeline | When the LLM might pick it |
|----------|----------------------------|
| `pipelines/factory/hello.dot` | Wiring smoke only — LLM is unsure of the graph; verify mechanics first with `--backend echo` |
| `pipelines/factory/gates.dot` | Code is on the branch and the LLM wants holdout + the 3 evidence gates |
| `pipelines/factory/pr_gates.dot` | Code is on the branch and the LLM wants the 3 evidence gates; the holdout is also run per the "Holdout-always policy" (the SKILL.md files at `.claude/skills/dark-factory/SKILL.md:40` and `.claude/skills/factory-spec/SKILL.md:280` say "bypasses holdouts" but the actual `.dot` does not — the LLM should trust the file). Requires `--feature <name>` for the holdout to resolve. |
| `pipelines/slim/minimal_pr.dot` | The LLM thinks the PR needs more implement/test work, not just gates; pass `--state slim.test_command=...` to parameterize the test command. The SKILL.md also says "bypasses holdouts" but the .dot does include holdout — the LLM should trust the file and pass `--feature` if a sealed holdout exists. |
| `pipelines/bug_fix.dot` | The PR is a TDD bug fix with red/green discipline |
| `pipelines/slim/spec_gen.dot` | The LLM has decided /fs is needed first — STOP and recommend running it before any /f pipeline; this is a valid terminal state |
| **no /f pipeline** | Docs-only / test-only / config-only PRs have no behavioral surface for the holdout to grade. The LLM should say so and stop; the standard PR-review path (CI + Skeptic + CodeRabbit + Bugbot) is the actual validation. This is a valid terminal state. |

### feature-mode (no open PR detected)

Invoke **factory-spec Step 0** (greenfield vs brownfield) and pick a
pipeline from [docs/pipeline-selection.md](../../docs/pipeline-selection.md):

- New feature, full loop → `pipelines/slim/minimal_feature.dot`
- PR iteration → `pipelines/slim/minimal_pr.dot` (only when the user
  has indicated they want to iterate an as-yet-unpushed branch)
- Replace/delete (brownfield) → brownfield goal + appropriate graph;
  apply the factory-spec delete-first rules
- Wiring only → `pipelines/factory/hello.dot`
- Gates on existing diff → `pipelines/factory/gates.dot` or `pr_gates.dot`

The `/f` skill's brownfield rules (delete-first, executor node for
deletion, net-LOC ≤ 0 guard, dead-code gate, same-call-site replace,
post-deletion proof) are still mandatory for replace/delete work and
must be encoded in the `--goal` string. The factory-spec skill
documents the rules in full.

### Construct the command and run

After picking a pipeline (in either mode), **state the chosen pipeline
and rationale** to the user, then construct:

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

Quote the runner's final JSON line (`final_outcome`, `pipeline`,
`steps`) and the per-node trace. If `final_outcome != "success"`, run
`df-healer --cxdb <path>` and surface the report.

## Honesty rules (LLM, do not skip)

- Quote the **actual** `dark-factory` command you ran. No paraphrasing.
- If the LLM decided /fs is needed first, **say so and stop** — do not
  silently fall through to `gates.dot` and pretend the PR is green.
- If the LLM decided no /f pipeline fits (e.g. docs-only PR), **say so
  and stop** — do not silently fall through to a holdout-bearing
  pipeline that will fail at the holdout node.
- If `--backend echo` was used, label the run as a wiring smoke, not a
  real validation. The 3-gate verdicts from echo are not real LLM
  verdicts.
- The `gate_er` priority queue is `codex > minimax > agy > claude-sonnet`
  by default. If the LLM passes `--backend claude` for a real run, the
  `gate_er` reviewer is still resolved via the priority queue (not
  clauded by default unless the queue falls all the way through).
- Do not invent `--feature` values. If you cannot find a holdout
  directory at `~/projects/dark-factory-holdouts/holdouts/<feature>/`,
  do not pass `--feature`.
- When the goal is unrelated to the open PR, **ask the user** which
  mode they meant. Do not silently route to PR-mode for unrelated work.

## Why this is ZFC

The decision (PR-mode vs feature-mode, pipeline choice, backend choice,
--feature) is the LLM's based on the **context** (open PR? goal related
to PR?) and the **menu**. There is no `if is_draft then X` branch, no
`if labels contains 'bug' then bug_fix.dot` rule, no keyword-routing.
The model reads the context and reasons about it. If the LLM gets it
wrong, the user corrects it and the LLM learns — there is no code to
maintain.

The detection step itself (`gh pr list --head <branch>`) is a fact-
gathering call, not a routing decision. The LLM is given the fact and
makes the call.

## See also

- `/f-pr` — the **explicit** PR-mode entry point. Use this when you
  want to skip the auto-detect step and force PR-mode (e.g. to drive
  a specific PR by number that isn't the current branch's PR). `/f`
  calls the same routing logic automatically.
- `/factory` — alias for `/f` with the same auto-detect behavior.
- `/fs` — the **spec generation** entry point. `/f` may decide `/fs` is
  needed first and stop with a recommendation; the user can then run
  `/fs` explicitly.
