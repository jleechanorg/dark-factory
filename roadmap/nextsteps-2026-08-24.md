# Next steps — dark-factory — 2026-08-24

## Table of contents

- [Executive summary](#executive-summary)
- [Evidence snapshot](#evidence-snapshot)
- [Bead and issue index](#bead-and-issue-index)
- [Executable handoff sequence](#executable-handoff-sequence)
- [Pull-request state](#pull-request-state)
- [Learnings and roadmap pointers](#learnings-and-roadmap-pointers)

## Executive summary

The earlier “runner pool offline” conclusion was a scope error, not an infrastructure outage. The repository-scoped Actions endpoint returns zero because this project’s self-hosted runners are registered at the organization level. The organization endpoint currently reports 14 matching online runners (1 busy), and the repository has no queued or in-progress workflow runs.

The handoff therefore has no runner-restoration blocker. The next execution unit is the open rate-limit circuit-breaker PR (#734), followed by one canonical web-advice/Lane-D implementation, then the independent Lane E/F remediation. Duplicate PR generations must not be merged in parallel.

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
| `jleechan-xn4n` | **CLOSED** — restoration was not needed | [Issue #286](https://github.com/jleechanorg/dark-factory/issues/286) remains OPEN and stale; [PR #287](https://github.com/jleechanorg/dark-factory/pull/287) is MERGED | P3 cleanup; do not restore runners |
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

1. Close or annotate stale [Issue #286](https://github.com/jleechanorg/dark-factory/issues/286) with the organization-vs-repository runner-scope explanation, subject to the normal operator authorization.
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
