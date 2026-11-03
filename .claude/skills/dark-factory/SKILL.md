---
name: dark-factory
description: "Run the Dark Factory DOT pipeline runner against a goal. Slash command: /factory. Implements StrongDM's Attractor pattern as an external Python runner — .dot files are the versioned artifact, sealed holdouts live in a separate repo, every step is recorded to CXDB, and the Healer clusters failures into diagnoses. Use when you want the goal_harness idea executed as a reproducible external pipeline instead of in-Claude subagent dispatch."
---

# /factory — Dark Factory Pipeline Runner

## Purpose

Run a goal through the Dark Factory `.dot` pipeline runner. The runner is the
Python implementation of StrongDM's Attractor pattern (cf. brynary/attractor,
strongdm/attractor).

**Never assume where the runner lives — resolve it.** `DARK_FACTORY_HOME`
comes from the `dark-factory` binary on PATH (see the resolution block below),
which today puts it at `$HOME/Snapchat/Dev/jleechanorg-dark-factory-main-latest`.
It has never been under `$HOME/projects/`. A `ls` of a guessed path that comes
back empty says nothing about whether the runner is available — the binary
being on PATH is what decides that.

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
| `/fs` | Spec-generation entry point; runs `pipelines/slim/spec_gen.dot` — run this first when the goal needs a spec |

This skill file is the single source of truth for the binary-first contract;
`.claude/commands/f.md` is the canonical command entry point, with `/factory`
and `/f-pr` as thin aliases so auto-detect behavior stays identical across
entry points.

**What "binary-first" bans**: an in-Claude prose-only workflow that claims a
factory run without a logged binary invocation is not a valid run — see
**Honesty rules** below. The default `two_node` graph guarantees a worker, a
fresh Codex reviewer running in the worker's worktree, and a bounded fix loop
(`max_visits=3`). It has no spec producer, no evidence gates, and no exit
summary; pass `--pipeline gates`, `minimal_feature`, or `pr_gates` when the run
needs evidence gates.

## Repos

- `$DARK_FACTORY_HOME` — the runner (factory code; agent can see this).
  Resolved from the binary on PATH, not hardcoded; currently
  `$HOME/Snapchat/Dev/jleechanorg-dark-factory-main-latest`. Several other
  `$HOME/Snapchat/Dev/*dark*factory*` directories exist (worktrees, run
  archives, patch-refresh clones) — none of them is the runner. Trust the
  symlink, not the name.
- `~/projects/dark-factory-holdouts/` — sealed scenarios (agent must NEVER read).
  This path is correct as written; only the runner moved.
- Pushed: `https://github.com/jleechanorg/dark-factory` + `dark-factory-holdouts`

Not to be confused with Snap's own holdout repo,
`$HOME/Snapchat/Dev/dark_factory_holdback` (`Snapchat/dark_factory_holdback`),
which belongs to `snap_factory` and is a different set of packs.

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
   change (if so, `/fs` is skippable; if needed, report the prerequisite
   and carry out its already-authorized preparation or return it to the
   parent task under active repository role and execution constraints.
   Do not force a pipeline run before it is ready); holdout
   eligibility (pass `--feature <name>` only if
   `~/projects/dark-factory-holdouts/holdouts/<feature>/` actually
   exists — never invent one); and what evidence mix (`/es` + `/er` +
   `/code_standards` minimum, `holdout_eval` for behavior-grade) the
   pipeline needs to deliver without over-running.
4. Execute only after needed preparation is complete and a pipeline
   actually fits: pick the pipeline from **Pipelines** above
   using that reasoning, pick the backend (`echo` for wiring smoke;
   `claude` unless the PR's reviewer queue or `gate_er` priority queue
   says otherwise), then construct, show, and run the command — same
   shape as Step 3 below, run from the PR's target repo.
