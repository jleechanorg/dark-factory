# Daemon Cutover Exit Criteria — Adversarial Reviewer Charter

Scope: sign-off for retiring factory-lite and granting the Rust daemon production authority
(beads jleechan-732a → jleechan-xrdx). Hardened by 3-reviewer /advice adversarial pass 2026-07-04
(Opus hostile review, game-proof-criteria research, Cursor repo-grounded hostile review — all
findings folded in below).

## Ground rules (apply to every criterion; violating any one fails the whole round)

- **R1 — Binary, executable, externally anchored.** Every criterion is pass/fail via a stated
  command whose expected observable lives in an external system of record (GitHub API via `gh`,
  `br`, `git` refs, process table, kernel sandbox denial) — the layer users experience.
- **R2 — Implementer artifacts are never sufficient.** `daemon.jsonl` telemetry, daemon logs,
  test output, and the evidence bundle are corroborating only. Every lifecycle claim must be
  cross-checked against an external artifact (PR state, bead state, branch SHA). Telemetry is
  implementer-authored and therefore forgeable by construction.
- **R3 — No mocks, no dry-run, no pre-seeded state.** Any criterion satisfied via `--dry-run`,
  in-memory CXDB, subprocess mocks, `#[ignore]`d tests, hand-appended JSONL lines, or state
  seeded before the run = automatic FAIL of the entire review round (not just that criterion).
- **R4 — Reviewer reproduces, never inspects.** The reviewer executes the runs themselves on
  fresh state. A reproduction that yields the same PR URL as the implementer's evidence is
  replay and FAILS; a fresh bead must yield a fresh PR.
- **R5 — Default verdict is FAIL.** Missing, ambiguous, or partially-satisfied evidence is FAIL,
  never "inconclusive". The reviewer is rewarded for finding gaps (inverted incentive).
- **R6 — Three skeptics are distinct; conflation is a FAIL.** (a) gate-7
  `verifier::parse_skeptic_verdict` on an LLM reply, (b) the GitHub Actions `/skeptic` workflow
  verdict on a commit SHA, (c) this sign-off review. Evidence for one never satisfies another.

## X1 — Binary provenance and no-Noop

- Built from a clean tree at pinned merge commit `S`: record `git rev-parse HEAD`, `git status
  --porcelain` (empty), and `sha256` of the built binary; the same sha256 must be re-derived by
  the reviewer building from `S`.
- The release binary contains no Noop dispatch path: `NoopAdapters` is absent from the release
  build (compile-gated to `--dry-run`/tests), demonstrated by reviewer inspection of the build
  config plus a failed attempt to run a no-adapter tick in release mode.

## X2 — Live end-to-end run (the core criterion)

- Reviewer (not implementer) creates a fresh sandbox `factory`-labeled work item, records
  wall-clock `T0`, then runs the production ticks (`daemon --once` as many times as the
  lifecycle needs — intake/dispatch tick then verify tick; no manual harness calls in between).
- PASS requires ALL, verified via `gh`/`br`/`git` (not telemetry):
  - a NEW PR exists with `createdAt > T0`, head ref `factory/<bead>-r1`, and a **non-empty diff
    that plausibly implements the bead** (reviewer judgment on content, not just diffstat > 0);
  - bead overlay transitions QUEUED→DISPATCHED→ATTESTED→(READY|HUMAN_HELD) each corroborated by
    the matching external artifact at the time of check;
  - every post-dispatch telemetry event carries the real PR head SHA; any event whose external
    artifact is missing or contradicts it = FAIL.
- Independence: the reviewer repeats on a second fresh bead and must obtain a **different** PR
  URL (R4). Same-machine rerun of the implementer's fixture does not count.

## X3 — Adapters proven against real binaries + tests proven able to fail

- Each of the 5 adapters (`CliTracker`/`CliScm`/`CliSessions`/`CliVcs`/`ChainLlm`) has ≥1 test
  invoking the REAL `br`/`gh`/`git`/session binary against a disposable sandbox, running in the
  default `cargo test` set (no `#[ignore]`, no mock at the subprocess boundary). CI fails if any
  such test is absent or skipped.
- Mutation check (tests can fail): reviewer breaks each adapter once (e.g. corrupt the gh args,
  return wrong SHA) and confirms the suite goes red each time. A suite that stays green under
  any of the 5 mutations = FAIL.

## X4 — Merge authority: blocked red + merged green, race-bound

- Two attempts through the IDENTICAL production merge function in one live run:
  - **Red control:** a mergeable, CI-green PR carrying a daemon-native `GATE_ASSESSMENT` with
    ≥1 gate=`red` → the daemon must ATTEMPT the merge, the guard must log an explicit refusal
    citing the gate name, and `gh pr view` must show OPEN afterward. "Not merged" without a
    logged attempt = FAIL (vacuous).
  - **Green control:** an all-green PR through the same path → state=MERGED.
