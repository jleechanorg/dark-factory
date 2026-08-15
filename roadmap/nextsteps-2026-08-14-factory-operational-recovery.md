# Nextsteps — dark-factory operational recovery — 2026-08-14

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- The prior observation window contained 57,522 records: 9 `PR_OPENED`, 21 gate assessments, 44 failures, 20 human-held events, and 0 newly `READY` events. The dominant 52,816 `SKIPPED_DUPLICATE` count is mostly an idempotent, GitHub-free fast path; it is not, by itself, wasted dispatch.
- A seven-hour no-transition lull is not proven scheduler starvation. That claim requires telemetry showing `ready > 0` while `dispatched = 0`; the observed window did not establish that condition.
- Proven recovery blockers are malformed duplicate ownership data ([jleechan-r28r](br show jleechan-r28r), P0), worktree/session/snapshot lifecycle ([jleechan-jw4c](br show jleechan-jw4c), P0), ER comment secondary-rate limits ([jleechan-icn7](br show jleechan-icn7), P1), and readiness observability ([jleechan-wf5q](br show jleechan-wf5q), P1). Notifier/reviewer fallbacks remain operationally relevant ([jleechan-zhh5](br show jleechan-zhh5), [jleechan-984e](br show jleechan-984e)); GraphQL hot-path work ([jleechan-48ou](br show jleechan-48ou)) is conditional on cohort proof.
- The completed AO bead [jleechan-9fh2](br show jleechan-9fh2) was reconciled and **closed** against merged PR 632. Continue with ownership/coalescing; lifecycle/reaper consolidation; transport backoff and notifier/reviewer fallbacks; readiness telemetry before assessing GraphQL; then a 12-hour canary with explicit cohort metrics.
- Mac policy sync is complete: `/home/jleechan/.claude/CLAUDE.md` and `/home/jleechan/.codex/AGENTS.md` match Mac. Backups are `/home/jleechan/.claude/CLAUDE.md.backup-20260815T004526Z` and `/home/jleechan/.codex/AGENTS.md.backup-20260815T004526Z`.

## Context

This handoff closes the 2026-08-14 dark-factory audit and records the recovery sequence for the next operating block. The scope is the factory intake/dispatch, worktree/session lifecycle, gate transport, readiness telemetry, and canary evidence; it does not authorize product-code changes in this repository. The prior window's counts are observational facts, while causal claims below are classified by the proof available. Duplicate branch coalescing ([jleechan-jur5](br show jleechan-jur5)) is a distinct repair from idempotent duplicate-skip logging and must not be collapsed into the same metric.

The local Mac policy synchronization was completed as a separate operational close-out. The two policy targets are `/home/jleechan/.claude/CLAUDE.md` and `/home/jleechan/.codex/AGENTS.md`; byte-level comparison against the Mac source is complete, and the pre-sync backups are retained at `/home/jleechan/.claude/CLAUDE.md.backup-20260815T004526Z` and `/home/jleechan/.codex/AGENTS.md.backup-20260815T004526Z`.

### Root-cause components and proof taxonomy

