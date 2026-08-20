---
name: auto-factory
description: End-to-end auto-factory driver — picks up beads + GH issues tagged factory, dispatches coders to drive worldai PRs to /green + /advice + /er, runs verifier ticks, repeats until done. Designed to work even when GH API is rate-limited (falls back to beads-only).
---

# /auto-factory — one end-to-end drive tick

The auto-factory is the agent-orchestrator-style system that drives worldai PRs to merge. This skill is its orchestrator: it picks up work (beads + GH issues), dispatches coder subagents, runs verifier ticks, and iterates until gates pass.

## 0. Execution host + Bead authority preflight

The invocation host is the candidate factory host. Continue on that host only
when a local factory service/config is present and its daemon configuration
supports `target_repo` (as the top-level target or in `[repos]`). If it is not
capable, stop without mutating intake; host selection and remote routing belong
to the user-scoped command that invoked this repository command.

Before any intake mutation, resolve the exact Bead DB configured by the local
factory supervisor into `DARK_FACTORY_BR_DB`. Bind `br`, the overlay, and any
manual tick to that same path; ambient `br where` discovery is not authority:

```bash
BR_DB="${DARK_FACTORY_BR_DB:?resolve from the local factory supervisor}"
[ -n "$BR_DB" ] && [ "${BR_DB#/}" != "$BR_DB" ] && [ -f "$BR_DB" ] || exit 1
export BR_DB
command -v br >/dev/null
br --db "$BR_DB" where
br --db "$BR_DB" sync --status --json
br --db "$BR_DB" doctor --quick
```

If the status is not healthy, or if both `jsonl_newer` and `db_newer` are true,
stop before `br create`, `br update`, or a factory tick. Ask the operator to
choose the authoritative representation, reconcile it with the Beads recovery
workflow, and rerun both checks. A GitHub fallback does not authorize a write
to an ambiguous Bead store.

Intake is a two-phase operation: create without the `factory` label, prove that
the selected factory can read the new Bead from the same store, and only then
apply the label:

```bash
bead_id="$(br --db "$BR_DB" create "<title>" --body "<body>" --json | jq -r '.id')"
br --db "$BR_DB" show "$bead_id" --json
br --db "$BR_DB" update "$bead_id" --add-label factory --json
br --db "$BR_DB" show "$bead_id" --json
br --db "$BR_DB" list --status open --label factory --json
```

For an existing Bead, perform the same read proof before adding the label. If
the read or label verification fails, stop. After labelling, check that
`$H list QUEUED` contains the Bead. Report `QUEUED` only when the overlay has
adopted it; otherwise report `intake verified; adoption pending`.

The Bead body must remain below the AO 4096-character task-description limit.

### 0a. Load contract + config

