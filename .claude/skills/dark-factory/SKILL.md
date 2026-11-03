---
name: dark-factory
description: "Run the Dark Factory DOT pipeline runner against a goal. Slash command: /f (aliases /factory, /df, /f-pr). Implements StrongDM's Attractor pattern as an external Python runner — .dot files are the versioned artifact, sealed holdouts live in a separate repo, every step is recorded to CXDB, and the Healer clusters failures into diagnoses. Use when you want the goal_harness idea executed as a reproducible external pipeline instead of in-Claude subagent dispatch."
---

# /f — Dark Factory Pipeline Runner

## Purpose

Run a goal through the Dark Factory `.dot` pipeline runner. The runner is the
Python implementation of StrongDM's Attractor pattern (cf. brynary/attractor,
strongdm/attractor).

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
| `/df` | User-scope alias for `/f`, identical behavior |
| `/f-pr` | Explicit PR-mode entry point (skips auto-route, always PR-mode) |

This skill file is the single source of truth for the binary-first contract;
the command files only load it. Run `/fs` (the `factory-spec` skill,
`pipelines/slim/spec_gen.dot`) first when the goal needs a spec.

**What "binary-first" bans**: an in-Claude prose-only workflow that claims a
factory run without a logged binary invocation is not a valid run — see
**Honesty rules** below. The default `two_node` graph guarantees a worker, a
fresh Codex reviewer running in the worker's worktree, and a bounded fix loop
(`max_visits=3`). It has no spec producer, no evidence gates, and no exit
summary; pass `--pipeline gates`, `minimal_feature`, or `pr_gates` when the run
needs evidence gates.

## Repos

- The runner is `$DARK_FACTORY_HOME`, the repo root of the `dark-factory` binary
  on PATH (Step 1 resolves it); never guess it, and an empty `ls` of a guessed
  path proves nothing. The other `$HOME/Snapchat/Dev/*dark*factory*` directories
  (worktrees, run archives, clones) are not the runner. Sealed holdouts stay at
  `~/projects/dark-factory-holdouts/` (agent must NEVER read), which is not
  Snap's `Snapchat/dark_factory_holdback`. Pushed:
  `https://github.com/jleechanorg/dark-factory` + `dark-factory-holdouts`.

## Pipelines

`--pipeline` takes a short name from this table or a `.dot` path. The runner
does not expand short names (`runner/paths.py::resolve_pipeline_path`), so the
skill passes the path. A custom `--pipeline` always wins; `/f` never falls
through silently to another graph. Full decision guide:
`$DARK_FACTORY_HOME/docs/pipeline-selection.md`. Any other `.dot` under
`$DARK_FACTORY_HOME/pipelines/` runs by path.

