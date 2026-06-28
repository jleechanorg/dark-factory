---
description: "/f — full Dark Factory loop; auto-routes to PR-mode when a PR is open on the current branch, otherwise feature-mode. DEFAULT: invoke the real dark-factory binary and echo proof metadata. Dynamic workflow/DOT generation is allowed only behind the binary."
type: quality
execution_mode: immediate
aliases: [f]
---

# /f — Full Dark Factory Loop (auto-routes PR vs feature)

Shortcut for `/factory` oriented toward **full production loops**. The
**default invocation path is the real `dark-factory` binary**. The binary may
run a selected static DOT graph, or it may run a binary-owned graph builder /
workflow that creates or selects a dynamic DOT graph. What is not allowed is an
in-Claude prose-only workflow that claims a factory run without a logged binary
invocation.

Every default run must preserve required default nodes or their graph-level
equivalents: plan/spec producer, independent review, bounded fix loop,
evidence gates, and exit summary. Dynamic graph generation is valid only when
the generated/selected DOT graph is saved or echoed in the run evidence.

**Prerequisite:** `./install.sh` once; `dark-factory` on PATH via `~/.local/bin`.

**Usage**:

```
/f <goal description>                          # DEFAULT: binary-first factory run + reviewer calibration
/f --pipeline gates <goal>                     # explicit binary pipeline
/f --backend echo <goal>                       # wiring smoke (no LLM)
/f --feature <name> <goal>                     # override holdout feature key
/f --reviewer-calibration=false <goal>         # explicit opt-out; must explain why
/f --dynamic-graph <goal>                      # binary-owned dynamic DOT/workflow builder
/f --phase spec_validation <goal>              # binary-owned dynamic/phase run, if supported
```

## Action

1. Parse `$ARGUMENTS`.
2. Run Step 0 to decide PR-mode vs feature-mode.
3. Construct and run a `dark-factory` binary command. If the LLM decides a
   dynamic graph is required, the command must still be a binary invocation
   and the generated/selected DOT graph must appear in the evidence envelope.

### Step 0: detect context (auto-routing — applies to BOTH paths)

The auto-detect (PR-mode vs feature-mode) is **identical** whether the user
uses a selected static graph or a binary-owned dynamic graph. Run once before
dispatch:

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

### Step 0.5: auto-detect CLI backend (NEW — added 2026-06-27)

When the user invokes Claude via a wrapper function in `~/.bashrc` (e.g.
`claudem`, `clauded`, `codex`, `agy`), bash exports a `BASH_FUNC_<wrapper>%%`
environment variable. `/f` reads these to pick the right `--backend` flag for
the dark-factory binary, so the LLM that runs the audit is the same one the
user picked from their shell.

Even WITHOUT a bash wrapper, the orchestrator's own env (`ANTHROPIC_BASE_URL`)
and parent process (`ps -o comm= -p $PPID`) reveal which CLI is running.
The detector below uses **multiple signals** with a priority order so the
detection works whether or not the user has a bash wrapper set up.

**Implementation note**: bash function-exported env var names contain `%%`,
which breaks both `${BASH_FUNC_X%%:-default}` parameter expansion (the `%%`
is interpreted as the "remove longest suffix" operator) and `${!key}` indirect
expansion (bash rejects `%%` in variable names). The detector below uses
`printenv` to read the literal env var name safely.

```bash
# Multi-signal detector: explicit > wrapper > orchestrator env > parent process > default
detect_cli_backend() {
  local key parent_comm ppid

  # 1. Bash wrapper env vars (exported when wrapper function is invoked)
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

  # 2. Orchestrator's ANTHROPIC_BASE_URL (strong signal of which API we're on)
  case "${ANTHROPIC_BASE_URL:-}" in
    *api.minimax.io*) echo "minimax"; return ;;
    *anthropic.com*)  echo "claude";  return ;;
    *openai.com*)     echo "codex";   return ;;
  esac

  # 3. Parent process name (ps -o comm= -p $PPID)
  ppid="${PPID:-}"
  if [ -n "$ppid" ]; then
    parent_comm="$(ps -o comm= -p "$ppid" 2>/dev/null | tr -d ' ')"
    case "$parent_comm" in
      *claude*|*claudem*) echo "claude"; return ;;
      *codex*)            echo "codex";  return ;;
      *agy*|*antigrav*)   echo "agy";    return ;;
    esac
  fi

  # 4. Hardcoded default
  echo "claude"
}

DETECTED_BACKEND="$(detect_cli_backend)"
```

