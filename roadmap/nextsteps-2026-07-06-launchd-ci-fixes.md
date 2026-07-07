# Nextsteps — dark-factory launchd + CI fixes — 2026-07-06

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **Outcomes**: Delivered the readonly review of 4 PRs (#168-#171) with 17+ findings, then executed the user's "do it all" directive. All 4 original PRs closed (none merged). Two superseding PRs opened: [PR #172](https://github.com/jleechanorg/dark-factory/pull/172) (launchd installer relocation + 7 bug fixes) and [PR #173](https://github.com/jleechanorg/dark-factory/pull/173) (CI bash-test wiring + callpath probe vendoring + daemon-tests `main` ref fetch).
- **Risks / blockers**: Both new PRs sit unmerged. The 4 P1 follow-up beads (`jleechan-2r1k`, `jleechan-q47c`, `jleechan-81wa`, `jleechan-7wud`) describe the same root cause — `0368a93e5` re-introduced direct sqlite UPDATE and bypasses the 4-guard safety net in `factory-overlay.sh:dispatch-record`. None of those 4 beads is addressed by PR #172/#173 and they remain in the auto-factory stack's P1 queue.
- **Next**: admin-merge PR #172 (installers + wrappers; pure infra) and PR #173 (CI gate; needs CI green before merge). Then sequence a follow-up PR against `jleechan-2r1k` that re-introduces the dispatch-record path with structured error codes (resolves all 4 P1 follow-ups at once).
- **Beads touched this run**: 9 created, 9 closed. Net delta: 0.

## Context

This session reviewed and superseded 4 dark-factory PRs:

1. [#168](https://github.com/jleechanorg/dark-factory/pull/168) `feat(daemon/launchd): af-tick poll-loop plist template + installer` — landed with 7 critical bugs.
2. [#169](https://github.com/jleechanorg/dark-factory/pull/169) `docs(daemon/verify): post-merge smoke-test evidence for PR #7888` — evidence file pinned to base `10dc5b16a`, 1 commit behind current `0368a93e5`.
3. [#170](https://github.com/jleechanorg/dark-factory/pull/170) `feat(callpath): overlay-harness layer probes factory-overlay.sh subcommands` — test depended on user-scope `~/.claude/skills/callpath/profiles/dark-factory/run.sh`.
4. [#171](https://github.com/jleechanorg/dark-factory/pull/171) `feat(daemon/tick): route-record + dispatch-record for QUEUED beads` — superseded by `0368a93e5` 35 min after the PR opened; reintroduced the direct sqlite UPDATE that PR #171 was removing.

The user's directives progressed through three phases: (a) "review this work readonly" → (b) "move installer to repo root and fix an issue if a bug reintroduced" → (c) "do it all". All three executed; PR #172 (installer) and PR #173 (CI) ready for review.

## Bead index

| Bead | Title | Priority | Status | Link |
|------|-------|----------|--------|------|
| [jleechan-2r1k](https://github.com/jleechanorg/dark-factory/issues/2r1k) | fix(daemon): 0368a93e5 reintroduced 4 silent failure modes from removed direct UPDATE | P1 | OPEN | [jleechan-2r1k](https://github.com/jleechanorg/dark-factory/issues/2r1k) |
| [jleechan-q47c](https://github.com/jleechanorg/dark-factory/issues/q47c) | fix(daemon/tick): factory-af-tick error routing uses stderr keyword matching (ZFC violation) | P1 | OPEN | [jleechan-q47c](https://github.com/jleechanorg/dark-factory/issues/q47c) |
| [jleechan-81wa](https://github.com/jleechanorg/dark-factory/issues/81wa) | fix(daemon/tick): factory-af-tick hardcodes specific bead IDs (ZFC + spec violation) | P1 | OPEN | [jleechan-81wa](https://github.com/jleechanorg/dark-factory/issues/81wa) |
| [jleechan-7wud](https://github.com/jleechanorg/dark-factory/issues/139) | [daemon/router] ZFC: replace verdict enum with LLM-classified tier | P1 | OPEN | [jleechan-7wud](https://github.com/jleechanorg/dark-factory/issues/139) |

**Closed this session** (work landed via PR #172/#173 or superseded the original PR):
- `jleechan-gv9u` — launchd installer critical bugs (fixed by #172)
- `jleechan-q2wu` — test_cli_vcs_real_git flake (fixed by #173)
- `jleechan-50jf` — bash tests not in CI (fixed by #173)
- `jleechan-8xxl` — callpath user-scope dep (fixed by #173)
- `jleechan-y869` — close PR #171 (done)
- `jleechan-lf26` — PR #169 stale base (closed as superseded)
- `jleechan-df94` — callpath overlay-harness (satisfied by #173)
- `jleechan-57h0` — launchd plist for af-tick (satisfied by #172)
- `jleechan-xzsh` — factory-af-tick refactor (superseded by `0368a93e5`)

## Work queue

1. **Admin-merge PR [#172](https://github.com/jleechanorg/dark-factory/pull/172)** — single PR can land independently; pure installer + wrapper infra, no auto-factory logic change. Pre-merge verification: dry-run install on dev machine; confirm plutil-lint OK; chmod 0644 enforced. → tracks bead jleechan-57h0.
2. **Wait for PR [#173](https://github.com/jleechanorg/dark-factory/pull/173) CI to pass**, then admin-merge. This PR adds the bash-test gate and the `main` ref fetch step. If CI fails on the new bash-test step, expect `test_factory_overlay.sh` (30/30) and `test_callpath_overlay_harness.sh` (3/3) to pass locally — failures likely come from missing tooling in the GHA runner (sqlite3, br, gh). → tracks beads jleechan-50jf, jleechan-q2wu, jleechan-8xxl.
3. **Sequence follow-up PR against [jleechan-2r1k](https://github.com/jleechanorg/dark-factory/issues/2r1k) that re-introduces the dispatch-record path with structured exit codes** — factory-overlay.sh:dispatch-record should exit `2` (over capacity), `3` (branch conflict), `4` (require_state), `5` (valid_branch), `6` (valid_bead_id). Then dispatcher cases on `$rc` instead of stderr substring matching. This single PR resolves all 4 P1 follow-ups: `jleechan-2r1k` (silent failure modes), `jleechan-q47c` (ZFC stderr keyword routing), `jleechan-81wa` (hardcoded bead IDs in SQL CASE — replace with `dispatch_priority` column), and `jleechan-7wud` (related verdict-enum ZFC work).
4. **Optional: update user-scope `~/.claude/skills/callpath/profiles/dark-factory/run.sh`** to delegate `overlay_harness_check` to the new vendored `bin/overlay-harness-check.sh`. Keeps user-scope profile and repo probe in sync. Not blocking.

## PR / merge state

- https://github.com/jleechanorg/dark-factory/pull/168 — CLOSED (superseded by #172)
- https://github.com/jleechanorg/dark-factory/pull/169 — CLOSED (superseded by #173)
- https://github.com/jleechanorg/dark-factory/pull/170 — CLOSED (superseded by #173)
- https://github.com/jleechanorg/dark-factory/pull/171 — CLOSED (superseded by `0368a93e5`)
- https://github.com/jleechanorg/dark-factory/pull/172 — OPEN (ready to admin-merge)
- https://github.com/jleechanorg/dark-factory/pull/173 — OPEN (ready to admin-merge after CI green)

## Learnings pointer

- `~/roadmap/learnings-2026-07.md` — appended entry "2026-07-06 (evening) — Launchd installer relocation + CI bash-tests wiring + callpath probe vendoring" covering the 7 critical installer bugs found by code review, the `${!i}` indirect-expansion arg-parser bug, and the 3 CI infrastructure bugs (bash tests not run, daemon-tests `main` ref missing, callpath probe out-of-repo).

## Roadmap pointer

- Appended `roadmap/activity/2026-07-06.md` with the "Evening — Launchd installer relocation + CI bash-tests + callpath probe vendoring" bullet. Same date as the existing `auto-factory alignment and bug fixes` and `Late evening — PR #8189 GHA fallback` bullets, so no `roadmap/README.md` date link added (single-day consolidation).