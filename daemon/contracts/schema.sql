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
                   'REDISPATCHED','BUDGET_HELD','HUMAN_HELD')),
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
  reroll_deferral_count INTEGER NOT NULL DEFAULT 0
);

-- Deletion guard: the daemon/skills may delete ONLY refs recorded here (spec §4.2.8).
CREATE TABLE IF NOT EXISTS branch_registry (
  branch     TEXT PRIMARY KEY,
  bead_id    TEXT NOT NULL,
  created_at TEXT NOT NULL
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

-- Provider-health ledger (reviewer-outage-resilience Task 1): per-vendor row
-- tracking whether each external review-bot provider ("coderabbit" or
-- "bugbot") is currently in-outage or recovered, with strict semantics and a
-- full audit trail. Populated from the production assessment path in
-- `tick::run_fast_tier` via `StateStore::record_vendor_observation`; read by
-- the verification step's outage-aware CI-pending logic. A provider is marked
-- in-outage when its responses carry outage/limit markers or it stays pending
-- for N consecutive assessments; it is marked recovered ONLY when a successful
-- review/status is observed for the PR's current head. The absence of errors
-- alone must never flip the state to recovered, and a successful review must be
-- recorded as a success observation (never as an outage observation). Legacy
-- DBs that pre-date this table get it via the idempotent
-- `ensure_vendor_health_table` migration in `SqliteStateStore::open` (probes
-- `sqlite_master` then `CREATE TABLE IF NOT EXISTS` — safe to call repeatedly).
CREATE TABLE IF NOT EXISTS vendor_health (
  vendor               TEXT PRIMARY KEY,        -- "coderabbit" or "bugbot"
  in_outage            INTEGER NOT NULL DEFAULT 0,  -- 1 = currently in outage, 0 = healthy
  consecutive_pending  INTEGER NOT NULL DEFAULT 0,  -- consecutive assessments where status was "unknown"/pending
  outage_observations  INTEGER NOT NULL DEFAULT 0,  -- total outage marker observations (audit trail)
  success_observations INTEGER NOT NULL DEFAULT 0,  -- total success observations (audit trail)
  last_success_head    TEXT,                    -- PR head SHA of the last successful review/status
  last_outage_epoch    INTEGER,                 -- unix epoch when in_outage was first set to 1
  last_observed_head   TEXT,                    -- PR head SHA at the last observation
  last_observed_epoch  INTEGER                  -- unix epoch of the last observation
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