| Component | Backend behavior | Proof state | Evidence | Verdict |
|---|---|---|---|---|
| Duplicate identity persistence / malformed `external_ref` | Intake persists URL-shaped or duplicate ownership references, causing parser failures and ambiguous PR ownership. | **server-owned invariant** | Reproduced malformed-reference failures and duplicate PR pairs under [jleechan-r28r](br show jleechan-r28r). | Fix canonical identity persistence and per-pair ownership before dispatch tuning. |
| Duplicate branch adoption | Adoption refuses branch/PR collisions and parks `HUMAN_HELD`; coalescing is distinct from skip suppression. | **server-owned invariant** | Collision behavior is tracked by [jleechan-jur5](br show jleechan-jur5); no evidence that every skip dispatched work. | Coalesce active ownership; preserve separate idempotent skip accounting. |
| Worktree/session/snapshot ownership | Stale locks, nested worktrees, and snapshot/session mismatches block coder spawns or blur checkout ownership. | **server-owned invariant** | Lifecycle failures and isolation risk tracked by [jleechan-jw4c](br show jleechan-jw4c), with overlapping y189/gk2r/lght work. | One lifecycle/reaper authority; consolidate overlapping remediation. |
| `SKIPPED_DUPLICATE` suppression | Server short-circuits already-seen intake without a GitHub dispatch call. | **server-owned idempotency invariant (KEEP)** | 52,816 records are predominantly the gh-free fast path; count alone is not wasted dispatch. | Keep suppression and measure it separately from adoption collisions. |
| ER comment transport / rate-limit backoff | Bursty `addComment` calls can receive “submitted too quickly” secondary-limit errors. | **server-owned invariant** | Repeated transient failures tracked by [jleechan-icn7](br show jleechan-icn7). | Add jittered delay, bounded retry, and explicit transport classification. |
| Notifier and reviewer fallbacks | Escalations or cross-model reviews can become undeliverable when configured vendors/endpoints are unavailable. | **unproven fallback** | Existing risks are tracked by [jleechan-zhh5](br show jleechan-zhh5) and [jleechan-984e](br show jleechan-984e); verify current gate evidence. | Add delivery/reviewer canaries; do not infer a new outage from parked counts alone. |
| Readiness contract / telemetry | `/af`, shell tick, and Rust overlay expose overlapping READY semantics without aligned cohort counters. | **server-owned invariant** | Contract mismatch and missing `ready > 0` / `dispatched = 0` proof tracked by [jleechan-wf5q](br show jleechan-wf5q). | Instrument first; use telemetry to classify starvation rather than patching a duration symptom. |
| GraphQL-caused lull | `pr_number_for_branch` may drain GraphQL, but aggregate lull duration does not establish causality. | **unproven fallback** | [jleechan-48ou](br show jleechan-48ou) requires controlled cohort proof after readiness telemetry; rate-limit-aware transport remains server-owned. | Assess only with quota/cohort deltas; do not claim GraphQL root cause yet. |
| Seven-hour no-transition claim | A long no-transition interval without simultaneous `ready > 0` and `dispatched = 0` is observationally ambiguous. | **unproven fallback** | No qualifying readiness telemetry in the prior window. | Add instrumentation, not a blind scheduler fix or requeue. |

## Bead index

The beads below were touched by this audit or are required by the recovery queue. Where no GitHub issue URL is present, the link intentionally records the executable `br show <id>` fallback so the next operator can resolve the authoritative local bead.

| Bead | Title / role | Priority and status | Link |
|---|---|---|---|
| [jleechan-9fh2](br show jleechan-9fh2) | AO backend verification reconciled against merged PR 632 | P2, **CLOSED** (merged PR 632) | `br show jleechan-9fh2` |
| [jleechan-r28r](br show jleechan-r28r) | Repair malformed duplicate `external_ref` and ownership | P0, open | `br show jleechan-r28r` |
| [jleechan-jur5](br show jleechan-jur5) | Coalesce duplicate branch/PR adoption | P1, open | `br show jleechan-jur5` |
| [jleechan-jw4c](br show jleechan-jw4c) | Isolate worktrees and add lifecycle reaper | P0, open | `br show jleechan-jw4c` |
| [jleechan-y189](br show jleechan-y189) | Self-healing worktree checkout | P0, open; overlapping scope to consolidate | `br show jleechan-y189` |
| [jleechan-gk2r](br show jleechan-gk2r) | Recover locks and provision fresh worktrees | P1, open; overlapping scope to consolidate | `br show jleechan-gk2r` |
| [jleechan-lght](br show jleechan-lght) | Fresh worktree fallback on remediation failure | P2, open; overlapping scope to consolidate | `br show jleechan-lght` |
| [jleechan-icn7](br show jleechan-icn7) | ER comment jitter and transport backoff | P1, open | `br show jleechan-icn7` |
| [jleechan-zhh5](br show jleechan-zhh5) | Working notifier and escalation-delivery canary | P1, open | `br show jleechan-zhh5` |
| [jleechan-984e](br show jleechan-984e) | Reviewer/vendor fallback and degraded-review signal | P1, open | `br show jleechan-984e` |
| [jleechan-wf5q](br show jleechan-wf5q) | Reconcile readiness contracts and telemetry | P1, open | `br show jleechan-wf5q` |
| [jleechan-48ou](br show jleechan-48ou) | GraphQL hot-path reduction | P1, open; cohort-proof gated | `br show jleechan-48ou` |

## Work queue

