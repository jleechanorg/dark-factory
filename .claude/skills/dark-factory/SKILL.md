---
name: dark-factory
description: "Run the Dark Factory DOT pipeline runner against a goal. Slash command: /f (aliases /factory, /df; /f-pr forces PR-mode). Implements StrongDM's Attractor pattern as an external Python runner — .dot files are the versioned artifact, sealed holdouts live in a separate repo, every step is recorded to CXDB, and the Healer clusters failures into diagnoses. Use when you want the goal_harness idea executed as a reproducible external pipeline instead of in-Claude subagent dispatch."
---

# /f — Dark Factory Pipeline Runner

## Purpose

Run a goal through the Dark Factory `.dot` pipeline runner, the Python
implementation of StrongDM's Attractor pattern (cf. brynary/attractor,
strongdm/attractor). Use `/factory` for a reproducible external pipeline and
`/h` for an interactive in-session loop
([goal-harness-comparison.md](goal-harness-comparison.md)).

## Entry points

| Command | Behavior |
|---------|----------|
| `/f` | Full loop, auto-routes PR-mode vs feature-mode (Step 0a) |
| `/factory`, `/df` (user scope) | Aliases for `/f`, identical behavior |
| `/f-pr` | Explicit PR-mode entry point (skips auto-route, always PR-mode) |

The user types `/f $ARGUMENTS` (or an alias); parse args per **Arg parsing**
and run the steps below. This skill file is the single source of truth for the
binary-first contract; the command files only load it. Run `/fs` (the `factory-spec` skill,
`pipelines/slim/spec_gen.dot`) first when the goal needs a spec.

**What "binary-first" bans**: an in-Claude prose-only workflow that claims a
factory run without a logged binary invocation is not a valid run — see
**Honesty rules**. The default `two_node` graph guarantees a worker, a
fresh Codex reviewer on an isolated runner-minted snapshot, and a bounded fix
loop (`max_visits=3`). It has no spec producer, no evidence gates, and no exit
summary; pass `--pipeline gates`, `minimal_feature`, or `pr_gates` when the run
needs evidence gates.

## Repos

- The runner is `$DARK_FACTORY_HOME`, the repo root of the `dark-factory` binary
  on PATH (Step 1 resolves it); never guess it, and an empty `ls` of a guessed
  path proves nothing. Other dark-factory directories (worktrees, run archives,
  clones) are not the runner. Sealed holdouts stay at
  `~/projects/dark-factory-holdouts/` (agent must NEVER read). Pushed:
  `https://github.com/jleechanorg/dark-factory` + `dark-factory-holdouts`.
- To add a sealed-holdout feature, follow [adding-a-feature.md](adding-a-feature.md).

## Pipelines

`--pipeline` takes a short name or a `.dot` path; with no `--pipeline` the
runner runs the default `two_node` (`pipelines/slim/two_node.dot`). The runner
does not expand short names (`runner/paths.py::resolve_pipeline_path`), so the
skill maps each short name to its path from [pipelines.md](pipelines.md), which
lists every short name, path, flow, and when to use it. A custom `--pipeline`
always wins; `/f` never falls through silently to another graph. Full decision
guide: `$DARK_FACTORY_HOME/docs/pipeline-selection.md`. Any other `.dot` under
`$DARK_FACTORY_HOME/pipelines/` runs by path. When no pipeline fits the
inspected PR, see **Honesty rules**; never force an inapplicable holdout
pipeline.

### Step 0a — Auto-route PR-mode vs feature-mode (applies to `/f` and `/factory`; `/f-pr` skips this and forces PR-mode)

Run once before dispatch:

```bash
gh pr list --head "$(git rev-parse --abbrev-ref HEAD)" \
  --json number,title,state,isDraft,additions,deletions,changedFiles,baseRefName,headRefName,labels
echo "$ARGUMENTS"
```

Reasoning (LLM, not rules):
- **No open PR** → feature-mode (new work).
- **Open PR exists** AND goal relates to it → PR-mode (drive the existing PR to
  green): read and follow [pr-mode.md](pr-mode.md).
