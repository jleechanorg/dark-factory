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
}

pub trait StateStore {
    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError>;
    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError>;
    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError>;
    /// Deletion guard: daemon may delete ONLY refs returned here (spec §4.2.8).
    fn owned_branches(&self) -> Result<Vec<String>, DaemonError>;
    /// Reverse-lookup: branch → bead_id (used by fast_tier to find drive-existing-pr beads).
    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError>;
    fn increment_active_autonomy(&self, elapsed_secs: u64) -> Result<Vec<BeadOverlay>, DaemonError>;
    fn save_rejection(&self, bead_id: &str, attempt: u32, reviewer: &str, feedback_hash: &str, feedback_text: &str) -> Result<(), DaemonError>;
    fn load_rejection(&self, bead_id: &str, attempt: u32) -> Result<Option<(String, String)>, DaemonError>;
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
        Ok(Self { conn })
    }

    /// In-memory store for tests: same pragmas, schema supplied by the caller
    /// (normally `include_str!("../contracts/schema.sql")`).
    pub fn open_in_memory_with_schema(schema_sql: &str) -> Result<Self, DaemonError> {
        let conn = Connection::open_in_memory().map_err(|e| tool_err("open", e))?;
        Self::configure(&conn, true)?;
        conn.execute_batch(schema_sql)
            .map_err(|e| tool_err("apply schema", e))?;
        Ok(Self { conn })
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
}

impl StateStore for SqliteStateStore {
    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "UPDATE bead_overlay SET state = 'QUEUED' WHERE state = 'DISPATCHING'",
                [],
            )
            .map_err(|e| tool_err("reconcile_dispatching", e))?;
        Ok(())
    }

    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
        self.conn
            .query_row(
                "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, \
                 pr_number, branch, session_id FROM bead_overlay WHERE bead_id = ?1",
                params![bead_id],
                |row| {
                    let state_str: String = row.get(1)?;
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
                 (bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(bead_id) DO UPDATE SET \
                   state=excluded.state, attempt=excluded.attempt, reroll_count=excluded.reroll_count, \
                   autonomy_secs=excluded.autonomy_secs, spend_usd=excluded.spend_usd, \
                   pr_number=excluded.pr_number, branch=excluded.branch, session_id=excluded.session_id, updated_at=excluded.updated_at",
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
                ],
            )
            .map_err(|e| tool_err("save", e))?;
        Ok(())
    }

    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError> {
        self.conn
            .execute(
                "INSERT INTO branch_registry (branch, bead_id, created_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(branch) DO UPDATE SET bead_id=excluded.bead_id, created_at=excluded.created_at",
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
        if elapsed_secs > 0 {
            self.conn.execute(
                "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + ?1, updated_at = ?2 \
                 WHERE state IN ('DISPATCHED', 'ATTESTED')",
                params![elapsed_secs, now_iso8601()],
            ).map_err(|e| tool_err("increment_active_autonomy update", e))?;
        }

        let mut stmt = self.conn.prepare(
            "SELECT bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id \
             FROM bead_overlay WHERE state IN ('DISPATCHED', 'ATTESTED')"
        ).map_err(|e| tool_err("increment_active_autonomy prepare", e))?;
        let rows = stmt.query_map([], |row| {
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
            ))
        }).map_err(|e| tool_err("increment_active_autonomy query", e))?;

        let mut out = Vec::new();
        for r in rows {
            let (bead_id, state_str, attempt, reroll_count, autonomy_secs, spend_usd, pr_number, branch, session_id) = r.map_err(|e| tool_err("increment_active_autonomy row", e))?;
            let state = OverlayState::from_str(&state_str)?;
            out.push(BeadOverlay {
                bead_id,
                state,
                attempt: attempt as u32,
                reroll_count: reroll_count as u32,
                autonomy_secs: autonomy_secs as u64,
                spend_usd,
                pr_number: pr_number.map(|v| v as u64),
                branch,
                session_id,
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn register_branch_upserts_on_conflict() {
        let s = store();
        s.register_branch("b1", "factory/b1-r1").unwrap();
        s.register_branch("b2", "factory/b1-r1").unwrap(); // re-register same branch, different bead
        let branches = s.owned_branches().unwrap();
        assert_eq!(branches, vec!["factory/b1-r1".to_string()]);
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
}