| Bash wrapper | `ANTHROPIC_BASE_URL` | `--backend` flag | Notes |
|---|---|---|---|
| `claudem` | `https://api.minimax.io/anthropic` | `minimax` | MiniMax-M3 / MiniMax-M2.5 |
| `clauded` | (default Anthropic) | `claude` | Anthropic Sonnet / Opus via user's subscription |
| `agy` | (Antigravity) | `agy` | Antigravity backend |
| `codex` | (OpenAI) | `codex` | OpenAI Codex |
| `claude` (default) | (default Anthropic) | `claude` | Anthropic direct |

**Override precedence** (highest to lowest):
1. Explicit `--backend <x>` flag in `$ARGUMENTS` wins.
2. `BASH_FUNC_*` env var (explicit wrapper signal).
3. `ANTHROPIC_BASE_URL` (orchestrator's actual API endpoint).
4. Parent process name (`ps -o comm= -p $PPID`).
5. Hardcoded default (`claude`).

**Why multiple signals?** A user may run `claude` directly (no wrapper), or
via `clauded`, or via `claudem`. The `BASH_FUNC_*` signal is only present for
wrapped invocation. The `ANTHROPIC_BASE_URL` signal works regardless of how
the orchestrator was launched (it's set by the orchestrator's runtime, not
by bash). The PPID signal is a last-resort fallback for unusual setups.

**Honesty rule**: Always echo the detected backend + source in the proof
block, e.g.:
```
# CLI backend: minimax (source: ANTHROPIC_BASE_URL=api.minimax.io); --backend flag: minimax
```

If the user passed `--backend X` explicitly, that wins and the proof block
should say `# CLI backend: <detected>, but --backend flag override: <X>`.

**Verified test matrix** (11/11 PASS):
- `BASH_FUNC_claudem%%` → minimax
- `BASH_FUNC_clauded%%` → claude
- `BASH_FUNC_agy%%` → agy
- `BASH_FUNC_codex%%` → codex
- `BASH_FUNC_claude%%` → claude
- `ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic` → minimax
- `ANTHROPIC_BASE_URL=https://api.anthropic.com` → claude
- Wrapper + URL conflict → wrapper wins (explicit signal beats heuristic)
- No env at all → defaults to `claude`
- `ANTHROPIC_BASE_URL=http://localhost:9000` (no pattern match) → defaults to `claude`

### Default path: invoke the dark-factory binary

After Step 0 resolves PR-vs-feature, select the appropriate pipeline or
binary-owned dynamic graph mode. The default static choices are:

| Pipeline | When the LLM might pick it |
|----------|----------------------------|
| `pipelines/factory/hello.dot` | Wiring smoke only — verify mechanics first with `--backend echo` |
| `pipelines/factory/gates.dot` | Code is on the branch and the LLM wants holdout + the 3 evidence gates |
| `pipelines/factory/pr_gates.dot` | Code is on the branch and the LLM wants the 3 evidence gates without sealed holdout |
| `pipelines/slim/minimal_pr.dot` | The LLM thinks the PR needs more implement/test work, not just gates |
| `pipelines/bug_fix.dot` | The PR is a TDD bug fix with red/green discipline |
| `pipelines/slim/spec_gen.dot` | The LLM has decided /fs is needed first — STOP and recommend running it before any /f pipeline |
| `pipelines/factory/level5_feature.dot` | Full Level-5 reference pipeline with hard-tier gates wired in |
| **dynamic DOT via binary** | A static graph cannot express the needed phase/fanout, but the binary will save/echo the generated graph |
| **no /f pipeline** | Docs-only / test-only / config-only PRs have no behavioral surface for the holdout to grade. The LLM should say so and stop |

Then construct:

```bash
export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
export PATH="$HOME/.local/bin:$PATH"
cd <target repo>   # the repo the work lands in
# Backend: prefer explicit --backend flag, else Step 0.5 detection, else 'claude'
BACKEND="<DETECTED_BACKEND or --backend override>"
dark-factory \
  --pipeline <CHOSEN_OR_GENERATED_DOT> \
  --goal "<1-line LLM rationale + scope>" \
  --backend "$BACKEND" \
  [--feature <holdout-feature-if-applicable>] \
  [--state <KEY>=<VALUE> if needed] \
  --cxdb ~/.dark-factory/cxdb.sqlite
```

If the user passed `--phase <name>`, pass it through only if the binary
supports that phase/dynamic mode. Otherwise pick the closest explicit DOT
pipeline and state the limitation.

### Reviewer calibration (default on)

`--reviewer-calibration=true` is the default for every real `/f` run. Treat
the flag as present unless the user explicitly passes
`--reviewer-calibration=false`.

Calibration means the final `/f` evidence must compare at least:

- the factory/in-graph reviewer outcome when the selected DOT has one;
- a raw terminal mirror review via `codex exec --yolo -m gpt-5.3-codex-spark`;
- a delegated reviewer/subagent outcome when available in the current session.

All reviewers must receive the same frozen envelope: target repo, target PR or
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

### Report the verdict

Quote the runner's final JSON line (`final_outcome`, `pipeline`, `steps`) and
the per-node trace. If `final_outcome != "success"`, run
`df-healer --cxdb <path>` and surface the report.

End every `/f` response with this proof block. Missing any required line means
the factory run is unproven and must be reported as such:

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

## Honesty rules (LLM, do not skip)

- Quote the **actual** `dark-factory` command you ran. No paraphrasing.
- Do not claim a factory run based on an in-Claude workflow, `Skill()` call,
  or prose summary. The only valid proof is an actual `dark-factory` binary
  invocation plus the required proof block from that run.
- If the LLM decided /fs is needed first, **say so and stop** — do not
  silently fall through to `gates.dot` and pretend the PR is green.
- If the LLM decided no /f pipeline fits (e.g. docs-only PR), **say so and
  stop** — do not silently fall through to a holdout-bearing pipeline.
- If `--backend echo` was used, label the run as a wiring smoke, not a
  real validation. The 3-gate verdicts from echo are not real LLM verdicts.
- Reviewer calibration is default-on. If `--reviewer-calibration=false` is
  passed, quote the explicit opt-out reason and do not imply raw-Codex-vs-
  subagent quality was measured.
- The `gate_er` priority queue is `codex > minimax > agy > claude-sonnet`
  by default. If the LLM passes `--backend claude` for a real run, the
  `gate_er` reviewer is still resolved via the priority queue.
- Do not invent `--feature` values. If you cannot find a holdout directory
  at `~/projects/dark-factory-holdouts/holdouts/<feature>/`, do not pass
  `--feature`.
- When the goal is unrelated to the open PR, **ask the user** which mode
  they meant. Do not silently route to PR-mode for unrelated work.
- When the binary-owned workflow/dynamic graph exhausts its fix loop (3 attempts), surface the
  diagnosis verbatim and **stop** — do not auto-merge.

## Why binary-first is the default

The user's 2026-06-27 clarification:

> "default to the real binary but that binary can use a claude workflow to
> build dynamic dot graph but always have default nodes"

Translation: the durable artifact is still the DOT graph and the runner log.
Claude/workflow logic can help create or select the graph, but the operator
must see a real `dark-factory` invocation and run metadata. The default nodes
remain `codergen`, `tool`, `holdout_eval`, `gate_*`, reviewer/shadow-review
nodes, fix loops, and `exit`.

Use static DOT dispatch when it fits. Use dynamic graph generation only when
the binary saves/echoes the generated graph and the evidence envelope points
to it.

## See also

- `/f-pr` — explicit PR-mode entry point.
- `/factory` — alias for `/f` with identical behavior.
- `/fs` — spec-generation entry point; default binary run of
  `pipelines/slim/spec_gen.dot` or a binary-owned dynamic spec graph.
- `~/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-06-22_user_pivot_default_nodes_over_custom.md`
  — the architectural pivot that motivated this change.
