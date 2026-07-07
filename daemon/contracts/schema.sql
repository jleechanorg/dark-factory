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
  updated_at    TEXT    NOT NULL              -- ISO-8601 UTC
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
-- `ALTER TABLE ... ADD COLUMN` is idempotent on modern SQLite when guarded
-- with `IF NOT EXISTS`-style logic; for older DBs, the Rust side detects
-- "no such column" and falls back to (0, None) so the runner still works.
ALTER TABLE bead_overlay ADD COLUMN attempt_er_runner_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE bead_overlay ADD COLUMN last_er_runner_attempt_at INTEGER;
