// Task 4: SQLite-backed overlay state store per contracts/schema.sql and
// design doc §3. WAL + busy_timeout=5000 match the discipline used by the
// Python runner's CXDB (runner/cxdb.py), but this is a separate DB file with
// no cross-language schema coupling.
use crate::errors::DaemonError;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Spec §4.2.7 overlay states, incl. r3 pre-PR states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OverlayState {
    Queued,       // bead accepted by intake, awaiting dispatch
    Dispatching, // dispatch intent + branch registered, spawn not yet confirmed (spec §4.2.2/§4.2.4)
    Dispatched,  // worker/pipeline running, no PR yet
    Attested,    // PR open, under verification
    Ready,       // terminal: 7-green, readiness posted, daemon stops driving
    ReRoll,      // re-roll in progress (Stage 2)
    Recovery,    // spec mutated, awaiting re-dispatch (Stage 2)
    Redispatched, // handed back to the queue
    BudgetHeld,  // budget exhaustion (monitoring-only in Stage 1/2)
    HumanHeld,   // terminal until human action
}

impl OverlayState {
    /// SCREAMING_SNAKE_CASE string mapping matching contracts/schema.sql's CHECK constraint.
    pub fn as_str(&self) -> &'static str {
        match self {
            OverlayState::Queued => "QUEUED",
            OverlayState::Dispatching => "DISPATCHING",
            OverlayState::Dispatched => "DISPATCHED",
            OverlayState::Attested => "ATTESTED",
            OverlayState::Ready => "READY",
            OverlayState::ReRoll => "RE_ROLL",
            OverlayState::Recovery => "RECOVERY",
            OverlayState::Redispatched => "REDISPATCHED",
            OverlayState::BudgetHeld => "BUDGET_HELD",
            OverlayState::HumanHeld => "HUMAN_HELD",
        }
    }

    // Mirrors `OverlayState::as_str` (SCREAMING_SNAKE_CASE strings tied to
    // contracts/schema.sql's CHECK constraint) rather than the general-purpose
    // parsing `std::str::FromStr` implies, so the inherent method is kept
    // instead of adding a trait impl clippy would consider more idiomatic here.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, DaemonError> {
        match s {
            "QUEUED" => Ok(OverlayState::Queued),
            "DISPATCHING" => Ok(OverlayState::Dispatching),
            "DISPATCHED" => Ok(OverlayState::Dispatched),
            "ATTESTED" => Ok(OverlayState::Attested),
            "READY" => Ok(OverlayState::Ready),
            "RE_ROLL" => Ok(OverlayState::ReRoll),
            "RECOVERY" => Ok(OverlayState::Recovery),
            "REDISPATCHED" => Ok(OverlayState::Redispatched),
            "BUDGET_HELD" => Ok(OverlayState::BudgetHeld),
            "HUMAN_HELD" => Ok(OverlayState::HumanHeld),
            other => Err(DaemonError::Parse(format!("unknown overlay state: {other}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BeadOverlay {
    pub bead_id: String,
    pub state: OverlayState,
    pub attempt: u32,       // r<n> counter
    pub reroll_count: u32,
    pub autonomy_secs: u64, // cumulative — nothing on the automated path resets it
    pub spend_usd: f64,     // monitoring-only metric (spec §4.2.8)
    pub pr_number: Option<u64>,
    pub branch: Option<String>,
    pub session_id: Option<String>,
    /// Adopted-PR provenance (bead jleechan-tfs1): `true` iff `branch` is an
    /// external contributor's own head_ref_name (adopted via
    /// `intake::normalize_labeled_prs`, set in `tick::run_slow_tier` at
    /// adoption time), `false` for factory-fabricated branches. `reroll()`
    /// reads this to pick append-only remediation (adopted) vs. the
    /// fabricate-new-branch + close-old-PR path (factory-fabricated).
    /// Explicit stored flag, NOT inferred from branch name.
    pub is_adopted: bool,
    /// Consecutive transient `Sessions::spawn` failures since the last
    /// confirmed `DISPATCHED` (bead jleechan follow-up to #198: a bead whose
    /// spawn deterministically fails never reaches `DISPATCHED`, so it was
    /// invisible to `query_active_overlays`'s `DISPATCHED`/`ATTESTED` scope,
    /// `autonomy_secs` accumulation, and the 30-minute wedge-detection net —
    /// this dedicated counter closes that livelock gap independently of
    /// `attempt` (which already double-duties as the branch/re-roll suffix
    /// AND the `MAX_HUMAN_HELD_RECOVERY_ATTEMPT` cap; overloading it a third
    /// time here would let raw infra retries corrupt both the `factory/<id>-r<n>`
    /// branch numbering and the recovery-cap semantics). Reset to `0` on a
    /// confirmed `DISPATCHED` save; NOT reset by `recover_human_held` (a bead
    /// that gets auto-recovered and immediately fails to spawn again must
    /// re-trip the cap quickly rather than being granted a fresh budget).
    pub spawn_failure_count: u32,
    /// Pre-remediation-session HEAD SHA of the adopted branch, captured by
    /// `reroll::execute_adopted` immediately before dispatching a coder
    /// session onto it (bead jleechan-tfs1 amendment: post-hoc force-push
    /// detection). `None` for factory-fabricated branches (never set) and
    /// for adopted beads that haven't been through a remediation dispatch
    /// yet. While the bead sits `DISPATCHED`, `tick::run_tick`'s
    /// wedge-detection sweep re-checks every tick that this SHA is still an
    /// ancestor of the branch's current tip (`Vcs::is_ancestor`) — if not,
    /// the branch was force-pushed/rewritten since capture, which is a
    /// direct violation of the append-only guarantee for adopted branches,
    /// and the bead is parked `HUMAN_HELD` with an escalation comment
    /// naming both SHAs rather than silently promoted as if remediation
    /// succeeded.
    pub pre_session_head_sha: Option<String>,
    /// Machine-readable reason the bead most recently transitioned to
    /// `HUMAN_HELD` (bead jleechan-4jn1: live incident jleechan-93ft / PR
    /// worldarchitect.ai#7888 — `recover_human_held` was requeuing
    /// circuit-breaker parks identically to transient parks like
    /// `session_stalled`, causing a 769x re-trigger loop of the same
    /// rejected fix in 30 minutes). Set alongside every `state =
    /// HumanHeld` write (`reroll::execute`/`execute_adopted`,
    /// `dispatch::dispatch_ready`, `tick::run_tick`/`run_recovery_step`).
    /// `recover_human_held` filters on this column to exclude
    /// circuit-breaker parks (`"circuit-breaker..."` prefix) from
    /// automatic requeue — those exist specifically to STOP retrying, and
    /// requeuing them defeats their purpose. Other park reasons
    /// (`session_stalled`, `autonomy_timebox_exceeded`, etc.) are
    /// unaffected and keep their existing auto-recovery behavior. `None`
    /// for beads that have never been parked, and cleared back to `None`
    /// by `recover_human_held` on successful requeue.
    pub park_reason: Option<String>,
    /// Per-bead repo identity (bead jleechan-35y4 / Stage A of the
    /// multi-repo dispatch fix, see
    /// `docs/multirepo-dispatch-investigation-2026-07-11.md`). `None` is the
    /// "legacy" case — every overlay record written before this field
    /// existed (and every bead whose intake could not resolve an explicit
    /// repo) has no column value here, which means "use the daemon's global
    /// `cfg.target_repo`" for full backward compatibility. Set by
    /// intake (`intake::resolve_target_repo`) from, in order: an explicit
    /// `target_repo:` body field, else the `owner/repo` prefix of the
    /// bead's `external_ref`, else left `None`. Call sites that need "which
    /// repo does this bead belong to" MUST go through [`BeadOverlay::repo`]
    /// rather than re-implementing this None-means-global fallback
    /// themselves — see the accessor's doc comment for why.
    pub target_repo: Option<String>,
}

impl BeadOverlay {
    /// The single accessor for "which repo does this bead belong to".
    /// Returns the bead's explicit `target_repo` when Stage A intake
    /// resolved one, else falls back to the daemon's global
    /// `cfg.target_repo` (the pre-multi-repo, single-repo behavior).
    /// Every call site that currently reads `cfg.target_repo` directly to
    /// answer "which repo" should be migrated to call `overlay.repo(cfg)`
    /// instead (that full call-site sweep is Stage D / bead jleechan-9xrs —
    /// this accessor is the capability those call sites will adopt one at a
    /// time, not a call-site migration itself).
    pub fn repo<'a>(&'a self, cfg: &'a crate::config::Config) -> &'a str {
        self.target_repo.as_deref().unwrap_or(&cfg.target_repo)
    }
}

pub trait StateStore {
    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError>;
    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError>;
    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError>;
    /// Deletion guard: daemon may delete ONLY refs returned here (spec §4.2.8).
    fn owned_branches(&self) -> Result<Vec<String>, DaemonError>;
    /// Reverse-lookup: branch → bead_id (used by fast_tier to find drive-existing-pr beads).
    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError>;
    /// Return the overlay rows currently in `DISPATCHED` or `ATTESTED`. The
    /// caller decides whether (and when) to bump `autonomy_secs` per row via
    /// [`bump_autonomy_secs`] — splitting these two ops is what lets the
    /// `ci_pending` pause (jleechan-54ky / sub-fix for jleechan-gib) freeze
    /// the autonomy clock for healthy PRs that are waiting on slow CI,
    /// instead of silently burning the 3h timebox against operator/CI
    /// wall-clock time and parking the bead `HUMAN_HELD`.
    fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError>;
    /// Increment a single overlay's `autonomy_secs` by `delta_secs` and
    /// refresh `updated_at`. Pair with [`list_active_overlays`] when the
    /// caller needs to skip the bump for specific rows (e.g.
    /// `ci_pending=true`); the SQL-level "increment everything" form that
    /// [`increment_active_autonomy`] used to provide is preserved below as
    /// a default-method convenience for callers that don't need the skip.
    fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), DaemonError>;
    /// Convenience for callers that don't need the ci_pending pause:
    /// list all active overlays AND bump their autonomy_secs by
    /// `elapsed_secs` in one call. Equivalent to `list_active_overlays`
    /// followed by `bump_autonomy_secs` for every returned row.
    fn increment_active_autonomy(&self, elapsed_secs: u64) -> Result<Vec<BeadOverlay>, DaemonError> {
        let overlays = self.list_active_overlays()?;
        for overlay in &overlays {
            if elapsed_secs > 0 {
                self.bump_autonomy_secs(&overlay.bead_id, elapsed_secs)?;
            }
        }
        // Re-read so the returned rows reflect the just-applied bump; this
        // matches the original "increment then return" semantics the tick
        // loop depended on for the budget-warning crossing check.
        self.list_active_overlays()
    }
    /// Requeue every `HUMAN_HELD` bead whose attempt is below `max_attempt`
    /// back to `QUEUED`, incrementing `attempt` and zeroing `autonomy_secs`.
    /// Returns the recovered overlays so the caller can emit telemetry.
    /// This is the Rust port of the shell overlay's `recover-held`
    /// (daemon/factory-overlay.sh:319), and the fix for jleechan-gib's
    /// "100% of intake dead-ends terminally" blocker: without an automated
    /// exit from `HUMAN_HELD`, every non-green gate assessment parks a
    /// bead forever.
    fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, DaemonError> {
        let _ = max_attempt;
        Ok(Vec::new())
    }
    /// Return `HUMAN_HELD` overlays that have reached the automated recovery
    /// cap and therefore require explicit escalation instead of silent retry
    /// suppression.
    fn human_held_at_or_above_attempt(
        &self,
        max_attempt: u32,
    ) -> Result<Vec<BeadOverlay>, DaemonError>;
    fn save_rejection(&self, bead_id: &str, attempt: u32, reviewer: &str, feedback_hash: &str, feedback_text: &str) -> Result<(), DaemonError>;
    fn load_rejection(&self, bead_id: &str, attempt: u32) -> Result<Option<(String, String)>, DaemonError>;
    /// Read back the raw feedback text for a stored rejection (companion to
    /// `load_rejection`, which only returns `(reviewer, feedback_hash)`). The
    /// reroll circuit-breaker's semantic comparison (spec §4.2.6) needs the
    /// actual text, not just the hash, to ask the model whether two rejections
    /// describe the same underlying issue. Default `Ok(None)` so stores that
    /// predate this feature (or fakes that don't need reroll's circuit-breaker
    /// exercised) don't need to implement it; `None` means the circuit-breaker
    /// safely no-ops (never fires) rather than erroring or guessing.
    fn load_rejection_text(&self, _bead_id: &str, _attempt: u32) -> Result<Option<String>, DaemonError> {
        Ok(None)
    }
    /// Read the `(attempt_count, last_attempt_epoch_secs)` pair for the
    /// `/er` runner (bead jleechan-qqq). Default impl returns `(0, None)`
    /// so test fakes that don't override it get the "never spawned" state.
    fn er_runner_attempt(&self, _bead_id: &str) -> Result<(u32, Option<u64>), DaemonError> {
        Ok((0, None))
    }
    /// Atomically increment the `/er` runner attempt counter for `bead_id`
    /// and stamp `last_attempt_epoch_secs` to `now_epoch`. Returns the
    /// new count. Default impl just returns `1` so fakes that don't
    /// override it still satisfy the call (they aren't used in production).
    fn incr_er_runner_attempt(&self, _bead_id: &str, _now_epoch: u64) -> Result<u32, DaemonError> {
        Ok(1)
    }
    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        Ok(())
    }
}

/// `StateStore` impl against `~/.dark-factory/daemon-cxdb.sqlite` (WAL mode,
/// 5s busy_timeout — same discipline as `runner/cxdb.py`, but a separate DB
/// file: no cross-language schema coupling with the Python runner's CXDB).
pub struct SqliteStateStore {
    conn: Connection,
}

fn tool_err(op: &str, e: rusqlite::Error) -> DaemonError {
    DaemonError::Tool {
        tool: "sqlite".into(),
        rc: -1,
        stderr: format!("{op}: {e}"),
    }
}

fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Minimal dependency-free ISO-8601 UTC formatting (no chrono per design doc §2's
    // five-dependency budget: rusqlite, serde, serde_json, toml, thiserror).
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn is_permanent_human_hold_reason(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| {
        reason.starts_with("circuit-breaker")
            || matches!(
                reason,
                "unmapped_target_repo"
                    | "worktree_remote_mismatch"
                    | "worktree_remote_unverifiable"
                    | "spawn_cleanup_failed"
                    | "spawn_branch_mismatch"
                    | "ambiguous_dispatching_recovery"
            )
    })
}

/// Howard Hinnant's civil_from_days algorithm (public domain), days since epoch -> (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

impl SqliteStateStore {
    /// Open (or create) the on-disk store at `path`, apply WAL + busy_timeout=5000,
    /// and ensure the schema from `contracts/schema.sql` exists.
    pub fn open(path: &Path) -> Result<Self, DaemonError> {
        let conn = Connection::open(path).map_err(|e| tool_err("open", e))?;
        Self::configure(&conn, false)?;
        conn.execute_batch(include_str!("../contracts/schema.sql"))
            .map_err(|e| tool_err("apply schema", e))?;
        Self::ensure_er_runner_columns(&conn)?;
        Self::ensure_is_adopted_column(&conn)?;
        Self::ensure_spawn_failure_count_column(&conn)?;
        Self::ensure_pre_session_head_sha_column(&conn)?;
        Self::ensure_park_reason_column(&conn)?;
        Self::ensure_target_repo_column(&conn)?;
        Ok(Self { conn })
    }

    /// In-memory store for tests: same pragmas, schema supplied by the caller
    /// (normally `include_str!("../contracts/schema.sql")`).
    pub fn open_in_memory_with_schema(schema_sql: &str) -> Result<Self, DaemonError> {
        let conn = Connection::open_in_memory().map_err(|e| tool_err("open", e))?;
        Self::configure(&conn, true)?;
        conn.execute_batch(schema_sql)
            .map_err(|e| tool_err("apply schema", e))?;
        Self::ensure_er_runner_columns(&conn)?;
        Self::ensure_is_adopted_column(&conn)?;
        Self::ensure_spawn_failure_count_column(&conn)?;
        Self::ensure_pre_session_head_sha_column(&conn)?;
        Self::ensure_park_reason_column(&conn)?;
        Self::ensure_target_repo_column(&conn)?;
        Ok(Self { conn })
    }

    /// Idempotent migration for the `/er` runner columns (bead jleechan-qqq).
    /// Older on-disk DBs predate the columns declared in the CREATE TABLE
    /// block; SQLite has no `ADD COLUMN IF NOT EXISTS`, so we probe
    /// `pragma_table_info` first and only ALTER when a column is missing.
    /// Safe to call repeatedly — a no-op when both columns are already
    /// present.
    fn ensure_er_runner_columns(conn: &Connection) -> Result<(), DaemonError> {
        let has_count: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'attempt_er_runner_count'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: pragma count", e))?;
        if !has_count {
            conn.execute(
                "ALTER TABLE bead_overlay \
                 ADD COLUMN attempt_er_runner_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: add count", e))?;
        }
        let has_last: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'last_er_runner_attempt_at'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: pragma last", e))?;
        if !has_last {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN last_er_runner_attempt_at INTEGER",
                [],
            )
            .map_err(|e| tool_err("ensure_er_runner_columns: add last", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `is_adopted` column (bead jleechan-tfs1).
    /// Same `pragma_table_info` probe-then-`ALTER` pattern as
    /// `ensure_er_runner_columns` — older on-disk DBs predate the column
    /// declared in the CREATE TABLE block, and SQLite has no
    /// `ADD COLUMN IF NOT EXISTS`. Safe to call repeatedly; defaults every
    /// pre-existing row to `0` (factory-fabricated), which is the correct
    /// conservative default since only adopted-PR intake ever sets it `1`.
    fn ensure_is_adopted_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_is_adopted: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'is_adopted'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_is_adopted_column: pragma", e))?;
        if !has_is_adopted {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN is_adopted INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_is_adopted_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `spawn_failure_count` column (follow-up
    /// to #198's dispatch-batch-isolation fix). Same probe-then-`ALTER`
    /// pattern as `ensure_is_adopted_column`. Defaults every pre-existing
    /// row to `0` — conservative, since a bead already `DISPATCHED` has by
    /// definition not accumulated any unresolved transient spawn failures.
    fn ensure_spawn_failure_count_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_spawn_failure_count: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'spawn_failure_count'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_spawn_failure_count_column: pragma", e))?;
        if !has_spawn_failure_count {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN spawn_failure_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_spawn_failure_count_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `pre_session_head_sha` column (bead
    /// jleechan-tfs1 amendment: post-hoc force-push detection). Same
    /// probe-then-`ALTER` pattern as `ensure_is_adopted_column`. Nullable —
    /// every pre-existing row (and every non-adopted row) legitimately has
    /// no baseline SHA.
    fn ensure_pre_session_head_sha_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'pre_session_head_sha'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_pre_session_head_sha_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN pre_session_head_sha TEXT",
                [],
            )
            .map_err(|e| tool_err("ensure_pre_session_head_sha_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `park_reason` column (bead jleechan-4jn1:
    /// live incident jleechan-93ft / PR worldarchitect.ai#7888). Same
    /// probe-then-`ALTER` pattern as `ensure_is_adopted_column`. Nullable —
    /// every pre-existing row (and every row that has never been parked)
    /// legitimately has no reason recorded.
    fn ensure_park_reason_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'park_reason'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_park_reason_column: pragma", e))?;
        if !has_col {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN park_reason TEXT", [])
                .map_err(|e| tool_err("ensure_park_reason_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `target_repo` column (bead jleechan-35y4,
    /// Stage A of the multi-repo dispatch fix). Same probe-then-`ALTER`
    /// pattern as `ensure_park_reason_column`. Nullable, and every
    /// pre-existing row legitimately has no value here — `None` means "use
    /// the global `cfg.target_repo`" (see `BeadOverlay::repo`), which is
    /// exactly the pre-migration single-repo behavior every existing row
    /// implicitly had.
    fn ensure_target_repo_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'target_repo'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_target_repo_column: pragma", e))?;
        if !has_col {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN target_repo TEXT", [])
                .map_err(|e| tool_err("ensure_target_repo_column: add column", e))?;
        }
        Ok(())
    }

    /// `is_memory` distinguishes the two `configure` call sites: `open()` (file-backed,
    /// `is_memory=false`) and `open_in_memory_with_schema()` (`is_memory=true`). WAL is a
    /// documented no-op against `:memory:` connections, so failures/non-"wal" readbacks are
    /// ignored there; on a real file, a failure to switch journal modes (unsupported
    /// filesystem, permissions, NFS, etc.) must not be silently swallowed (jleechan-8in).
    fn configure(conn: &Connection, is_memory: bool) -> Result<(), DaemonError> {
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .map_err(|e| tool_err("busy_timeout", e))?;
        let mode: rusqlite::Result<String> =
            conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0));
        if is_memory {
            // WAL is a no-op (and briefly errors) against :memory: connections; ignore there.
            return Ok(());
        }
        match mode {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => Ok(()),
            Ok(mode) => Err(DaemonError::Config(format!(
                "PRAGMA journal_mode=WAL did not take effect on file-backed DB: got \"{mode}\""
            ))),
            Err(e) => Err(DaemonError::Config(format!(
                "PRAGMA journal_mode=WAL failed on file-backed DB: {e}"
            ))),
        }
    }

    /// Shared SELECT for `list_active_overlays` and `increment_active_autonomy`'s
    /// "read after bump" return value. Filters to `DISPATCHED` + `ATTESTED`
    /// (the only states where autonomy_secs is allowed to accumulate per
    /// spec §4.2.8).
    fn query_active_overlays(&self, op: &str) -> Result<Vec<BeadOverlay>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, \
                 park_reason, target_repo \
                 FROM bead_overlay WHERE state IN ('DISPATCHED', 'ATTESTED')",
            )
            .map_err(|e| tool_err(&format!("{op} prepare"), e))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            })
            .map_err(|e| tool_err(&format!("{op} query"), e))?;
        let mut out = Vec::new();
        for r in rows {
            let (bead_id, state_str, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, park_reason, target_repo) =
                r.map_err(|e| tool_err(&format!("{op} row"), e))?;
            out.push(BeadOverlay {
                bead_id,
                state: OverlayState::from_str(&state_str)?,
                attempt: attempt as u32,
                reroll_count: reroll_count as u32,
                autonomy_secs: autonomy_secs as u64,
                spend_usd,
                pr_number: pr_number.map(|v| v as u64),
                branch,
                session_id,
                is_adopted: is_adopted != 0,
                spawn_failure_count: spawn_failure_count as u32,
                pre_session_head_sha,
                park_reason,
                target_repo,
            });
        }
        Ok(out)
    }
}

impl StateStore for SqliteStateStore {
    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "UPDATE bead_overlay \
                 SET state = 'HUMAN_HELD', park_reason = 'ambiguous_dispatching_recovery' \
                 WHERE state = 'DISPATCHING'",
                [],
            )
            .map_err(|e| tool_err("reconcile_dispatching", e))?;
        Ok(())
    }

    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
        self.conn
            .query_row(
                "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, \
                 park_reason, target_repo \
                 FROM bead_overlay WHERE bead_id = ?1",
                params![bead_id],
                |row| {
                    let state_str: String = row.get(1)?;
                    let is_adopted: i64 = row.get(9)?;
                    let spawn_failure_count: i64 = row.get(10)?;
                    let pre_session_head_sha: Option<String> = row.get(11)?;
                    let park_reason: Option<String> = row.get(12)?;
                    let target_repo: Option<String> = row.get(13)?;
                    Ok((
                        state_str,
                        BeadOverlay {
                            bead_id: row.get(0)?,
                            state: OverlayState::Queued, // placeholder, replaced below
                            attempt: row.get(2)?,
                            reroll_count: row.get(3)?,
                            autonomy_secs: row.get(4)?,
                            spend_usd: row.get(5)?,
                            pr_number: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                            branch: row.get(7)?,
                            session_id: row.get(8)?,
                            is_adopted: is_adopted != 0,
                            spawn_failure_count: spawn_failure_count as u32,
                            pre_session_head_sha,
                            park_reason,
                            target_repo,
                        },
                    ))
                },
            )
            .optional()
            .map_err(|e| tool_err("load", e))?
            .map(|(state_str, mut overlay)| {
                overlay.state = OverlayState::from_str(&state_str)?;
                Ok(overlay)
            })
            .transpose()
    }

    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO bead_overlay \
                 (bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id, updated_at, is_adopted, spawn_failure_count, pre_session_head_sha, park_reason, target_repo) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
                 ON CONFLICT(bead_id) DO UPDATE SET \
                   state=excluded.state, attempt=excluded.attempt, reroll_count=excluded.reroll_count, \
                   autonomy_secs=excluded.autonomy_secs, spend_usd=excluded.spend_usd, \
                   pr_number=excluded.pr_number, branch=excluded.branch, session_id=excluded.session_id, updated_at=excluded.updated_at, \
                   is_adopted=excluded.is_adopted, spawn_failure_count=excluded.spawn_failure_count, pre_session_head_sha=excluded.pre_session_head_sha, \
                   park_reason=excluded.park_reason, target_repo=excluded.target_repo",
                params![
                    overlay.bead_id,
                    overlay.state.as_str(),
                    overlay.attempt,
                    overlay.reroll_count,
                    overlay.autonomy_secs,
                    overlay.spend_usd,
                    overlay.pr_number.map(|v| v as i64),
                    overlay.branch,
                    overlay.session_id,
                    now_iso8601(),
                    overlay.is_adopted as i64,
                    overlay.spawn_failure_count,
                    overlay.pre_session_head_sha,
                    overlay.park_reason,
                    overlay.target_repo,
                ],
            )
            .map_err(|e| tool_err("save", e))?;
        Ok(())
    }

    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError> {
        if let Some(existing) = self.bead_id_for_branch(branch)? {
            if existing == bead_id {
                return Ok(());
            }
            return Err(DaemonError::Config(format!(
                "branch {branch} is already registered to bead {existing}; refusing to reassign to {bead_id}"
            )));
        }
        self.conn
            .execute(
                "INSERT INTO branch_registry (branch, bead_id, created_at) VALUES (?1, ?2, ?3)",
                params![branch, bead_id, now_iso8601()],
            )
            .map_err(|e| tool_err("register_branch", e))?;
        Ok(())
    }

    fn owned_branches(&self) -> Result<Vec<String>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare("SELECT branch FROM branch_registry ORDER BY branch")
            .map_err(|e| tool_err("owned_branches prepare", e))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| tool_err("owned_branches query", e))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(|e| tool_err("owned_branches row", e))?);
        }
        Ok(out)
    }

    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare("SELECT bead_id FROM branch_registry WHERE branch = ?1")
            .map_err(|e| tool_err("bead_id_for_branch prepare", e))?;
        let mut rows = stmt
            .query_map(params![branch], |row| row.get::<_, String>(0))
            .map_err(|e| tool_err("bead_id_for_branch query", e))?;
        if let Some(Ok(bead)) = rows.next() {
            Ok(Some(bead))
        } else {
            Ok(None)
        }
    }

    fn increment_active_autonomy(&self, elapsed_secs: u64) -> Result<Vec<BeadOverlay>, DaemonError> {
        // Default-method wiring in the trait calls list_active_overlays +
        // bump_autonomy_secs; we still need to override here so the bump
        // happens via the existing single-UPDATE path (cheaper than
        // per-row updates when no caller asks for the ci_pending skip).
        if elapsed_secs > 0 {
            self.conn.execute(
                "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + ?1, updated_at = ?2 \
                 WHERE state IN ('DISPATCHED', 'ATTESTED')",
                params![elapsed_secs, now_iso8601()],
            ).map_err(|e| tool_err("increment_active_autonomy update", e))?;
        }
        self.list_active_overlays()
    }

    fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.query_active_overlays("list_active_overlays")
    }

    fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), DaemonError> {
        if delta_secs == 0 {
            return Ok(());
        }
        self.conn
            .execute(
                "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + ?1, updated_at = ?2 \
                 WHERE bead_id = ?3",
                params![delta_secs, now_iso8601(), bead_id],
            )
            .map_err(|e| tool_err("bump_autonomy_secs", e))?;
        Ok(())
    }

    fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, DaemonError> {
        // P2 fix (Codex review): the post-UPDATE SELECT must return ONLY the
        // rows we just flipped — earlier versions matched every QUEUED bead
        // with autonomy_secs=0 in the DB, polluting recovery telemetry with
        // beads that were never HUMAN_HELD. Capture the bead_ids first, then
        // SELECT WHERE bead_id IN (...). rusqlite has no clean RETURNING
        // support, so two statements + an in-memory id list is the simplest
        // fix.
        //
        // bead jleechan-4jn1 (live incident jleechan-93ft / PR
        // worldarchitect.ai#7888): `park_reason LIKE 'circuit-breaker%'`
        // rows are EXCLUDED from automatic requeue. The circuit breaker
        // (reroll.rs) parks a bead HUMAN_HELD specifically to STOP retrying
        // after the same reviewer rejects the same underlying issue twice
        // in a row — treating that park identically to a transient one
        // (`session_stalled`, `autonomy_timebox_exceeded`) caused a 769x
        // re-trigger loop of the same rejected fix in 30 minutes in
        // production. `park_reason IS NULL` rows (pre-migration data, or
        // any park site that hasn't been updated to set a reason) keep the
        // pre-existing auto-recovery behavior — the exclusion is opt-in via
        // the `circuit-breaker` prefix, not opt-out via NULL.
        //
        // bead jleechan-35y4 (adversarial review of PR #245): `park_reason =
        // 'unmapped_target_repo'` rows are EXCLUDED for the same reason —
        // an unmapped repo is a CONFIG problem (missing `[repos.*]` entry),
        // not something a bare requeue can fix. Without this exclusion the
        // bead would ping-pong HUMAN_HELD -> QUEUED -> re-park identically
        // every recovery cycle (attempt incrementing each time, up to the
        // `max_attempt` cap) instead of staying HUMAN_HELD until an
        // operator adds the missing config entry — silently defeating the
        // "fail loud, never guess" intent `dispatch::dispatch_ready`'s
        // unmapped-repo park is supposed to provide.
        //
        // bead jleechan-bqdv Stage C: `park_reason = 'worktree_remote_mismatch'`
        // rows are EXCLUDED for the same reason as `unmapped_target_repo` —
        // a worktree that cloned with the wrong remote is either a local
        // checkout/config problem (the AO project's repo clone itself is
        // misconfigured) or a genuine near-miss that needs a human's eyes
        // before any coder is spawned into that worktree again; it is never
        // something a bare requeue alone fixes, and mirroring
        // `unmapped_target_repo`'s auto-requeue exclusion keeps the two
        // "spawn-time fail loud" park reasons behaviorally consistent.
        // `worktree_remote_unverifiable` is the same safety class: absence
        // of inspectable remote evidence must not become permission to
        // respawn into the same opaque workspace on the next recovery pass.
        // `spawn_cleanup_failed` is also permanent: the daemon knows a
        // session may still be live because `ao session kill` failed. Auto-
        // requeueing that row would create a duplicate worker while erasing
        // the retained session identity needed for operator cleanup.
        // `ambiguous_dispatching_recovery` is the startup fail-safe for a
        // process crash or state-write failure after spawn may have begun.
        // The retained branch/session fields are the operator's recovery
        // handle; automatically requeueing would risk a duplicate worker.
        let mut id_stmt = self
            .conn
            .prepare(
                "SELECT bead_id, park_reason FROM bead_overlay \
                 WHERE state = 'HUMAN_HELD' AND attempt < ?1",
            )
            .map_err(|e| tool_err("recover_human_held id select prepare", e))?;
        let recovered_ids: Vec<String> = id_stmt
            .query_map(params![max_attempt as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })
            .map_err(|e| tool_err("recover_human_held id select query", e))?
            .filter_map(|row| row.ok())
            .filter_map(|(bead_id, park_reason)| {
                (!is_permanent_human_hold_reason(park_reason.as_deref())).then_some(bead_id)
            })
            .collect();
        if recovered_ids.is_empty() {
            return Ok(Vec::new());
        }
        // P2 fix (Codex review): clear stale PR metadata on requeue.
        // `pr_number` and `session_id` belong to the prior (failed) attempt
        // and would otherwise be carried into the new dispatch — `dispatch_ready`
        // overwrites `branch` but leaves the other fields, so the fast tier
        // would treat the freshly-DISPATCHED row as already ATTESTED against
        // the dead PR and re-park on the same gate. `branch` is kept so the
        // recovered-from telemetry still records what was being worked on;
        // dispatch will rewrite it on the next attempt.
        let placeholders = std::iter::repeat_n("?", recovered_ids.len()).collect::<Vec<_>>().join(",");
        let update_sql = format!(
            "UPDATE bead_overlay \
             SET state = 'QUEUED', attempt = attempt + 1, autonomy_secs = 0, \
                 pr_number = NULL, session_id = NULL, park_reason = NULL, updated_at = ?1 \
             WHERE bead_id IN ({})",
            placeholders
        );
        let now = now_iso8601();
        let mut update_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(recovered_ids.len() + 1);
        update_params.push(&now);
        for id in &recovered_ids {
            update_params.push(id as &dyn rusqlite::ToSql);
        }
        self.conn
            .execute(&update_sql, &update_params[..])
            .map_err(|e| tool_err("recover_human_held update", e))?;
        // SELECT the exact rows we just flipped — bounded by the captured
        // id list, not by state+autonomy heuristics that could match other rows.
        let select_sql = format!(
            "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
             pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, \
             park_reason, target_repo \
             FROM bead_overlay WHERE bead_id IN ({})",
            placeholders
        );
        let mut select_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(recovered_ids.len());
        for id in &recovered_ids {
            select_params.push(id as &dyn rusqlite::ToSql);
        }
        let mut stmt = self
            .conn
            .prepare(&select_sql)
            .map_err(|e| tool_err("recover_human_held prepare", e))?;
        let rows = stmt
            .query_map(&select_params[..], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            })
            .map_err(|e| tool_err("recover_human_held query", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (bead_id, state_str, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, park_reason, target_repo) =
                r.map_err(|e| tool_err("recover_human_held row", e))?;
            out.push(BeadOverlay {
                bead_id,
                state: OverlayState::from_str(&state_str)?,
                attempt: attempt as u32,
                reroll_count: reroll_count as u32,
                autonomy_secs: autonomy_secs as u64,
                spend_usd,
                pr_number: pr_number.map(|v| v as u64),
                branch,
                session_id,
                is_adopted: is_adopted != 0,
                spawn_failure_count: spawn_failure_count as u32,
                pre_session_head_sha,
                park_reason,
                target_repo,
            });
        }
        Ok(out)
    }

    fn human_held_at_or_above_attempt(
        &self,
        max_attempt: u32,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, \
                 park_reason, target_repo \
                 FROM bead_overlay WHERE state = 'HUMAN_HELD' AND attempt >= ?1",
            )
            .map_err(|e| tool_err("human_held_at_or_above_attempt prepare", e))?;
        let rows = stmt
            .query_map(params![max_attempt as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            })
            .map_err(|e| tool_err("human_held_at_or_above_attempt query", e))?;
        let mut out = Vec::new();
        for r in rows {
            let (bead_id, state_str, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id, is_adopted, spawn_failure_count, pre_session_head_sha, park_reason, target_repo) =
                r.map_err(|e| tool_err("human_held_at_or_above_attempt row", e))?;
            out.push(BeadOverlay {
                bead_id,
                state: OverlayState::from_str(&state_str)?,
                attempt: attempt as u32,
                reroll_count: reroll_count as u32,
                autonomy_secs: autonomy_secs as u64,
                spend_usd,
                pr_number: pr_number.map(|v| v as u64),
                branch,
                session_id,
                is_adopted: is_adopted != 0,
                spawn_failure_count: spawn_failure_count as u32,
                pre_session_head_sha,
                park_reason,
                target_repo,
            });
        }
        Ok(out)
    }

    fn save_rejection(&self, bead_id: &str, attempt: u32, reviewer: &str, feedback_hash: &str, feedback_text: &str) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO review_rejection (bead_id, attempt, reviewer, feedback_hash, feedback_text, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(bead_id, attempt) DO UPDATE SET \
                   reviewer=excluded.reviewer, feedback_hash=excluded.feedback_hash, \
                   feedback_text=excluded.feedback_text, created_at=excluded.created_at",
                params![
                    bead_id,
                    attempt,
                    reviewer,
                    feedback_hash,
                    feedback_text,
                    now_iso8601(),
                ],
            )
            .map_err(|e| tool_err("save_rejection", e))?;
        Ok(())
    }

    fn load_rejection(&self, bead_id: &str, attempt: u32) -> Result<Option<(String, String)>, DaemonError> {
        self.conn
            .query_row(
                "SELECT reviewer, feedback_hash FROM review_rejection WHERE bead_id = ?1 AND attempt = ?2",
                params![bead_id, attempt],
                |row| {
                    let reviewer: String = row.get(0)?;
                    let feedback_hash: String = row.get(1)?;
                    Ok((reviewer, feedback_hash))
                },
            )
            .optional()
            .map_err(|e| tool_err("load_rejection", e))
    }

    fn load_rejection_text(&self, bead_id: &str, attempt: u32) -> Result<Option<String>, DaemonError> {
        self.conn
            .query_row(
                "SELECT feedback_text FROM review_rejection WHERE bead_id = ?1 AND attempt = ?2",
                params![bead_id, attempt],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| tool_err("load_rejection_text", e))
    }

    fn er_runner_attempt(&self, bead_id: &str) -> Result<(u32, Option<u64>), DaemonError> {
        // Schema migration: the columns were added after the initial release;
        // older DB files won't have them. Detect that by attempting the
        // SELECT and falling back to (0, None) if the column is missing
        // (sqlite returns "no such column" rather than an empty result).
        let row: Result<(i64, Option<i64>), rusqlite::Error> = self.conn.query_row(
            "SELECT attempt_er_runner_count, last_er_runner_attempt_at \
             FROM bead_overlay WHERE bead_id = ?1",
            params![bead_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        );
        match row {
            Ok((count, last_at)) => Ok((count.max(0) as u32, last_at.map(|v| v.max(0) as u64))),
            Err(e) if no_such_column(&e) => Ok((0, None)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, None)),
            Err(e) => Err(tool_err("er_runner_attempt", e)),
        }
    }

    fn incr_er_runner_attempt(&self, bead_id: &str, now_epoch: u64) -> Result<u32, DaemonError> {
        // Try the UPDATE first (modern schema). If the column is missing
        // (legacy DB), fall back to no-op + return 1 so the runner's
        // attempt cap still fires on the in-memory counter.
        let res = self.conn.execute(
            "UPDATE bead_overlay SET \
                attempt_er_runner_count = COALESCE(attempt_er_runner_count, 0) + 1, \
                last_er_runner_attempt_at = ?2, \
                updated_at = ?3 \
             WHERE bead_id = ?1",
            params![bead_id, now_epoch as i64, now_iso8601()],
        );
        match res {
            Ok(_) => {
                let (count, _) = self.er_runner_attempt(bead_id)?;
                Ok(count)
            }
            Err(e) if no_such_column(&e) => Ok(1),
            Err(e) => Err(tool_err("incr_er_runner_attempt", e)),
        }
    }
}