1. **Completed checkpoint — AO bead reconciled and closed.** [jleechan-9fh2](br show jleechan-9fh2) is reconciled to merged PR [#632](https://github.com/jleechanorg/dark-factory/pull/632), with the AO opt-in acceptance record retained. Do not reopen it or create another implementation PR for the same completed work.
2. **Repair data ownership before dispatch tuning.** For [jleechan-r28r](br show jleechan-r28r), normalize malformed `external_ref` values, identify a canonical bead for each duplicate PR/branch pair, and record the losing-to-winning ownership mapping. Then implement [jleechan-jur5](br show jleechan-jur5) so future branch collisions coalesce onto the active bead. Acceptance requires no parser thrash, no ambiguous owner, and a regression test distinguishing coalescing from idempotent `SKIPPED_DUPLICATE`.
3. **Implement lifecycle recovery and consolidate overlapping work.** Use [jleechan-jw4c](br show jleechan-jw4c) as the single lifecycle/reaper authority. Fold the compatible requirements from [jleechan-y189](br show jleechan-y189), [jleechan-gk2r](br show jleechan-gk2r), and [jleechan-lght](br show jleechan-lght) into that plan, close or supersede overlapping PRs, and do not run parallel duplicate remediation PRs. Acceptance requires stale-lock cleanup, active-session protection, fresh isolated fallback, snapshot cleanup, and tests for dirty, locked, detached, missing, and active-session cases.
4. **Add transport backoff, then restore notification/reviewer fallbacks.** Land [jleechan-icn7](br show jleechan-icn7) first with jittered ER comment delay and retry classification. After transport is stable, implement [jleechan-zhh5](br show jleechan-zhh5)'s working notifier plus startup/delivery canary, and verify [jleechan-984e](br show jleechan-984e)'s reviewer/vendor fallback with an explicit degraded-review outcome. Acceptance requires no burst-induced comment failures and a human-visible escalation path.
5. **Instrument readiness before judging GraphQL.** Implement [jleechan-wf5q](br show jleechan-wf5q) so `/af`, shell tick, and Rust overlay emit aligned `ready`, `routed`, `dispatched`, and reason/cohort counters. Only after this telemetry is live, assess [jleechan-48ou](br show jleechan-48ou) using a controlled cohort (same bead class, branch state, and quota window). Do not claim GraphQL causality from aggregate lull duration alone.
6. **Run a 12-hour canary with explicit cohort metrics.** Exercise a bounded cohort after items 1–5: record eligible/ready/routed/dispatched counts, duplicate-skip and coalescing counts, worktree/session recovery outcomes, gate failures by transport/vendor, notifier delivery, GraphQL and REST quota deltas, transition latency, and human-held reasons. Define pass/fail thresholds before start; retain raw telemetry and a final reconciliation that distinguishes idempotent skips from refused adoption and proven starvation.

## PR / merge state

Same-session PR truth for references in this handoff:

- [PR #632](https://github.com/jleechanorg/dark-factory/pull/632): **MERGED**.
- [PR #633](https://github.com/jleechanorg/dark-factory/pull/633): **CLOSED** (unmerged).
- [PR #634](https://github.com/jleechanorg/dark-factory/pull/634): **CLOSED** (unmerged).
- [PR #635](https://github.com/jleechanorg/dark-factory/pull/635): **CLOSED** (unmerged).
- [PR #636](https://github.com/jleechanorg/dark-factory/pull/636): **CLOSED** (unmerged).

PRs 633–636 are not merge evidence and must not be treated as landed fixes. In particular, overlapping lifecycle branches for y189/gk2r/lght should be consolidated into the single [jleechan-jw4c](br show jleechan-jw4c) recovery authority before any new PR is opened.

## Learnings pointer

- [`/home/jleechan/roadmap/learnings-2026-08.md`](file:///home/jleechan/roadmap/learnings-2026-08.md) — entry `2026-08-14 — Separate noisy idempotency from proven factory blockers` records the causal correction, with beads r28r, jw4c, icn7, and wf5q and this handoff as the execution pointer.
- Claude auto-memory: [`feedback_2026-08-14_factory_audit_causal_guardrails.md`](file:///home/jleechan/.claude/projects/-home-jleechan-projects-dark-factory/memory/feedback_2026-08-14_factory_audit_causal_guardrails.md) records the durable feedback rule and points back here.

## Roadmap pointer

- Appended [`roadmap/activity/2026-08-14.md`](file:///home/jleechan/projects/dark-factory/roadmap/activity/2026-08-14.md) with the bead reconciliation, factory recovery queue, and completed Mac policy sync.
- Added the new date link to [`roadmap/README.md`](file:///home/jleechan/projects/dark-factory/roadmap/README.md) under **Recent activity (by day)**.
