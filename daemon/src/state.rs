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

/// Task 1 (reviewer-outage-resilience): one row of the `vendor_health`
/// ledger, tracking whether each external review-bot provider ("coderabbit"
/// or "bugbot") is currently in-outage or recovered, with strict semantics
/// and a full audit trail. Populated from the production assessment path in
/// `tick::run_fast_tier` via `StateStore::record_vendor_observation`; read
/// by the verification step's outage-aware CI-pending logic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VendorHealth {
    pub vendor: String,
    /// 1 = currently in outage, 0 = healthy
    pub in_outage: bool,
    /// consecutive assessments where status was "unknown"/pending
    pub consecutive_pending: u32,
    /// total outage marker observations (audit trail)
    pub outage_observations: u32,
    /// total success observations (audit trail)
    pub success_observations: u32,
    /// PR head SHA of the last successful review/status
    pub last_success_head: Option<String>,
    /// unix epoch when in_outage was first set to 1
    pub last_outage_epoch: Option<u64>,
    /// PR head SHA at the last observation
    pub last_observed_head: Option<String>,
    /// unix epoch of the last observation
    pub last_observed_epoch: Option<u64>,
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
    /// Escalation dedup query (1s2q-escalation-dedup): returns `true` if an
    /// ESCALATION_REQUIRED / ESCALATION_NOTIFICATION_FAILED event should be
    /// emitted for `(bead_id, reason)` at `now_epoch` — i.e. no prior record
    /// exists, the context hash changed, or the last emit is older than
    /// `refire_secs`. Returns `false` (suppress) when the same context was
    /// emitted within the backoff window. Default `Ok(true)` so fakes that
    /// don't persist the ledger never suppress (preserve prior behavior).
    fn escalation_should_emit(
        &self,
        _bead_id: &str,
        _reason: &str,
        _context_hash: &str,
        _now_epoch: u64,
        _refire_secs: u64,
    ) -> Result<bool, DaemonError> {
        Ok(true)
    }
    /// Record that an escalation event was just emitted for
    /// `(bead_id, reason)` with `context_hash` at `now_epoch` (upsert the
    /// ledger row). Default no-op for fakes that don't persist the ledger.
    fn record_escalation_emit(
        &self,
        _bead_id: &str,
        _reason: &str,
        _context_hash: &str,
        _now_epoch: u64,
    ) -> Result<(), DaemonError> {
        Ok(())
    }
    /// 1s2q-escalation-dedup Task 2: mark the `(bead_id, reason)` escalation
    /// ledger row as terminal ("escalation_undeliverable") so
    /// `escalation_should_emit` returns `Ok(false)` for it on every future
    /// tick, regardless of context hash or backoff window. Used when the
    /// notification failure was caused by a PERMANENT (non-transient per
    /// `DaemonError::is_transient`) gh error that will never resolve (e.g.
    /// `invalid issue format: "local-xxx"`). Upserts the ledger row with
    /// `terminal = 1` (inserts a fresh terminal row if none existed, or flips
    /// an existing row to terminal). Default no-op for fakes that don't
    /// persist the ledger.
    fn mark_escalation_undeliverable(
        &self,
        _bead_id: &str,
        _reason: &str,
    ) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Read the vendor_health row for `vendor`. Returns None if no row exists
    /// (vendor has never been observed). Used by the production assessment path
    /// and by the verification step's outage-aware CI-pending logic.
    fn vendor_health(&self, _vendor: &str) -> Result<Option<VendorHealth>, DaemonError> {
        Ok(None)
    }
    /// Record a vendor health observation and return the resulting VendorHealth
    /// row (so the caller can emit telemetry with the new state). The
    /// implementation applies the outage/recovery semantics described in the
    /// plan:
    /// - If the observation is an outage marker (status pending/unknown):
    ///   increment consecutive_pending and outage_observations. If
    ///   consecutive_pending >= N, set in_outage=1 and last_outage_epoch (if
    ///   not already set).
    /// - If the observation is a success (approved/clean) for the PR's current
    ///   head: record it as a success_observation (NEVER as an outage
    ///   observation), set consecutive_pending=0, last_success_head=head. If
    ///   in_outage was 1, flip to 0 (recovered) and return the row so the
    ///   caller can emit VENDOR_RECOVERED.
    /// - The absence of errors alone must NEVER flip in_outage to 0.
    fn record_vendor_observation(
        &self,
        _vendor: &str,
        _is_outage_marker: bool,
        _is_success: bool,
        _head_sha: &str,
        _now_epoch: u64,
        _consecutive_pending_threshold: u32,
    ) -> Result<VendorHealth, DaemonError> {
        Ok(VendorHealth {
            vendor: _vendor.to_string(),
            in_outage: false,
            consecutive_pending: 0,
            outage_observations: 0,
            success_observations: 0,
            last_success_head: None,
            last_outage_epoch: None,
            last_observed_head: None,
            last_observed_epoch: None,
        })
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
        Self::ensure_reroll_deferral_count_column(&conn)?;
        Self::ensure_held_recheck_after_column(&conn)?;
        Self::ensure_last_er_evidence_hash_column(&conn)?;
        Self::ensure_disposition_required_state(&conn)?;
        Self::ensure_escalation_ledger_table(&conn)?;
        Self::ensure_escalation_ledger_terminal_column(&conn)?;
        Self::ensure_vendor_health_table(&conn)?;
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
        Self::ensure_reroll_deferral_count_column(&conn)?;
        Self::ensure_held_recheck_after_column(&conn)?;
        Self::ensure_last_er_evidence_hash_column(&conn)?;
        Self::ensure_disposition_required_state(&conn)?;
        Self::ensure_escalation_ledger_table(&conn)?;
        Self::ensure_escalation_ledger_terminal_column(&conn)?;
        Self::ensure_vendor_health_table(&conn)?;
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
                |row: &rusqlite::Row| row.get(0),
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
                |row: &rusqlite::Row| row.get(0),
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
                |row: &rusqlite::Row| row.get(0),
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
                |row: &rusqlite::Row| row.get(0),
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
                |row: &rusqlite::Row| row.get(0),
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
                |row: &rusqlite::Row| row.get(0),
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
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_target_repo_column: pragma", e))?;
        if !has_col {
            conn.execute("ALTER TABLE bead_overlay ADD COLUMN target_repo TEXT", [])
                .map_err(|e| tool_err("ensure_target_repo_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `reroll_deferral_count` column (bead
    /// jleechan-zeij / issue #322 r2). Same probe-then-`ALTER` pattern as
    /// `ensure_target_repo_column`. The consecutive-defer counter the
    /// fail-closed re-roll proceed predicate uses; every pre-existing row
    /// correctly defaults to `0` ("never deferred").
    fn ensure_reroll_deferral_count_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'reroll_deferral_count'",
                [],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_reroll_deferral_count_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN reroll_deferral_count INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_reroll_deferral_count_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `held_recheck_after` column (bead
    /// jleechan-zaga / issue #348 r3). Same probe-then-`ALTER` pattern as
    /// `ensure_reroll_deferral_count_column`. Nullable (NULL = "re-assess
    /// now"). MUST run before `ensure_disposition_required_state` so the
    /// table's column set is complete before the CHECK rebuild.
    fn ensure_held_recheck_after_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'held_recheck_after'",
                [],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_held_recheck_after_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN held_recheck_after INTEGER",
                [],
            )
            .map_err(|e| tool_err("ensure_held_recheck_after_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `last_er_evidence_hash` column (bead
    /// jleechan-yoqy / issue #323). Same probe-then-`ALTER` pattern. Nullable
    /// (NULL = "no /er run recorded"). MUST run before
    /// `ensure_disposition_required_state` so the column is present for the
    /// CHECK rebuild's column-intersection copy.
    fn ensure_last_er_evidence_hash_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('bead_overlay') \
                 WHERE name = 'last_er_evidence_hash'",
                [],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_last_er_evidence_hash_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE bead_overlay ADD COLUMN last_er_evidence_hash TEXT",
                [],
            )
            .map_err(|e| tool_err("ensure_last_er_evidence_hash_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `escalation_ledger` table
    /// (1s2q-escalation-dedup). Unlike the `ensure_*_column` migrations above
    /// (which probe `pragma_table_info` for a column), this probes
    /// `sqlite_master` for the table's existence, then issues
    /// `CREATE TABLE IF NOT EXISTS` (idempotent on its own, but the probe keeps
    /// the migration log honest for legacy DBs that already ran the old
    /// schema.sql before this table was added). Safe to call repeatedly.
    /// Runs AFTER `ensure_disposition_required_state` since it is an
    /// independent table (NOT a column on `bead_overlay`) and therefore does
    /// NOT participate in that CHECK rebuild's column-intersection copy.
    fn ensure_escalation_ledger_table(conn: &Connection) -> Result<(), DaemonError> {
        let has_table: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'escalation_ledger'",
                [],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_table: probe", e))?;
        if !has_table {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS escalation_ledger (\
                   bead_id           TEXT NOT NULL,\
                   reason            TEXT NOT NULL,\
                   context_hash      TEXT NOT NULL,\
                   last_emitted_epoch INTEGER NOT NULL,\
                   PRIMARY KEY (bead_id, reason)\
                 )",
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_table: create", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `escalation_ledger.terminal` column
    /// (1s2q-escalation-dedup Task 2). Older on-disk DBs that got the
    /// `escalation_ledger` table from a pre-Task-2 `ensure_escalation_ledger_table`
    /// predate the `terminal` column declared in the CREATE TABLE block; SQLite
    /// has no `ADD COLUMN IF NOT EXISTS`, so we probe `pragma_table_info` first
    /// and only ALTER when the column is missing. Safe to call repeatedly — a
    /// no-op when the column is already present. Defaults every pre-existing
    /// row to `0` (not terminal), preserving the pre-Task-2 dedup behavior for
    /// rows written before the terminal concept existed.
    fn ensure_escalation_ledger_terminal_column(conn: &Connection) -> Result<(), DaemonError> {
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('escalation_ledger') \
                 WHERE name = 'terminal'",
                [],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_terminal_column: pragma", e))?;
        if !has_col {
            conn.execute(
                "ALTER TABLE escalation_ledger \
                 ADD COLUMN terminal INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| tool_err("ensure_escalation_ledger_terminal_column: add column", e))?;
        }
        Ok(())
    }

    /// Idempotent migration for the `vendor_health` table
    /// (reviewer-outage-resilience Task 1). Same pattern as
    /// `ensure_escalation_ledger_table`: probes `sqlite_master` for the
    /// table's existence, then issues `CREATE TABLE IF NOT EXISTS`. Safe to
    /// call repeatedly. Tracks whether each external review-bot provider
    /// ("coderabbit" or "bugbot") is in-outage or recovered, with a full
    /// audit trail. Runs after `ensure_escalation_ledger_terminal_column`
    /// since it is an independent table.
    fn ensure_vendor_health_table(conn: &Connection) -> Result<(), DaemonError> {
        let has_table: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'vendor_health'",
                [],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(|e| tool_err("ensure_vendor_health_table: probe", e))?;
        if !has_table {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS vendor_health (\
                   vendor               TEXT PRIMARY KEY,\
                   in_outage            INTEGER NOT NULL DEFAULT 0,\
                   consecutive_pending  INTEGER NOT NULL DEFAULT 0,\
                   outage_observations  INTEGER NOT NULL DEFAULT 0,\
                   success_observations INTEGER NOT NULL DEFAULT 0,\
                   last_success_head    TEXT,\
                   last_outage_epoch    INTEGER,\
                   last_observed_head   TEXT,\
                   last_observed_epoch  INTEGER\
                 )",
            )
            .map_err(|e| tool_err("ensure_vendor_health_table: create", e))?;
        }
        Ok(())
    }

    /// Canonical `bead_overlay` column list (in `schema.sql` order). The
    /// DISPOSITION_REQUIRED CHECK rebuild uses this to build the new table and
    /// to copy data by EXPLICIT column name. Keep in sync with `schema.sql`'s
    /// CREATE TABLE and the `ensure_*_column` migrations above.
    const BEAD_OVERLAY_COLUMNS: &'static [&'static str] = &[
        "bead_id",
        "state",
        "attempt",
        "reroll_count",
        "autonomy_secs",
        "spend_usd",
        "pr_number",
        "branch",
        "session_id",
        "updated_at",
        "attempt_er_runner_count",
        "last_er_runner_attempt_at",
        "is_adopted",
        "spawn_failure_count",
        "pre_session_head_sha",
        "park_reason",
        "target_repo",
        "reroll_deferral_count",
        "held_recheck_after",
        "last_er_evidence_hash",
    ];

    /// The canonical `CREATE TABLE bead_overlay` statement (with the current
    /// state CHECK list, incl. `DISPOSITION_REQUIRED`) as shipped in
    /// `schema.sql`, but under a temp name for the rebuild. Hardcoded rather
    /// than transformed from the live DDL so the migration is robust to
    /// whitespace variants, quoted identifiers, and any other legal DDL
    /// formatting that a string-edit would silently break.
    const REBUILD_TABLE_DDL: &'static str = "CREATE TABLE bead_overlay_disposition_migrated (\
        bead_id TEXT PRIMARY KEY, \
        state TEXT NOT NULL CHECK (state IN \
            ('QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY','RE_ROLL','RECOVERY',\
             'REDISPATCHED','BUDGET_HELD','HUMAN_HELD','DISPOSITION_REQUIRED')), \
        attempt INTEGER NOT NULL DEFAULT 1, \
        reroll_count INTEGER NOT NULL DEFAULT 0, \
        autonomy_secs INTEGER NOT NULL DEFAULT 0, \
        spend_usd REAL NOT NULL DEFAULT 0, \
        pr_number INTEGER, \
        branch TEXT, \
        session_id TEXT, \
        updated_at TEXT NOT NULL, \
        attempt_er_runner_count INTEGER NOT NULL DEFAULT 0, \
        last_er_runner_attempt_at INTEGER, \
        is_adopted INTEGER NOT NULL DEFAULT 0, \
        spawn_failure_count INTEGER NOT NULL DEFAULT 0, \
        pre_session_head_sha TEXT, \
        park_reason TEXT, \
        target_repo TEXT, \
        reroll_deferral_count INTEGER NOT NULL DEFAULT 0, \
        held_recheck_after INTEGER, \
        last_er_evidence_hash TEXT)";

    /// Bead jleechan-zaga / issue #348: migrate the `bead_overlay.state` CHECK
    /// constraint to allow `'DISPOSITION_REQUIRED'`. Unlike every other
    /// migration above (which add nullable/defaulted COLUMNS via `ALTER TABLE
    /// … ADD COLUMN`), this changes a CHECK constraint — and SQLite supports
    /// no `ALTER TABLE … DROP/ALTER CONSTRAINT`. `CREATE TABLE IF NOT EXISTS`
    /// in `schema.sql` is a NO-OP on a live DB that already has the table, so
    /// it never updates the constraint: a live daemon started against a
    /// pre-#348 DB would hit `CHECK constraint failed` the first time it tried
    /// to persist a `DISPOSITION_REQUIRED` bead. The only portable fix is the
    /// documented SQLite table-rebuild dance (create-copy-drop-rename).
    ///
    /// Robust migration (r3): the need to migrate is detected by PROBING —
    /// attempting a `DISPOSITION_REQUIRED` INSERT inside a savepoint that is
    /// always rolled back — NOT by string-matching the stored DDL (fragile to
    /// whitespace / quoted identifiers). When a migration IS needed, the new
    /// table is built from a CANONICAL hardcoded CREATE (`REBUILD_TABLE_DDL`),
    /// and data is copied by EXPLICIT column name for the intersection of the
    /// live table's columns and the canonical set — never a positional
    /// `SELECT *` against a transformed copy of the old DDL. Runs after every
    /// `ensure_*_column` migration so the live column set is complete.
    fn ensure_disposition_required_state(conn: &Connection) -> Result<(), DaemonError> {
        // Probe: is `DISPOSITION_REQUIRED` already accepted by the live CHECK?
        // Attempt the INSERT inside a savepoint we ALWAYS roll back so the
        // probe row never persists; a `CHECK constraint failed` means migrate.
        conn.execute_batch("SAVEPOINT drp_probe")
            .map_err(|e| tool_err("ensure_disposition_required_state: savepoint", e))?;
        let probe = conn.execute(
            "INSERT INTO bead_overlay (bead_id, state, updated_at) \
             VALUES ('__drp_migration_probe__', 'DISPOSITION_REQUIRED', '')",
            [],
        );
        conn.execute_batch("ROLLBACK TO drp_probe; RELEASE drp_probe")
            .map_err(|e| tool_err("ensure_disposition_required_state: rollback probe", e))?;
        let needs_migration = match probe {
            Ok(_) => false, // CHECK already allows it (fresh/already-migrated DB).
            Err(ref e) if e.to_string().to_ascii_lowercase().contains("check constraint") => true,
            Err(e) => {
                // A different failure (e.g. a table shape we don't understand)
                // — do not attempt a rebuild we can't reason about; surface it.
                return Err(tool_err("ensure_disposition_required_state: probe", e));
            }
        };
        if !needs_migration {
            return Ok(());
        }

        // Copy only columns that exist in BOTH the live table and the
        // canonical schema, by explicit name (order-independent).
        let mut live_cols: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_table_info('bead_overlay')")
                .map_err(|e| tool_err("ensure_disposition_required_state: pragma prepare", e))?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| tool_err("ensure_disposition_required_state: pragma query", e))?;
            for name in rows {
                live_cols
                    .insert(name.map_err(|e| tool_err("ensure_disposition_required_state: pragma row", e))?);
            }
        }
        let copy_cols: Vec<&str> = Self::BEAD_OVERLAY_COLUMNS
            .iter()
            .copied()
            .filter(|c| live_cols.contains(*c))
            .collect();
        let col_list = copy_cols.join(", ");

        let batch = format!(
            "BEGIN IMMEDIATE;\n\
             {create};\n\
             INSERT INTO bead_overlay_disposition_migrated ({cols}) SELECT {cols} FROM bead_overlay;\n\
             DROP TABLE bead_overlay;\n\
             ALTER TABLE bead_overlay_disposition_migrated RENAME TO bead_overlay;\n\
             COMMIT;",
            create = Self::REBUILD_TABLE_DDL,
            cols = col_list,
        );
        if let Err(e) = conn.execute_batch(&batch) {
            // Best-effort rollback so a half-applied rebuild doesn't wedge the
            // next open; surface the original error either way.
            let _ = conn.execute_batch("ROLLBACK");
            return Err(tool_err("ensure_disposition_required_state: rebuild", e));
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
            .query_map([], |row: &rusqlite::Row| {
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



    fn escalation_should_emit(
        &self,
        bead_id: &str,
        reason: &str,
        context_hash: &str,
        now_epoch: u64,
        refire_secs: u64,
    ) -> Result<bool, DaemonError> {
        let row: Result<(String, i64, i64), rusqlite::Error> = self.conn.query_row(
            "SELECT context_hash, last_emitted_epoch, terminal FROM escalation_ledger \
             WHERE bead_id = ?1 AND reason = ?2",
            params![bead_id, reason],
            |row: &rusqlite::Row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        );
        match row {
            // No prior record — emit.
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(true),
            Err(e) => Err(tool_err("escalation_should_emit: load", e)),
            Ok((prior_hash, last_epoch, terminal)) => {
                let (prior_hash, last_epoch, terminal): (String, i64, i64) = (prior_hash, last_epoch, terminal);
                // 1s2q-escalation-dedup Task 2: a terminal row means the
                // escalation was classified undeliverable (permanent gh
                // error). Never re-emit, regardless of hash or backoff.
                if terminal != 0 {
                    return Ok(false);
                }
                // Hash changed — re-emit regardless of backoff.
                if prior_hash != context_hash {
                    return Ok(true);
                }
                // Same hash: re-emit only if the backoff window has elapsed.
                let last = last_epoch.max(0_i64) as u64;
                Ok(now_epoch.saturating_sub(last) >= refire_secs)
            }
        }
    }


    fn record_escalation_emit(
        &self,
        bead_id: &str,
        reason: &str,
        context_hash: &str,
        now_epoch: u64,
    ) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO escalation_ledger (bead_id, reason, context_hash, last_emitted_epoch) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(bead_id, reason) DO UPDATE SET \
                   context_hash = excluded.context_hash, \
                   last_emitted_epoch = excluded.last_emitted_epoch",
                params![bead_id, reason, context_hash, now_epoch as i64],
            )
            .map_err(|e| tool_err("record_escalation_emit: upsert", e))?;
        Ok(())
    }


    fn mark_escalation_undeliverable(
        &self,
        bead_id: &str,
        reason: &str,
    ) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO escalation_ledger (bead_id, reason, context_hash, last_emitted_epoch, terminal) \
                 VALUES (?1, ?2, '', 0, 1) \
                 ON CONFLICT(bead_id, reason) DO UPDATE SET terminal = 1",
                params![bead_id, reason],
            )
            .map_err(|e| tool_err("mark_escalation_undeliverable: upsert", e))?;
        Ok(())
    }


    #[allow(clippy::type_complexity)]
    fn vendor_health(&self, vendor: &str) -> Result<Option<VendorHealth>, DaemonError> {
        let row: Result<
            (i64, i64, i64, i64, Option<String>, Option<i64>, Option<String>, Option<i64>),
            rusqlite::Error,
        > = self.conn.query_row(
            "SELECT in_outage, consecutive_pending, outage_observations, \
                    success_observations, last_success_head, last_outage_epoch, \
                    last_observed_head, last_observed_epoch \
             FROM vendor_health WHERE vendor = ?1",
            params![vendor],
            |row: &rusqlite::Row| -> rusqlite::Result<(i64, i64, i64, i64, Option<String>, Option<i64>, Option<String>, Option<i64>)> {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        );
        match row {
            Ok((in_outage, consecutive_pending, outage_observations, success_observations, last_success_head, last_outage_epoch, last_observed_head, last_observed_epoch)) => {
                let (in_outage, consecutive_pending, outage_observations, success_observations, last_success_head, last_outage_epoch, last_observed_head, last_observed_epoch): (i64, i64, i64, i64, Option<String>, Option<i64>, Option<String>, Option<i64>) = (in_outage, consecutive_pending, outage_observations, success_observations, last_success_head, last_outage_epoch, last_observed_head, last_observed_epoch);
                Ok(Some(VendorHealth {
                vendor: vendor.to_string(),
                in_outage: in_outage != 0,
                consecutive_pending: consecutive_pending.max(0_i64) as u32,
                outage_observations: outage_observations.max(0_i64) as u32,
                success_observations: success_observations.max(0_i64) as u32,
                last_success_head,
                last_outage_epoch: last_outage_epoch.map(|v: i64| v.max(0_i64) as u64),
                last_observed_head,
                last_observed_epoch: last_observed_epoch.map(|v: i64| v.max(0_i64) as u64),
            }))
            },
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(tool_err("vendor_health: load", e)),
        }
    }


    fn record_vendor_observation(
        &self,
        vendor: &str,
        is_outage_marker: bool,
        is_success: bool,
        head_sha: &str,
        now_epoch: u64,
        consecutive_pending_threshold: u32,
    ) -> Result<VendorHealth, DaemonError> {
        let prior = self.vendor_health(vendor)?;
        let mut row = prior.clone().unwrap_or(VendorHealth {
            vendor: vendor.to_string(),
            in_outage: false,
            consecutive_pending: 0,
            outage_observations: 0,
            success_observations: 0,
            last_success_head: None,
            last_outage_epoch: None,
            last_observed_head: None,
            last_observed_epoch: None,
        });
        row.last_observed_head = Some(head_sha.to_string());
        row.last_observed_epoch = Some(now_epoch);
        if is_success {
            row.success_observations += 1;
            row.consecutive_pending = 0;
            row.last_success_head = Some(head_sha.to_string());
            if row.in_outage {
                row.in_outage = false;
            }
        } else if is_outage_marker {
            row.outage_observations += 1;
            row.consecutive_pending += 1;
            if row.consecutive_pending >= consecutive_pending_threshold && !row.in_outage {
                row.in_outage = true;
                row.last_outage_epoch = Some(now_epoch);
            }
        }
        self.conn
            .execute(
                "INSERT INTO vendor_health (\
                   vendor, in_outage, consecutive_pending, outage_observations, \
                   success_observations, last_success_head, last_outage_epoch, \
                   last_observed_head, last_observed_epoch\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(vendor) DO UPDATE SET \
                   in_outage = excluded.in_outage, \
                   consecutive_pending = excluded.consecutive_pending, \
                   outage_observations = excluded.outage_observations, \
                   success_observations = excluded.success_observations, \
                   last_success_head = excluded.last_success_head, \
                   last_outage_epoch = excluded.last_outage_epoch, \
                   last_observed_head = excluded.last_observed_head, \
                   last_observed_epoch = excluded.last_observed_epoch",
                params![
                    vendor,
                    if row.in_outage { 1 } else { 0 },
                    row.consecutive_pending as i64,
                    row.outage_observations as i64,
                    row.success_observations as i64,
                    row.last_success_head,
                    row.last_outage_epoch.map(|v| v as i64),
                    row.last_observed_head,
                    row.last_observed_epoch.map(|v| v as i64),
                ],
            )
            .map_err(|e| tool_err("record_vendor_observation: upsert", e))?;
        Ok(row)
    }
    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "UPDATE bead_overlay SET state = 'QUEUED', session_id = NULL, branch = NULL \
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
                |row: &rusqlite::Row| {
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
        let mut id_stmt = self
            .conn
            .prepare(
                "SELECT bead_id FROM bead_overlay \
                 WHERE state = 'HUMAN_HELD' AND attempt < ?1 \
                 AND (park_reason IS NULL \
                      OR (park_reason NOT LIKE 'circuit-breaker%' \
                          AND park_reason != 'unmapped_target_repo' \
                          AND park_reason != 'worktree_remote_mismatch'))",
            )
            .map_err(|e| tool_err("recover_human_held id select prepare", e))?;
        let recovered_ids: Vec<String> = id_stmt
            .query_map(params![max_attempt as i64], |row| row.get::<_, String>(0))
            .map_err(|e| tool_err("recover_human_held id select query", e))?
            .filter_map(|r| r.ok())
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
            .query_map(&select_params[..], |row: &rusqlite::Row| {
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
            .query_map(params![max_attempt as i64], |row: &rusqlite::Row| {
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
                |row: &rusqlite::Row| {
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
                |row: &rusqlite::Row| row.get(0),
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