5. Report the outcome: when a pipeline ran, report the verdict per
   **Output contract** below; otherwise report the unmet prerequisite or
   no-fitting-pipeline limitation and return to the parent task with no
   fabricated run ID, exit code, or review verdict, preserving authorized
   parent continuation.

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
| `--feature <name>` | Holdout feature for `holdout_eval` nodes | Pass only after confirming `~/projects/dark-factory-holdouts/holdouts/<name>/` exists; `hello` is a real feature-agnostic holdout when nothing more specific exists. Never invent one |
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

Before Steps 1–2, define this resolver. It preserves an explicit
`DARK_FACTORY_HOME`; otherwise it resolves the `dark-factory` binary selected
from `PATH`, follows symlinks, derives its repository root, and exports that
same source for pipeline and prompt resolution:

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
```

1. **Verify binary install**. The factory runs via the **`dark-factory` binary**
   (not `python -m runner` from source). If missing, report the unmet prerequisite
   and inspect the existing installation before cloning or reinstalling. Complete
   setup within the current authorized scope and repository execution rules, or
   return the limitation to the parent for other authorized work. Ask only for
   missing authority or access; an unavailable binary is not a completed run.
   The following check runs as a tool command; its failure ends that command,
   then the parent follows the prerequisite guidance above:
   ```bash
   export PATH="$HOME/.local/bin:$PATH"
   resolve_dark_factory_home || exit 1
   command -v dark-factory || exit 1
   dark-factory --help || exit 1
   test -x "$DARK_FACTORY_HOME/bin/dark-factory" || {
     echo "ERROR: run $DARK_FACTORY_HOME/install.sh first"
     exit 1
   }
   ```

2. **Environment**:
   ```bash
   resolve_dark_factory_home || exit 1
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

Calibration routes the Codex backend through the binary-owned controller
command:

```bash
dark-factory review \
  --workdir <repo> \
  --base-sha <full-40-hex-sha> \
  --head-sha <full-40-hex-sha> \
  --task-file <path> \
  --output-dir <dir> \
  --backend codex
```

`dark-factory review` owns the static prompt, canonical envelope, backend
dispatch, response validation, and `controller-receipt.json`. Controller v1
accepts only Codex through its tool-free JSONL transport. A valid PASS requires
non-empty `evidence_checked` and `commands_executed: []`; the
`ReviewTransportReceipt` and controller terminal receipt bind the prompt,
envelope, response, reviewed revision/tree, and evidence manifest. Do not
author reviewer instructions, do not inline prompts, do not pass vendor CLI
flags. The controller binds `prompt SHA-256`, `envelope SHA-256`, `task
SHA-256`, `diff SHA-256`, `changed-files SHA-256`, and
`evidence-manifest SHA-256` in the controller receipt so calibration lanes can
hash the receipt itself for provenance.

Each accepted lane must expose a controller receipt digest containing
the prompt SHA-256, envelope SHA-256, task SHA-256, diff SHA-256,
changed-files SHA-256, evidence-manifest SHA-256, head SHA, base SHA,
and the response's own response SHA-256 digest. Hashing the
`controller-receipt.json` file itself gives a stable SHA-256 digest for
the controller receipt.

`prompt.txt` is a binary-emitted audit capture, not an authority file.
The static authority lives in `prompts/catalog/controller_cold_review_v1.md`
and is bound by its SHA-256 of the controller receipt.

