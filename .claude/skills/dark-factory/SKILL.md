---
name: dark-factory
description: "Run the Dark Factory DOT pipeline runner against a goal. Slash command: /factory. Implements StrongDM's Attractor pattern as an external Python runner — .dot files are the versioned artifact, sealed holdouts live in a separate repo, every step is recorded to CXDB, and the Healer clusters failures into diagnoses. Use when you want the goal_harness idea executed as a reproducible external pipeline instead of in-Claude subagent dispatch."
---

# /factory — Dark Factory Pipeline Runner

## Purpose

Run a goal through the Dark Factory `.dot` pipeline runner at
`~/projects/dark-factory/`. The runner is the Python implementation of
StrongDM's Attractor pattern (cf. brynary/attractor, strongdm/attractor).

Versus `/h` / `/goal_harness`:

| Aspect | `/h` (goal_harness) | `/factory` (this skill) |
|--------|---------------------|-------------------------|
| Engine | Claude session dispatches subagents | **`dark-factory` binary** (uv install) |
| Artifact | Skill prose + tool calls | `.dot` graphviz file |
| Recording | Conversation transcript | SQLite CXDB row-per-step |
| Holdouts | Adversarial subagent reads diff | Sealed repo agent never sees |
| Reuse | Tied to this session | Replayable from `.dot` alone |
| Cost | High (multi-agent context) | Low (one CLI per node) |

Use `/factory` when you want the goal_harness idea as a reproducible external
pipeline; use `/h` when you want an interactive in-session loop.

## Entry points

| Command | Behavior |
|---------|----------|
| `/f` | Full loop, auto-routes PR-mode vs feature-mode (Step 0a below) |
| `/factory` | Alias for `/f`, identical behavior |
| `/f-pr` | Explicit PR-mode entry point (skips auto-route, always PR-mode) |
| `/fs` | Spec-generation entry point; runs `pipelines/slim/spec_gen.dot` or a binary-owned dynamic spec graph — run this first when the goal needs a spec |

This skill file is the single source of truth for the binary-first contract;
`.claude/commands/f.md` is the canonical command entry point, with `/factory`
and `/f-pr` as thin aliases so auto-detect behavior stays identical across
entry points.

**What "binary-first" bans**: an in-Claude prose-only workflow that claims a
factory run without a logged binary invocation is not a valid run — see
**Honesty rules** below. Every default run must preserve required default
nodes or their graph-level equivalents: plan/spec producer, independent
review, bounded fix loop, evidence gates, and exit summary. Dynamic graph
generation is valid only when the generated/selected DOT graph is saved or
echoed in the run evidence.

## Repos

- `~/projects/dark-factory/` — the runner (factory code; agent can see this)
- `~/projects/dark-factory-holdouts/` — sealed scenarios (agent must NEVER read)
- Pushed: `https://github.com/jleechanorg/dark-factory` + `dark-factory-holdouts`

## Available pipelines

| `.dot` file | Flow | When to use |
|-------------|------|-------------|
| `pipelines/factory/hello.dot` | plan → implement → holdout → fix-loop → exit | Add a new feature with a holdout scenario |
| `pipelines/factory/gates.dot` | start → holdout → /es → /er → /code_standards → exit | Validate an already-implemented diff (Attractor-style 4-gate harness as `.dot`) |
| `pipelines/factory/pr_gates.dot` | start → holdout → /es → /er → /code_standards → exit | Validate an already-implemented in-flight PR diff (Holdout-always policy; requires `--feature <name>` for the holdout) |
| `pipelines/slim/minimal_feature.dot` | explore → plan → implement → test → review → holdout → gates → exit | Full production pipeline from scratch with a holdout |
| `pipelines/slim/minimal_pr.dot` | explore → plan → implement → test → review → holdout → gates → exit | Slim in-flight PR iteration loop with parameterized tests (Holdout-always policy; requires `--feature <name>` for the holdout) |
| `pipelines/slim/spec_gen.dot` | spec-only generation | The LLM decided `/fs` is needed first — STOP and recommend running it before any other pipeline |
| `pipelines/bug_fix.dot` | red → green → refactor | TDD bug fix with red/green discipline |
| `pipelines/factory/level5_feature.dot` | full reference pipeline | Full Level-5 reference pipeline with hard-tier gates wired in |
| **dynamic DOT via binary** | binary-owned graph builder | A static graph can't express the needed phase/fanout; the binary saves/echoes the generated graph in run evidence |
| **no pipeline** | — | Docs-only / test-only / config-only PRs have no behavioral surface for the holdout to grade — say so and stop |

