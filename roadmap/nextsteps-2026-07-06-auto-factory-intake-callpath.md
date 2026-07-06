# Nextsteps — dark-factory auto-factory intake + /callpath — 2026-07-06

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **Factory trigger label confirmed:** GitHub/bead intake uses **`factory`** only (no `autof`, no daemon override). Shell + Rust paths aligned (`daemon/src/intake.rs`, `daemon/factory-intake-from-gh.sh`).
- **Intake path shipped:** `factory-overlay.sh` (recover-held, intake-upsert), `factory-tick.sh` (recover → GH sync → callpath), global `/callpath` at `~/.claude/skills/callpath/`.
- **Gap analysis persisted:** `docs/auto_factory_spec_gap_analysis.md` — direct-to-main (`DIRECT_PATH`/`PR_LESS`) remains **spec-only**, not coded.
- **Live callpath verdict AMBER:** intake GREEN, 6 GH `factory` issues linked to beads via `external_ref`; overlay **HUMAN_HELD=6** again (factory-lite verifier re-parking); smoke issue [#8164](https://github.com/jleechanorg/worldarchitect.ai/issues/8164) stuck at **QUEUED** (no dispatch).
- **Top priority:** run `factory-tick.sh` + recover-held before `/af`; close [jleechan-732a](https://github.com/jleechanorg/dark-factory/issues) (production adapters); kill orphan `run-factory-lite.sh` processes ([jleechan-imj](https://github.com/jleechanorg/dark-factory/issues)).

## Context

Session 2026-07-06 continued auto-factory factory-building work: evaluated `factory` label vs `autof`, installed global `/callpath`, fixed GH→bead intake without decommissioned `factory-lite-harness.sh`, and tested real factory-labeled GH issues + worldai drive PRs. Repo: `jleechanorg/dark-factory`, branch `main` (local ahead + uncommitted scripts). Target repo for intake: `jleechanorg/worldarchitect.ai`.

## Bead index

| Bead | Title | Priority | Link |
|------|-------|----------|------|
| jleechan-9byt.1 | Drive worldai PR #8058 | P1 | [jleechan-9byt.1](https://github.com/jleechanorg/worldarchitect.ai/issues/8167) |
| jleechan-9byt.2 | Drive worldai PR #8116 | P1 | [jleechan-9byt.2](https://github.com/jleechanorg/worldarchitect.ai/issues/8168) |
| jleechan-9byt.3 | Drive worldai PR #8064 | P1 | [jleechan-9byt.3](https://github.com/jleechanorg/worldarchitect.ai/issues/8169) |
| jleechan-9byt.4 | Drive worldai PR #8060 | P1 | [jleechan-9byt.4](https://github.com/jleechanorg/worldarchitect.ai/issues/8170) |
| jleechan-9byt.5 | Drive worldai PR #8061 | P1 | [jleechan-9byt.5](https://github.com/jleechanorg/worldarchitect.ai/issues/8171) |
| jleechan-fk9q | Auto-factory smoke test (GH #8164) | P2 | [jleechan-fk9q](https://github.com/jleechanorg/worldarchitect.ai/issues/8164) |
| jleechan-732a | Production adapters (daemon cutover) | P1 | `br show jleechan-732a` |
| jleechan-nmll | Daemon-triggered auto-tick (no operator intake) | P1 | `br show jleechan-nmll` |
| jleechan-sniw | Sweep external PRs labeled factory | P1 | `br show jleechan-sniw` |
| jleechan-ptj | Rust recover-held equivalent | P2 | `br show jleechan-ptj` |
| jleechan-imj | Stop orphan factory-lite loops | P1 | `br show jleechan-imj` |
| jleechan-9xb2 | LOCAL_PATH / direct-to-main router | P2 | `br show jleechan-9xb2` |

## Work queue

1. **Every `/af` tick — deterministic intake first** — run `bash daemon/factory-tick.sh --issue 8164 --issue 8171 --prs 8061`; acceptance: callpath shows intake PASS, HUMAN_HELD=0 after recover-held; tracks [jleechan-nmll](https://github.com/jleechanorg/dark-factory/issues).

2. **Kill or replace orphan factory-lite loops** — `pgrep -f run-factory-lite` shows coder+verifier alive but `daemon/run-factory-lite.sh` missing on disk; acceptance: no orphan processes OR restored harness; tracks [jleechan-imj](https://github.com/jleechanorg/dark-factory/issues).

3. **Drive stack sequencing (worldai)** — fix [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060) CONFLICTING/DIRTY first after [#8064](https://github.com/jleechanorg/worldarchitect.ai/pull/8064); then [#8058](https://github.com/jleechanorg/worldarchitect.ai/pull/8058), [#8116](https://github.com/jleechanorg/worldarchitect.ai/pull/8116), [#8061](https://github.com/jleechanorg/worldarchitect.ai/pull/8061) last (NON_PRODUCTION); tracks [jleechan-9byt.1](https://github.com/jleechanorg/worldarchitect.ai/issues/8167)–[.5](https://github.com/jleechanorg/worldarchitect.ai/issues/8171).

4. **Smoke path proof (#8164)** — bead [jleechan-fk9q](https://github.com/jleechanorg/worldarchitect.ai/issues/8164) must progress QUEUED → DISPATCHED after dispatch tick; acceptance: callpath route/dispatch PASS on issue #8164.

5. **Rust daemon cutover** — implement production adapters + `aow attach`; `cargo run -- --once` must not hang on LLM route; tracks [jleechan-732a](https://github.com/jleechanorg/dark-factory/issues).

6. **Direct-to-main (future)** — spec in `docs/auto_factory_spec_gap_analysis.md`; implement `LOCAL_PATH` in router + verifier + VCS push gate; tracks [jleechan-9xb2](https://github.com/jleechanorg/dark-factory/issues).

## PR / merge state

Verified this run (`gh pr view`):

- [PR #8058](https://github.com/jleechanorg/worldarchitect.ai/pull/8058): **OPEN** — MERGEABLE, UNSTABLE
- [PR #8116](https://github.com/jleechanorg/worldarchitect.ai/pull/8116): **OPEN** — MERGEABLE, UNSTABLE
- [PR #8064](https://github.com/jleechanorg/worldarchitect.ai/pull/8064): **OPEN** — MERGEABLE, UNSTABLE
- [PR #8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060): **OPEN** — CONFLICTING, DIRTY
- [PR #8061](https://github.com/jleechanorg/worldarchitect.ai/pull/8061): **OPEN** — MERGEABLE, UNSTABLE
- [PR #133](https://github.com/jleechanorg/dark-factory/pull/133): **MERGED**
- [PR #161](https://github.com/jleechanorg/dark-factory/pull/161): **CLOSED** (unmerged, CONFLICTING/DIRTY)

## Learnings pointer

- `~/roadmap/learnings-2026-07.md` — section **2026-07-06 — factory intake + global /callpath**

## Roadmap pointer

- Updated `roadmap/README.md` — **Recent activity (rolling)** — 2026-07-06 entry

---

# Addendum — 2026-07-06 PM — /callpath trace + TDD false-READY fix

## Table of contents (addendum)

- [Executive summary (PM)](#executive-summary-pm)
- [Context (PM)](#context-pm)
- [Bead index (PM)](#bead-index-pm)
- [Work queue (PM)](#work-queue-pm)
- [PR / merge state (PM)](#pr--merge-state-pm)
- [Learnings pointer (PM)](#learnings-pointer-pm)
- [Roadmap pointer (PM)](#roadmap-pointer-pm)

## Executive summary (PM)

- **/callpath traced 5 factory PRs:** dual execution lanes (parent `jleechan-9byt.*` + remediation beads); 3 merged **manually** by `jleechan2015`, not factory auto-merge; overlay `READY @ 17:04` did not match live CI.
- **Root cause fixed (TDD):** `adapters.rs` treated `pending` CI as green; `DARK_FACTORY_ITERATION_STUB=1` ignored fail buckets. Added `ci_success_from_check_buckets` + 4 unit tests + integration test `drive_existing_pr_pending_ci_does_not_reach_ready`. **`cargo test` green (78+ tests).**
- **Overlay fix:** `recover-held` now re-queues to `QUEUED` (not `ATTESTED`) so held beads re-route instead of skipping gate assessment.
- **Still open:** [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060), [#7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888) — dual AO stacks (TS `wa-*` + Go `worldarchitect-*`), no continuous rust daemon tick.
- **Top next:** dedupe AO per PR, drive [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060) rebase via [jleechan-bxjy](https://github.com/jleechanorg/dark-factory/issues) bead, respawn [#7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888) after `wa-3149` killed.

## Context (PM)

Session continued global `/callpath` on auto-factory PRs [#8058](https://github.com/jleechanorg/worldarchitect.ai/pull/8058), [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060), [#8061](https://github.com/jleechanorg/worldarchitect.ai/pull/8061), [#7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888), [#8116](https://github.com/jleechanorg/worldarchitect.ai/pull/8116). Fresh investigation found rust daemon one-shot ticks @ 17:04 (false READY) and 17:29 (5× dispatch). User requested `/nextsteps` + TDD/`/4layer` fix in `dark-factory` daemon.

## Bead index (PM)

| Bead | Title | Link |
|------|-------|------|
| jleechan-4b5 | Pending CI must not yield READY | `br show jleechan-4b5` |
| jleechan-bxjy | Force rebase PR #8060 | `br show jleechan-bxjy` |
| jleechan-93ft | Drive PR #7888 to /green | `br show jleechan-93ft` |
| jleechan-9byt.4 | Drive PR #8060 (parent) | [issue #8170](https://github.com/jleechanorg/worldarchitect.ai/issues/8170) |
| jleechan-nmll | Daemon-triggered auto-tick | `br show jleechan-nmll` |
| jleechan-ubas | False-positive PARKED_HUMAN_HELD | `br show jleechan-ubas` |

## Work queue (PM)

1. **Close bead [jleechan-4b5](https://github.com/jleechanorg/dark-factory/issues)** after rebuild + deploy rust daemon — acceptance: `callpath run dark-factory` no longer shows `all_green=true` while PR has pending/fail CI. Files: `daemon/src/adapters.rs`, `daemon/factory-overlay.sh`.

2. **Dedupe AO sessions per open PR** — kill stale TS `wa-*` when Go `worldarchitect-*` claims same PR; acceptance: one worker per PR. Tracks [jleechan-bxjy](https://github.com/jleechanorg/dark-factory/issues) / [jleechan-93ft](https://github.com/jleechanorg/dark-factory/issues).

3. **Drive [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060)** — rebase existing branch only; close stray factory PR [#8178](https://github.com/jleechanorg/worldarchitect.ai/pull/8178); `/4layer` only if CI failures reproduce after rebase.

4. **Drive [#7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888)** — respawn after `wa-3149` exit (no commits); wait for 24 pending checks to conclude.

5. **Continuous rust daemon** — replace one-shot ticks; tracks [jleechan-nmll](https://github.com/jleechanorg/dark-factory/issues).

## PR / merge state (PM)

Verified this run (`gh pr view`):

- [PR #8058](https://github.com/jleechanorg/worldarchitect.ai/pull/8058): **MERGED** @ 2026-07-06T17:31:32Z
- [PR #8116](https://github.com/jleechanorg/worldarchitect.ai/pull/8116): **MERGED** @ 2026-07-06T17:42:33Z
- [PR #8061](https://github.com/jleechanorg/worldarchitect.ai/pull/8061): **MERGED** @ 2026-07-06T17:53:33Z
- [PR #8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060): **OPEN** — MERGEABLE, CI 4 fail / 19 pending
- [PR #7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888): **OPEN** — MERGEABLE, CI 0 fail / 24 pending

## Learnings pointer (PM)

- `~/roadmap/learnings-2026-07.md` — section **2026-07-06 — callpath false READY + pending CI TDD fix**

## Roadmap pointer (PM)

- Updated `roadmap/README.md` — **Recent activity (rolling)** — 2026-07-06 PM entry


---

## Table of contents (evening — harness / factory offline)

- [Executive summary (evening)](#executive-summary-evening)
- [Context (evening)](#context-evening)
- [Bead index (evening)](#bead-index-evening)
- [Work queue (evening)](#work-queue-evening)
- [PR / merge state (evening)](#pr--merge-state-evening)
- [Learnings pointer (evening)](#learnings-pointer-evening)
- [Roadmap pointer (evening)](#roadmap-pointer-evening)

## Executive summary (evening)

- **`/harness` root cause:** Factory is **offline** because cutover never completed **X8 liveness** — no launchd rust-daemon job, no route process, **`factory-intake-from-gh.sh` missing**; `factory-af-tick.sh` cannot run intake.
- **`/callpath` verdict RED** @ 2026-07-06T19:52Z: `intake_normalizer` FAIL, `route` FAIL; overlay has stale **6× READY**, **1× QUEUED** (`jleechan-9byt.4` for [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060)) but last tick routed **0** beads.
- **Orchestration scripts:** `factory-overlay.sh`, `factory-af-tick.sh`, etc. exist locally (staged) but **`factory-intake-from-gh.sh` absent**; TDD fix `ci_success_from_check_buckets` **not on current `main` HEAD** (still in stash/WIP).
- **Open drive targets:** [#8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060), [#7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888) — MERGEABLE but CI not green; `wa-3153` working #8060 **outside** daemon route.
- **Top sequence:** (1) commit restore intake + orchestration → (2) install launchd per jleechan-a5p / jleechan-nmll → (3) `/harness --fix` skill gate → (4) redrive #8060/#7888 via factory only.

## Context (evening)

After readonly `/callpath` on four checks and `/harness` analysis of why the stack reports offline. User requested `/nextsteps` to persist harness findings. Repo: `jleechanorg/dark-factory`, branch `main` with staged factory shell scripts; overlay DB at `~/.dark-factory/daemon-cxdb.sqlite`. Assessment and artifact sync only.

## Bead index (evening)

| Bead | Title | Link |
|------|-------|------|
| jleechan-38w8 | Restore `factory-intake-from-gh.sh` + commit orchestration scripts | `br show jleechan-38w8` |
| jleechan-oale | Harness: auto-factory liveness gate (replace factory-lite restart) | `br show jleechan-oale` |
| jleechan-a5p | launchd plist template + installer | `br show jleechan-a5p` |
| jleechan-nmll | Daemon-triggered auto-tick (no operator intake) | `br show jleechan-nmll` |
| jleechan-9byt.4 | Drive PR #8060 (parent bead) | [issue #8170](https://github.com/jleechanorg/worldarchitect.ai/issues/8170) |
| jleechan-93ft | Drive PR #7888 to /green | `br show jleechan-93ft` |
| jleechan-4b5 | Pending CI must not yield READY | `br show jleechan-4b5` |
| jleechan-imj | Stop orphan factory-lite loops | `br show jleechan-imj` |

## Work queue (evening)

1. **Restore intake + land orchestration on `main`** — tracks jleechan-38w8. Acceptance: `git ls-files daemon/factory-intake-from-gh.sh`; `callpath` `intake_normalizer` PASS; include pending-CI TDD from stash if not merged.

2. **Install launchd rust-daemon liveness (cutover X8)** — tracks jleechan-a5p + jleechan-nmll. Acceptance: `launchctl list | rg dark-factory`; restart after kill; real tick with `beadsRouted>0` when QUEUED rows exist.

3. **Harness fix: auto-factory skill liveness gate** — tracks jleechan-oale. Acceptance: `.claude/skills/auto-factory/SKILL.md` Step 0 runs `callpath run dark-factory`; block `/af` when `route=FAIL`.

4. **Redrive open PRs through factory (no direct worldai edits)** — after (1)+(2). #8060: jleechan-9byt.4 QUEUED; #7888: jleechan-93ft QUEUED, park jleechan-ccfin duplicate.

5. **Re-apply pending-CI TDD fix** — tracks jleechan-4b5. Acceptance: `ci_success_from_check_buckets` in adapters.rs; no READY while CI pending/fail.

## PR / merge state (evening)

Verified this run (`gh pr view`):

- [PR #8058](https://github.com/jleechanorg/worldarchitect.ai/pull/8058): **MERGED** @ 2026-07-06T17:31:32Z
- [PR #8116](https://github.com/jleechanorg/worldarchitect.ai/pull/8116): **MERGED** @ 2026-07-06T17:42:33Z
- [PR #8061](https://github.com/jleechanorg/worldarchitect.ai/pull/8061): **MERGED** @ 2026-07-06T17:53:33Z
- [PR #8064](https://github.com/jleechanorg/worldarchitect.ai/pull/8064): **MERGED** @ 2026-07-06T17:32:01Z
- [PR #8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060): **OPEN** — MERGEABLE, CI fail=8 pass=24 pending=2
- [PR #7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888): **OPEN** — MERGEABLE, CI fail=9 pass=24 pending=3

## Learnings pointer (evening)

- `~/roadmap/learnings-2026-07.md` — section **2026-07-06 — factory offline: no launchd route + missing intake script**

## Roadmap pointer (evening)

- Updated `roadmap/README.md` — **Recent activity (rolling)** — 2026-07-06 evening harness entry
