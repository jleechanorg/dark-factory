# Nextsteps — auto-factory ironclad drive to /green + /er + /code-standards — 2026-07-05

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **The agentf push guard is fixed.** The over-firing `agent-f([^a-z]|$)` pattern (which false-positived on path mentions like `~/.claude-agent-f`) was DROPPED 2026-07-05. The new pattern set is `agnt-f|agf-|jleechan-af|#agentf|agentf-` (5 patterns, all hyphenated tokens; no path-mention false positives). Verified by positive tests (agf-, Agnt-F, jleechan-af, #agentf all match) + negative test (`~/.claude-agent-f` no longer matches). Live `git push wa HEAD:refs/heads/test-agf-hook-verify` succeeded after the fix.
- **ZFC and Auto-Factory Audit completed.** Audited `/f` auto-routing and the Rust auto-factory daemon implementation. Identified 3 routing constraints in `/f` auto-routing and 5 gaps between the Rust codebase and the `docs/auto-factory-daemon-spec.md` spec (lack of SCM ETag cache, lack of offline local bead protocol, naive security redaction, simple single-threaded loop pacing, and lack of dynamic analysis/spec routing). Filed 4 new P2 task beads to track these improvements.
- **7 ironclad candidate PRs identified**, all `mergeable=MERGEABLE` as of this run:
  - dark-factory: PR #161 (qw5 followups, head `471a3657`); PR #133 (parallel-reviewer base, head `98417c8`, review=CHANGES_REQUESTED — needs CR re-trigger)
  - worldai: PR #8058 (quota banner, head `713d7b71`), PR #8116 (XP delta additive, head `cd6dae07`), PR #8064 (user-directive supremacy, head `c011a658`), PR #8060 (rewards box, head `7ac22a9f`), PR #8061 (Nocturna PR-green-drive docs, head `c8998bf5`)
  - Per `roadmap/2026-07-05-auto-factory-exit-criteria.md` (17.7 KB) the ironclad binding for each: 7-green PRODUCTION (5 PRs) or 4-green NON_PRODUCTION (1 doc PR), bead-closed, telemetry-persisted, /er PASS
- The auto-factory must drive each of these through 7-green + /er + /code-standards (per the two-tier rule in `~/.claude/commands/green.md`). The in-between stuff (rebasing, fixing inline comments, running code-standards) is OK to validate; the ironclad exit is `bead.status=closed ∧ PR.merged=true ∧ telemetry-persisted`.
- 3 of the 6 gap beads have landed since 2026-07-04: `jleechan-577t` (/er Gate integration, commit `62f8f3d3`) + `jleechan-vlqi` (autonomy/wedge tracker, commit `bae9374c`) + `jleechan-2uoy` (gates-compute subcommand, commit `bb0d7f22` "Implement gates-compute"). 3 still open: `jleechan-732a` (production adapters), `jleechan-2ka` (Stage 2 re-roll), `jleechan-xrdx` (decommission).
- Decommission time-box 2026-07-11 still 6 days out; escalate on miss per `~/roadmap/nextsteps-2026-07-03-auto-factory-bootstrap.md` ironclad.

## Context

Session 2026-07-05 (this run). The user asked: (1) fix the agentf hook, (2) use /nextsteps for exit criteria, (3) focus on driving the ironclad candidate PRs to /green + /er + /code-standards. Separately, the user requested running /nextsteps and auditing the auto-factory daemon implementation and the original spec to identify areas of improvement. We created 4 new beads (`jleechan-q9ze`, `jleechan-iclg`, `jleechan-gfn6`, `jleechan-e28q`) to track these improvements.

## Bead index

| Bead | Title | Priority | Link |
|------|-------|----------|------|
| jleechan-732a | Production Adapters | P1 | dark-factory PR #161 commit 0ea5f229 |
| jleechan-577t | /er Gate integration | P1 | merged commit 62f8f3d3 |
| jleechan-vlqi | Autonomy / wedge tracker | P1 | merged commit bae9374c |
| jleechan-2uoy | gates-compute subcommand | P1 | merged commit bb0d7f22 |
| jleechan-2ka | Stage 2 Re-Roll Engine | P2 | Stage 1 substitution rule active |
| jleechan-xrdx | Decommissioning Legacy Loop | P2 | time-box 2026-07-11 |
| jleechan-q9ze | [daemon] Implement GitHub API ETag caching | P2 | br show jleechan-q9ze |
| jleechan-iclg | [daemon] Implement offline bead local fallback | P2 | br show jleechan-iclg |
| jleechan-gfn6 | [daemon] Add generic task/readonly routing in router | P2 | br show jleechan-gfn6 |
| jleechan-e28q | [daemon] Harden security redaction logic in constraints | P2 | br show jleechan-e28q |

## Work queue

1. Drive PR #133 to /green (parallel-reviewer base). Current: mergeable=MERGEABLE but review=CHANGES_REQUESTED. Action: rebase onto main, then post @coderabbitai all good? after addressing the CHANGES_REQUESTED comments.
2. Drive PR #161 to /green (qw5 followups). Current: mergeable=MERGEABLE. Action: post /er, confirm all workflows green, then mark bead-closed.
3. Drive PR #8058 to /green (quota banner, worldai). Current: mergeable=MERGEABLE.
4. Drive PR #8116 to /green (XP delta additive, worldai). Current: mergeable=MERGEABLE.
5. Drive PR #8064 to /green (user-directive supremacy, worldai). Current: mergeable=MERGEABLE.
6. Drive PR #8060 to /green (rewards box, worldai). Current: mergeable=MERGEABLE.
7. Drive PR #8061 to /green (4-green NON_PRODUCTION tier, Nocturna PR-green-drive docs, worldai). Current: mergeable=MERGEABLE.
8. Re-test the agentf hook with a real merge of the worldai candidates. The fix means future force-pushes won't be blocked by the over-firing pattern.
9. Implement GitHub API ETag caching in daemon SCM adapter (`jleechan-q9ze`) to prevent GH Actions rate limits.
10. Implement offline local bead fail-safe fallback protocol (`jleechan-iclg`) when GitHub is down.
11. Update Rust Router to support generic tasks and read-only research pipeline routing (`jleechan-gfn6`).
12. Harden security redaction parsing in constraint extractor (`jleechan-e28q`).

## PR / merge state

- PR #161 (dark-factory): OPEN, mergeable=MERGEABLE, head=471a3657
- PR #133 (dark-factory): OPEN, mergeable=MERGEABLE, head=98417c8, review=CHANGES_REQUESTED
- PR #8058 (worldai): OPEN, mergeable=MERGEABLE, head=713d7b71
- PR #8116 (worldai): OPEN, mergeable=MERGEABLE, head=cd6dae07
- PR #8064 (worldai): OPEN, mergeable=MERGEABLE, head=c011a658
- PR #8060 (worldai): OPEN, mergeable=MERGEABLE, head=7ac22a9f
- PR #8061 (worldai): OPEN, mergeable=MERGEABLE, head=c8998bf5

## Learnings pointer

- `~/roadmap/learnings-2026-07.md` — section `2026-07-05 — ZFC and Auto-Factory Audit`

## Roadmap pointer

- Appended `roadmap/activity/2026-07-05.md` — Recent activity (per-day file)
