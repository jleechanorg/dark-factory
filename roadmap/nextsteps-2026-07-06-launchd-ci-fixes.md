# Nextsteps — dark-factory factory merge + launchd — 2026-07-06

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **Outcomes:** Landed factory stack on `origin/main` (`72b5087c1`): PRs #168–#171 code via local merge commits (launchd plist, callpath harness, route-record/dispatch-record tick, smoke evidence); CI `main` ref fetch fix; launchd agent **`ai.dark-factory.af-tick`** installed locally; script tests green (21/21 af-tick, callpath harness PASS, cargo test ok).
- **Risks / blockers:** Launchd tick **exit code 1** — `FileNotFoundError: 'br'` in `af-tick.err.log` (bare launchd PATH; fix is in open [PR #172](https://github.com/jleechanorg/dark-factory/pull/172)). [PR #172](https://github.com/jleechanorg/dark-factory/pull/172) and [PR #173](https://github.com/jleechanorg/dark-factory/pull/173) are **CONFLICTING** and need rebase onto current `main`. ZFC structured dispatch fix lives on branch `fix/zfc-structured-dispatch-codes` (`a7ce4b0f0`) — **not on `main` yet**. Linux CI clippy still red (12 warnings).
- **Next (sequencing):** (1) Rebase + merge [PR #172](https://github.com/jleechanorg/dark-factory/pull/172) → relaunch launchd with wrapper; (2) rebase + merge [PR #173](https://github.com/jleechanorg/dark-factory/pull/173); (3) land `fix/zfc-structured-dispatch-codes`; (4) drive worldai PRs [#7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888), [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060), [#8189](https://github.com/jleechanorg/worldarchitect.ai/pull/8189) via `/af` tick.
- **Beads:** jleechan-v2wv (#172 rebase), jleechan-ebe1 (#173 rebase), jleechan-t4m8 (structured dispatch); drive beads jleechan-93ft, jleechan-9byt.4, jleechan-7re5.

## Context

This block wrapped the `/green` merge session for four factory PRs and installed the poll-loop launchd agent. GitHub shows PRs [#168](https://github.com/jleechanorg/dark-factory/pull/168)–[#171](https://github.com/jleechanorg/dark-factory/pull/171) as **CLOSED** with `mergedAt=null`, but their commit SHAs are on `main` via direct local merge + push (`dd7983258`, `966de37c5`, `8a2e94855`, `4fa649440`). Superseding PRs [#172](https://github.com/jleechanorg/dark-factory/pull/172) and [#173](https://github.com/jleechanorg/dark-factory/pull/173) were opened earlier in the evening to fix installer bugs and CI gaps; they now conflict with the landed stack and must be rebased.

Launchd was installed with `daemon/launchd/install-launchagents.sh` (240s interval). The agent runs but fails each tick because `factory-af-tick.sh` invokes `br` and launchd's PATH does not include Homebrew or user bin dirs. PR #172's `launchd-wrapper.sh` is the intended fix.

Repo: `jleechanorg/dark-factory`, branch `main`, HEAD `72b5087c1`.

## Bead index

| Bead | Title | Priority | Status |
|------|-------|----------|--------|
| jleechan-v2wv | Rebase and merge PR #172 (launchd wrapper fixes af-tick PATH) | P1 | OPEN |
| jleechan-ebe1 | Rebase and merge PR #173 (CI bash-tests + daemon main ref fetch) | P1 | OPEN |
| jleechan-t4m8 | Land fix/zfc-structured-dispatch-codes on main | P1 | OPEN |
| jleechan-7wud | ZFC: replace verdict enum with LLM-classified tier | P1 | OPEN |
| jleechan-8dyu | Post-merge smoke + first real dispatch on PR #7888 | P1 | OPEN |
| jleechan-93ft | Drive PR #7888 to /green | P1 | OPEN |
| jleechan-9byt.4 | Drive PR #8060 to 7-green | P1 | OPEN |
| jleechan-7re5 | Fix GHA ensurepip fallback (PR #8189) | P1 | OPEN |
| jleechan-oale | auto-factory skill: liveness gate vs factory-lite restart | P2 | OPEN |

**Closed this run:** jleechan-38w8 (intake script on main), jleechan-a5p (superseded by launchd on main).

**Previously closed (reference):** jleechan-2r1k, jleechan-q47c, jleechan-81wa fixed on branch `fix/zfc-structured-dispatch-codes` pending merge; jleechan-57h0, jleechan-df94, jleechan-xzsh satisfied by #168–#171 stack.

## Work queue

1. **Rebase and merge [PR #172](https://github.com/jleechanorg/dark-factory/pull/172)** — tracks jleechan-v2wv. Goal: `launchd-wrapper.sh` sources `~/.bash_profile` so `br`, `gh`, `sqlite3` resolve under launchd. Acceptance: `launchctl kickstart -k gui/$(id -u)/ai.dark-factory.af-tick`; no `FileNotFoundError: br` in `af-tick.err.log`. Blocker: CONFLICTING with `main`.

2. **Rebase and merge [PR #173](https://github.com/jleechanorg/dark-factory/pull/173)** — tracks jleechan-ebe1. Goal: wire `tests/scripts/test_*.sh` into CI; vendored callpath probe; daemon-tests main ref fetch. Depends on conflict resolution with #172.

3. **Land `fix/zfc-structured-dispatch-codes` on `main`** — tracks jleechan-t4m8. Structured exit codes on dispatch-record; resolves jleechan-2r1k/q47c/81wa on main.

4. **Drive canonical PRs via `/af` tick** — tracks jleechan-8dyu, jleechan-93ft, jleechan-9byt.4, jleechan-7re5. After #172: `MAX_DISPATCH=1 bash daemon/factory-af-tick.sh --prs 7888` then 8060, 8189. Avoid `daemon --once`.

5. **Linux clippy CI** — 12 `-D warnings` failures on GHA; file bead if not fixed with #173.

## PR / merge state

**dark-factory:** [#167](https://github.com/jleechanorg/dark-factory/pull/167) MERGED; [#168](https://github.com/jleechanorg/dark-factory/pull/168)–[#171](https://github.com/jleechanorg/dark-factory/pull/171) CLOSED (code on main, mergedAt=null); [#172](https://github.com/jleechanorg/dark-factory/pull/172) OPEN CONFLICTING; [#173](https://github.com/jleechanorg/dark-factory/pull/173) OPEN CONFLICTING; [#164](https://github.com/jleechanorg/dark-factory/pull/164) OPEN; [#165](https://github.com/jleechanorg/dark-factory/pull/165) OPEN.

**worldarchitect.ai:** [#7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888) OPEN; [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060) OPEN; [#8189](https://github.com/jleechanorg/worldarchitect.ai/pull/8189) OPEN.

## Learnings pointer

- `~/roadmap/learnings-2026-07.md` — section **2026-07-06 (late night) — Factory merge landed; launchd PATH blocker**

## Roadmap pointer

- Updated `roadmap/README.md` — Recent activity (rolling), 2026-07-06 late-night wrap-up
- Activity log: `roadmap/activity/2026-07-06.md`