You can also write your own `.dot` and pass it via `--pipeline`.

## How to invoke this skill

The user types `/f $ARGUMENTS` (or `/factory $ARGUMENTS`). Parse the
arguments and run the steps below. **Do not default to a single pipeline** —
select the graph that matches the task (see **Pipeline selection** below)
unless the user passed `--pipeline`.

### Step 0a — Auto-route PR-mode vs feature-mode (applies to `/f` and `/factory`; `/f-pr` skips this and forces PR-mode)

Run once before dispatch:

```bash
gh pr list --head "$(git rev-parse --abbrev-ref HEAD)" \
  --json number,title,state,isDraft,additions,deletions,changedFiles,baseRefName,headRefName,labels
echo "$ARGUMENTS"
```

Reasoning (LLM, not rules):
- **No open PR** → feature-mode (new work).
- **Open PR exists** AND goal relates to it → PR-mode (drive the existing PR to green).
- **Open PR exists** AND goal is unrelated → **ask the user** which mode they meant. Do not silently route.

### `/f-pr` — explicit PR-mode entry point

`/f-pr` skips Step 0a and always runs PR-mode. Use the LLM to **read the
PR's actual context and pick the right pipeline** — this is a reasoning
task, not a deterministic rule table (no `if is_draft then X`, no
`if labels contains 'bug' then bug_fix.dot`, no keyword-routing).

1. **Resolve the PR number**: from `$ARGUMENTS` if present, else
   `gh pr list --head "$(git rev-parse --abbrev-ref HEAD)" --json number --jq '.[0].number'`,
   else ask the user.
2. **Read PR context as facts, not routing rules**:
   ```bash
   gh pr view <N> --json number,title,state,isDraft,additions,deletions,changedFiles,baseRefName,headRefName,body,files,labels
   gh pr view <N> --json statusCheckRollup
   gh pr diff <N> --name-only
   gh pr view <N> --comments   # optional
   ls specs/ roadmap*/ docs/design/ 2>/dev/null   # optional
   ```
3. **Reason about**: what kind of work this is (new feature / bug fix /
   refactor / docs / test-only / infra — from diff+body+files, never
   pre-bucketed by label alone); whether a spec already covers the
   change (if so, `/fs` is skippable — deciding `/fs` is needed first
   and stopping is a valid terminal state, do not force a run); holdout
   eligibility (pass `--feature <name>` only if
   `~/projects/dark-factory-holdouts/holdouts/<feature>/` actually
   exists — never invent one); and what evidence mix (`/es` + `/er` +
   `/code_standards` minimum, `holdout_eval` for behavior-grade) the
   pipeline needs to deliver without over-running.