- Mislabel control: a `red` gate recorded as `unknown` must be caught (guard treats
  unknown-that-was-red as red or the assessment writer is proven unable to emit that state).
- TOCTOU: the merge call must bind the PR head SHA it assessed (merge fails if head moved
  between assessment and merge; reviewer forces this by pushing to the branch in the gap).
- `safe-push-main.sh` semantics: reviewer advances origin/main mid-run once; the daemon's push
  path must rebase+verify HEAD==origin or fail loudly — a silent no-op push = FAIL.

## X5 — Holdout isolation on the production spawn path

- Proven from INSIDE a coder session spawned by the daemon (not the daemon parent, not a fresh
  shell): zero env keys matching `*HOLDOUT*` (checked at spawn and after first tool call), AND a
  read attempt on `$DARK_FACTORY_HOLDOUTS/evaluator/run.py` from that session fails with a
  sandbox denial (EACCES-equivalent). Env-name greps alone = FAIL; the filesystem denial is the
  criterion.

## X6 — Fault injection (real faults, pre-stated hypotheses)

Each control states its expected end-state BEFORE injection; all use real binaries, no mocks:
- **(a) Failing bead:** intentionally test-failing work item → never reaches READY; parked with
  external evidence (PR checks red / reroll recorded).
- **(b) PR closed underneath:** reviewer closes the PR mid-flight → bead ends HUMAN_HELD,
  distinguished from merged-then-closed (the #155 race class).
- **(c) Real gh failure:** revoked token or invalid repo against the real `gh` → daemon enters
  `error` state; no state advances, no success telemetry after failure.
- **(d) Mid-tick partial failure:** rate-limit (429) injected AFTER dispatch, before
  verification → no state is marked past what external artifacts support.
- **(e) kill -9 mid-flight, then restart:** exactly one coder session and one PR exist for the
  in-flight bead afterward (no double-dispatch, no orphaned untracked session); CXDB
  (WAL) uncorrupted per `PRAGMA integrity_check`.
- **(f) Stall:** a wedged coder (alive but silent) → bead moves to HUMAN_HELD within the
  configured wall-clock bound, computed by the daemon, not an LLM.

## X7 — Concurrency and caps

- One tick dispatches ≥2 file-disjoint beads concurrently; both produce distinct PRs; CXDB shows
  no lock errors; no bead is dispatched twice across two consecutive ticks.
- Session cap respected: with the cap set to N, a queue of N+2 dispatches exactly N sessions
  (reviewer counts real processes, not telemetry).
- Single-writer during cutover: nothing else (harness scripts, merge-guard timer) writes the
  daemon's CXDB or `daemon.jsonl` while the daemon runs — reviewer verifies the old writers are
  disabled before X2, else SLO/state evidence is uninterpretable.

## X8 — Liveness and SLO, fired end-to-end

- Daemon runs under launchd (plist template in repo, loaded — `launchctl list` shows it);
  `kill -9` → restarted and a subsequent REAL tick completes (heartbeat alone insufficient).
- Wedge detection: daemon blocked on a hung subprocess → self-timeout + alert (the 575-min
  silent-death class), not KeepAlive-alive-but-idle.
- SLO alert path fired by its real caller: reviewer induces a genuine >60-min inter-event gap
  (or compresses thresholds via config, not by editing JSONL) and the alert arrives at a sink a
  human actually monitors (Slack/notification), triggered by launchd/daemon — not by calling the
  alert function directly.

## X9 — Parity, drain, and reversible decommission

- **Parity:** on the same class of sandbox bead, the daemon's outcome (PR opened, gates
  assessed, correct terminal state) is ≥ factory-lite's recorded behavior; any capability the
  loop had that the daemon lacks is enumerated and explicitly accepted by the operator.
- **Drain:** at cutover, zero beads are in DISPATCHED/ATTESTED under the old system, stale
  rows are reconciled with a written audit (bug fixed, not evidence deleted — the stale-READY
  read-as-live defect must have a regression test).
- **Reversible:** decommission is a revert-able commit (tag recorded); factory-lite scripts
  archived not lost; a documented 1-command restore path exists until 7 days post-cutover.
- **No survivors:** no caller of any factory-lite entry point remains — checked in git grep AND
  launchd/cron/shell aliases/skills (`.claude/skills/factory-lite*`).

## X10 — Sign-off mechanics

- The reviewer's verdict is recorded against pinned SHA `S`; any new commit voids it.
- The GitHub Actions `/skeptic` workflow posts `VERDICT: PASS` for `S` on the cutover PR
  (distinct from gate-7 parsing and from this review — R6).
- The evidence bundle lists, per criterion: command run, raw output, external artifact URL, and
  who executed it (implementer vs reviewer). Criteria X2, X4, X5, X6(e) must have been executed
  by the reviewer personally.
