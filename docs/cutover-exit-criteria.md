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

## Exit criterion X1 — Binary provenance and no-Noop

- Built from a clean tree at pinned merge commit `S`: record `git rev-parse HEAD`, `git status
  --porcelain` (empty), and `sha256` of the built binary; the same sha256 must be re-derived by
  the reviewer building from `S`.
- The release binary contains no Noop dispatch path: `NoopAdapters` is absent from the release
  build (compile-gated to `--dry-run`/tests), demonstrated by reviewer inspection of the build
  config plus a failed attempt to run a no-adapter tick in release mode.

## Exit criterion X2 — Live end-to-end run (the core criterion)

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

## Exit criterion X3 — Adapters proven against real binaries + tests proven able to fail

- Each of the 5 adapters (`CliTracker`/`CliScm`/`CliSessions`/`CliVcs`/`ChainLlm`) has ≥1 test
  invoking the REAL `br`/`gh`/`git`/session binary against a disposable sandbox, running in the
  default `cargo test` set (no `#[ignore]`, no mock at the subprocess boundary). CI fails if any
  such test is absent or skipped.
- Mutation check (tests can fail): reviewer breaks each adapter once (e.g. corrupt the gh args,
  return wrong SHA) and confirms the suite goes red each time. A suite that stays green under
  any of the 5 mutations = FAIL.

## Exit criterion X4 — Merge authority: blocked red + merged green, race-bound

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

### X4 — live result (2026-07-10, PARTIAL — milestone, not sign-off)

Bead jleechan-vbbi. First **live** exercise of the production merge function
`daemon/scripts/auto-merge-guard.sh` under the operator-approved **Option A** path — the
externally-scheduled guard (systemd user timer `dark-factory-merge-guard.timer`, 60s,
active since 2026-07-10 12:39 PDT), which is the spec §4.2.8-conformant resolution of the
"daemon merges: never" vs X4 contradiction (the Rust daemon never merges; merge authority
is this one externally-run policy script). Evidence bundle:
[`evidence/x4-live-merge-20260710/`](../evidence/x4-live-merge-20260710/) (README + raw
`gh api` JSON, guard-log excerpts, `merge-timestamps` ledger, timer status, guard sha256).