- **Open PR exists** AND the goal does not relate to it → see **Honesty rules** (mode ambiguity).

### `/f-pr` — explicit PR-mode entry point

`/f-pr` skips Step 0a and always runs PR-mode: read and follow
[pr-mode.md](pr-mode.md) (PR resolution, context reasoning, pipeline choice).

### Step 0b — Backend

Always pass `--backend`. Precedence: explicit flag > invoking `~/.bashrc`
wrapper > `ANTHROPIC_BASE_URL` > parent process name > `claude`. A wrapper
exports `BASH_FUNC_<wrapper>%%`; check `clauded`, `agy`, `codex`, `claude` in
that order and take the first one set. Read it
with `printenv 'BASH_FUNC_<wrapper>%%'`, because `%%` breaks `${X:-default}` and
`${!key}` expansion (`clauded` maps to `claude`, the others to themselves).
`ANTHROPIC_BASE_URL`: `*anthropic.com*` → `claude`, `*openai.com*` → `codex`.
Parent process (`ps -o comm= -p "$PPID"`): `*claude*` → `claude`, `*codex*` →
`codex`, `*agy*`/`*antigrav*` → `agy`. Echo the choice and its source in the
proof block; when `--backend` was explicit, note the override.

### Arg parsing

Honor the flags in `$ARGUMENTS` per [arguments.md](arguments.md); every runner
flag there appears in `dark-factory --help`. Skill conventions: always pass
`--backend` (Step 0b) and `--cxdb ~/.dark-factory/cxdb.sqlite` (so the Healer
clusters across runs); `--feature` follows **Honesty rules**;
`--allow-live-pr-target` and `--allow-push` stay off unless the task authorizes
them; `--reviewer-calibration=true|false` is parsed by this skill, never passed
to the runner, and defaults on (**Reviewer calibration**). Whatever is left
after flag parsing is the **goal description**.

### Steps

Step 1 must run in the same Bash invocation as Step 2 (or be re-sourced first),
because its exports do not survive between shell calls.

1. **Setup**. The factory runs via the installed **`dark-factory` binary** (not
   `python -m runner` from source). If it is missing, report the unmet
   prerequisite and inspect the existing installation before cloning or
   reinstalling. Complete setup within the current authorized scope, or return
   the limitation to the parent for other authorized work; ask only for missing
   authority or access. An unavailable binary is not a completed run. The
   resolver keeps an explicit `DARK_FACTORY_HOME`; otherwise it follows the
   PATH-selected binary's symlinks to its repository root:
   ```bash
   resolve_dark_factory_home() {
     if [ -z "${DARK_FACTORY_HOME:-}" ]; then
       DARK_FACTORY_BIN="$(command -v dark-factory 2>/dev/null)" || {
         echo "ERROR: dark-factory is not on PATH; run ./install.sh first"
         return 1
       }
       DARK_FACTORY_BIN="$(python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$DARK_FACTORY_BIN")" || {
         echo "ERROR: unable to resolve the installed dark-factory binary"
         return 1
       }
       DARK_FACTORY_HOME="$(cd "$(dirname "$DARK_FACTORY_BIN")/.." && pwd -P)" || return 1
     fi
     export DARK_FACTORY_HOME
   }
   export PATH="$HOME/.local/bin:$PATH"
   resolve_dark_factory_home || exit 1
   command -v dark-factory || exit 1
   dark-factory --help || exit 1
   test -x "$DARK_FACTORY_HOME/bin/dark-factory" || {
     echo "ERROR: run $DARK_FACTORY_HOME/install.sh first"
     exit 1
   }
   export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
   ```

2. **Run the pipeline** from the **target repo** (implementation workdir = cwd).
   Pipelines and prompts resolve from `$DARK_FACTORY_HOME`; code changes land in cwd:
   ```bash
   cd "<TARGET_REPO>"
   dark-factory \
     --pipeline pipelines/<PATH_TO_DOT>.dot \
     --goal "<GOAL>" \
     --backend <BACKEND> \
     --feature <FEATURE> \
     --cxdb <CXDB_PATH> \
     --state <KEY>=<VALUE>
   ```