4. Pick the pipeline from **Available pipelines** above using that
   reasoning, pick the backend (`echo` for wiring smoke; `claude` unless
   the PR's reviewer queue or `gate_er` priority queue says otherwise),
   then construct, show, and run the command — same shape as Step 0c
   below but `cd` into the PR's target repo, not `dark-factory`.
5. Report the verdict per **Output contract** below.

`/f-pr` honesty rules (in addition to the shared ones under **Honesty
rules**): the `gate_er` priority queue (`codex > minimax > agy >
claude-sonnet`) still resolves the reviewer even when `--backend claude`
was passed for the run itself — passing `--backend claude` does not mean
`gate_er` uses claude by default.

### Step 0b — Auto-detect CLI backend

When the user invokes Claude via a `~/.bashrc` wrapper (`claudem`, `clauded`,
`codex`, `agy`), bash exports a `BASH_FUNC_<wrapper>%%` env var. Read these to
pick the right `--backend` flag so the LLM that runs the audit is the same
one the user picked from their shell. `%%` in the var name breaks
`${BASH_FUNC_X%%:-default}` and `${!key}` expansion, so use `printenv`:

```bash
detect_cli_backend() {
  local key parent_comm ppid
  for key in 'BASH_FUNC_claudem%%' 'BASH_FUNC_clauded%%' 'BASH_FUNC_agy%%' 'BASH_FUNC_codex%%' 'BASH_FUNC_claude%%'; do
    if [ -n "$(printenv "$key" 2>/dev/null)" ]; then
      case "$key" in
        *claudem%%) echo "minimax"; return ;;
        *clauded%%) echo "claude";  return ;;
        *agy%%)     echo "agy";     return ;;
        *codex%%)   echo "codex";   return ;;
        *claude%%)  echo "claude";  return ;;
      esac
    fi
  done
  case "${ANTHROPIC_BASE_URL:-}" in
    *api.minimax.io*) echo "minimax"; return ;;
    *anthropic.com*)  echo "claude";  return ;;
    *openai.com*)     echo "codex";   return ;;
  esac
  ppid="${PPID:-}"
  if [ -n "$ppid" ]; then
    parent_comm="$(ps -o comm= -p "$ppid" 2>/dev/null | tr -d ' ')"
    case "$parent_comm" in
      *claude*|*claudem*) echo "claude"; return ;;
      *codex*)            echo "codex";  return ;;
      *agy*|*antigrav*)   echo "agy";    return ;;
    esac
  fi
  echo "claude"
}
DETECTED_BACKEND="$(detect_cli_backend)"
```

Override precedence (highest to lowest): explicit `--backend <x>` flag >
`BASH_FUNC_*` wrapper signal > `ANTHROPIC_BASE_URL` > parent process name >
hardcoded default (`claude`). Always echo the detected backend + source in
the proof block (see **Output contract**). If `--backend` was passed
explicitly, note the override rather than the raw detection.

### Step 0c — Select pipeline (mandatory when `--pipeline` omitted)

1. Read the goal and apply **factory-spec Step 0** (greenfield vs brownfield).
2. Choose a pipeline from
   [docs/pipeline-selection.md](../../../docs/pipeline-selection.md) (or the table in
   **Available pipelines** below).
3. **Tell the user** which pipeline you chose and why before running.
4. If brownfield replace/delete: encode delete-first rules in the goal; do not
   use a greenfield additive pipeline by default.

If `$ARGUMENTS` already contains `--pipeline`, skip auto-selection.

### Short-name expansion

When `--pipeline` is a short name (no `/` or `.dot`), expand under
`$DARK_FACTORY_HOME`:

| Short name | Path |
|------------|------|
| `gates` | `pipelines/factory/gates.dot` |
| `hello` | `pipelines/factory/hello.dot` |
| `pr_gates` | `pipelines/factory/pr_gates.dot` |
| `minimal_feature` | `pipelines/slim/minimal_feature.dot` |
| `minimal_pr` | `pipelines/slim/minimal_pr.dot` |
| `review_slim` | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` |
| `review_full` | `benchmarks/attractor-spec-review/pipelines/review_full.dot` |
| `spec_gen` | `pipelines/slim/spec_gen.dot` |
| `bug_fix` | `pipelines/bug_fix.dot` |
| `level5_feature` | `pipelines/factory/level5_feature.dot` |

### Arg parsing

Honor these flags inside `$ARGUMENTS`:

- `--pipeline <name>` — short name (`gates`, `hello`, `pr_gates`, `minimal_pr`,
  `minimal_feature`, `review_slim`, `review_full`) or path to a `.dot`. If
  omitted, **auto-select from the goal** (Step 0c above) — never blindly default
  to `gates.dot` or `minimal_feature.dot`.
- `--feature <name>` — holdout feature (required when the pipeline includes a
  `holdout_eval` node; default `hello`)
- `--backend echo|claude|codex|ao|agy` — default `claude`. Use `echo` for cost-free
  dry-runs that verify wiring without LLM calls.
- `--max-steps <N>` — default 100
- `--cxdb <path>` — default `~/.dark-factory/cxdb.sqlite` (shared across runs
  so the Healer can cluster across history)
- `--state KEY=VALUE` — pre-seed context variables; repeatable. Extremely useful for setting parameterized test commands (e.g. `--state slim.test_command="pytest tests/..."`) during PR iteration loops.

Whatever is left after flag parsing is the **goal description**.

### Steps

1. **Verify binary install**. The factory runs via the **`dark-factory` binary**
   (not `python -m runner` from source). Check:
   ```bash
   export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
   export PATH="$HOME/.local/bin:$PATH"
   command -v dark-factory && dark-factory --help 2>/dev/null || true
   test -x "$DARK_FACTORY_HOME/bin/dark-factory" || {
     echo "ERROR: run $DARK_FACTORY_HOME/install.sh first"
     exit 1
   }
   ```
   If missing, tell the user to clone
   `https://github.com/jleechanorg/dark-factory` and run `./install.sh`, then stop.

2. **Environment**:
   ```bash
   export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
   export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
   export PATH="$HOME/.local/bin:$PATH"
   ```

3. **Run the pipeline** from the **target repo** (implementation workdir = cwd).
   Pipelines and prompts resolve from `$DARK_FACTORY_HOME`; code changes land in cwd:
   ```bash
   cd "<TARGET_REPO>"   # e.g. the product repo being built
   dark-factory \
     --pipeline pipelines/<PATH_TO_DOT>.dot \
     --goal "<GOAL>" \
     --backend <BACKEND> \
     --feature <FEATURE> \
     --cxdb <CXDB_PATH> \
     --state <KEY>=<VALUE>
   ```

4. **Surface the verdict**. The last line of stdout is a JSON summary with
   `final_outcome`, `pipeline`, `goal`, `steps`, `trace`. Report:
   - Pipeline name + final outcome
   - Per-node trace (one line each: `node  outcome  preview`)
   - Exit code of the runner

5. **Run the Healer** (only if `final_outcome != "success"`):
   ```bash
   df-healer --cxdb <CXDB_PATH>
   ```
   Render the resulting markdown table inline. Each row is a failure cluster
   with a prescription template the user can act on.

6. **Sealed holdouts reminder**. If the run included a `holdout_eval` node,
   remind the user that you (and any subagent) did NOT see the scenarios at
   `~/projects/dark-factory-holdouts/holdouts/<feature>/` — only the
   PASS/FAIL verdict per scenario name leaked back.

## Reviewer calibration (default on)

`--reviewer-calibration=true` is the default for every real `/f`/`/factory`
run. Treat the flag as present unless the user explicitly passes
`--reviewer-calibration=false`.

Calibration compares at least:
- the factory/in-graph reviewer outcome when the selected DOT has one;
- a raw terminal mirror review via `codex exec --yolo -m gpt-5.3-codex-spark`;
- a delegated reviewer/subagent outcome when available in the current session.

All reviewers receive the same frozen envelope: target repo, target PR or
work item, head SHA, base SHA, diff, PR/task text, evidence paths, test logs,
factory run ID, and the exact shared review prompt. Store outputs under:

```text
evidence/<run-id>/reviewer-calibration/
  envelope.json
  prompt.txt
  raw-codex.output.md
  raw-codex.findings.json
  subagent.output.md
  subagent.findings.json
  factory-reviewer.output.md
  factory-reviewer.findings.json
  comparison.json
  adjudication.md
```

Do not claim delegated subagents underperformed raw Codex unless the same
envelope and prompt were reviewed at the same SHA and raw Codex found a
later-confirmed blocker the delegated reviewer missed. If calibration is
disabled, the final response must say `Reviewer calibration: disabled` and
give the explicit reason.

The `gate_er` priority queue is `codex > minimax > agy > claude-sonnet` by
default; passing `--backend claude` for the run itself does not change how
`gate_er` resolves its reviewer.

## Output contract

End every `/f`/`/factory` invocation with this proof block. Missing any
required line means the run is unproven and must be reported as such:

```bash
# CLI backend: <detected-or-override> (source: <BASH_FUNC_X%%|explicit --backend|default>)
# Literal command run:
cd /Users/jleechan/projects/<target-repo>
DARK_FACTORY_HOME=~/projects/dark-factory \
DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts \
PATH="$HOME/.local/bin:$PATH" \
dark-factory \
  --pipeline <chosen-or-generated-dot> \
  --goal "<echo of $ARGUMENTS>" \
  --backend <backend> \
  --feature <feature-if-any> \
  --cxdb ~/.dark-factory/cxdb.sqlite
# Run ID: <id>
# CXDB SHA: <sha>
# Final outcome: <success|failure|exhausted|error>
# Exit code: <integer>
# Wall-clock: <duration>
# Logs: <path>
# Evidence envelope: <path>
# Reviewer calibration: <enabled|disabled> <artifact-path-or-reason>
```

Plus the per-node trace and, on failure, the Healer report.

## Adding a new feature

To add a new sealed-holdout feature `foo`:

1. Write `~/projects/dark-factory/specs/foo.md` (agent-visible).
2. Write `~/projects/dark-factory/prompts/foo/{plan,implement,fix}.md`.
3. Write `~/projects/dark-factory-holdouts/holdouts/foo/scenarios.yaml`
   in the sealed repo (the implementing agent must never read this).
4. Either reuse `pipelines/factory/hello.dot` (which is feature-agnostic) by
   passing `--feature foo`, or write `pipelines/factory/foo.dot` with custom
   nodes.

## Honesty rules

- Always tell the user the **actual command** you ran (`dark-factory ...` or `df-healer ...`). No paraphrasing.
- Always quote the **runner's exit code** verbatim.
- Never claim a holdout passed if you read the scenarios — that breaks the
  adversarial guarantee. If you read `~/projects/dark-factory-holdouts/`,
  declare the run **invalid** and rerun the pipeline.
- If `--backend echo` was used, label the run as a wiring smoke, not a real
  validation.
- Reviewer calibration is default-on. If `--reviewer-calibration=false` is
  passed, quote the explicit opt-out reason and do not imply raw-Codex-vs-
  subagent quality was measured.
- Do not claim a factory run based on an in-Claude workflow, `Skill()` call,
  or prose summary. The only valid proof is an actual `dark-factory` binary
  invocation plus the proof block above.
- If the LLM decided `/fs` is needed first, **say so and stop** — do not
  silently fall through to `gates.dot` and pretend the PR is green.
- If no pipeline fits (e.g. docs-only PR), **say so and stop** — do not
  silently fall through to a holdout-bearing pipeline.
- Do not invent `--feature` values. If there's no holdout directory at
  `~/projects/dark-factory-holdouts/holdouts/<feature>/`, don't pass `--feature`.
- When the goal is unrelated to the open PR (Step 0a), **ask the user** which
  mode they meant. Do not silently route to PR-mode for unrelated work.
- When the fix loop exhausts (3 attempts), surface the diagnosis verbatim and
  **stop** — do not auto-merge.

## Known limits

- The `claude` backend shells out to `claude --print
  --dangerously-skip-permissions` — each node is a fresh non-interactive
  session. Long agentic loops should use `/h` instead.
- `_parse_verdict` falls back to scanning the tail for a standalone PASS/FAIL
  token; if the gate command emits a debug line with just "PASS" the runner
  will trust it. For high-stakes gates require explicit `VERDICT:` markers.
- CXDB is per-process — don't share one across threads.
