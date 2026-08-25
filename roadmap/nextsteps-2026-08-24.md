# Next steps — dark-factory — 2026-08-24

## Table of contents

- [Executive summary](#executive-summary)
- [Evidence snapshot](#evidence-snapshot)
- [Bead and issue index](#bead-and-issue-index)
- [Executable handoff sequence](#executable-handoff-sequence)
- [Pull-request state](#pull-request-state)
- [Learnings and roadmap pointers](#learnings-and-roadmap-pointers)
- [2026-08-24 (later) — Three-lane funnel analysis + repeatable df-funnel-lanes CLI](#2026-08-24-later--three-lane-funnel-analysis--repeatable-df-funnel-lanes-cli)
- [2026-08-24 (later) — existing-PR dispatch/worktree failure](#2026-08-24-later--existing-pr-dispatchworktree-failure)
- [2026-08-24 (historical snapshot) — Funnel metrics and draft remediation](#2026-08-24-historical-snapshot--funnel-metrics-and-draft-remediation)
- [2026-08-24 (current, verified refresh) — Funnel metrics and PR readiness](#2026-08-24-current-verified-refresh--funnel-metrics-and-pr-readiness)

## Executive summary

The earlier “runner pool offline” conclusion was a scope error, not an infrastructure outage. The repository-scoped Actions endpoint returns zero because this project’s self-hosted runners are registered at the organization level. The organization endpoint currently reports 14 matching online runners (1 busy), and the repository has no queued or in-progress workflow runs.

The handoff therefore has no runner-restoration blocker. PRs #734, #742, and #744 are already merged. The current critical path is funnel measurement and remediation: independently review draft PRs #748 (honest CodeRabbit reporting), #749 (exact-head attribution), and #750 (gate-8 pytest routing), then re-run the corrected 24h/3d/30d metrics. PR #746 and issue #743 remain preserved but are explicitly deprioritized behind this funnel work; no ironclad criterion or 48-hour sustain check has passed.

## Evidence snapshot

Point-in-time checks run on 2026-08-24:

```text
gh api /repos/jleechanorg/dark-factory/actions/runners
  total_count=0 (expected for repo-scoped registration)

gh api /orgs/jleechanorg/actions/runners
  total_count=14, online=14, busy=1

gh run list --repo jleechanorg/dark-factory --status queued
  []
gh run list --repo jleechanorg/dark-factory --status in_progress
  []
```

The canonical selector is documented in [docs/self-hosted-runner-selector.md](../docs/self-hosted-runner-selector.md). Re-run the organization-scoped probe before calling CI unavailable; do not open a runner-restoration task from the repository endpoint alone.

## Bead and issue index

Bead IDs below are Beads identifiers, not GitHub issue URLs. GitHub links point only to the corresponding issue or pull request.

| Bead | Bead status | GitHub tracking | Handoff role |
| :--- | :--- | :--- | :--- |
| `jleechan-xn4n` | **OPEN** | [Issue #286](https://github.com/jleechanorg/dark-factory/issues/286) is CLOSED; [PR #287](https://github.com/jleechanorg/dark-factory/pull/287) is MERGED | Unrelated Linux-container `sqlite3` portability work associated with PR #577; never use this bead as runner-restoration tracking |
| `jleechan-azso` | OPEN | [Issue #668](https://github.com/jleechanorg/dark-factory/issues/668) | Pipeline fold (PR #693 family) |
| `jleechan-57ym` | OPEN | [Issue #669](https://github.com/jleechanorg/dark-factory/issues/669) | Lane D follow-ups, included by the canonical fold candidate |
| `jleechan-gagl` | OPEN | [Issue #670](https://github.com/jleechanorg/dark-factory/issues/670) | Lane E/F remediation (PR #694) |
| `jleechan-18mu` | OPEN parent bead | No GitHub URL asserted here | Parent tracking only |

“Handoff priority” below is the order an incoming operator should execute work. It is intentionally separate from Bead priority metadata; a closed bead or an issue’s label does not make a stale PR canonical.

## Executable handoff sequence

### Completed — rate-limit circuit breaker (PR #734)

1. [PR #734](https://github.com/jleechanorg/dark-factory/pull/734) fixed the generic-403 false positive while preserving explicit GitHub primary-limit, secondary-limit, abuse, and `Retry-After` signals. It reached exact-head CI, evidence, independent-review, and real-browser Gemini+Grok approval before merging as `96f8d82055b9530324c861ef57620aedbf99847f`.
2. Duplicate generations #692, #698, #702, #706, #710, #713, #717, #720, and #727 are CLOSED as superseded; their branches were retained.

### P0 — land one web-advice/Lane-D superset

1. Use [PR #742](https://github.com/jleechanorg/dark-factory/pull/742) as the current canonical candidate. It rebases the complete handler, prompt, production-pipeline fold, Lane-D files, tests, and E2E artifact onto current `main`; it also makes the canonical invocation pass `--require-holdouts` with a regression test.
2. Treat #691, #693, #700, #701, #707, #708, #711, #715, #718, and #723 as duplicate/subset generations; do not merge them independently of #742.
3. A real Grok browser review found that CDP liveness was incorrectly coupled to Aside CLI. Head `2b63cf0cd658f0bcc3884a4d3c68c3f32f51fad2` replaces the bare-port/Aside check with validation of Chrome's `/json/version`; focused Python evidence is 147 passing tests and the live Linux probe reports Aside absent with CDP healthy.
4. Before merge, resolve the remaining design/implementation review, rerun all exact-head gates, and obtain a valid real-browser panel result with honest four-provider attempt and public-share accounting. APIs, CLIs, and subagents are not substitutes for `/web-advice` seats.

### P1 — land Lane E/F remediation

1. [PR #694](https://github.com/jleechanorg/dark-factory/pull/694) is the rebased Lane E/F candidate. Its production behavior passed independent review; remaining cleanup is test-environment restoration/serialization and removal of four trailing-whitespace additions.
2. Rerun its shell/Python/Rust checks and exact-head review after cleanup. Land #742 first or rebase #694 afterward because both reference `docs/web-advice-failopen-e2e-log.md`.

### P2 — tracking hygiene

1. Keep closed [Issue #286](https://github.com/jleechanorg/dark-factory/issues/286) closed; its organization-vs-repository runner-scope explanation is complete.
2. Keep Bead IDs as Bead IDs; never manufacture `github.com/.../issues/jleechan-*` links.
3. Record future cross-machine documentation synchronization as a process improvement only when independently verified; no Mac-session or SKILL-sync causal claim is part of this handoff.

## Pull-request state

- [PR #740](https://github.com/jleechanorg/dark-factory/pull/740): **MERGED** — this roadmap handoff.
- [PR #714](https://github.com/jleechanorg/dark-factory/pull/714): **MERGED** — prior roadmap synchronization.
- [PR #734](https://github.com/jleechanorg/dark-factory/pull/734): **MERGED** as `96f8d82055b9530324c861ef57620aedbf99847f` after `/ready` and real-browser approval.
- [PR #742](https://github.com/jleechanorg/dark-factory/pull/742): **OPEN / CANONICAL** — rebased web-advice/Lane-D superset; exact-head review and browser-panel work remain.
- [PR #693](https://github.com/jleechanorg/dark-factory/pull/693) and [PR #691](https://github.com/jleechanorg/dark-factory/pull/691): **OPEN / SUPERSEDED CANDIDATES** — do not merge independently of #742.
- [PR #694](https://github.com/jleechanorg/dark-factory/pull/694): **OPEN / CONDITIONAL REVIEW** — Lane E/F remediation with small test-hygiene cleanup pending.
- PRs #692, #698, #702, #706, #710, #713, #717, #720, and #727: **CLOSED / SUPERSEDED BY #734** — branches retained.
- [PR #700](https://github.com/jleechanorg/dark-factory/pull/700), [#701](https://github.com/jleechanorg/dark-factory/pull/701), [#707](https://github.com/jleechanorg/dark-factory/pull/707), [#708](https://github.com/jleechanorg/dark-factory/pull/708), [#711](https://github.com/jleechanorg/dark-factory/pull/711), [#715](https://github.com/jleechanorg/dark-factory/pull/715), [#718](https://github.com/jleechanorg/dark-factory/pull/718), [#723](https://github.com/jleechanorg/dark-factory/pull/723): **OPEN duplicate generations** of the pipeline fold; do not merge in parallel.
- [PR #287](https://github.com/jleechanorg/dark-factory/pull/287): **MERGED** — selector/drift fix; it is not an open runner-restoration task.

## Learnings and roadmap pointers

- The runner-scope correction is recorded in the user-scope learnings ledger as `feedback_2026-08-24_runner_pool_repo_vs_org_scope.md`.
- [Activity log](activity/2026-08-24.md) records the verified status and this corrected execution order.
- [Roadmap index](README.md) links this day’s activity.

---

## 2026-08-24 (later) — Three-lane funnel analysis + repeatable df-funnel-lanes CLI

### Executive summary

Ran `/factory-funnel` for the last 24h, then 3d, then a 30-day 3-lane breakdown split by intake origin. Converted the analysis into a tested CLI (`runner/funnel_lanes.py` + `bin/df-funnel-lanes`, [PR #744](https://github.com/jleechanorg/dark-factory/pull/744), OPEN). The first implementation incorrectly joined origin and downstream events by `(bead_id, attempt_id)`; the corrected bead-level join finds **2 of 409 beads (0.5%)** reaching `READY_FOR_MERGE` in 30 days. The earlier 0/359 and July-24-last-event claims are superseded, not current findings.

### Evidence snapshot

```text
runner.funnel_report --since 3d
  TASK_DISPATCHED=82, PR_OPENED=58, GATE_ASSESSMENT=68, READY_FOR_MERGE=0
  PARKED_HUMAN_HELD=72 (88% of dispatched), ESCALATION_REQUIRED=42 (51%)

Per-gate breakdown (context.gates, 3d, 68 GATE_ASSESSMENT events):
  all_green: 0 true
  bugbot: pass=68
  ci_green: pass=36, unknown=24, fail=8
  no_conflicts: pass=63, fail=5
  evidence_review: pass=30, fail=35
  skeptic: pass=21, fail=47
  vacuous_red_green: pass=7, unknown=55, fail=6
  coderabbit: pass=4, unknown=64
  comments_resolved: pass=15, fail=53

runner.funnel_lanes --since 30d
  bead_start: 1/142 reach READY_FOR_MERGE
  gh_issue_start: 0/156 reach READY_FOR_MERGE
  pr_adopted_start: 1/111 reach READY_FOR_MERGE
  total: 2/409 (0.5%)

Last READY_FOR_MERGE daemon event: 2026-08-19 (schema-tolerant `timestamp`/`ts` lookup)
```

### Bead and issue index (this section's additions)

| Bead | Bead status | GitHub tracking | Handoff role |
| :--- | :--- | :--- | :--- |
| `jleechan-evtv` | OPEN, P1 | not yet issue-linked | CodeRabbit gate stuck at 94% unknown — previously untracked, likely a top-3 contributor to 0% READY_FOR_MERGE |
| `jleechan-4n2e` | OPEN goal | [PR #744](https://github.com/jleechanorg/dark-factory/pull/744) supplies the measurement tool | Ironclad funnel-improvement target; default FAIL until its seven externally anchored criteria and 48-hour sustain check pass |

`comments_resolved` (78% fail in 3d) is flagged but NOT yet filed as a bead — cross-reference with `jleechan-evtv` before filing; may share root cause (external service polling).

### Executable handoff sequence (this section's addition)

1. **Drive [PR #744](https://github.com/jleechanorg/dark-factory/pull/744) to `/ready` before landing it** — at exact head `b7502224391933d482dfd317a1f05986e2098554`, the focused funnel suite reports 34 passed and 1 skipped, and normal CI is green. Evidence Gate is red; CodeRabbit/Bugbot returned quota-limit notices rather than substantive reviews; and an unresolved current review thread correctly notes that `bin/df-funnel-lanes` is missing from `install.sh`'s chmod/symlink lists. Wire the installer, correct the PR body from the original 0% figures, resolve both threads, and bind fresh evidence/reviews to the new exact head.
2. **Investigate `jleechan-evtv` only after its structural precondition** — current #744 comments prove at least one live CodeRabbit rate-limit event, so distinguish external service availability from daemon polling defects before changing daemon code.
3. **File a bead for `comments_resolved`** (78% fail) once `jleechan-evtv`'s root cause is known — if they share a cause, one fix may resolve both.
4. **Re-run `/factory-funnel --since 3d` and `--since 30d`** after any gate fix lands — target: `all_green=true` starts appearing, `READY_FOR_MERGE > 0`.

### Pull-request state (this section's addition)

- [PR #744](https://github.com/jleechanorg/dark-factory/pull/744): **OPEN / NOT READY** — corrected bead-level lane join; 34 focused tests pass and 1 is skipped; Evidence Gate is red, `df-funnel-lanes` is not installed by `install.sh`, and external-review quota notices are not approvals.

---

## 2026-08-24 (later) — existing-PR dispatch/worktree failure

[Issue #743](https://github.com/jleechanorg/dark-factory/issues/743) tracks a distinct factory defect: drive-existing-PR intake marks work `DISPATCHED` before AO proves that it can safely attach to a branch already owned by another worktree. A failed forced checkout leaves false dispatched state and requires manual redrive.

The four affected WorldArchitect beads had their `factory` labels removed and were parked `HUMAN_HELD`; do not silently re-add routing labels before #743's pre-dispatch ownership regression passes:

| Bead | Existing PR | Existing branch | Live state at audit | Direct-work scope from the parked handoff |
| :--- | :--- | :--- | :--- | :--- |
| `dark-factory-cfla` | #8931 | `worktree_resource_prs` | OPEN / DIRTY / CONFLICTING at `cb0fcbd…` | After #8935, strengthen real streamed rest-choice and provenance evidence |
| `dark-factory-a98p` | #8934 | `fix/8913-normalize-spell-slot-keys` | OPEN / DIRTY / CONFLICTING at `4d023a…` | Canonicalize `/spells` aliases, make collision coalescing depletion-aware, add endpoint tests |
| `dark-factory-vavc` | #8935 | `fix/8911-8914-rest-timestamp-spell-consumption` | OPEN / DIRTY / CONFLICTING at `14d4fe…` | Correct timestamp prompting to canonical structured `world_time`, add regressions |
| `dark-factory-3al0` | #9300 | `feat/dice-roll-34pct-xp` | OPEN / CLEAN / MERGEABLE at `611ce96…` | Apply deterministic XP from authoritative tool results at streaming/persistence boundary; test multi-roll, suppression, idempotency |

These are daemon overlay identifiers, not rows in the local dark-factory Bead store. The parked-handoff scopes and #8935→#8931 ordering come from the product review session; issue #743 proves only the dispatch/worktree-ownership failure.

### Executable sequence

1. Fix #743 in dark-factory with a pre-dispatch branch/worktree ownership check, safe reuse/allocation behavior, structured requeue reason, and an integration test proving no force-checkout or false `DISPATCHED` state.
2. Keep the four beads unlabelled/parked while direct PR owners work in their existing worktrees; no new PR and no merge from factory workers. If work is re-entered through `/af`, that session is tracking/telemetry-only and must not hand-fix product or daemon code.
3. Execute the product fixes in dependency order: #8935 before #8931; #8934 and #9300 are otherwise independent.
4. Re-enable factory routing only after #743 is merged/deployed and a live canary proves an already-owned existing branch can be adopted without branch mutation or stale dispatch state.

### Learnings and roadmap pointers (this section's addition)

- `~/roadmap/learnings-2026-08.md` — new entry `2026-08-24 (later) — Three-lane factory funnel + repeatable CLI`
- `.claude/skills/factory-funnel/SKILL.md` (user-scope) updated with §4 (3-lane methodology) and the "always run 24h AND 3d" mandatory rule

## 2026-08-24 (historical snapshot) — Funnel metrics and draft remediation

### Executive summary

- **Measurement foundation landed:** [PR #744](https://github.com/jleechanorg/dark-factory/pull/744) is **MERGED** at `422e86bc5e2c04df3af23c27ebbece5b2d000c31`. Its corrected bead-level join is the source of truth for lane metrics.
- **Current corrected baseline:** 2 of 412 classified lifecycles reached `READY_FOR_MERGE` in the fresh 30-day window (**0.485%**): `bead_start` 1/142, `gh_issue_start` 0/156, and `pr_adopted_start` 1/114 (**0.877%**).
- **CodeRabbit is not healthy by the ironclad criterion:** fresh 3-day exact-head telemetry is 68 unique PR-heads = 1 direct approval, 3 `waived_unavailable`, 64 unknown (**1.47% direct; FAIL**). Fresh 30-day telemetry is 356 = 37 direct, 12 waived, 291 unknown, 16 fail.
- **Active funnel work:** [PR #748](https://github.com/jleechanorg/dark-factory/pull/748) (honest reporting), [PR #749](https://github.com/jleechanorg/dark-factory/pull/749) (exact-head attribution), and [PR #750](https://github.com/jleechanorg/dark-factory/pull/750) (gate-8 pytest routing) are draft candidates. Focused checks are green, but Evidence Gate is not green and no ironclad criterion is complete.
- **Deprioritized but preserved:** [PR #746](https://github.com/jleechanorg/dark-factory/pull/746) remains a draft with a verified dispatch-race test failure; keep it behind the funnel lanes. [Issue #743](https://github.com/jleechanorg/dark-factory/issues/743) remains open.

### Context

This update supersedes stale status in earlier sections without deleting the historical corrections. It covers the dark-factory repository after the funnel CLI merge and the three parallel remediation drafts. The runner fleet is healthy at organization scope; no runner-restoration work is authorized or needed. This session owns measurement integrity, exact-head review attribution, and gate-8 routing evidence—not merge authorization or product-repository work.

### Bead index

| Bead | Status | Tracking / fallback | Current role |
| :--- | :--- | :--- | :--- |
| [jleechan-4n2e](br%20show%20jleechan-4n2e) | OPEN, P1 goal | `br show jleechan-4n2e` | Ironclad funnel target; all seven criteria remain FAIL or NOT-YET. |
| [jleechan-evtv](br%20show%20jleechan-evtv) | OPEN, P1 | `br show jleechan-evtv` | Root-cause CodeRabbit unknown/waiver telemetry only after structural precondition. |
| [jleechan-6xje](https://github.com/jleechanorg/dark-factory/issues/570) | OPEN, P0 | Issue #570 / existing PR lineage | Gate-8 pytest routing candidate is #750; do not close from focused tests alone. |
| [jleechan-sk55](br%20show%20jleechan-sk55) | OPEN, P0 | `br show jleechan-sk55` | Cross-repo target-worktree path diagnosis; #750 is the current candidate. |

No new bead or GitHub issue was created in this update; the existing four beads were annotated with the corrected evidence. Do not create duplicate tracking rows for #748–#750.

### Work queue

1. **Verify the honest CodeRabbit metric and exact-head binding.** Review [PR #749](https://github.com/jleechanorg/dark-factory/pull/749) against the adapter tests and [PR #748](https://github.com/jleechanorg/dark-factory/pull/748) against the report schema. Acceptance requires direct approval, vendor-unavailable waiver, unknown, fail, and unobserved observations remain distinct; stale or absent review OIDs cannot count as direct approval; existing waiver/is-green/merge semantics remain unchanged. This tracks [jleechan-evtv](br%20show%20jleechan-evtv) and [jleechan-4n2e](br%20show%20jleechan-4n2e). Blocker: Evidence Gate and fresh public evidence are still pending.
2. **Complete gate-8 target-worktree routing.** Review [PR #750](https://github.com/jleechanorg/dark-factory/pull/750) at exact head `c0cadf76f4edde2c2c0ce234ffcba301fa2889d4`. Acceptance requires Cargo precedence for mixed repositories, pytest routing for Python targets, genuine/vacuous regression coverage, exact-head CI, and independent evidence. This tracks [jleechan-6xje](https://github.com/jleechanorg/dark-factory/issues/570) and [jleechan-sk55](br%20show%20jleechan-sk55). Blocker: CI/evidence is not complete.
3. **Re-run the funnel after candidates land.** Use the merged `df-funnel-lanes` CLI for 24h plus 3d operational windows and 30d origin-lane analysis. Independently cross-check every counted `READY_FOR_MERGE` against GitHub merge state and reject admin/bypass merges. Do not call criteria 1–4 complete until the external anchors and independent verifier checks in the ironclad goal pass.
4. **Preserve and defer the dispatch/worktree fix.** Keep [PR #746](https://github.com/jleechanorg/dark-factory/pull/746) open as draft and [Issue #743](https://github.com/jleechanorg/dark-factory/issues/743) tracked, but do not let its current failing verified-state test displace the funnel work. Reassess after #748–#750 produce fresh funnel evidence.
5. **Run the 48-hour sustain check.** Even if criteria 1–4 pass, re-run them at least 48 hours later with a different verifier. Until then the ironclad goal is explicitly **not achieved**.

### PR / merge state

- [PR #734](https://github.com/jleechanorg/dark-factory/pull/734): **MERGED** at `96f8d82055b9530324c861ef57620aedbf99847f`.
- [PR #742](https://github.com/jleechanorg/dark-factory/pull/742): **MERGED** at `61c69efb1a7ee3fa886b076126dd74c90d4ad717`.
- [PR #744](https://github.com/jleechanorg/dark-factory/pull/744): **MERGED** at `422e86bc5e2c04df3af23c27ebbece5b2d000c31`.
- [PR #746](https://github.com/jleechanorg/dark-factory/pull/746): **OPEN / DRAFT** at `f25647a4132705258164e31c4fdec4472a95c0e9`; test check failed on the verified-state race; explicitly deprioritized.
- [PR #748](https://github.com/jleechanorg/dark-factory/pull/748): **OPEN / DRAFT** at `ad29150d5627652430d97a9a237a954899c111a7`; independent approval captured, CI green except Evidence Gate; Gemini/Grok browser approvals captured, public share URLs still pending.
- [PR #749](https://github.com/jleechanorg/dark-factory/pull/749): **OPEN / DRAFT** at `4035ba0dbd4274f43846420abc400f62c2e91377`; independent `APPROVE`, CI green except Evidence Gate.
- [PR #750](https://github.com/jleechanorg/dark-factory/pull/750): **OPEN / DRAFT** at `c0cadf76f4edde2c2c0ce234ffcba301fa2889d4`; independent `APPROVE`, CI running/evidence pending.
- [Issue #743](https://github.com/jleechanorg/dark-factory/issues/743): **OPEN**; factory routing remains removed from the four parked product beads.

### Learnings pointer

- `~/roadmap/learnings-2026-08.md` — appended `2026-08-24 — Corrected funnel baseline and draft remediation state`; records the 2/412 baseline, direct-vs-waived CodeRabbit accounting, and the rule that focused tests do not satisfy external evidence or sustain criteria.
- Claude auto-memory: `project_2026-08-24_funnel_metrics_pr748_750.md`, linked from the project `MEMORY.md`.
- mem0: unavailable because `/home/jleechan/.hermes/scripts/mem0_shared_client.py` is absent; no silent success claim was made.

### Roadmap pointer

- `roadmap/activity/2026-08-24.md` was appended with this funnel refresh and live PR state. The README date link already exists, so `roadmap/README.md` did not require a new entry.

---

## 2026-08-24 (current, verified refresh) — Funnel metrics and PR readiness

### Executive summary

- **Measurement foundation:** [PR #744](https://github.com/jleechanorg/dark-factory/pull/744) is **MERGED** at `422e86bc5e2c04df3af23c27ebbece5b2d000c31`; its corrected bead-level join remains the source of truth.
- **Current funnel baseline:** the fresh 3-day window has **0 `READY_FOR_MERGE`** events; the 30-day window has **2/412 = 0.485%**, with `pr_adopted_start` at **1/114 = 0.877%**. These are operational measurements, not ironclad success criteria.
- **CodeRabbit structural blocker:** current 3-day direct substantive observations are **1/58 = 1.72%**. CodeRabbit is operational, but this account is quota/rate-limited; vendor-unavailable, unknown, fail, and direct approval observations remain separate. Do not reinterpret quota responses as substantive reviews or as a daemon defect.
- **Funnel remediation status:** #748, #749, and #750 are all **OPEN/non-draft** with exact-head evidence and green Evidence Gate checks. None claims a substantive CodeRabbit review; each has independent/browser evidence appropriate to its scope. No ironclad criterion is complete.
- **Preserved blocker:** [Issue #743](https://github.com/jleechanorg/dark-factory/issues/743) remains **OPEN**; factory routing is removed and the four affected beads remain parked. The 48-hour sustain check has not started.

### Work queue

1. **Preserve exact-head reporting and evidence.** #748 is the reporting lane; #749 binds CodeRabbit attribution to the exact head; #750 routes gate 8 through the correct target-worktree backend. Keep direct approval, vendor-unavailable/quota, unknown, fail, and unobserved observations distinct. Evidence Gate is green on all three current heads, but this does not claim a CodeRabbit substantive review.
2. **Treat the CodeRabbit quota/rate-limit as an external blocker.** Verify service/account availability before changing daemon polling. Do not grind on `jleechan-evtv` or recast quota responses as pass. Once the structural precondition clears, re-run the 3-day metric and sample real GitHub reviews independently.
3. **Attack the next internal bottlenecks without weakening gates.** Prioritize `comments_resolved` and skeptic calibration after the quota blocker, then re-run `df-funnel-lanes` on 24h + 3d operational windows and 30d origin lanes. New non-factory P1 bead `jleechan-cqaf` tracks the bounded comments-resolved fix: 56/71 failures (78.9%) are real GraphQL counts, but remediation currently receives only a numeric count; enrich thread bodies/paths/IDs as untrusted feedback, preserve Unknown on GraphQL errors, and add filtering/prompt-propagation tests. New non-factory P1 bead `jleechan-uqgu` tracks skeptic startup: 149/149 fresh 3d/7d caller runs are `startup_failure` with `jobs=[]`, all six pin variables are absent, and this must be fixed/reclassified before calling it a policy failure. Add a CI contract for config-check materialization; absent pins may skip only the real skeptic gate. Cross-check every counted `READY_FOR_MERGE` against non-admin GitHub merge state.
4. **Preserve and defer the dispatch/worktree fix.** Keep [Issue #743](https://github.com/jleechanorg/dark-factory/issues/743) open and its four factory-routed beads parked/unlabelled until a pre-dispatch ownership check and live canary prove safe existing-branch adoption. Do not re-add routing labels from this handoff.
5. **Run the 48-hour sustain check.** Criteria 1–4 must first pass independently; then re-run them at least 48 hours later with a different verifier. Until that happens, the ironclad goal is explicitly **not achieved**.

### Exact PR state (verified 2026-08-25)

- [PR #748](https://github.com/jleechanorg/dark-factory/pull/748): **OPEN / NON-DRAFT / CLEAN** at `155d344ed1de5c64c6df8c9baef34e4b910810b4`; Evidence Gate, test, daemon-tests, and runner-selector checks passed; evidence gist and no-cookie Gemini/Grok shares are in the PR body; 0 unresolved threads.
- [PR #749](https://github.com/jleechanorg/dark-factory/pull/749): **OPEN / NON-DRAFT / CLEAN** at `4035ba0dbd4274f43846420abc400f62c2e91377`; Evidence Gate, test, daemon-tests, and runner-selector checks passed; exact-head evidence gist contains Gemini/Grok browser evidence. This is not a CodeRabbit substantive review.
- [PR #750](https://github.com/jleechanorg/dark-factory/pull/750): **OPEN / NON-DRAFT / CLEAN** at `c0cadf76f4edde2c2c0ce234ffcba301fa2889d4`; Evidence Gate, test, daemon-tests, and runner-selector checks passed; exact-head evidence gist contains Gemini/Grok browser evidence. This is not a CodeRabbit substantive review.
- [PR #746](https://github.com/jleechanorg/dark-factory/pull/746): **OPEN / DRAFT**, preserved and deprioritized behind the funnel lanes.
- [Issue #743](https://github.com/jleechanorg/dark-factory/issues/743): **OPEN**; factory routing remains removed from the four parked product beads.

### Learnings and memory pointers

- `~/roadmap/learnings-2026-08.md` records this corrected baseline, the account-level quota/rate-limit blocker, and the rule that #748–#750 evidence is not CodeRabbit substantive review.
- Claude auto-memory `project_2026-08-24_funnel_metrics_pr748_750.md` records the exact heads/readiness and external quota blocker; `MEMORY.md` points to it.
- mem0 remains unavailable because `/home/jleechan/.hermes/scripts/mem0_shared_client.py` is absent; do not claim a sync.