```text
evidence/<run-id>/reviewer-calibration/
  <backend>/
    controller-receipt.json
    envelope.json
    prompt.txt
    reviewer.output.md
    findings.json
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

## Review Verdict Interpretation Contract (Zero Inversion)

- `status: "valid"` / `backend_returncode: 0`: Indicates only that the reviewer CLI/transport executed cleanly and emitted valid JSON matching the schema. It does **NOT** indicate that the code passed.
- `verdict: "fail"` / `exit_code: 2`: Indicates the independent review **REJECTED** the change.
- `verdict: "pass"` / `exit_code: 0`: Indicates the review **ACCEPTED** the change.
- **NEVER** treat `status: "valid"` or a successful LLM invocation as a passing verdict when `verdict == "fail"`.

### Review Result Surface Requirements

Whenever `dark-factory review` completes, the agent MUST immediately report:
- **If `verdict: fail` (exit code 2)**:
  1. `VERDICT: REJECTED (FAIL)`
  2. The exact reviewed `HEAD_SHA` and `BASE_SHA`.
  3. List of all failing Criteria Gates (`C0`–`C7`) and Evidence Gates (`E0`–`E14`).
  4. Full verbatim list of Critical, High, and Blocker findings from `reviewer.output.md`.
  5. Explicit statement that the PR is NOT `/ready` and CANNOT be merged.
- **If `verdict: pass` (exit code 0)**:
  1. `VERDICT: ACCEPTED (PASS)`
  2. The exact reviewed `HEAD_SHA` and `controller-receipt.json` digest.

### Loop Exhaustion Invariant

- Reaching the maximum cycle budget (e.g., 3/3 cycles) without achieving `verdict: pass` constitutes a **FAILED RUN**.
- The agent MUST report `STATUS: REVIEW FAILED / EXHAUSTED AFTER N CYCLES` and
  end that factory invocation and return its failure evidence to the parent task.
  The parent continues authorized work within the mission deadline and the
  active repository's role and execution rules. Where coding must run through
  auto-factory, route repairs through a fresh or adjusted factory run.
  An invocation's cycle budget is not a mission stop.
- An agent must **NEVER** summarize cycle exhaustion as "addressed findings", "all gates passing", or "/ready".

## Honesty rules

- Always tell the user the **actual command** you ran (`dark-factory ...` or `df-healer ...`). No paraphrasing.
- Always quote the **runner's exit code** and **actual review verdict** (`pass` / `fail`) verbatim.
- Never rewrite or summarize a failed review (`verdict: fail`, exit code 2) as a success, pass, or confirmation.
- Never claim a PR is `/ready`, `/green`, or "All gates passing" when the review verdict is `fail` or when CI checks are pending/failing.
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
- If `/fs` is needed first, follow `/f-pr` item 3: report the prerequisite and
  carry out its already-authorized preparation or return it to the parent task
  under active repository role and execution constraints. Ask only for missing
  authority; do not silently fall through to `gates.dot` and pretend the PR is green.
- If no pipeline fits (e.g. docs-only PR), report that limitation and return to
  the parent task for authorized diagnosis or preparation. A materially different
  requested method needs authorization; do not silently substitute a
  holdout-bearing pipeline.
- Do not invent `--feature` values. If there's no holdout directory at
  `~/projects/dark-factory-holdouts/holdouts/<feature>/`, don't pass `--feature`.
- When the goal is unrelated to the open PR (Step 0a), **ask the user** which
  mode they meant. Do not silently route to PR-mode for unrelated work.
- When the fix loop exhausts (3 attempts), surface its failure diagnosis and
  return control to the parent as above. Preserve the failed verdict; never
  auto-merge or retry an unchanged failing approach merely to get a pass.

## Known limits

- The `claude` backend shells out to `claude --print
  --dangerously-skip-permissions` — each node is a fresh non-interactive
  session. Long agentic loops should use `/h` instead.
- `_parse_verdict` falls back to scanning the tail for a standalone PASS/FAIL
  token; if the gate command emits a debug line with just "PASS" the runner
  will trust it. For high-stakes gates require explicit `VERDICT:` markers.
- CXDB is per-process — don't share one across threads.

## Pinning proves authority, not parity

`/f` source pinning, fail-closed receipts, and historical replays prove reviewer
authority and regression coverage — they do NOT prove equal quality to a fresh
terminal. Claim parity only after an independent `/f` run and a fresh-terminal run
receive the same input and are scored by a predeclared rule against a predeclared
acceptance threshold. Otherwise report the narrower result.