| Short name | Path | Flow | When to use |
|------------|------|------|-------------|
| **`two_node`** ⭐ default | `pipelines/slim/two_node.dot` | worker → fresh Codex reviewer → exit | Default when no `--pipeline` is passed. The reviewer is a fresh fully tooled `codex exec --ephemeral --yolo` process in the worker's worktree; its response is copied to the worker on failure. |
| `two_node_holdout` | `pipelines/slim/two_node_holdout.dot` | worker → fresh Codex reviewer → holdout → exit | Two-node plus behavioral holdouts (opt-in) |
| `hello` | `pipelines/factory/hello.dot` | plan → implement → holdout → fix-loop → exit | New feature with a holdout scenario; wiring smoke with `--backend echo` |
| `gates` | `pipelines/factory/gates.dot` | start → holdout → /es → /er → /code_standards → exit | Validate an already-implemented diff (fixed `hello` holdout) |
| `pr_gates` | `pipelines/factory/pr_gates.dot` | start → holdout → /es → /er → /code_standards → exit | Validate an in-flight PR diff (holdout from `--feature <name>`) |
| `minimal_feature` | `pipelines/slim/minimal_feature.dot` | explore → plan → implement → test → review → holdout → gates → exit | Full production pipeline from scratch with a holdout |
| `minimal_pr` | `pipelines/slim/minimal_pr.dot` | explore → plan → implement → test → review → holdout → gates → exit | In-flight PR iteration with `--state slim.test_command=...` (requires `--feature <name>`) |
| `ready` | `pipelines/slim/ready.dot` | test → /es → /er → /advice → holdout → fix (max 3) | PR `/ready` enforcement + fix iteration |
| `bug_fix` | `pipelines/bug_fix.dot` | reproduce → red gate → fix → green gate → holdout → evidence | Bug fix with red/green discipline |
| `spec_gen` | `pipelines/slim/spec_gen.dot` | explore → plan → cold spec review → fix loop | Reviewed spec only (the `/fs` pipeline) |
| `minimal_research` | `pipelines/slim/minimal_research.dot` | explore fanout → research → exit | Research report only |
| `review_slim` | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` | acceptance + codex reviewer | Spec review (slim) |
| `review_full` | `benchmarks/attractor-spec-review/pipelines/review_full.dot` | review_slim + fullstack smoke | Spec review with stack smoke |
| `level5_feature` | `pipelines/factory/level5_feature.dot` | full reference pipeline | Level-5 reference pipeline with hard-tier gates |
| **no pipeline** | — | — | No pipeline fits the inspected PR: see **Honesty rules**; do not force an inapplicable holdout pipeline |

### How to invoke this skill

The user types `/f $ARGUMENTS` (or `/factory $ARGUMENTS`). Parse args per
**Arg parsing** and run the steps below.

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
- **Open PR exists** AND the goal does not relate to it → see **Honesty rules** (mode ambiguity).

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
   change (if not, see **Honesty rules** on `/fs`); holdout eligibility
   (see **Honesty rules** on `--feature`); and what evidence mix (`/es` + `/er` +
   `/code_standards` minimum, `holdout_eval` for behavior-grade) the
   pipeline needs to deliver without over-running.
4. Execute only after needed preparation is complete and a pipeline
   actually fits: pick the pipeline from **Pipelines** above
   using that reasoning, pick the backend (`echo` for wiring smoke;
   `claude` unless the PR's reviewer queue or `gate_er` priority queue
   says otherwise), then construct, show, and run the command — same
   shape as Step 2 below, run from the PR's target repo.
5. Report the outcome per **Output contract** below.

`/f-pr` honesty rules (in addition to the shared ones under **Honesty
rules**): the `gate_er` priority queue (`codex > minimax > agy >
claude-sonnet`) still resolves the reviewer even when `--backend claude`
was passed for the run itself — passing `--backend claude` does not mean
`gate_er` uses claude by default.

### Step 0b — Backend

Always pass `--backend`. Precedence: explicit flag > invoking wrapper
(`clauded`, `codex`, `claude`) > `ANTHROPIC_BASE_URL` > parent process >
`claude`. Echo the choice and its source in the proof block.

### Arg parsing

Honor these flags inside `$ARGUMENTS`. Every runner flag below appears in
`dark-factory --help`; rows marked *skill convention* are parsed by this skill
and never passed to the runner.

| Flag | Meaning | Skill convention |
|------|---------|------------------|
| `--pipeline <name>` | Graph to run | Short name from **Pipelines** (mapped to its path) or a `.dot` path; omit for `two_node` |
| `--goal <text>` | Goal text | Whatever is left after flag parsing; mutually exclusive with `--target` |
| `--target <locator>` / `--target-intent <text>` | Review a resolved target (`PR 811`, path, SHA) instead of a goal | Use for review-only runs; the run refuses to start if the locator does not resolve |
| `--feature <name>` | Holdout feature for `holdout_eval` nodes | Holdout name; see **Honesty rules** |
| `--backend echo\|claude\|codex\|ao\|agy` | LLM backend for codergen nodes | Skill default: always passed explicitly (Step 0b). `echo` is a cost-free wiring smoke |
| `--cxdb <path>` | CXDB SQLite step log | Skill default: always pass `~/.dark-factory/cxdb.sqlite` so the Healer clusters across runs |
| `--max-steps <N>` | Step budget (default 100) | — |
| `--state KEY=VALUE` | Pre-seed `ctx.state`; repeatable | e.g. `--state slim.test_command="pytest tests/..."` in PR loops |
| `--preflight` | Validate the pipeline, print diagnostics, exit | — |
| `--resume <checkpoint>` | Resume from a checkpoint file | — |
| `--evidence-bundle <dir>` | Per-run evidence bundle (default `evidence/<run-id>`) | — |
| `--events <path>` | JSONL structured event log | — |
| `--workdir <dir>` | Implementation workdir (default cwd) | — |
| `--require-holdouts` | Fail fast when the feature has no `scenarios.yaml` | — |
| `--allow-live-pr-target` / `--allow-push` | Run on a branch with an open PR / let codergen nodes `git push` | Both default off; pass only when the task authorizes it |
| `--reviewer-calibration=true\|false` | — | *Skill convention*, default on (**Reviewer calibration**) |

Whatever is left after flag parsing is the **goal description**.

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
   cd "<TARGET_REPO>"   # e.g. the product repo being built
   dark-factory \
     --pipeline pipelines/<PATH_TO_DOT>.dot \
     --goal "<GOAL>" \
     --backend <BACKEND> \
     --feature <FEATURE> \
     --cxdb <CXDB_PATH> \
     --state <KEY>=<VALUE>
   ```