- **Green control — OBSERVED.** PR
  [#228](https://github.com/jleechanorg/dark-factory/pull/228)
  (`factory/ez-gh-actions-mw5a-r2`, a real work item) merged through the guard:
  `merged_at 2026-07-10T20:48:04Z`, `merge_commit_sha f59b4888…`. External anchor is the
  `gh api` PR state + git merge SHA; the guard (not a hand-merge) performed it —
  `merged_by` is `jleechan2015`, the identity the externally-scheduled guard authenticates
  as, corroborated by the guard's `PR 228 MERGED, bead … closed+READY` log line and the
  `~/.dark-factory/merge-timestamps` epoch `1783716480` (= 13:48 PDT, guard-only ledger).
  The guard held the merge until a GATE_ASSESSMENT existed AND the PR was mergeable (18
  `assessment missing — refusing merge (green CI is insufficient)` ticks first).
- **Refusal control — OBSERVED (adjacent branch, not strict red control).** PRs
  [#205](https://github.com/jleechanorg/dark-factory/pull/205) (2534×),
  [#208](https://github.com/jleechanorg/dark-factory/pull/208) (2569×) refused on
  `verifier assessment missing — refusing merge (green CI is insufficient)` and
  [#207](https://github.com/jleechanorg/dark-factory/pull/207) on `CI FAILED — skip`,
  all through the SAME guard function in the same run; all three remain `state: open`
  afterward. PR #205 is `mergeable_state: clean` (green CI) yet refused every tick —
  the "green-CI-is-insufficient" policy demonstrated end-to-end.
- **Assessment source (no overclaim):** the guard's gate reads the latest `GATE_ASSESSMENT`
  event from the daemon JSONL, which is implementer-authored telemetry (charter R2,
  corroborating only). The load-bearing anchors are the GitHub PR state and merge SHA.
- **Still owed before X4 = PASS (this is PARTIAL):** the refusals above are the
  *assessment-missing* and *CI-failed* branches, **not** the strict X4 red control (a PR
  carrying a GATE_ASSESSMENT with a gate=`red`/`fail`, guard refusing while citing the gate
  name). The **mislabel control**, **TOCTOU head-SHA binding**, and **`safe-push-main.sh`**
  semantics were also not exercised by this passive observation. These remain to be shown in
  a reviewer-personal run (R4) before X4 can be marked PASS.

## Exit criterion X5 — Holdout isolation on the production spawn path

- Proven from INSIDE a coder session spawned by the daemon (not the daemon parent, not a fresh
  shell): zero env keys matching `*HOLDOUT*` (checked at spawn and after first tool call), AND a
  read attempt on `$DARK_FACTORY_HOLDOUTS/evaluator/run.py` from that session fails with a
  sandbox denial (EACCES-equivalent). Env-name greps alone = FAIL; the filesystem denial is the
  criterion.

## Exit criterion X6 — Fault injection (real faults, pre-stated hypotheses)

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

## Exit criterion X7 — Concurrency and caps

- One tick dispatches ≥2 file-disjoint beads concurrently; both produce distinct PRs; CXDB shows
  no lock errors; no bead is dispatched twice across two consecutive ticks.
- Session cap respected: with the cap set to N, a queue of N+2 dispatches exactly N sessions
  (reviewer counts real processes, not telemetry).
- Single-writer during cutover: nothing else (harness scripts, merge-guard timer) writes the
  daemon's CXDB or `daemon.jsonl` while the daemon runs — reviewer verifies the old writers are
  disabled before X2, else SLO/state evidence is uninterpretable.

## Exit criterion X8 — Liveness and SLO, fired end-to-end

- Daemon runs under launchd (plist template in repo, loaded — `launchctl list` shows it);
  `kill -9` → restarted and a subsequent REAL tick completes (heartbeat alone insufficient).
- Wedge detection: daemon blocked on a hung subprocess → self-timeout + alert (the 575-min
  silent-death class), not KeepAlive-alive-but-idle.
- SLO alert path fired by its real caller: reviewer induces a genuine >60-min inter-event gap
  (or compresses thresholds via config, not by editing JSONL) and the alert arrives at a sink a
  human actually monitors (Slack/notification), triggered by launchd/daemon — not by calling the
  alert function directly.

## Exit criterion X9 — Parity, drain, and reversible decommission

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

## Exit criterion X10 — Sign-off mechanics

- The reviewer's verdict is recorded against pinned SHA `S`; any new commit voids it.
- The GitHub Actions `/skeptic` workflow posts `VERDICT: PASS` for `S` on the cutover PR
  (distinct from gate-7 parsing and from this review — R6).
- The evidence bundle lists, per criterion: command run, raw output, external artifact URL, and
  who executed it (implementer vs reviewer). Criteria X2, X4, X5, X6(e) must have been executed
  by the reviewer personally.

## Appendix A — Adversarial /advice pass reproducibility

This charter was hardened by a 3-reviewer `/advice` adversarial pass on 2026-07-04. The
following table records the inputs, model routing, and findings so the criteria can be
re-derived (and re-attacked) on any future cutover attempt.

| Reviewer lane | Model / route | Input envelope | Verdict |
|---|---|---|---|
| Opus hostile review | opus / claude-code, prompt c/medium | Decision point + ≤60-line charter extract | Hostile-attack fold-in: ~20 loopholes closed |
| Game-proof research | `/research` web search, prior-art audit | Decision topic "daemon cutover exit criteria" | Research fold-in: ~14 missing failure classes closed |
| Cursor repo-grounded | cursor-agent -f, prompt b/high | Repo state at 2026-07-04 + charter extract | Repo-grounded fold-in: state-mismatch defects closed |

### A.1 — Closed loopholes (post-Opus hostile review)

The Opus hostile reviewer enumerated ~20 attack vectors against the pre-hardening draft. Each
is folded into a numbered criterion below; all were originally admitted by the pre-/advice
draft:

- **Red control vacuous-merge loophole** — a guard that simply *did not merge* without
  logging the attempt would PASS X4 vacuously. Closed: X4 now requires a logged
  `ATTEMPT + refusal` with the gate name cited.
- **Green positive twin missing** — without a green twin, a guard that *never merged* would
  appear to "respect" X4. Closed: X4 mandates the green twin (all-green PR → state=MERGED).
- **Telemetry-as-evidence loophole** — daemon-internal `daemon.jsonl` could be forged by the
  implementer. Closed: R2 makes implementer telemetry corroborating only; every lifecycle
  claim must cross-reference an external artifact (`gh pr view`, `br show`, `git rev-parse`).
- **Same-URL replay loophole** — a reviewer rerunning the implementer's fixture on the same
  PR URL would see the same artifacts. Closed: R4 requires a *fresh bead* yielding a *fresh
  PR URL*; same-machine rerun of the implementer's fixture does not count.
- **No-op dispatch path in release** — `NoopAdapters` could be compiled into release. Closed:
  X1 requires NoopAdapters absent from release build + a failed attempt to run a no-adapter
  tick in release mode.
- **Mislabeled-red-as-unknown** — a guard that treats `unknown` as `green` is exploitable.
  Closed: X4 mislabel control requires the guard to catch a `red`-recorded-as-`unknown`
  state, OR the assessment writer to be proven unable to emit that state.
- **TOCTOU between assessment and merge** — head SHA may move between gate read and merge.
  Closed: X4 binds the PR head SHA at assessment time; merge fails if head moved.
- **Sandbox-exec-but-env-still-leaks** — `*HOLDOUT*` env vars could survive sanitization.
  Closed: X5 requires env keys checked at spawn AND after first tool call (no point-in-time
  check alone).
- **Filesystem-readable-despite-sandbox** — env-strip is necessary but not sufficient.
  Closed: X5 requires a filesystem denial (EACCES-equivalent) on the read attempt, not
  env-name grep alone.
- **Stale-READY read-as-live** — beads in DISPATCHED/ATTESTED that reconcile to stale-READY
  could be re-dispatched. Closed: X9 mandates drain with written audit + regression test
  for the stale-READY defect.
- **Surviving factory-lite caller** — decommission is never complete if any caller remains.
  Closed: X9 mandates `git grep` of entry points AND launchd/cron/shell aliases/skills
  (`.claude/skills/factory-lite*`).
- **Process-table-only-merge-guard test** — a guard tested against synthetic processes
  would miss the real launchd path. Closed: X4 / X8 require real `launchctl list` evidence.
- **Revert path undocumented** — decommission is irreversible if no restore path exists.
  Closed: X9 requires a documented 1-command restore path until 7 days post-cutover.
- **No-caller-of-alert-path test** — the SLO alert might exist but never fire from launchd.
  Closed: X8 mandates alert arrival at a human-monitored sink (Slack/notification), triggered
  by launchd/daemon — not by calling the alert function directly.
- **Test-stays-green-under-mutation gap** — a test suite that cannot fail cannot prove
  anything. Closed: X3 mutation check (reviewer breaks each adapter once, suite must go red).
- **Single-writer assumption unverified** — daemon CXDB could be written concurrently by
  harness scripts. Closed: X7 single-writer check: reviewer verifies old writers are disabled
  before X2.
- **KeepAlive-alive-but-idle false health** — `launchctl list` shows the daemon alive even
  when wedged. Closed: X8 wedge detection via self-timeout + alert, computed by the daemon
  (not LLM).
- **Impossibly-strict-all-7-green lockout** — unknown (infra unavailability) gates could
  deadlock. Closed in `daemon/scripts/auto-merge-guard.sh`: `unknown` is treated as
  passable-but-flagged so cutover cannot deadlock on CodeRabbit/Bugbot quota walls.
- **Silent non-ff push data loss** — unattended push could rewrite remote history.
  Closed: `daemon/scripts/safe-push-main.sh` enforces rebase + HARD-VERIFY `HEAD == origin/main`.
- **Per-hour cascade blast radius** — an unguarded auto-merge could spam many PRs at once.
  Closed: `auto-merge-guard.sh` per-hour merge budget (default 8) caps blast radius.

### A.2 — Closed missing failure classes (post-research)

Research surfaced ~14 missing failure classes not originally enumerated in the draft
charter; each is folded into a numbered criterion below:

- **(X6a) Failing bead** — must end HUMAN_HELD, never READY, with external evidence (PR
  checks red / reroll recorded).
- **(X6b) PR closed underneath** — must end HUMAN_HELD, distinguished from
  merged-then-closed (the #155 race class).
- **(X6c) Real gh failure** — revoked token or invalid repo must surface `error` state;
  no state advances after failure.
- **(X6d) Mid-tick partial failure** — 429 rate-limit injected AFTER dispatch, BEFORE
  verification; no state is marked past what external artifacts support.
- **(X6e) kill -9 mid-flight, then restart** — exactly one coder session + one PR exist
  after restart (no double-dispatch, no orphaned untracked session); CXDB (WAL) uncorrupted
  per `PRAGMA integrity_check`.
- **(X6f) Stall / wedge** — alive-but-silent coder → HUMAN_HELD within configured
  wall-clock bound, computed by the daemon, not an LLM.
- **(X8) 575-min silent death** — historical poll-loop failure class; closed by launchd
  wedge detection + alert path.
- **(X9) Stale-READY read-as-live** — beads stuck in DISPATCHED/ATTESTED after the legacy
  loop died; must be drained with a written audit.
- **(X9) 1-command restore path** — decommission must be revert-able until 7 days
  post-cutover.
- **(X7) Double-dispatch across ticks** — same bead must not be dispatched twice across two
  consecutive ticks; verified via CXDB uniqueness.
- **(X7) Session cap actual vs nominal** — cap must be enforced at the process table layer,
  not at telemetry layer.
- **(X10) Three-skeptic conflation** — gate-7 `parse_skeptic_verdict` ≠ GHA `/skeptic`
  workflow verdict ≠ this sign-off review (R6).
- **(X2) Diff plausibility** — a non-empty diffstat is not enough; reviewer judgment on
  content (does the diff plausibly implement the bead?).
- **(X5) Isolation from inside the spawned coder** — sandbox-exec read-denial must be
  observed from inside the spawned coder session, not the daemon parent.

### A.3 — Re-derivation contract

To re-run the adversarial pass on any future cutover attempt:

1. Extract the candidate charter (≤150 lines) into `~/.claude/skills/advice/` envelope.
2. Fan out three reviewers in parallel:
   - Opus hostile (claude-code, prompt c/medium) — extract-only.
   - `/research` web search on "daemon cutover exit criteria" prior art.
   - Cursor repo-grounded (cursor-agent -f, prompt b/high) — repo state + extract.
3. Fold any new loophole or missing failure class into the appropriate X-criterion;
   bump the charter version + re-pin SHA `S`.
4. Re-issue the bead (e.g. `jleechan-732a-v2`) referencing the new SHA.

**Why this appendix exists:** without the fold-in record, future agents re-deriving the
charter would re-discover the same ~34 gaps one by one, burning review cycles and risking
a cutover that ships with a known-closed loophole re-opened. The appendix is the durable
attack surface, not just the current criterion list.

**Related:** [[factory-lite-decommission-decision]] (no further factory-lite investment;
cutover is a supervised event), [[factory-latency-poll-bound]] (codergen p50 vs roadmap),
[[factory-ops-guardrails]] (safe-push + auto-merge-guard scripts).

## Supplementary: /web-advice fail-open advisory lane (2026-08-21)

A non-blocking advisory reviewer (`type="web_advice"` in `runner/handlers.py`) sits between
the strict gates and `exit` in the PR lane graph (see `pipelines/factory/pr_gates.dot` and
the standalone test pipeline `pipelines/factory/web-advice-failopen.dot`).

**Operational invariant:** the node ALWAYS returns `outcome=success` to the .dot engine.
Its verdict — APPROVE / NOT MERGE / infrastructure-unavailable — is surfaced only through
three out-of-band channels:

1. CXDB structured event under `event_type: web_advice_review` (durable audit trail).
2. PR comment via `gh pr comment` wrapped in `<!-- web-advice-review -->` markers.
3. Follow-up bead via `br create` when the panel converges on infra-failure or ≥3-of-4
   NOT MERGE with concrete findings; the bead body MUST start with
   `target_repo: jleechanorg/dark-factory` per the phantom-dispatch guardrail.

This advisory lane does not change any X-criterion above; the strict gates remain
authoritative for blocking decisions. /web-advice is a lens, not a gate. For the design
charter see `docs/web-advice-failopen-design.md`.
