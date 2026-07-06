---
name: auto-factory
description: End-to-end auto-factory driver — picks up beads + GH issues tagged factory, dispatches coders to drive worldai PRs to /green + /advice + /er, runs verifier ticks, repeats until done. Designed to work even when GH API is rate-limited (falls back to beads-only).
---

# /auto-factory — one end-to-end drive tick

The auto-factory is the agent-orchestrator-style system that drives worldai PRs to merge. This skill is its orchestrator: it picks up work (beads + GH issues), dispatches coder subagents, runs verifier ticks, and iterates until gates pass.

## 0. Load contract + config

```bash
H=daemon/factory-overlay.sh
CONFIG=config/daemon.toml
[ -f "$CONFIG" ] || CONFIG=daemon/contracts/daemon.toml.example
$H init  # idempotent
```

The overlay harness `daemon/factory-overlay.sh` is the canonical executable spec for the auto-factory state machine (restored from `e60b5a31b~1:daemon/factory-lite-harness.sh` and extended in PR #167). All sqlite3 mutations to `~/.dark-factory/daemon-cxdb.sqlite` flow through it. Subcommands: `init`, `intake-upsert`, `route-record`, `capacity`, `dispatch-record`, `pr-opened`, `autonomy-tick`, `gate-assessment`, `prev-gate-assessment`, `ready`, `reroll-verdict`, `park`, `park-duplicate`, `bead-closed-check`, `tick-summary`, `recover-held`, `unstick-dispatching`, `redrive-pr`, `list`.

> Historical note: the original `daemon/factory-lite-harness.sh` and `daemon/run-factory-lite.sh` were removed in commit `e60b5a31b` (2026-07-05, jleechan-xrdx). The decommissioned factory-lite-coder / factory-lite-verifier skills are gone; their protocols are now inline in this skill + factory-af-tick.sh + factory-overlay.sh.

## 1. Intake (work pickup)

Pick up work from BOTH sources (bead store + GitHub):

### 1a. Bead pickup (primary)
```bash
br list --status open --label factory --json
```

For each bead: read body, detect `drive-existing-pr` mode (fields `existing_pr`, `existing_branch`, `target_repo`). If present, this bead drives an existing PR — coder must push to existing branch via `git push wa <existing_branch>`. Otherwise default to new-work (create `factory/<bead>-r<attempt>` branch).

### 1b. GitHub pickup (fallback when beads empty OR to capture fresh work)
```bash
gh issue list --repo "$TARGET_REPO" --label factory --state open --json number,title,body --limit 20
```

If GH API rate-limited (returns error), skip this step — beads-only mode. Log the fallback: `[intake] GH API rate-limited, beads-only mode`.

For each GH issue, treat as a bead: read body, file a local bead via `br create "<title>" --body "<body>" --label factory --label drive-existing-pr`, then continue with bead pickup.

### 1c. Drive-existing-pr detection
A bead/issue has `drive-existing-pr` mode if body contains ALL of:
- `existing_pr: <N>`
- `existing_branch: <name>`
- `target_repo: <owner>/<name>`

The coder MUST push to the existing branch via `git push <remote> <existing_branch>` (NOT create a new factory/* branch). This is the general-purpose pattern.

## 2. Route each QUEUED bead

`$H list QUEUED` → for each bead, use LLM judgment to pick `SMALL_PATH` (single-file fix) vs `STANDARD_PATH` (multi-file/architecture). Record via `$H route-record <bead_id> <VERDICT> "<note>"`.

## 3. Capacity

`free=$($H capacity)` — `min(max_workers - active, max_batch)`. If 0, skip to step 7.

## 4. Dispatch — PARALLEL coder subagents

Select up to `$free` routed QUEUED beads (file-overlap rule: serialize if any share a mutable file). For each selected bead:

### Drive-existing-pr mode
```bash
$H dispatch-record <bead_id> <existing_branch>  # e.g., fix/quota-banner-modal-cta-7945
```

### New-work mode (default)
```bash
$H dispatch-record <bead_id> factory/<bead_id>-r<attempt>
```

**Spawn ALL selected coders in ONE message** — multiple `Agent` tool calls, each `subagent_type: minimax-pair-coder`, `run_in_background: true`. Parallel dispatch is the point.

Each coder prompt MUST include:
- Bead id + full title/description
- Drive-existing-pr vs new-work instruction (and the existing_branch/existing_pr/target_repo fields if drive mode)
- The exact push command (`git push wa <branch>` for worldai, `git push origin <branch>` for dark-factory)
- An isolation requirement (worktree at `/tmp/<bead>-wt`; remove after push; never check out branches in the shared repo working tree)
- A clear "do NOT open new PR / do NOT merge" rule

## 5. (reserved)

Step number kept stable.

## 6. Detect PRs opened/updated by dispatched coders

For every row in `$H list DISPATCHED`:
```bash
$H bead-closed-check <bead_id>  # guards against direct bead closure
```

For drive-existing-pr beads, check the existing PR branch for new commits:
```bash
gh pr view <existing_pr> --repo <target_repo> --json headRefOid,mergeable,statusCheckRollup,reviewDecision,comments
```

If new commits detected → `$H pr-opened <bead_id> <existing_pr> <url>`.

For new-work beads, check for new PR:
```bash
gh pr list --repo "$TARGET_REPO" --head "factory/<bead_id>-r<attempt>" --state open --json number,url
```

## 7. Verifier tick (gate assessment)

For every ATTESTED bead (PR opened/updated), run the 7 gates:

```bash
gh pr view <pr> --repo "$TARGET_REPO" --json headRefOid,mergeable,reviewDecision,statusCheckRollup
gh pr checks <pr> --repo "$TARGET_REPO" --json name,state,conclusion
```

Assess each gate:
- ci_green: every check `conclusion=success` (or state=SUCCESS)
- no_conflicts: `mergeable=MERGEABLE`, mergeStateStatus not DIRTY
- coderabbit: latest `coderabbitai[bot]` review APPROVED
- bugbot: zero error-severity `cursor[bot]` comments
- comments_resolved: every reviewThread `isResolved=true`
- evidence_review: 5-criterion /er rubric pass
- skeptic: parallel minimax cold review

Record via `$H gate-assessment <bead_id> <pr> '<gates_json>'`.

All-green → `$H ready <bead_id> <pr>` (terminal state; verifier stops driving).
Any-red → `$H reroll-verdict <bead_id> <pr> <in_place_fixable|reroll_worthy> "<rationale>"`.

## 8. Autonomy time-box

`$H autonomy-tick $ELAPSED_SECS` — increment actives, warn at 80%, park over-box.

## 9. End-of-tick summary

`$H tick-summary coder` (or verifier if you ran verifier steps).

## NEVER

- NEVER run sqlite3 directly against the CXDB — every mutation via `$H`.
- NEVER force-push or push directly to `base_branch`.
- NEVER run `gh pr merge` — dispatch is not the merge authority.
- NEVER delete a branch.
- NEVER spawn a coder the harness refused to `dispatch-record`.
- NEVER dispatch two beads with overlapping files in the same tick.
- NEVER keyword-route — routing is model judgment (ZFC).
- NEVER await coder subagents inside the tick — spawn parallel, in background.
- NEVER push to a new `factory/*` branch when the bead body has drive-existing-pr fields — push to the existing branch.

## Failure modes & recovery

- **GH API rate-limited**: skip GH pickup, use beads-only mode; continue.
- **Daemon DOWN** (no auto-factory tick loop running): invoke `bash daemon/factory-af-tick.sh` for one tick; for a 24/7 poll loop, install the launchd plist from `daemon/launchd/ai.dark-factory.af-tick.plist` (bead jleechan-57h0) and `launchctl bootstrap gui/$UID <plist>`.
- **Bead stuck HUMAN_HELD**: query previous `gate-assessment` — if cooldown over, reset bead to QUEUED via direct sqlite3 update + increment attempt counter.
- **PR ci_green stuck on pre-existing infra**: document in PR comment, treat as known-issue; do NOT block readiness.
- **File-overlap conflict across multiple PRs**: serialize per stacked-PR single-writer rule.