3. **Surface the verdict**. The last line of stdout is a JSON summary with
   `final_outcome`, `pipeline`, `goal`, `steps`, `trace`. Report:
   - Pipeline name + final outcome
   - Per-node trace (one line each: `node  outcome  preview`)
   - Exit code of the runner

4. **Run the Healer** (only if `final_outcome != "success"`):
   ```bash
   df-healer --cxdb <CXDB_PATH>
   ```
   Render the resulting markdown table inline. Each row is a failure cluster
   with a prescription template the user can act on.

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
receipt digest binds the prompt SHA-256, envelope SHA-256, task, diff,
changed-files and evidence-manifest SHA-256, response SHA-256, and the head and
base SHA; hashing `controller-receipt.json` gives the receipt digest.
`prompt.txt` is a binary-emitted audit capture, not an authority file; the static
`prompts/catalog/controller_cold_review_v1.md` is bound by its SHA-256 of the controller receipt.
Artifacts: `evidence/<run-id>/reviewer-calibration/`. The in-graph `two_node`
reviewer (`codex exec --yolo`) is not this read-only controller; A/B reviewer
comparison belongs to the `reviewer-calibration` skill.

## Output contract

When a pipeline ran, end the `/f`/`/factory` invocation with this proof block
(missing any required line means the run is unproven and must be reported as
such). When no pipeline ran because a prerequisite was unmet or no applicable
pipeline fits, report the unmet prerequisite or no-fitting-pipeline limitation
and return to the parent task without fabricated run metadata (no fabricated
run ID, exit code, or review verdict), preserving authorized parent
continuation.

For executed pipeline runs, provide this proof block:

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

## Adding a new feature

To add a new sealed-holdout feature `foo`:

1. Write `$DARK_FACTORY_HOME/specs/foo.md` (agent-visible).
2. Write `$DARK_FACTORY_HOME/prompts/foo/{plan,implement,fix}.md`.
3. Write `~/projects/dark-factory-holdouts/holdouts/foo/scenarios.yaml`
   in the sealed repo (the implementing agent must never read this).
4. Either reuse `pipelines/factory/hello.dot` (which is feature-agnostic) by
   passing `--feature foo`, or write `pipelines/factory/foo.dot` with custom
   nodes.

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
  runner's exit code, and the review verdict (`pass` / `fail`) verbatim, as the
  proof block's literal lines. No paraphrasing.
- Never claim `/green` while CI checks are pending/failing, and never claim a PR
  is `/ready`, `/green`, or "All gates passing" when the review verdict is `fail`.
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
- Reviewer calibration is default-on. If `--reviewer-calibration=false` is
  passed, quote the explicit opt-out reason and do not imply raw-Codex-vs-
  subagent quality was measured.
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