/// True when `err` is the SQLite "no such column" schema-mismatch signal.
/// `rusqlite::Error` does not expose a typed `message` field on every
/// feature combo, so we stringify + match — this is the same trick
/// the JSON parsers in tools.rs use for `find('{')` fallback.
fn no_such_column(err: &rusqlite::Error) -> bool {
    err.to_string().to_lowercase().contains("no such column")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn store() -> SqliteStateStore {
        SqliteStateStore::open_in_memory_with_schema(include_str!("../contracts/schema.sql"))
            .unwrap()
    }

    #[test]
    fn overlay_roundtrip() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "b1".into(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: None,
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        };
        s.save(&o).unwrap();
        let got = s.load("b1").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::Queued);
        assert_eq!(got.attempt, 1);
        assert_eq!(got.bead_id, "b1");
        assert_eq!(got.pr_number, None);
        assert_eq!(got.branch, None);
    }

    #[test]
    fn overlay_roundtrip_updates_on_conflict() {
        let s = store();
        let mut o = BeadOverlay {
            bead_id: "b2".into(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 10,
            spend_usd: 0.5,
            pr_number: None,
            branch: None,
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        };
        s.save(&o).unwrap();
        o.state = OverlayState::Attested;
        o.attempt = 2;
        o.pr_number = Some(42);
        o.branch = Some("factory/b2-r2".into());
        s.save(&o).unwrap();
        let got = s.load("b2").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::Attested);
        assert_eq!(got.attempt, 2);
        assert_eq!(got.pr_number, Some(42));
        assert_eq!(got.branch, Some("factory/b2-r2".into()));
    }

    #[test]
    fn load_missing_bead_returns_none() {
        let s = store();
        assert!(s.load("nope").unwrap().is_none());
    }

    #[test]
    fn reconcile_dispatching_parks_ambiguity_and_preserves_recovery_handles() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "b-stale".into(),
            state: OverlayState::Dispatching,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("stale-branch".into()),
            session_id: Some("stale-session-id".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        };

        s.save(&o).unwrap();
        s.reconcile_dispatching().unwrap();

        let got = s.load("b-stale").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::HumanHeld);
        assert_eq!(got.session_id.as_deref(), Some("stale-session-id"));
        assert_eq!(got.branch.as_deref(), Some("stale-branch"));
        assert_eq!(
            got.park_reason.as_deref(),
            Some("ambiguous_dispatching_recovery")
        );
        assert!(
            s.recover_human_held(10).unwrap().is_empty(),
            "ambiguous dispatch must never auto-requeue and spawn a duplicate"
        );
    }

    #[test]
    fn reconcile_dispatching_preserves_cleanup_failure_hold_and_live_session() {
        let s = store();
        let o = BeadOverlay {
            bead_id: "cleanup-held".into(),
            state: OverlayState::HumanHeld,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/cleanup-held-r1".into()),
            session_id: Some("known-live-session".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: Some("spawn_cleanup_failed".into()),
            target_repo: None,
        };

        s.save(&o).unwrap();
        s.reconcile_dispatching().unwrap();

        let got = s.load("cleanup-held").unwrap().unwrap();
        assert_eq!(got.state, OverlayState::HumanHeld);
        assert_eq!(got.session_id.as_deref(), Some("known-live-session"));
        assert_eq!(got.branch.as_deref(), Some("factory/cleanup-held-r1"));
        assert_eq!(got.park_reason.as_deref(), Some("spawn_cleanup_failed"));
    }

    #[test]
    fn illegal_state_string_rejected_by_schema() {
        let s = store();
        let r = s.conn.execute(
            "INSERT INTO bead_overlay (bead_id,state,updated_at) VALUES ('x','BOGUS','now')",
            [],
        );
        assert!(r.is_err(), "CHECK constraint must reject unknown states");
    }

    #[test]
    fn all_ten_overlay_states_accepted_by_schema() {
        let s = store();
        for (i, state) in [
            OverlayState::Queued,
            OverlayState::Dispatching,
            OverlayState::Dispatched,
            OverlayState::Attested,
            OverlayState::Ready,
            OverlayState::ReRoll,
            OverlayState::Recovery,
            OverlayState::Redispatched,
            OverlayState::BudgetHeld,
            OverlayState::HumanHeld,
        ]
        .into_iter()
        .enumerate()
        {
            let o = BeadOverlay {
                bead_id: format!("bead-{i}"),
                state,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: None,
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            };
            s.save(&o).unwrap();
            let got = s.load(&o.bead_id).unwrap().unwrap();
            assert_eq!(got.state, state);
        }
    }

    #[test]
    fn owned_branches_lists_only_registered() {
        let s = store();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        assert_eq!(s.owned_branches().unwrap(), vec!["factory/b1-r1".to_string()]);
    }

    #[test]
    fn owned_branches_deletion_guard_survives_bead_overlay_delete() {
        // Spec §4.2.8: the daemon may delete ONLY refs recorded in branch_registry.
        // There is intentionally no FK between bead_overlay and branch_registry in
        // contracts/schema.sql, so deleting a bead_overlay row must NOT cascade,
        // error, or silently orphan/mutate branch_registry — owned_branches() must
        // keep returning exactly what was registered, independent of overlay state.
        let s = store();
        let o = BeadOverlay {
            bead_id: "b1".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(7),
            branch: Some("factory/b1-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        };
        s.save(&o).unwrap();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        assert_eq!(s.owned_branches().unwrap(), vec!["factory/b1-r1".to_string()]);

        // Delete the overlay row directly (simulates a bead being purged/reset).
        s.conn
            .execute("DELETE FROM bead_overlay WHERE bead_id = ?1", params!["b1"])
            .unwrap();

        // branch_registry must be unaffected: still exactly the registered branch,
        // proving the guard is enforced at the DB layer (no FK cascade exists to
        // exploit) rather than merely documented in a comment.
        assert_eq!(
            s.owned_branches().unwrap(),
            vec!["factory/b1-r1".to_string()],
            "deleting bead_overlay must not orphan/violate branch_registry"
        );
        assert!(s.load("b1").unwrap().is_none());
    }

    #[test]
    fn register_branch_rejects_conflicting_owner() {
        let s = store();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        s.register_branch("b1", "factory/b1-r1").unwrap(); // idempotent for same bead
        let err = s.register_branch("b2", "factory/b1-r1").unwrap_err();
        assert!(
            err.to_string().contains("already registered to bead b1"),
            "unexpected conflict error: {err}"
        );
        let branches = s.owned_branches().unwrap();
        assert_eq!(branches, vec!["factory/b1-r1".to_string()]);
        assert_eq!(
            s.bead_id_for_branch("factory/b1-r1").unwrap(),
            Some("b1".to_string())
        );
    }

    /// jleechan-8in: file-backed `open()` must not silently swallow a failed
    /// `PRAGMA journal_mode=WAL`. This asserts the happy path actually took
    /// effect (readback == "wal") on a real temp-file-backed connection —
    /// proving `configure(is_memory=false)` verifies rather than discards.
    #[test]
    fn open_on_real_file_actually_sets_wal_mode() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dark-factory-state-test-{}-{}.sqlite",
            std::process::id(),
            now_iso8601().replace([':', '-', 'T', 'Z'], "")
        ));
        let _cleanup = TempFileGuard(path.clone());

        let s = SqliteStateStore::open(&path).unwrap();
        let mode: String = s
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    /// jleechan-8in: a non-"wal" (or errored) readback on a file-backed connection
    /// must propagate as `DaemonError::Config`, not be discarded. Exercises
    /// `configure` directly with `is_memory=false` against an in-memory connection
    /// (WAL cannot take effect there), which is exactly the failure mode the bead
    /// describes for real files with unsupported filesystems/permissions/NFS.
    #[test]
    fn configure_file_backed_propagates_wal_failure_as_config_error() {
        let conn = Connection::open_in_memory().unwrap();
        let err = SqliteStateStore::configure(&conn, false).unwrap_err();
        match err {
            DaemonError::Config(msg) => {
                assert!(
                    msg.contains("journal_mode=WAL"),
                    "error message should mention the failing pragma: {msg}"
                );
            }
            other => panic!("expected DaemonError::Config, got {other:?}"),
        }
    }

    /// Cleans up the temp file (+ WAL/SHM sidecars) created by
    /// `open_on_real_file_actually_sets_wal_mode`, even on panic.
    struct TempFileGuard(std::path::PathBuf);
    impl Drop for TempFileGuard {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    /// jleechan-qqq: an on-disk DB that lacks the `/er` runner columns must
    /// be auto-migrated by `open()` and `open_in_memory_with_schema()`, so
    /// the runner's first `incr_er_runner_attempt` doesn't fail with
    /// "no such column". This pins the idempotency of the migration: a
    /// second `open()` against the same file must NOT error on the
    /// re-applied `ALTER TABLE` (a hard crash here would also block every
    /// daemon restart, since `execute_batch(schema.sql)` re-runs).
    #[test]
    fn open_migrates_legacy_db_missing_er_runner_columns() {
        use rusqlite::Connection;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dark-factory-er-migrate-{}-{}.sqlite",
            std::process::id(),
            now_iso8601().replace([':', '-', 'T', 'Z'], "")
        ));
        let _cleanup = TempFileGuard(path.clone());

        // Build a "legacy" DB: just `bead_overlay` and `branch_registry`,
        // no `/er` runner columns.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE bead_overlay (bead_id TEXT PRIMARY KEY, state TEXT NOT NULL, \
                 attempt INTEGER NOT NULL DEFAULT 1, reroll_count INTEGER NOT NULL DEFAULT 0, \
                 autonomy_secs INTEGER NOT NULL DEFAULT 0, spend_usd REAL NOT NULL DEFAULT 0, \
                 pr_number INTEGER, branch TEXT, session_id TEXT, updated_at TEXT NOT NULL); \
                 CREATE TABLE branch_registry (branch TEXT PRIMARY KEY, bead_id TEXT NOT NULL, \
                 created_at TEXT NOT NULL);",
            )
            .unwrap();
        }

        // First open: must apply the migration without error.
        let _store = SqliteStateStore::open(&path).expect("legacy DB should auto-migrate");

        // Re-open: must NOT fail on the second ALTER (idempotency).
        let _store2 = SqliteStateStore::open(&path).expect("second open must be idempotent");

        // Both columns are present, with the expected defaults.
        let conn = Connection::open(&path).unwrap();
        let count_col: i64 = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'attempt_er_runner_count'",
                [],
                |row| row.get(0),
            )
            .expect("attempt_er_runner_count column should exist after migration");
        let last_col: i64 = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'last_er_runner_attempt_at'",
                [],
                |row| row.get(0),
            )
            .expect("last_er_runner_attempt_at column should exist after migration");
        assert_eq!(count_col, 1);
        assert_eq!(last_col, 1);
    }

    /// jleechan-gib: the real-SQLite `recover_human_held` requeues every
    /// HUMAN_HELD row under the cap and leaves HUMAN_HELD rows at/above the
    /// cap alone. Mirrors the shell overlay's
    /// `recover-held` (daemon/factory-overlay.sh:319-333) exactly so the
    /// Rust tick can replace the shell caller.
    #[test]
    fn recover_human_held_requeues_below_cap_leaves_at_or_above_alone() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "below".to_string(),
            BeadOverlay {
                bead_id: "below".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 9999,
                spend_usd: 0.0,
                pr_number: Some(11),
                branch: Some("factory/below-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            },
        );
        overlays.insert(
            "at-cap".to_string(),
            BeadOverlay {
                bead_id: "at-cap".into(),
                state: OverlayState::HumanHeld,
                attempt: 10,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(22),
                branch: Some("factory/at-cap-r10".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            },
        );
        overlays.insert(
            "over-cap".to_string(),
            BeadOverlay {
                bead_id: "over-cap".into(),
                state: OverlayState::HumanHeld,
                attempt: 12,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(33),
                branch: Some("factory/over-cap-r12".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            },
        );
        // Non-HUMAN_HELD rows must never be touched.
        overlays.insert(
            "dispatched".to_string(),
            BeadOverlay {
                bead_id: "dispatched".into(),
                state: OverlayState::Dispatched,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 100,
                spend_usd: 0.0,
                pr_number: Some(44),
                branch: Some("factory/dispatched-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            },
        );
        overlays.insert(
            "ready".to_string(),
            BeadOverlay {
                bead_id: "ready".into(),
                state: OverlayState::Ready,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(55),
                branch: Some("factory/ready-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(recovered.len(), 1, "only `below` should be recovered");
        assert_eq!(recovered[0].bead_id, "below");

        // `below` was requeued and reset
        let below = store.load("below").unwrap().unwrap();
        assert_eq!(below.state, OverlayState::Queued);
        assert_eq!(below.attempt, 3);
        assert_eq!(below.autonomy_secs, 0);
        // P2 (Codex): stale PR metadata must be cleared on requeue so the
        // next dispatch doesn't conflate the fresh attempt with the dead PR.
        assert_eq!(
            below.pr_number, None,
            "recover_human_held must clear pr_number to prevent stale-PR churn"
        );
        assert_eq!(
            below.session_id, None,
            "recover_human_held must clear session_id (belonged to the prior attempt)"
        );
        // branch is intentionally preserved so the recovered_from telemetry
        // can still record what was being worked on; dispatch will rewrite
        // it on the next attempt.
        assert_eq!(
            below.branch.as_deref(),
            Some("factory/below-r2"),
            "recover_human_held keeps branch as a stale-attempt breadcrumb"
        );

        // `at-cap` and `over-cap` are still HUMAN_HELD
        let at_cap = store.load("at-cap").unwrap().unwrap();
        assert_eq!(at_cap.state, OverlayState::HumanHeld);
        assert_eq!(at_cap.attempt, 10);
        let over_cap = store.load("over-cap").unwrap().unwrap();
        assert_eq!(over_cap.state, OverlayState::HumanHeld);
        assert_eq!(over_cap.attempt, 12);
        let capped = store.human_held_at_or_above_attempt(10).unwrap();
        let capped_ids: std::collections::HashSet<_> =
            capped.iter().map(|overlay| overlay.bead_id.as_str()).collect();
        assert_eq!(capped_ids.len(), 2);
        assert!(capped_ids.contains("at-cap"));
        assert!(capped_ids.contains("over-cap"));

        // Non-HUMAN_HELD rows are untouched
        let dispatched = store.load("dispatched").unwrap().unwrap();
        assert_eq!(dispatched.state, OverlayState::Dispatched);
        assert_eq!(dispatched.autonomy_secs, 100);
        let ready = store.load("ready").unwrap().unwrap();
        assert_eq!(ready.state, OverlayState::Ready);
    }

    /// bead jleechan-4jn1 (live incident jleechan-93ft / PR
    /// worldarchitect.ai#7888): a bead parked HUMAN_HELD by the circuit
    /// breaker (`park_reason` starting with `"circuit-breaker"`) must NOT be
    /// requeued by `recover_human_held`, even though its `attempt` is well
    /// under the recovery cap — the circuit breaker parks it specifically to
    /// STOP the same reviewer/feedback loop from re-triggering. A bead
    /// parked for a transient reason (`session_stalled`, mirroring
    /// `tick::run_tick`'s wedge-detection park) at the exact same attempt
    /// must still be recovered normally. This is the behavioral difference
    /// this bead's fix depends on: before the fix, `recover_human_held`'s
    /// SQL (`WHERE state = 'HUMAN_HELD' AND attempt < ?1`) could not tell
    /// the two apart and requeued both identically.
    #[test]
    fn recover_human_held_excludes_circuit_breaker_parks_but_recovers_transient_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "circuit-broken".to_string(),
            BeadOverlay {
                bead_id: "circuit-broken".into(),
                state: OverlayState::HumanHeld,
                attempt: 6,
                reroll_count: 3,
                autonomy_secs: 500,
                spend_usd: 0.0,
                pr_number: Some(7888),
                branch: Some("factory/circuit-broken-r6".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some(crate::reroll::CIRCUIT_BREAKER_PARK_REASON.to_string()),
                target_repo: None,
            },
        );
        overlays.insert(
            "transient-stalled".to_string(),
            BeadOverlay {
                bead_id: "transient-stalled".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: Some(99),
                branch: Some("factory/transient-stalled-r2".into()),
                session_id: Some("session-abc".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("session_stalled".to_string()),
                target_repo: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the transient park should be recovered; the circuit-breaker park must be excluded"
        );
        assert_eq!(recovered[0].bead_id, "transient-stalled");

        // The circuit-breaker-parked bead is untouched: still HUMAN_HELD,
        // same attempt, park_reason preserved. This is the exact regression
        // this bead fixes — production requeued this bead 21 seconds after
        // park and re-triggered the same rejected fix 769 times in 30
        // minutes.
        let circuit_broken = store.load("circuit-broken").unwrap().unwrap();
        assert_eq!(
            circuit_broken.state,
            OverlayState::HumanHeld,
            "circuit-breaker park must NOT be auto-requeued"
        );
        assert_eq!(circuit_broken.attempt, 6, "attempt must not be bumped");
        assert_eq!(
            circuit_broken.park_reason.as_deref(),
            Some(crate::reroll::CIRCUIT_BREAKER_PARK_REASON),
            "park_reason must survive an excluded recovery pass"
        );

        // The transient park recovers exactly like the pre-existing
        // `session_stalled` / `autonomy_timebox_exceeded` behavior: QUEUED,
        // attempt bumped, autonomy reset, park_reason cleared.
        let transient = store.load("transient-stalled").unwrap().unwrap();
        assert_eq!(transient.state, OverlayState::Queued);
        assert_eq!(transient.attempt, 3);
        assert_eq!(transient.autonomy_secs, 0);
        assert_eq!(
            transient.park_reason, None,
            "recover_human_held clears park_reason once a bead is back in play"
        );
    }

    /// jleechan-35y4 (adversarial review of PR #245): a bead parked
    /// HUMAN_HELD with `park_reason = "unmapped_target_repo"` (bead
    /// jleechan-35y4's own dispatch-time park — see
    /// `dispatch::dispatch_ready`) must NOT be auto-requeued, for the same
    /// reason circuit-breaker parks aren't: an unmapped repo is a config
    /// problem no bare requeue fixes. Without this exclusion the bead would
    /// ping-pong HUMAN_HELD -> QUEUED -> re-park identically every recovery
    /// cycle instead of staying HUMAN_HELD until an operator fixes the
    /// config. A same-attempt transient park (`session_stalled`) must still
    /// recover normally, exactly like the circuit-breaker test above.
    #[test]
    fn recover_human_held_excludes_unmapped_target_repo_parks_but_recovers_transient_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "unmapped-repo".to_string(),
            BeadOverlay {
                bead_id: "unmapped-repo".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: None,
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("unmapped_target_repo".to_string()),
                target_repo: Some("someorg/unrelated-repo".to_string()),
            },
        );
        overlays.insert(
            "transient-stalled".to_string(),
            BeadOverlay {
                bead_id: "transient-stalled".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: Some(99),
                branch: Some("factory/transient-stalled-r2".into()),
                session_id: Some("session-abc".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("session_stalled".to_string()),
                target_repo: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the transient park should be recovered; the unmapped-repo park must be excluded"
        );
        assert_eq!(recovered[0].bead_id, "transient-stalled");

        let unmapped = store.load("unmapped-repo").unwrap().unwrap();
        assert_eq!(
            unmapped.state,
            OverlayState::HumanHeld,
            "unmapped_target_repo park must NOT be auto-requeued"
        );
        assert_eq!(unmapped.attempt, 2, "attempt must not be bumped");
        assert_eq!(
            unmapped.park_reason.as_deref(),
            Some("unmapped_target_repo"),
            "park_reason must survive an excluded recovery pass"
        );

        let transient = store.load("transient-stalled").unwrap().unwrap();
        assert_eq!(transient.state, OverlayState::Queued);
        assert_eq!(transient.attempt, 3);
    }

    /// jleechan-bqdv Stage C: `worktree_remote_mismatch` parks must be
    /// excluded from `recover_human_held`'s auto-requeue exactly like
    /// `unmapped_target_repo` — this is the "fail loud, never guess"
    /// spawn-time park for a coder worktree whose git remote doesn't match
    /// the bead's resolved repo (jleechan-9sh5 discipline). Without this
    /// exclusion the bead would ping-pong HUMAN_HELD -> QUEUED -> re-spawn
    /// into the same (still misconfigured) worktree every recovery cycle.
    #[test]
    fn recover_human_held_excludes_worktree_remote_mismatch_parks_but_recovers_transient_parks() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        let mut overlays = HashMap::new();
        overlays.insert(
            "wrong-remote".to_string(),
            BeadOverlay {
                bead_id: "wrong-remote".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/wrong-remote-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("worktree_remote_mismatch".to_string()),
                target_repo: Some("jleechanorg/worldarchitect.ai".to_string()),
            },
        );
        overlays.insert(
            "transient-stalled-2".to_string(),
            BeadOverlay {
                bead_id: "transient-stalled-2".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 1800,
                spend_usd: 0.0,
                pr_number: Some(99),
                branch: Some("factory/transient-stalled-2-r2".into()),
                session_id: Some("session-abc".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("session_stalled".to_string()),
                target_repo: None,
            },
        );
        overlays.insert(
            "cleanup-failed".to_string(),
            BeadOverlay {
                bead_id: "cleanup-failed".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/cleanup-failed-r2".into()),
                session_id: Some("still-live-session".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("spawn_cleanup_failed".to_string()),
                target_repo: None,
            },
        );
        overlays.insert(
            "remote-unverifiable".to_string(),
            BeadOverlay {
                bead_id: "remote-unverifiable".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/remote-unverifiable-r2".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("worktree_remote_unverifiable".to_string()),
                target_repo: None,
            },
        );
        overlays.insert(
            "branch-mismatch-cleanup-failed".to_string(),
            BeadOverlay {
                bead_id: "branch-mismatch-cleanup-failed".into(),
                state: OverlayState::HumanHeld,
                attempt: 2,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/branch-mismatch-cleanup-failed-r2".into()),
                session_id: Some("still-live-wrong-branch-session".into()),
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: Some("spawn_branch_mismatch".to_string()),
                target_repo: None,
            },
        );
        for overlay in overlays.values() {
            store.save(overlay).unwrap();
        }

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the transient park should be recovered; permanent remote/cleanup safety parks must be excluded"
        );
        assert_eq!(recovered[0].bead_id, "transient-stalled-2");

        let wrong_remote = store.load("wrong-remote").unwrap().unwrap();
        assert_eq!(
            wrong_remote.state,
            OverlayState::HumanHeld,
            "worktree_remote_mismatch park must NOT be auto-requeued"
        );
        assert_eq!(wrong_remote.attempt, 2, "attempt must not be bumped");
        assert_eq!(
            wrong_remote.park_reason.as_deref(),
            Some("worktree_remote_mismatch"),
            "park_reason must survive an excluded recovery pass"
        );

        let cleanup_failed = store.load("cleanup-failed").unwrap().unwrap();
        assert_eq!(cleanup_failed.state, OverlayState::HumanHeld);
        assert_eq!(
            cleanup_failed.session_id.as_deref(),
            Some("still-live-session"),
            "the known live session identity must survive recovery"
        );
        assert_eq!(
            cleanup_failed.park_reason.as_deref(),
            Some("spawn_cleanup_failed")
        );

        let remote_unverifiable = store.load("remote-unverifiable").unwrap().unwrap();
        assert_eq!(remote_unverifiable.state, OverlayState::HumanHeld);
        assert_eq!(
            remote_unverifiable.park_reason.as_deref(),
            Some("worktree_remote_unverifiable")
        );

        let branch_mismatch = store
            .load("branch-mismatch-cleanup-failed")
            .unwrap()
            .unwrap();
        assert_eq!(branch_mismatch.state, OverlayState::HumanHeld);
        assert_eq!(
            branch_mismatch.session_id.as_deref(),
            Some("still-live-wrong-branch-session")
        );
        assert_eq!(
            branch_mismatch.park_reason.as_deref(),
            Some("spawn_branch_mismatch")
        );

        let transient = store.load("transient-stalled-2").unwrap().unwrap();
        assert_eq!(transient.state, OverlayState::Queued);
        assert_eq!(transient.attempt, 3);
    }

    /// P2 (Codex review): `recover_human_held` must return ONLY the rows
    /// that were actually requeued — not every QUEUED bead with
    /// `autonomy_secs = 0` (which is what the original heuristic-query did).
    /// Pre-existing QUEUED beads with `autonomy_secs = 0` would otherwise
    /// be reported as RECOVERED_FROM_HELD and pollute telemetry + the
    /// tick summary. Run with a populated QUEUED backlog + a single
    /// recoverable HUMAN_HELD bead, and assert the returned vec contains
    /// only the bead we just recovered.
    #[test]
    fn recover_human_held_returns_only_rows_actually_recovered() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        // Pre-existing QUEUED backlog with autonomy_secs=0 (these are
        // exactly the rows that would have been incorrectly reported as
        // RECOVERED_FROM_HELD before the fix).
        for (id, attempt) in [("queued-a", 1), ("queued-b", 2), ("queued-c", 3)] {
            store
                .save(&BeadOverlay {
                    bead_id: id.into(),
                    state: OverlayState::Queued,
                    attempt,
                    reroll_count: 0,
                    autonomy_secs: 0,
                    spend_usd: 0.0,
                    pr_number: None,
                    branch: None,
                    session_id: None,
                    is_adopted: false,
                    spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
                })
                .unwrap();
        }

        // One recoverable HUMAN_HELD bead.
        store
            .save(&BeadOverlay {
                bead_id: "held-only".into(),
                state: OverlayState::HumanHeld,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 4321,
                spend_usd: 0.0,
                pr_number: Some(7777),
                branch: Some("factory/held-only-r1".into()),
                session_id: Some("session-xyz".into()),
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            })
            .unwrap();

        let recovered = store.recover_human_held(10).unwrap();
        assert_eq!(
            recovered.len(),
            1,
            "only the HUMAN_HELD row should be reported as recovered"
        );
        assert_eq!(recovered[0].bead_id, "held-only");

        // Pre-existing QUEUED rows are untouched.
        for id in ["queued-a", "queued-b", "queued-c"] {
            let o = store.load(id).unwrap().unwrap();
            assert_eq!(o.state, OverlayState::Queued, "{id} must stay QUEUED");
            assert_eq!(o.autonomy_secs, 0, "{id} autonomy_secs unchanged");
        }
    }

    /// jleechan-54ky: `list_active_overlays` returns the active set
    /// unchanged (no implicit increment). `bump_autonomy_secs` advances a
    /// single row. Together they replace the old
    /// `increment_active_autonomy` SQL update + select in a way that lets
    /// the caller skip rows whose PR has `ci_pending=true` (the
    /// sub-fix-for-gib autonomy pause).
    #[test]
    fn list_active_overlays_does_not_bump_and_bump_autonomy_secs_targets_one_row() {
        let schema = include_str!("../contracts/schema.sql");
        let store = SqliteStateStore::open_in_memory_with_schema(schema).unwrap();

        store
            .save(&BeadOverlay {
                bead_id: "d".into(),
                state: OverlayState::Dispatched,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 100,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/d-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            })
            .unwrap();
        store
            .save(&BeadOverlay {
                bead_id: "a".into(),
                state: OverlayState::Attested,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 200,
                spend_usd: 0.0,
                pr_number: Some(7),
                branch: Some("factory/a-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            })
            .unwrap();
        store
            .save(&BeadOverlay {
                bead_id: "h".into(),
                state: OverlayState::HumanHeld,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 300,
                spend_usd: 0.0,
                pr_number: None,
                branch: Some("factory/h-r1".into()),
                session_id: None,
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            })
            .unwrap();

        let active = store.list_active_overlays().unwrap();
        assert_eq!(active.len(), 2, "DISPATCHED + ATTESTED only");
        let names: Vec<&str> = active.iter().map(|o| o.bead_id.as_str()).collect();
        assert!(names.contains(&"d"));
        assert!(names.contains(&"a"));

        // list_active_overlays must NOT have bumped autonomy_secs
        let d_before = store.load("d").unwrap().unwrap();
        assert_eq!(
            d_before.autonomy_secs, 100,
            "list_active_overlays must not bump autonomy_secs"
        );

        // bump_autonomy_secs targets exactly the row named
        store.bump_autonomy_secs("d", 50).unwrap();
        store.bump_autonomy_secs("a", 75).unwrap();
        store.bump_autonomy_secs("h", 9999).unwrap(); // HUMAN_HELD row still bumps

        let d_after = store.load("d").unwrap().unwrap();
        assert_eq!(d_after.autonomy_secs, 150);
        let a_after = store.load("a").unwrap().unwrap();
        assert_eq!(a_after.autonomy_secs, 275);
        let h_after = store.load("h").unwrap().unwrap();
        assert_eq!(h_after.autonomy_secs, 10299);
    }
}