3. **Surface the verdict**. stdout ends with an indented JSON summary (parse it whole)
   (`final_outcome`, `pipeline`, `goal`, `steps`, `trace`). Report the pipeline
   name and final outcome, the per-node trace (one `node  outcome  preview`
   line each), and the runner's exit code.

4. **Run the Healer** (only if `final_outcome != "success"`):
   ```bash
   df-healer --cxdb <CXDB_PATH>
   ```
   Render its markdown table inline: one failure cluster per row, each with
   an actionable prescription template.

5. **Sealed holdouts reminder**. If the run included a `holdout_eval` node,
   remind the user that you (and any subagent) did NOT see the sealed
   scenarios — only the PASS/FAIL verdict per scenario name leaked back.

6. **Reviewer calibration** (default on). Run `dark-factory review` per
   **Reviewer calibration** below against the run's base and head SHAs, and
   report the result per **Verdicts**.

## Reviewer calibration (default on)

`--reviewer-calibration=true` is the default for every real `/f`/`/factory` run;
only `--reviewer-calibration=false` with a stated reason turns it off, and the
final response then says `Reviewer calibration: disabled <reason>`. Step 6 runs:

```bash
dark-factory review \
  --workdir <repo> --base-sha <full-40-hex-sha> --head-sha <full-40-hex-sha> \
  --task-file <path> --output-dir <dir> --backend codex
```

The controller owns the static prompt, envelope, Codex-only tool-free JSONL
dispatch against a read-only workdir, response validation, and
`controller-receipt.json`; it exits 0 pass, 2 fail, 1 invalid. A valid PASS needs
non-empty `evidence_checked` and `commands_executed: []`. Never author reviewer
instructions, inline prompts, or pass vendor CLI flags. Each lane's controller
receipt digest binds the prompt SHA-256, envelope SHA-256, task,
changed-files and evidence-manifest SHA-256 (no diff digest; the tree SHA commits to the change), response SHA-256, the head and
base SHA, and the reviewed tree SHA; hashing `controller-receipt.json` gives the receipt digest.
`prompt.txt` is a binary-emitted audit capture, not an authority file; the static
`prompts/catalog/controller_cold_review_v1.md` is bound by its SHA-256 of the controller receipt.
Artifacts: `evidence/<run-id>/reviewer-calibration/`. The in-graph `two_node`
reviewer (`codex exec --yolo`) is not this read-only controller. Every calibration
run also follows `reviewer-calibration` (`comparison.json`, `adjudication.md`, and
its underperformance-claim rule).

