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

### P0 — repair the rate-limit circuit breaker (PR #734)

1. Inspect [PR #734](https://github.com/jleechanorg/dark-factory/pull/734), the latest rate-limit generation. It is OPEN and currently UNSTABLE; older generations (#692, #698, #702, #706, #710, #713, #717, #720, #727) are duplicate/obsolete candidates with DIRTY or superseded bases.
2. Fix the reported Rust/API issues (including the `GhCircuitBreaker` constructor/default contract), run the focused daemon tests and clippy, and provide a recognized Evidence Gate verdict.
3. Rebase only the selected branch onto current `origin/main`; do not merge or close duplicates until the selected PR is green and reviewed.

### P1 — land one web-advice/Lane-D superset

1. Use [PR #693](https://github.com/jleechanorg/dark-factory/pull/693) as the current canonical candidate: it is OPEN/CLEAN and contains the web-advice handler, prompt, fail-open integration in all three target pipelines, Lane-D files, tests, and the E2E artifact.
2. Treat [PR #691](https://github.com/jleechanorg/dark-factory/pull/691) as the Lane-D-only subset; do not merge it independently of #693.
3. Treat #700, #701, #707, #708, #711, #715, #718, and #723 as later duplicate generations of the same pipeline fold. Their current states range from CLEAN to UNSTABLE; select one branch, rebase it, and supersede the rest after verification.
4. Before merge, make the canonical E2E/full-pipeline invocation pass `--require-holdouts` and add a regression test for that exact command. Run targeted Python tests, graph audit, Rust tests, and the sealed holdout evaluator where available.

### P2 — land Lane E/F remediation

1. Rebase [PR #694](https://github.com/jleechanorg/dark-factory/pull/694) after the selected P1 branch is established. It is OPEN/CLEAN and covers the runner-warning, anchor-comment, and related hygiene changes.
2. Run its shell/Python tests and documentation checks independently; update the E2E disposition if the canonical P1 branch changes the referenced artifact.

### P3 — tracking hygiene

1. Close or annotate stale [Issue #286](https://github.com/jleechanorg/dark-factory/issues/286) with the organization-vs-repository runner-scope explanation, subject to the normal operator authorization.
2. Keep Bead IDs as Bead IDs; never manufacture `github.com/.../issues/jleechan-*` links.
3. Record future cross-machine documentation synchronization as a process improvement only when independently verified; no Mac-session or SKILL-sync causal claim is part of this handoff.

## Pull-request state

- [PR #740](https://github.com/jleechanorg/dark-factory/pull/740): **MERGED** — this roadmap handoff.
- [PR #714](https://github.com/jleechanorg/dark-factory/pull/714): **MERGED** — prior roadmap synchronization.
- [PR #734](https://github.com/jleechanorg/dark-factory/pull/734): **OPEN / UNSTABLE** — selected rate-limit circuit-breaker generation.
- [PR #693](https://github.com/jleechanorg/dark-factory/pull/693): **OPEN / CLEAN** — selected web-advice/Lane-D superset candidate.
- [PR #691](https://github.com/jleechanorg/dark-factory/pull/691): **OPEN / CLEAN** — Lane-D subset; superseded by #693.
- [PR #694](https://github.com/jleechanorg/dark-factory/pull/694): **OPEN / CLEAN** — Lane E/F remediation.
- [PR #700](https://github.com/jleechanorg/dark-factory/pull/700), [#701](https://github.com/jleechanorg/dark-factory/pull/701), [#707](https://github.com/jleechanorg/dark-factory/pull/707), [#708](https://github.com/jleechanorg/dark-factory/pull/708), [#711](https://github.com/jleechanorg/dark-factory/pull/711), [#715](https://github.com/jleechanorg/dark-factory/pull/715), [#718](https://github.com/jleechanorg/dark-factory/pull/718), [#723](https://github.com/jleechanorg/dark-factory/pull/723): **OPEN duplicate generations** of the pipeline fold; do not merge in parallel.
- [PR #287](https://github.com/jleechanorg/dark-factory/pull/287): **MERGED** — selector/drift fix; it is not an open runner-restoration task.

## Learnings and roadmap pointers

- The runner-scope correction is recorded in the user-scope learnings ledger as `feedback_2026-08-24_runner_pool_repo_vs_org_scope.md`.
- [Activity log](activity/2026-08-24.md) records the verified status and this corrected execution order.
- [Roadmap index](README.md) links this day’s activity.
