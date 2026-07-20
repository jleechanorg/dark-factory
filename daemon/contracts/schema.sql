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
  reroll_deferral_count INTEGER NOT NULL DEFAULT 0,
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
  -- Bead jleechan-yoqy r3 (refs #323): 64-bit hash of the canonical
  -- `**Evidence**: <gist_url>` marker taken from the PR body AT THE TIME
  -- the runner posted its verdict comment. Together with
  -- `last_er_runner_attempt_at` (== head epoch at post time) this forms
  -- the second component of the AlreadyPosted idempotence key — a fresh
  -- evidence-only body update (same head SHA, new gist URL) MUST
  -- re-trigger `/er` instead of being short-circuited by a stale "verdict
  -- posted after this head" guard. Default 0 ("never posted"); older DBs
  -- get this via the idempotent
  -- `ensure_last_er_runner_marker_hash_column` migration in
  -- `SqliteStateStore::open`.
  last_er_runner_marker_hash INTEGER NOT NULL DEFAULT 0
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