A `gate_er` node with a `backend_priority` attribute resolves its reviewer from
the priority queue (default `codex > minimax > agy > claude-sonnet`;
`DARK_FACTORY_ADVERSARIAL_PRIORITY` overrides the node's list), so run-level
`--backend` does not pick that reviewer. A node with an explicit `backend` uses
it; a node with neither follows run-level `--backend`.

## Output contract

When no pipeline ran because a prerequisite was unmet or no applicable
pipeline fits, report that limitation and return to the parent task without
fabricated run metadata (no fabricated run ID, exit code, or review verdict),
preserving authorized parent continuation. When a pipeline ran, end the
invocation with this proof block; a missing required line means the run is
unproven and must be reported as such:

```bash
# CLI backend: <backend> (source: <explicit --backend|wrapper|ANTHROPIC_BASE_URL|parent process|default>)
# Literal command run:
cd "<TARGET_REPO>"
DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts \
PATH="$HOME/.local/bin:$PATH" \
dark-factory \
  --pipeline <chosen-dot> \
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

## Verdicts

- `status: "valid"` / `backend_returncode: 0` means only that the reviewer
  transport ran and emitted schema-valid JSON, **not** that the code passed.
  `verdict: "pass"` / exit 0 = ACCEPTED; `verdict: "fail"` / exit 2 = REJECTED.
  Never treat a valid status or a successful LLM call as a pass, and never
  rewrite or summarize a failed review as a success, pass, or confirmation.
- When `dark-factory review` completes, report at once. On fail:
  `VERDICT: REJECTED (FAIL)`, the reviewed `HEAD_SHA` and `BASE_SHA`, all
  `findings` and `caveats` from the response verbatim, and that the PR is NOT
  `/ready` and cannot be merged. On pass: `VERDICT: ACCEPTED (PASS)`, the
  reviewed `HEAD_SHA`, and the `controller-receipt.json` digest.
- **Loop exhaustion**: reaching the cycle budget (e.g. 3/3) without
  `verdict: pass` is a **FAILED RUN**. Report
  `STATUS: REVIEW FAILED / EXHAUSTED AFTER N CYCLES`, end that invocation, and
  return its failure evidence and diagnosis to the parent task, which continues
  authorized work within the mission deadline and the active repository's role
  and execution rules; where coding must run through auto-factory, route repairs
  through a fresh or adjusted factory run. A cycle budget is not a mission stop.
  Never call exhaustion "addressed findings", "all gates passing", or "/ready".

## Honesty rules

- Quote the **actual command** (`dark-factory ...` or `df-healer ...`), the
  runner's exit code, and the actual review verdict (`pass` / `fail`)
  verbatim. No paraphrasing.
- Never claim a PR is `/ready`, `/green`, or "All gates passing" while CI
  checks are pending or failing, or when the review verdict is `fail`.
- Never auto-merge, and never retry an unchanged failing approach merely to get
  a pass; preserve the failed verdict.
- Never claim a holdout passed if you read the scenarios — that breaks the
  adversarial guarantee. If you read `~/projects/dark-factory-holdouts/`,
  declare the run **invalid** and rerun the pipeline.
- `--feature` is required when the pipeline has a `holdout_eval` node, but pass
  it only when `~/projects/dark-factory-holdouts/holdouts/<feature>/` exists;
  never invent a value. `hello` is a real feature-agnostic holdout when nothing
  more specific exists — confirm its directory too.
- If `--backend echo` was used, label the run as a wiring smoke, not a real
  validation.
- If `--reviewer-calibration=false` is passed, quote the explicit opt-out
  reason and do not imply raw-Codex-vs-subagent quality was measured.
- Do not claim a factory run based on an in-Claude workflow, `Skill()` call,
  or prose summary. The only valid proof is an actual `dark-factory` binary
  invocation plus the proof block above.
- If `/fs` is needed first, report the prerequisite and carry out its
  already-authorized preparation or return it to the parent task under active
  repository role and execution constraints. Ask only for missing authority; do
  not force a run before it is ready or silently fall through to `gates.dot`
  and pretend the PR is green.
- If no pipeline fits (e.g. docs-only PR), report that limitation and return to
  the parent task for authorized diagnosis or preparation. A materially different
  requested method needs authorization; do not silently substitute a
  holdout-bearing pipeline.
- When the goal is unrelated to the open PR (Step 0a), **ask the user** which
  mode they meant. Do not silently route to PR-mode.
- Pinning proves authority, not parity: `/f` source pinning, fail-closed
  receipts, and historical replays prove reviewer authority and regression
  coverage, not equal quality to a fresh terminal. Claim parity only after an
  independent `/f` run and a fresh-terminal run receive the same input and are
  scored by a predeclared rule against a predeclared acceptance threshold;
  otherwise report the narrower result.

## Known limits

- The `claude` backend shells out to `claude --print
  --dangerously-skip-permissions` — each node is a fresh non-interactive
  session. Long agentic loops should use `/h` instead.
- `_parse_verdict` falls back to scanning the tail for a standalone PASS/FAIL
  token; if the gate command emits a debug line with just "PASS" the runner
  will trust it. For high-stakes gates require explicit `VERDICT:` markers.
- CXDB is per-process — don't share one across threads.