Only after the execution-host preflight passes:

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
br --db "$BR_DB" list --status open --label factory --json
```

For each bead: read body, detect `drive-existing-pr` mode (fields `existing_pr`, `existing_branch`, `target_repo`). If present, this bead drives an existing PR — coder must push to existing branch via `git push wa <existing_branch>`. Otherwise default to new-work (create `factory/<bead>-r<attempt>` branch).

### 1b. GitHub pickup (fallback when beads empty OR to capture fresh work)
```bash
gh issue list --repo "$TARGET_REPO" --label factory --state open --json number,title,body --limit 20
```

If GH API rate-limited (returns error), skip this step — beads-only mode. Log the fallback: `[intake] GH API rate-limited, beads-only mode`.

For each GH issue, treat it as a Bead: read the body, create it without the
`factory` label (other non-routing labels are allowed), read it back through
`br --db "$BR_DB" show "$bead_id" --json`, then add the factory label with
`br --db "$BR_DB" update "$bead_id" --add-label factory --json`. Continue with
Bead pickup only after the same-store verification succeeds.

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

For every ATTESTED bead (PR opened/updated), run the canonical 8 gates as defined in
`daemon/src/verifier.rs::GateName` (`Ci, NoConflicts, CodeRabbitApproved, BugbotClean,
CommentsResolved, EvidenceFloor, Skeptic, VacuousRedGreen`). The `code_standards` and `zfc`
checks are optional advisory reviews — they are NOT required keys in the gate-assessment JSON
and their absence never blocks `all_green`. If you DO record them, they participate like any
recorded gate: a `fail` verdict blocks `all_green` and routes through the same `reroll-verdict`
fix loop. See bead jleechan-1gft for promoting them to required `GateName` gates in the Rust
verifier. Gate 8 (`VacuousRedGreen`, issue #387 / bead jleechan-ijod) was added in
PR #389 / r5 commit 175c6ad — runtime vacuous-test detector verdict.

| Verdict  | Meaning                                                                | Gate result     |
|----------|------------------------------------------------------------------------|-----------------|
| `pass`   | Reviewer returned positive evidence; ready to advance                  | counts toward `all_green` |
| `warn`   | Reviewer returned mixed evidence; non-blocking, surface in next tick   | counts toward `all_green` |
| `fail`   | Reviewer returned negative evidence; reroll required                   | forces reroll-verdict |
| `unknown`| Reviewer could not gather evidence yet; wait for the next tick         | blocks `all_green` |

`all_green=true` iff every gate returned `pass` or `warn`. `fail` routes through
`reroll-verdict → HUMAN_HELD → recover-held → QUEUED` — the bounded fix loop
shared with the original 8 gates; **no parallel implementation**. `unknown`
defers to the next tick rather than racing to READY.

```bash
gh pr view <pr> --repo "$TARGET_REPO" --json headRefOid,mergeable,reviewDecision,statusCheckRollup
gh pr checks <pr> --repo "$TARGET_REPO" --json name,state,conclusion
```

Each gate is a model-delegated review (NOT keyword routing); the verifier
dispatches them as subagents / `claude --print /<slash>` / `codex exec --yolo`
against the PR diff. The overlay only records verdicts — the pass/warn/fail
decision is the model's, not ours.

Assess each gate:
- ci_green: every check `conclusion=success` (or state=SUCCESS)
- no_conflicts: `mergeable=MERGEABLE`, mergeStateStatus not DIRTY
- coderabbit: latest `coderabbitai[bot]` review APPROVED
- bugbot: zero error-severity `cursor[bot]` comments
- comments_resolved: every reviewThread `isResolved=true`
- evidence_review: 5-criterion /er rubric pass
- skeptic: parallel minimax cold review
- code_standards: dispatch `code-standards` review (overrides `~/.claude/skills/code-standards/SKILL.md` or repo-local `.claude/skills/code-standards/SKILL.md`) — pass/warn only when every claimed pass carries file/line evidence per the lane's Iron Law; fail on bare assertions without file/line proof
- zfc: dispatch `zfc` review against `.claude/skills/zero-framework-cognition/SKILL.md` + `.claude/skills/root-cause-first/SKILL.md` — pass/warn only when no banned-pattern scan finds a violation; fail otherwise

Each gate's verdict value in `gates_json` is either a plain string OR a
structured object so reviewers can audit the verdict without re-running the
model:

```json
"zfc": "pass"                                              /* shorthand */
"zfc": {"verdict": "fail",                                  /* structured */
        "evidence": [{"path": "daemon/factory-overlay.sh",
                      "line": 201,
                      "msg": "keyword blacklist enforced in app code"}]}
```

Record via `$H gate-assessment <bead_id> <pr> '<gates_json>'`. The 7-key JSON
schema (strict, matching `daemon/src/verifier.rs::GateName`) is enforced by `factory-overlay.sh`:

```json
{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass",
 "bugbot":"pass","comments_resolved":"pass","evidence_review":"pass",
 "skeptic":"pass"}
 // optional advisory keys — not required; if present, a fail still blocks:
 // "code_standards":"pass","zfc":"pass"
```

Legacy aliases `"green" → "pass"`, `"red" → "fail"` are still accepted so
existing test fixtures don't have to migrate in lockstep, but new callers
should use the pass/warn/fail vocabulary.

All-green (`all_green=true` on stdout line 1) → `$H ready <bead_id> <pr>`
(terminal state; verifier stops driving). Any-fail → `$H reroll-verdict
<bead_id> <pr> <in_place_fixable|reroll_worthy> "<rationale>"`. The `cooldown_ready`
line indicates whether the prior GATE_ASSESSMENT for this PR was a `false`
result (cooldown handling is unchanged from the original 7-gate design).

## 8. Autonomy time-box

`$H autonomy-tick $ELAPSED_SECS` — increment actives, warn at 80%, park over-box.

## 9. End-of-tick summary & Conversation Audit

- Inspect live coding CLI conversations on the selected factory host to verify authentic agent progress before reporting status (see `.claude/skills/factory-status/SKILL.md`).
- `$H tick-summary coder` (or verifier if you ran verifier steps).

## NEVER

- NEVER report progress based on database flags alone without auditing live CLI transcripts.
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
- **Daemon DOWN** (no auto-factory tick loop running): inspect the selected
  host's local supervisor. Invoke `BR_DB="$BR_DB" bash daemon/factory-af-tick.sh`
  for one host-local tick only after the capability and Bead-authority preflight
  passes. Restore the daemon through that host's canonical deployment workflow.
- **Bead stuck HUMAN_HELD**: `factory-af-tick.sh` already calls `$H recover-held` every tick, which requeues any `HUMAN_HELD` bead with `attempt < 10` back to `QUEUED` (incrementing `attempt`, resetting `autonomy_secs`) automatically. To force it immediately: `$H recover-held` (no bead-id argument — it processes every eligible `HUMAN_HELD` row). Never mutate `bead_overlay` with a raw `sqlite3` command.
- **PR ci_green stuck on pre-existing infra**: document in PR comment, treat as known-issue; do NOT block readiness.
- **File-overlap conflict across multiple PRs**: serialize per stacked-PR single-writer rule.
