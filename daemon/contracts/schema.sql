-- Auto-Factory Daemon CXDB contract (spec §4.2.3, design doc §3).
-- Honored by daemon/factory-overlay.sh (bash path; restored in PR #167 from
-- e60b5a31b~1:daemon/factory-lite-harness.sh) and the Rust daemon (rusqlite).
-- The factory-lite skill plane was decommissioned in e60b5a31b.
-- DB file: ~/.dark-factory/daemon-cxdb.sqlite  (separate from the runner's cxdb.sqlite)
-- Init: sqlite3 ~/.dark-factory/daemon-cxdb.sqlite < daemon/contracts/schema.sql
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS bead_overlay (
  bead_id       TEXT PRIMARY KEY,
  state         TEXT NOT NULL CHECK (state IN
                  ('QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY','RE_ROLL','RECOVERY',
                   'REDISPATCHED','BUDGET_HELD','HUMAN_HELD','DISPOSITION_REQUIRED')),
  attempt       INTEGER NOT NULL DEFAULT 1,   -- r<n> counter
  reroll_count  INTEGER NOT NULL DEFAULT 0,
  autonomy_secs INTEGER NOT NULL DEFAULT 0,   -- cumulative; nothing automated resets it
  spend_usd     REAL    NOT NULL DEFAULT 0,   -- monitoring-only (spec §4.2.8)
  pr_number     INTEGER,
  branch        TEXT,
  session_id    TEXT,
  updated_at    TEXT    NOT NULL,             -- ISO-8601 UTC
  -- /er runner state (bead jleechan-qqq): per-bead attempt counter + last
  -- attempt timestamp (unix epoch seconds). Used by `er_runner::maybe_run`
  -- to enforce a per-bead attempt cap (default MAX_ER_RUNNER_ATTEMPTS=3)
  -- and a cooldown window (default ER_RUNNER_COOLDOWN_SECS=300).
  -- Older DBs that pre-date this column pair get them via the
  -- idempotent `ensure_er_runner_columns` migration in
  -- `SqliteStateStore::open` (which checks `pragma_table_info` first
  -- because SQLite has no `ADD COLUMN IF NOT EXISTS`).
  attempt_er_runner_count INTEGER NOT NULL DEFAULT 0,
  last_er_runner_attempt_at INTEGER,
  -- Adopted-PR provenance (bead jleechan-tfs1): 1 iff this bead's `branch`
  -- is an external contributor's own head_ref_name (adopted via
  -- `intake::normalize_labeled_prs`), 0 for factory-fabricated branches.
  -- `reroll()` (daemon/src/reroll.rs) reads this to pick its remediation
  -- strategy: adopted -> append-only fix commit on the existing branch,
  -- PR stays open; factory-fabricated -> today's fabricate-new-branch +
  -- close-old-PR path (no regression). Detection is an explicit stored
  -- flag set at adoption time in `tick::run_slow_tier`, NOT a branch-name
  -- pattern match (a contributor could legitimately name a branch
  -- `factory/...`). Older DBs pre-date this column and get it via the
  -- idempotent `ensure_is_adopted_column` migration in
  -- `SqliteStateStore::open` (same `pragma_table_info` guard pattern as
  -- `ensure_er_runner_columns`, since SQLite has no
  -- `ADD COLUMN IF NOT EXISTS`).
  is_adopted INTEGER NOT NULL DEFAULT 0,
  -- Consecutive transient `Sessions::spawn` failures since the last
  -- confirmed DISPATCHED (follow-up to #198 dispatch-batch-isolation): a
  -- bead whose spawn deterministically fails never reaches DISPATCHED, so
  -- it was invisible to the DISPATCHED/ATTESTED-scoped autonomy_secs +
  -- wedge-detection net. Deliberately separate from `attempt` (which
  -- already double-duties as the branch/re-roll suffix AND the
  -- MAX_HUMAN_HELD_RECOVERY_ATTEMPT cap) so raw infra retries can't
  -- corrupt either of those. Older DBs pre-date this column and get it via
  -- the idempotent `ensure_spawn_failure_count_column` migration in
  -- `SqliteStateStore::open` (same guard pattern as `ensure_is_adopted_column`).
  spawn_failure_count INTEGER NOT NULL DEFAULT 0,
  -- Consecutive transient processing errors since the last successful fast-tier
  -- progress (follow-up to #510/#517). PR #517 isolated per-bead transient
  -- errors from incrementing global consecutive_failures, but a bead hitting
  -- transient errors forever would spin indefinitely. This counter tracks
  -- consecutive transient errors per bead and parks HUMAN_HELD once it reaches
  -- MAX_TRANSIENT_PROCESSING_RETRY (10).
  -- Older DBs pre-date this column and get it via the idempotent
  -- `ensure_transient_error_count_column` migration in `SqliteStateStore::open`.
  transient_error_count INTEGER NOT NULL DEFAULT 0,
  -- Pre-remediation-session HEAD SHA for adopted-branch force-push
  -- detection (bead jleechan-tfs1 amendment). Nullable: only set for
  -- adopted beads that have been through a remediation dispatch. Kept in
  -- sync with the idempotent `ensure_pre_session_head_sha_column`
  -- migration in `SqliteStateStore::open` (same guard pattern as
  -- `ensure_is_adopted_column`).
  pre_session_head_sha TEXT,
  -- Machine-readable reason the bead most recently transitioned to
  -- HUMAN_HELD (bead jleechan-4jn1: live incident jleechan-93ft / PR
  -- worldarchitect.ai#7888). Set alongside every state = 'HUMAN_HELD'
  -- write. `recover_human_held` filters on this column: rows whose
  -- park_reason starts with 'circuit-breaker' are EXCLUDED from automatic
  -- requeue (the circuit breaker in reroll.rs parks a bead specifically to
  -- STOP retrying after repeated identical rejections — requeuing it
  -- defeats the purpose and caused a 769x re-trigger loop in production).
  -- Other park reasons (session_stalled, autonomy_timebox_exceeded, etc.)
  -- are unaffected. Nullable: NULL for beads never parked, and cleared
  -- back to NULL by `recover_human_held` on successful requeue. Kept in
  -- sync with the idempotent `ensure_park_reason_column` migration in
  -- `SqliteStateStore::open` (same guard pattern as
  -- `ensure_is_adopted_column`).
  park_reason TEXT,
  -- Per-bead repo identity (bead jleechan-35y4, Stage A of the multi-repo
  -- dispatch fix; docs/multirepo-dispatch-investigation-2026-07-11.md).
  -- NULL ("legacy") means "use the daemon's global cfg.target_repo" — see
  -- `BeadOverlay::repo` in daemon/src/state.rs, the single accessor every
  -- call site must use instead of re-implementing this fallback. Set by
  -- intake from an explicit `target_repo:` body field, else the
  -- `owner/repo` prefix of the bead's external_ref, else left NULL. Older
  -- DBs pre-date this column and get it via the idempotent
  -- `ensure_target_repo_column` migration in `SqliteStateStore::open` (same
  -- guard pattern as `ensure_is_adopted_column`).
  target_repo TEXT,
  -- Immutable spawn-time routing provenance for a persisted AO session.
  ao_project TEXT,
  -- Consecutive re-roll deferral counter (bead jleechan-zeij / issue #322
  -- r2). The fail-closed re-roll proceed predicate in daemon/src/reroll.rs
  -- supersedes a worker ONLY once it can positively confirm the previous
  -- session is safe to replace (SessionNotFound, terminal+stable HEAD, or
  -- idle+stable HEAD). When it cannot (active session, moving HEAD, failed
  -- stop()), it DEFERS instead of parking: the bead is left ATTESTED and
  -- retried next tick, incrementing this counter, and only escalates to
  -- HUMAN_HELD once the counter hits MAX_REROLL_DEFERRALS. Reset to 0 on a
  -- confirmed proceed. Owned by the reroll engine via the
  -- `reroll_deferral_count`/`incr_reroll_deferral`/`reset_reroll_deferral`
  -- StateStore methods (NOT a BeadOverlay field — same decoupling as
  -- `attempt_er_runner_count`). Older DBs pre-date this column and get it via
  -- the idempotent `ensure_reroll_deferral_count_column` migration in
  -- `SqliteStateStore::open` (same guard pattern as `ensure_is_adopted_column`).
  reroll_deferral_count INTEGER NOT NULL DEFAULT 0,
  -- Consecutive PERMANENT (non-transient) reroll head-probe failure counter
  -- (advice-627-630-20260809 PR #628 finding 2). The fail-closed
  -- `evaluate_proceed` head-stability check in daemon/src/reroll.rs samples
  -- `Vcs::head_sha_within_for_repo` every poll; a TRANSIENT failure
  -- (`DaemonError::is_transient() == true`) is deferred and logged via
  -- `REROLL_QUIESCENCE_HEAD_TRANSIENT` without touching this column, but a
  -- PERMANENT (non-transient) failure -- a genuinely misconfigured repo,
  -- deleted branch, or expired `gh` auth -- still defers (keep
  -- defer-not-crash) while incrementing this counter, so the daemon can
  -- distinguish "one bad tick" from "this bead has been silently failing
  -- the same way for N ticks in a row" and escalate a loud warning once the
  -- count crosses `reroll_head_permanent_fail_threshold()` (default 3,
  -- env-overridable via `DARK_FACTORY_REROLL_HEAD_PERMANENT_FAIL_THRESHOLD`).
  -- Reset to 0 on any successful probe. Owned by the reroll engine via the
  -- `reroll_head_permanent_failure_count`/`incr_reroll_head_permanent_failure`/
  -- `reset_reroll_head_permanent_failure` StateStore methods (NOT a
  -- BeadOverlay field — same decoupling as `reroll_deferral_count`). Older
  -- DBs pre-date this column and get it via the idempotent
  -- `ensure_reroll_head_permanent_failure_count_column` migration in
  -- `SqliteStateStore::open` (same guard pattern as
  -- `ensure_reroll_deferral_count_column`).
  reroll_head_permanent_failure_count INTEGER NOT NULL DEFAULT 0,
  -- Bead jleechan-zaga / issue #348 r3: earliest unix epoch (seconds) at
  -- which a bead held at DISPOSITION_REQUIRED may be re-assessed by the fast
  -- tier. NULL means "no cooldown / re-assess now". Set to now +
  -- `held_recheck_cooldown_secs` whenever the daemon (re)holds a bead, so a
  -- persistent structural condition (CodeRabbit unavailable for hours) does
  -- not re-hit the SCM API every fast tick. Owned by the tick engine via the
  -- `held_recheck_after`/`set_held_recheck_after` StateStore methods (NOT a
  -- BeadOverlay field — same decoupling as `reroll_deferral_count`). Older
  -- DBs pre-date this column and get it via the idempotent
  -- `ensure_held_recheck_after_column` migration in `SqliteStateStore::open`.
  held_recheck_after INTEGER,
  -- Bead jleechan-yoqy / issue #323: hash of the PR body's canonical evidence
  -- marker (`**Evidence**:` + gist + head) at the bead's last /er run. NULL =
  -- no run recorded. `er_runner::maybe_run` re-triggers /er when this differs
  -- from the current body's marker hash (an evidence-only body update, same
  -- head commit). Owned by the er_runner via the `last_er_evidence_hash`/
  -- `set_er_evidence_hash` StateStore methods (NOT a BeadOverlay field). Older
  -- DBs get it via the idempotent `ensure_last_er_evidence_hash_column`
  -- migration in `SqliteStateStore::open`.
  last_er_evidence_hash TEXT,
  -- Bead jleechan-6l1f: boolean flag the daemon stamps to the bead's latest
  -- gate assessment result (`true` iff `report.all_green == true`). Used by
  -- the regression-detection path in `tick::run_fast_tier` to recognise the
  -- green->red transition (a previously-green PR whose CI/review went red
  -- must NOT silently sit READY; live incident PR #540 dead-ended because
  -- the daemon recorded all_green=true once but never re-detected when CI
  -- later went red). Default 0 (= "never green") is the safe default: a
  -- bead that has never been green cannot be a regression candidate.
  -- Owned by `tick::run_fast_tier` via `last_all_green`/`set_last_all_green`
  -- (NOT a BeadOverlay field, same decoupling as `last_er_evidence_hash`).
  -- Older DBs get it via the idempotent `ensure_last_all_green_columns`
  -- migration in `SqliteStateStore::open`.
  last_all_green INTEGER NOT NULL DEFAULT 0,
  -- Bead jleechan-6l1f: cumulative green->red transition count for the bead.
  -- Used to enforce `tick::MAX_GATE_REGRESSIONS` (default 3) so a flapping
  -- check cannot ping-pong a bead through the reroll lane forever; once
  -- the cap is hit the daemon emits `GATE_REGRESSED_CAPPED` and parks
  -- HUMAN_HELD with `park_reason='gate_regression_capped'` (a new distinct
  -- reason, so the circuit-breaker-style retry suppression in
  -- `recover_human_held` does not requeue the bead identically to a
  -- transient red). Owned by the tick engine via `gate_regression_count`/
  -- `incr_gate_regression_count` (NOT a BeadOverlay field). Older DBs get
  -- it via the idempotent `ensure_last_all_green_columns` migration in
  -- `SqliteStateStore::open`.
  gate_regression_count INTEGER NOT NULL DEFAULT 0,
  -- Bead jleechan-g1ib / CLAIMED tag coordination: which machine holds the
  -- multi-machine claim for this bead, if any. NULL means "no claim" (free to
  -- dispatch). The tick dispatch loop skips rows where `claimed_by IS NOT NULL
  -- AND claimed_at > now - ttl_secs` (default ttl=30 min), so a machine crash
  -- that leaves a stale claim eventually frees the bead. Claim/release/heartbeat
  -- are atomic via `StateStore::try_claim`/`release_claim`/`heartbeat_claim`.
  -- Pre-existing rows legitimately default to NULL. Older DBs get these via the
  -- idempotent `ensure_claimed_by_columns` migration in `SqliteStateStore::open`.
  claimed_by TEXT,
  claimed_at INTEGER,
  -- Per-attempt wall-clock anchor (bead bze8.3: redispatch must not inherit
  -- elapsed autonomy from prior attempts). Unix epoch seconds stamped
  -- atomically when dispatch reservation completes successfully
  -- (`state = DISPATCHED` save in `dispatch::dispatch_ready`). Nullable:
  -- NULL means "this attempt has not yet been dispatched" — the timebox
  -- check in `tick::run_tick` consults this column first and falls back
  -- to cumulative `autonomy_secs` only when NULL (legacy / pre-fix rows
  -- that have never been re-dispatched since this column existed). Cleared
  -- by `recover_human_held` and on every HUMAN_HELD transition so the next
  -- attempt starts with a fresh anchor. Owned by the dispatch reservation,
  -- NOT bumped incrementally like `autonomy_secs` — the timebox wall-clock
  -- check is `now_epoch - attempt_started_at >= cfg.autonomy_timebox_secs`.
  -- Older DBs pre-date this column and get it via the idempotent
  -- `ensure_attempt_started_at_column` migration in `SqliteStateStore::open`
  -- (same guard pattern as `ensure_reroll_deferral_count_column`).
  attempt_started_at INTEGER
);

-- Deletion guard: the daemon/skills may delete ONLY refs recorded here (spec §4.2.8).
CREATE TABLE IF NOT EXISTS branch_registry (
  branch     TEXT PRIMARY KEY,
  bead_id    TEXT NOT NULL,
  created_at TEXT NOT NULL
);

-- Exact identity bound to an adopted PR branch.  branch_registry remains the
-- deletion guard; this table is the proof used when deciding whether a second
-- intake bead is the same PR or an attempted branch steal.
CREATE TABLE IF NOT EXISTS adopted_pr_binding (
  branch     TEXT PRIMARY KEY,
  repo       TEXT NOT NULL,
  pr_number  INTEGER NOT NULL,
  head_sha   TEXT NOT NULL,
  bead_id    TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK (length(repo) > 0),
  CHECK (pr_number > 0),
  CHECK (length(head_sha) > 0)
);


-- Circuit breaker: tracking of review rejections per attempt to detect consecutive failures
CREATE TABLE IF NOT EXISTS review_rejection (
  bead_id       TEXT NOT NULL,
  attempt       INTEGER NOT NULL,
  reviewer      TEXT NOT NULL,
  feedback_hash TEXT NOT NULL,
  feedback_text TEXT NOT NULL,
  created_at    TEXT NOT NULL,
  PRIMARY KEY (bead_id, attempt)
);

-- Durable lifecycle marker for adopted remediation. `pre_session_head_sha`
-- is written before worker spawn for crash reconciliation and cannot prove
-- that remediation actually began. This row is written only after a
-- successful sessions.spawn and DISPATCHED overlay persistence.
CREATE TABLE IF NOT EXISTS remediation_session_spawned (
  bead_id    TEXT PRIMARY KEY,
  attempt    INTEGER NOT NULL,
  updated_at TEXT NOT NULL
);

-- Escalation dedup ledger (1s2q-escalation-dedup): per-(bead_id, reason) record
-- of the last emitted ESCALATION_REQUIRED / ESCALATION_NOTIFICATION_FAILED
-- event's context hash + epoch. The tick engine consults this before emitting:
-- a re-fire is suppressed unless the context hash CHANGED or the last emit is
-- older than `Config::escalation_refire_secs` (default 1h). Stops the live
-- incident where a bead with an identical permanent condition re-fired every
-- ~40s. Legacy DBs that pre-date this table get it via the idempotent
-- `ensure_escalation_ledger_table` migration in `SqliteStateStore::open`
-- (probes `sqlite_master` then `CREATE TABLE IF NOT EXISTS` — safe to call
-- repeatedly). Separate from `review_rejection` (the permanent one-time guard
-- via `escalation_already_recorded`): the ledger is a backoff guard layered
-- ON TOP of that permanent guard.
CREATE TABLE IF NOT EXISTS escalation_ledger (
  bead_id           TEXT NOT NULL,
  reason            TEXT NOT NULL,
  context_hash      TEXT NOT NULL,
  last_emitted_epoch INTEGER NOT NULL,
  -- 1s2q-escalation-dedup Task 2: when 1, this (bead_id, reason) row is
  -- terminal ("escalation_undeliverable") — a permanent (non-transient) gh
  -- error classified by `!DaemonError::is_transient()` made the notification
  -- undeliverable, so the daemon must NEVER re-emit ESCALATION_REQUIRED /
  -- ESCALATION_NOTIFICATION_FAILED for it again. `escalation_should_emit`
  -- returns `Ok(false)` unconditionally when `terminal = 1`, regardless of
  -- context hash or backoff window. Set once by `mark_escalation_undeliverable`
  -- alongside a single final `ESCALATION_UNDELIVERABLE` event; never cleared.
  -- Older DBs that pre-date this column get it via the idempotent
  -- `ensure_escalation_ledger_terminal_column` migration in
  -- `SqliteStateStore::open` (probes `pragma_table_info` then `ALTER TABLE`).
  terminal          INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (bead_id, reason)
);

-- Bead jleechan-g1ib / CLAIMED tag coordination: per-(machine, bead) cache of
-- the peer daemon's last-reported live claims. Populated by `claimd daemon`'s
-- periodic GET /sync (or peer POST /sync push). Used by `StateStore::try_claim`
-- to refuse a local claim when the peer already holds it within the TTL.
-- Replaced wholesale on every sync (delete-then-insert in a transaction), so
-- a missing entry means "peer no longer reports this claim". Expires_at is
-- the peer's own assertion of when its claim dies (no local recompute), so
-- each daemon's TTL choice is honored.
CREATE TABLE IF NOT EXISTS peer_claims (
  machine        TEXT NOT NULL,
  bead_id        TEXT NOT NULL,
  claimed_at     INTEGER NOT NULL,
  expires_at     INTEGER NOT NULL,
  last_synced_at INTEGER NOT NULL,
  PRIMARY KEY (machine, bead_id)
);

-- /er runner state (bead jleechan-qqq): per-bead attempt counter + last
-- attempt timestamp (unix epoch seconds). Used by `er_runner::maybe_run`
-- to enforce a per-bead attempt cap (default MAX_ER_RUNNER_ATTEMPTS=3)
-- and a cooldown window (default ER_RUNNER_COOLDOWN_SECS=300).
--
-- The columns are declared in the CREATE TABLE block above for fresh
-- DBs. Older DBs that pre-date this column pair get them via the
-- idempotent `ensure_er_runner_columns` step in
-- `SqliteStateStore::open`, which checks `pragma_table_info` before
-- issuing an `ALTER TABLE ... ADD COLUMN` (SQLite has no
-- `ADD COLUMN IF NOT EXISTS`). Without that guard, re-running
-- `execute_batch` against an already-migrated DB would fail.
