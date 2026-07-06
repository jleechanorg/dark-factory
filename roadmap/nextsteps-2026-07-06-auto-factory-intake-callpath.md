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

---

# Addendum — 2026-07-06 late evening — PR #8189 GHA fallback fix + quota-blocked dispatch

## Table of contents (late-evening addendum)

- [Executive summary (late)](#executive-summary-late)
- [Context (late)](#context-late)
- [Bead index (late)](#bead-index-late)
- [Work queue (late)](#work-queue-late)
- [PR / merge state (late)](#pr--merge-state-late)
- [Learnings pointer (late)](#learnings-pointer-late)
- [Roadmap pointer (late)](#roadmap-pointer-late)

## Executive summary (late)

- **New PR opened during this window:** [PR #8189](https://github.com/jleechanorg/worldarchitect.ai/pull/8189) — `fix(ci): support --break-system-packages in ensurepip fallback` (branch `fix/GHA-ensurepip-break-system-packages-fallback`). Head `3447e85a`, MERGEABLE, CI pass=13/fail=8/skip=9.
- **Factory orchestration scripts still on disk but uncommitted:** `daemon/factory-overlay.sh` (modified) + `daemon/factory-intake-from-gh.sh` (untracked). Scripts restored from `stash@{0}` in commit `8e70c9075`, but these two are *not* on `main` yet.
- **Quota-blocked dispatch:** individual quota hit during prior session (resets ~22:30Z). No new AO dispatches issued in this pass; readonly snapshot only.
- **Three PRs remain OPEN and MERGEABLE:** #8189 (8 fail), #8060 (6 fail), #7888 (5 fail). None reachable to all-green today without further coder + verifier iterations.
- **Stranded AO workers:** ~15 processes still alive (PID 25741 on PR #8061 since 09:16, PID 10644 `agy.real` since 14:12) — staleness vs merged-PR targets means they are wasted resources.
- **Top sequence:** (1) land the 2 uncommitted files on `main`; (2) kill stranded AO workers; (3) restart `daemon/run-factory-lite.sh coder/verifier`; (4) dedupe the 4×#8060 and 2×#8116/#7888 bead duplicates; (5) one focused worker on **#7888** (lowest fail count) to green.

## Context (late)

Continuation of the auto-factory orchestration work. After the evening /nextsteps captured the offline-stack diagnosis, this pass opened and pushed PR #8189 to fix the self-hosted runner `ensurepip` PEP-668 bootstrap that had been failing the `Harness autonomy checks (self hosted)` job across multiple PRs. The fix was non-trivial — three commits on the branch before checks stabilised. Worker ran into the MiniMax-M3 individual quota mid-session, so dispatch work for #8060/#7888 was deferred. Repo: `jleechanorg/dark-factory` (main, 2 uncommitted files); target repo: `jleechanorg/worldarchitect.ai` (PRs #8189/#8060/#7888 still open). Read-only assessment only.

## Bead index (late)

| Bead | Title | Priority | Link |
|------|-------|----------|------|
| jleechan-mt675 | Fix self-hosted GHA runner python3-venv ensurepip fallback | P1 | [issue via factory PR #8189](https://github.com/jleechanorg/worldarchitect.ai/pull/8189) |
| jleechan-93ft | Drive PR #7888 to /green (single bead, parent of ccfin duplicate) | P1 | `br show jleechan-93ft` |
| jleechan-9byt.4 | Drive PR #8060 to 7-green + /er PASS | P1 | [issue #8170](https://github.com/jleechanorg/worldarchitect.ai/issues/8170) |
| jleechan-ccfin | Duplicate of jleechan-93ft — park, do not redrive | P2 | `br show jleechan-ccfin` |
| jleechan-bxjy | Force rebase #8060 onto origin/main | P0 | `br show jleechan-bxjy` |
| jleechan-4uzw | Rebase PR #8060 to resolve dirty→MERGEABLE | P1 | `br show jleechan-4uzw` |
| jleechan-38w8 | Land factory-intake-from-gh.sh + factory-overlay.sh on main | P0 | `br show jleechan-38w8` |
| jleechan-imj | Stop orphan factory-lite loops | P1 | `br show jleechan-imj` |
| jleechan-nyp1 | Make factory-ao-remediate.sh spawn async (non-blocking AF tick) | P1 | `br show jleechan-nyp1` |
| jleechan-nmll | Daemon-triggered auto-tick (replace operator intake) | P1 | `br show jleechan-nmll` |

## Work queue (late)

1. **Land `daemon/factory-overlay.sh` + `daemon/factory-intake-from-gh.sh` on `main`** — tracks jleechan-38w8. Acceptance: `git ls-files daemon/factory-intake-from-gh.sh` returns the path; `daemon/factory-overlay.sh` matches `origin/main`; commit + push. **Blocker for any redrive.**

2. **Kill stranded AO workers on already-merged PRs** — `pgrep -fl "ao spawn --claim-pr 806[14]" | xargs -r kill -TERM`. Acceptance: `ps -ef | rg "ao spawn" | wc -l` ≤ 5 (only the active driver should remain). **Frees GPU/CPU quota for the redrive.**

3. **Restart factory-lite loops** — `nohup bash daemon/run-factory-lite.sh coder 240 43200 &` + `nohup bash daemon/run-factory-lite.sh verifier 120 43200 &`. Acceptance: `launchctl list | rg factory-lite` (or process table) shows both loops; first tick should route ≥1 QUEUED bead to DISPATCHED.

4. **Dedupe overlay beads** — close the 3 jleechan-4uzw / jleechan-bxjy / jleechan-hslx duplicates (the parent jleechan-9byt.4/.1 stay); park jleechan-ccfin as duplicate of jleechan-93ft. Acceptance: `factory-overlay.sh list QUEUED` shows ≤7 rows.

5. **Drive [PR #7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888) end-to-end** — single focused worker, jleechan-93ft bead; cc-finish-level-commit has the smallest fail count (5) and the most-attested bead (parent ccfin already cycled). Acceptance: `gh pr checks 7888 --json state,conclusion | jq '[.[] | select(.conclusion=="failure")] | length'` = 0.

6. **Drive [PR #8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060)** — second priority (6 fails). Rebase first per jleechan-bxjy; then drive. Acceptance: all checks `conclusion=success`.

7. **Verify [PR #8189](https://github.com/jleechanorg/worldarchitect.ai/pull/8189)** — new PR is already MERGEABLE; it just needs to land on `main` so subsequent PRs benefit from the GHA fallback fix. Acceptance: merged + worldai runner no longer fails `Harness autonomy checks`.

## PR / merge state (late)

Verified this run (`gh pr view` against `jleechanorg/worldarchitect.ai`):

- [PR #8189](https://github.com/jleechanorg/worldarchitect.ai/pull/8189): **OPEN** — MERGEABLE, head `3447e85a`, CI pass=13/fail=8/skip=9 (Green Gate failing — GHA bootstrap now partially fixed)
- [PR #8060](https://github.com/jleechanorg/worldarchitect.ai/pull/8060): **OPEN** — MERGEABLE, head `98faf53e`, CI pass=20/fail=6/skip=4
- [PR #7888](https://github.com/jleechanorg/worldarchitect.ai/pull/7888): **OPEN** — MERGEABLE, head `c95249f4`, CI pass=23/fail=5/skip=2
- [PR #8058](https://github.com/jleechanorg/worldarchitect.ai/pull/8058): **MERGED** @ 2026-07-06T17:31:32Z
- [PR #8064](https://github.com/jleechanorg/worldarchitect.ai/pull/8064): **MERGED** @ 2026-07-06T17:32:01Z
- [PR #8116](https://github.com/jleechanorg/worldarchitect.ai/pull/8116): **MERGED** @ 2026-07-06T17:42:33Z
- [PR #8061](https://github.com/jleechanorg/worldarchitect.ai/pull/8061): **MERGED** @ 2026-07-06T17:53:33Z

## Learnings pointer (late)

- `~/roadmap/learnings-2026-07.md` — section **2026-07-06 (late) — GHA ensurepip fallback + quota-blocked dispatch**

## Roadmap pointer (late)

- Updated `roadmap/activity/2026-07-06.md` — late-evening entry appended (PR #8189 + quota-blocked dispatch).
- `roadmap/README.md` — no new date link needed (same day).
