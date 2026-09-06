// Cross-model review P2 #1 (bead jleechan-n6mk, follow-up to PR #447): the
// escalation-dedup tick-level tests in `tick_integration.rs` exercise the
// dispatch-before-recovery ordering + the context-hash dedup ledger, but they
// do so against the in-memory `FakeStateStore`. That hides bugs that only
// surface with real SQLite persistence (transaction boundaries, ordering of
// the `INSERT ... ON CONFLICT` upserts, the `terminal` column semantics
// surviving a real schema migration). This file ports those tests to run
// against a real `SqliteStateStore` opened against `schema.sql` so the
// escalation-dedup ledger + dispatch-scheduling-guarantee are validated
// against the same backend the daemon uses in production.
//
// We keep `FakeStateStore` everywhere else — the request is specifically to
// give the escalation dedup path real-SQLite coverage, not to abandon fakes
// (the fakes power ~99% of the tick tests and are faster / more introspectable).
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeTracker, FakeVcs};
use daemon::config::{Config, RepoConfig};
use daemon::state::{BeadOverlay, OverlayState, SqliteStateStore, StateStore};
use daemon::tick::{run_tick, TickDeps};
use daemon::tools::Bead;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

/// Wrapper around a `SqliteStateStore` that owns a parallel raw `Connection`
/// for test-introspection (delete the sentinel row, dump the ledger). The
/// raw conn is opened against the same on-disk file so writes through
/// `SqliteStateStore` are visible to the introspection conn.
struct SqliteTestStore {
    store: SqliteStateStore,
    inspect: Connection,
    /// Hold the path so the on-disk file (and its WAL/SHM sidecars) can be
    /// cleaned up in `Drop`. WAL mode requires the file to exist while any
    /// connection is open — we can't unlink it before the test ends.
    _path: std::path::PathBuf,
}

impl Drop for SqliteTestStore {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self._path.display()));
        }
    }
}

impl SqliteTestStore {
    fn new() -> Self {
        // Use a unique on-disk file rather than `:memory:` so the
        // `SqliteStateStore` (which calls `Connection::open`) and our
        // introspection `Connection` can both attach. A tempfile per test
        // gives us isolation without leaking state across tests. We DON'T
        // unlink the file while the test runs — WAL mode requires that no
        // other process hold the inode so SQLite can create the `-wal`/
        // `-shm` sidecars; the file is removed in `Drop`. The
        // (pid, monotonic counter) tuple guarantees uniqueness across tests
        // even when several tests share the same pid and same epoch second.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dark-factory-n6mk-{}-{}-{}.sqlite",
            std::process::id(),
            now_epoch(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        ));
        let store = SqliteStateStore::open(&path).expect("SqliteStateStore::open must succeed");
        let inspect = Connection::open(&path).expect("inspect Connection::open");
        Self {
            store,
            inspect,
            _path: path,
        }
    }

    fn ledger_row(&self, bead_id: &str, reason: &str) -> Option<(String, i64, i64)> {
        self.inspect
            .query_row(
                "SELECT context_hash, last_emitted_epoch, terminal \
                 FROM escalation_ledger WHERE bead_id = ?1 AND reason = ?2",
                params![bead_id, reason],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
            )
            .ok()
    }

    fn delete_sentinel(&self, bead_id: &str, attempt: u32) {
        self.inspect
            .execute(
                "DELETE FROM review_rejection WHERE bead_id = ?1 AND attempt = ?2",
                params![bead_id, attempt as i64],
            )
            .expect("DELETE FROM review_rejection");
    }
}

impl StateStore for SqliteTestStore {
    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, daemon::errors::DaemonError> {
        self.store.load(bead_id)
    }
    fn save(&self, overlay: &BeadOverlay) -> Result<(), daemon::errors::DaemonError> {
        self.store.save(overlay)
    }
    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), daemon::errors::DaemonError> {
        self.store.register_branch(bead_id, branch)
    }
    fn owned_branches(&self) -> Result<Vec<String>, daemon::errors::DaemonError> {
        self.store.owned_branches()
    }
    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, daemon::errors::DaemonError> {
        self.store.bead_id_for_branch(branch)
    }
    fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, daemon::errors::DaemonError> {
        self.store.list_active_overlays()
    }
    fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), daemon::errors::DaemonError> {
        self.store.bump_autonomy_secs(bead_id, delta_secs)
    }
    fn human_held_at_or_above_attempt(
        &self,
        max_attempt: u32,
    ) -> Result<Vec<BeadOverlay>, daemon::errors::DaemonError> {
        self.store.human_held_at_or_above_attempt(max_attempt)
    }
    fn save_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
        reviewer: &str,
        feedback_hash: &str,
        feedback_text: &str,
    ) -> Result<(), daemon::errors::DaemonError> {
        self.store
            .save_rejection(bead_id, attempt, reviewer, feedback_hash, feedback_text)
    }
    fn load_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<Option<(String, String)>, daemon::errors::DaemonError> {
        self.store.load_rejection(bead_id, attempt)
    }
    fn remediation_session_spawned_attempt(
        &self,
        bead_id: &str,
    ) -> Result<Option<u32>, daemon::errors::DaemonError> {
        self.store.remediation_session_spawned_attempt(bead_id)
    }
    fn mark_remediation_session_spawned(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<(), daemon::errors::DaemonError> {
        self.store.mark_remediation_session_spawned(bead_id, attempt)
    }
    fn save_remediation_session_spawned(
        &self,
        overlay: &BeadOverlay,
        attempt: u32,
        ao_project: &str,
    ) -> Result<(), daemon::errors::DaemonError> {
        self.store.save_remediation_session_spawned(overlay, attempt, ao_project)
    }
    fn escalation_should_emit(
        &self,
        bead_id: &str,
        reason: &str,
        context_hash: &str,
        now_epoch: u64,
        refire_secs: u64,
    ) -> Result<bool, daemon::errors::DaemonError> {
        self.store
            .escalation_should_emit(bead_id, reason, context_hash, now_epoch, refire_secs)
    }
    fn record_escalation_emit(
        &self,
        bead_id: &str,
        reason: &str,
        context_hash: &str,
        now_epoch: u64,
    ) -> Result<(), daemon::errors::DaemonError> {
        self.store
            .record_escalation_emit(bead_id, reason, context_hash, now_epoch)
    }
    fn mark_escalation_undeliverable(
        &self,
        bead_id: &str,
        reason: &str,
    ) -> Result<(), daemon::errors::DaemonError> {
        self.store.mark_escalation_undeliverable(bead_id, reason)
    }
    fn reconcile_dispatching(&self) -> Result<(), daemon::errors::DaemonError> {
        self.store.reconcile_dispatching()
    }
    fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, daemon::errors::DaemonError> {
        self.store.recover_human_held(max_attempt)
    }
}

fn now_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[test]
fn dependency_admission_survives_restarts_then_dispatches_once_when_ready() {
    let path = std::env::temp_dir().join(format!(
        "dark-factory-dependency-restart-{}-{}.sqlite",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let telemetry = path.with_extension("jsonl");
    let _ = std::fs::remove_file(&telemetry);
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "dependency-restart-bead".into(),
        title: "blocked across restart".into(),
        description: "target_repo: owner/repo".into(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#810".into()),
    });
    *tracker.ready_ids.borrow_mut() = Some(HashSet::new());
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"ready"}"#.into(),
    ));
    let cfg = test_cfg();
    let vcs = test_vcs();

    {
        let store = SqliteStateStore::open(&path).unwrap();
        store
            .save(&BeadOverlay {
                bead_id: "dependency-restart-bead".into(),
                state: OverlayState::Queued,
                attempt: 3,
                reroll_count: 1,
                autonomy_secs: 77,
                spend_usd: 2.5,
                pr_number: None,
                branch: None,
                session_id: None,
                session_ao_project: None,
                is_adopted: false,
                spawn_failure_count: 2,
                transient_error_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: Some("owner/repo".into()),
                attempt_started_at: None,
            })
            .unwrap();
        run_tick(
            &TickDeps {
                scm: &scm,
                tracker: &tracker,
                sessions: &sessions,
                llm: &llm,
                store: &store,
                vcs: &vcs,
                cfg: &cfg,
                telemetry_log: &telemetry,
                vendor_health: None,
            },
            0,
            0,
        )
        .unwrap();
    }

    for tick in [1_u64] {
        let store = SqliteStateStore::open(&path).unwrap();
        run_tick(
            &TickDeps {
                scm: &scm,
                tracker: &tracker,
                sessions: &sessions,
                llm: &llm,
                store: &store,
                vcs: &vcs,
                cfg: &cfg,
                telemetry_log: &telemetry,
                vendor_health: None,
            },
            tick,
            0,
        )
        .unwrap();
        let overlay = store.load("dependency-restart-bead").unwrap().unwrap();
        assert_eq!(overlay.state, OverlayState::Queued);
        assert_eq!(overlay.attempt, 3);
        assert_eq!(overlay.autonomy_secs, 77);
        assert_eq!(overlay.spawn_failure_count, 2);
    }
    assert!(llm.calls.borrow().is_empty());
    assert!(sessions.calls.borrow().is_empty());
    assert_eq!(
        std::fs::read_to_string(&telemetry)
            .unwrap()
            .lines()
            .filter(|line| line.contains("DEPENDENCY_BLOCKED"))
            .count(),
        1,
        "unchanged blocked disposition must be deduplicated across restart"
    );

    tracker
        .ready_ids
        .borrow_mut()
        .as_mut()
        .unwrap()
        .insert("dependency-restart-bead".into());
    {
        let store = SqliteStateStore::open(&path).unwrap();
        let summary = run_tick(
            &TickDeps {
                scm: &scm,
                tracker: &tracker,
                sessions: &sessions,
                llm: &llm,
                store: &store,
                vcs: &vcs,
                cfg: &cfg,
                telemetry_log: &telemetry,
                vendor_health: None,
            },
            2,
            0,
        )
        .unwrap();
        assert_eq!(summary.beads_routed, 1);
        assert_eq!(summary.beads_dispatched, 1);
        assert_eq!(
            store.load("dependency-restart-bead").unwrap().unwrap().state,
            OverlayState::Dispatched
        );
    }
    assert_eq!(
        sessions
            .calls
            .borrow()
            .iter()
            .filter(|call| call.starts_with("spawn("))
            .count(),
        1
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_file(telemetry);
}

#[test]
fn adopted_remediation_marker_is_migrated_and_persistent() {
    let path = std::env::temp_dir().join(format!(
        "dark-factory-marker-legacy-{}-{}.sqlite",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    {
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch(include_str!("../contracts/schema.sql"))
            .unwrap();
        legacy
            .execute_batch("DROP TABLE remediation_session_spawned")
            .unwrap();
    }
    let store = SqliteStateStore::open(&path).unwrap();
    assert_eq!(
        store
            .remediation_session_spawned_attempt("marker-bead")
            .unwrap(),
        None
    );
    store
        .mark_remediation_session_spawned("marker-bead", 7)
        .unwrap();
    assert_eq!(
        store
            .remediation_session_spawned_attempt("marker-bead")
            .unwrap(),
        Some(7)
    );
    let table_exists: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'remediation_session_spawned'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_exists, 1, "legacy-store migration must create marker table");
    drop(store);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn restarted_tick_reaps_a_persisted_routed_session_in_its_recorded_project() {
    let path = std::env::temp_dir().join(format!(
        "dark-factory-session-project-restart-{}-{}.sqlite",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let overlay = BeadOverlay {
        bead_id: "routed-restart-bead".into(),
        state: OverlayState::Dispatched,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: None,
        branch: Some("factory/routed-restart-bead-r1".into()),
        session_id: Some("session-routed-restart".into()),
        session_ao_project: Some("secondary-project".into()),
        is_adopted: false,
        spawn_failure_count: 0,
        transient_error_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: Some("owner/repo".into()),
        attempt_started_at: None,
    };
    {
        let store = SqliteStateStore::open(&path).unwrap();
        store.save(&overlay).unwrap();
        store
            .register_branch(&overlay.bead_id, overlay.branch.as_deref().unwrap())
            .unwrap();
    }

    // Reopen the store to model a daemon process restart. The current route
    // deliberately differs from the recorded owner: cleanup must use the
    // persisted `secondary-project`, not the config's `repo` project.
    let store = SqliteStateStore::open(&path).unwrap();
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.set_session_health_failure("session-routed-restart", "login expired");
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_routed_restart_{}_{}.jsonl",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            vendor_health: None,
        },
        0,
        10,
    )
    .unwrap();

    assert_eq!(
        sessions.stop_in_project_calls.borrow().as_slice(),
        [("session-routed-restart".into(), "secondary-project".into())],
        "restart cleanup must retain the durable routed AO project"
    );
    let persisted = store.load("routed-restart-bead").unwrap().unwrap();
    assert_eq!(persisted.state, OverlayState::Queued);
    assert_eq!(persisted.session_id, None);
    assert_eq!(persisted.session_ao_project, None);

    drop(store);
    let _ = std::fs::remove_file(&telemetry_log);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn restarted_tick_retains_a_routed_session_when_scoped_cleanup_fails() {
    let path = std::env::temp_dir().join(format!(
        "dark-factory-session-project-stop-failure-{}-{}.sqlite",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let overlay = BeadOverlay {
        bead_id: "routed-stop-failure-bead".into(),
        state: OverlayState::Dispatched,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: None,
        branch: Some("factory/routed-stop-failure-bead-r1".into()),
        session_id: Some("session-routed-stop-failure".into()),
        session_ao_project: Some("secondary-project".into()),
        is_adopted: false,
        spawn_failure_count: 0,
        transient_error_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: Some("owner/repo".into()),
        attempt_started_at: None,
    };
    {
        let store = SqliteStateStore::open(&path).unwrap();
        store.save(&overlay).unwrap();
        store
            .register_branch(&overlay.bead_id, overlay.branch.as_deref().unwrap())
            .unwrap();
    }

    let store = SqliteStateStore::open(&path).unwrap();
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.set_session_health_failure("session-routed-stop-failure", "login expired");
    sessions.fail_stop_for("session-routed-stop-failure");
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_routed_stop_failure_{}_{}.jsonl",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            vendor_health: None,
        },
        0,
        10,
    )
    .unwrap();

    assert_eq!(
        sessions.stop_in_project_calls.borrow().as_slice(),
        [("session-routed-stop-failure".into(), "secondary-project".into())]
    );
    let persisted = store.load("routed-stop-failure-bead").unwrap().unwrap();
    assert_eq!(persisted.state, OverlayState::Dispatched);
    assert_eq!(persisted.session_id.as_deref(), Some("session-routed-stop-failure"));
    assert_eq!(persisted.session_ao_project.as_deref(), Some("secondary-project"));

    drop(store);
    let _ = std::fs::remove_file(&telemetry_log);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[test]
fn restarted_tick_fails_closed_and_refuses_session_ops_on_legacy_null_project_row() {
    let path = std::env::temp_dir().join(format!(
        "dark-factory-session-project-legacy-null-{}-{}.sqlite",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let overlay = BeadOverlay {
        bead_id: "legacy-null-bead".into(),
        state: OverlayState::Dispatched,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: None,
        branch: Some("factory/legacy-null-bead-r1".into()),
        session_id: Some("session-legacy-null".into()),
        session_ao_project: None,
        is_adopted: false,
        spawn_failure_count: 0,
        transient_error_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: Some("unrouted/legacy-repo".into()),
        attempt_started_at: None,
    };
    {
        let store = SqliteStateStore::open(&path).unwrap();
        store.save(&overlay).unwrap();
        store
            .register_branch(&overlay.bead_id, overlay.branch.as_deref().unwrap())
            .unwrap();
    }

    // Reopen the store to model a daemon process restart with a legacy NULL project row.
    // The tick MUST NOT guess authority from mutable routing or default projects;
    // it must refuse session operations and retain the handle.
    let store = SqliteStateStore::open(&path).unwrap();
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.set_session_health_failure("session-legacy-null", "login expired");
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_legacy_null_{}_{}.jsonl",
        std::process::id(),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            vendor_health: None,
        },
        0,
        10,
    )
    .unwrap();

    assert!(
        sessions.stop_in_project_calls.borrow().is_empty(),
        "unowned legacy session must not execute AO kill against any inferred project"
    );
    let persisted = store.load("legacy-null-bead").unwrap().unwrap();
    assert_eq!(persisted.state, OverlayState::Dispatched);
    assert_eq!(
        persisted.session_id.as_deref(),
        Some("session-legacy-null"),
        "legacy unowned session handle must be retained for operator triage"
    );
    assert_eq!(persisted.session_ao_project, None);

    drop(store);
    let _ = std::fs::remove_file(&telemetry_log);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn test_repo_cfg(project: &str) -> RepoConfig {
    RepoConfig {
        ao_project: project.into(),
        push_remote: "origin".into(),
        local_checkout: Some(std::env::current_dir().unwrap()),
    }
}

fn test_vcs() -> FakeVcs {
    let mut vcs = FakeVcs::default();
    vcs.heads.insert("main".into(), "base-sha-123".into());
    vcs
}

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
        ao_project: None,
        base_branch: "main".into(),
        stage: 1,
        max_workers: 30,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 60,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
        reroll_head_stability_window_secs: 1,
        reroll_death_confirm_secs: 0,
        held_recheck_cooldown_secs: 900,
        repos: HashMap::from([
            ("owner/repo".into(), test_repo_cfg("repo")),
            (
                "myorg/global-real-repo".into(),
                test_repo_cfg("global-real-repo"),
            ),
        ]),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

/// Mirrors `dispatch_guarantee_queued_bead_dispatched_despite_escalation_backlog`
/// from `tick_integration.rs` against a real `SqliteStateStore`. The dispatch
/// invariant — `run_slow_tier` must run BEFORE `run_recovery_step` — is the
/// structural fix for the 65-minute starvation incident; validating it through
/// the real SQLite backend ensures no regression hides behind `FakeStateStore`'s
/// in-memory transaction semantics.
#[test]
fn sqlite_dispatch_guarantee_queued_bead_dispatched_despite_escalation_backlog() {
    const QUEUED_BEAD_ID: &str = "queued-bead-sql-42";

    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = SqliteTestStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Seed 9 HUMAN_HELD beads at the recovery cap (attempt=10).
    for i in 0..9u32 {
        let bead_id = format!("escalation-bead-sql-{i}");
        let pr_number = 2000 + i as u64;
        store
            .save(&BeadOverlay {
                bead_id: bead_id.clone(),
                state: OverlayState::HumanHeld,
                attempt: 10,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(pr_number),
                branch: Some(format!("factory/{bead_id}-r10")),
                session_id: None,
                session_ao_project: None,
                is_adopted: false,
                spawn_failure_count: 0,
                transient_error_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: Some("owner/repo".to_string()),
                attempt_started_at: None,
            })
            .unwrap();
    }

    tracker.candidates.borrow_mut().push(Bead {
        id: QUEUED_BEAD_ID.into(),
        title: "Legitimately queued bead (sqlite)".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#9999".into()),
    });
    store
        .save(&BeadOverlay {
            bead_id: QUEUED_BEAD_ID.into(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: None,
            session_id: None,
            session_ao_project: None,
            is_adopted: false,
            spawn_failure_count: 0,
            transient_error_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: Some("owner/repo".to_string()),
            attempt_started_at: None,
        })
        .unwrap();

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_sqlite_dispatch_guarantee_{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store.store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");

    assert_eq!(
        summary.beads_dispatched, 1,
        "the QUEUED bead must be dispatched on the first tick despite 9 escalation beads (sqlite)"
    );
    let queued_overlay = store.load(QUEUED_BEAD_ID).unwrap().unwrap();
    assert_eq!(
        queued_overlay.state,
        OverlayState::Dispatched,
        "the QUEUED bead must reach DISPATCHED state (sqlite)"
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("TASK_DISPATCHED") && log.contains(QUEUED_BEAD_ID),
        "TASK_DISPATCHED must be emitted for the QUEUED bead; got: {log}"
    );

    assert_eq!(
        summary.beads_escalated, 9,
        "all 9 escalation beads at the recovery cap must be escalated (sqlite)"
    );
    assert!(
        log.contains("ESCALATION_REQUIRED"),
        "escalation telemetry must be emitted for the cap beads; got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Mirrors `escalation_dedup_tick_level_identical_payload_suppressed_changed_context_re_emits`
/// from `tick_integration.rs` against a real `SqliteStateStore`. Validates the
/// `escalation_ledger` upsert + `escalation_should_emit` SELECT path under real
/// SQLite — the same path that suppressed the live production incident.
#[test]
fn sqlite_escalation_dedup_tick_level_identical_payload_suppressed_changed_context_re_emits() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = SqliteTestStore::new();
    let vcs = test_vcs();
    let cfg = test_cfg();

    let bead_id = "bead-dedup-tick-sql";
    store
        .save(&BeadOverlay {
            bead_id: bead_id.into(),
            state: OverlayState::HumanHeld,
            attempt: 10,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(9006),
            branch: Some("factory/bead-dedup-r10".into()),
            session_id: None,
            session_ao_project: None,
            is_adopted: false,
            spawn_failure_count: 0,
            transient_error_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: Some("owner/repo".to_string()),
            attempt_started_at: None,
        })
        .unwrap();

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_sqlite_escalation_dedup_tick_level_{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store.store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    // ── Tick 1: first escalation → ESCALATION_REQUIRED emitted ──
    let summary1 = run_tick(&deps, 0, 0).expect("tick 1 should succeed");
    assert_eq!(summary1.beads_escalated, 1);
    assert_eq!(summary1.escalations_suppressed, 0);
    let log1 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(log1.contains("ESCALATION_REQUIRED"));

    let (ctx_hash, last_epoch, terminal) = store
        .ledger_row(bead_id, "human_held_recovery_attempt_cap_reached")
        .expect("tick 1: dedup ledger must have a row after emit");
    assert!(!ctx_hash.is_empty(), "context hash must be populated");
    assert!(last_epoch > 0, "last_emitted_epoch must be stamped");
    assert_eq!(terminal, 0, "non-terminal ledger row has terminal=0");
    assert!(store.load_rejection(bead_id, u32::MAX).unwrap().is_some());

    // ── Tick 2: clear sentinel, same context → suppressed ──
    store.delete_sentinel(bead_id, u32::MAX);
    let summary2 = run_tick(&deps, 1, 0).expect("tick 2 should succeed");
    assert_eq!(
        summary2.escalations_suppressed, 1,
        "tick 2: same context hash within backoff must be suppressed (sqlite)"
    );
    let log2 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        log2.matches("ESCALATION_REQUIRED").count(),
        1,
        "ESCALATION_REQUIRED must NOT be re-emitted on tick 2 (sqlite)"
    );
    // Ledger row unchanged on suppression: same hash, same epoch, terminal still 0.
    let (ctx_hash2, last_epoch2, terminal2) = store
        .ledger_row(bead_id, "human_held_recovery_attempt_cap_reached")
        .unwrap();
    assert_eq!(
        ctx_hash2, ctx_hash,
        "context hash must NOT change on a suppressed re-fire"
    );
    assert_eq!(
        last_epoch2, last_epoch,
        "last_emitted_epoch must NOT change on a suppressed re-fire"
    );
    assert_eq!(terminal2, 0);

    // ── Tick 3: clear sentinel, change context (pr_number/branch) → re-emit ──
    store.delete_sentinel(bead_id, u32::MAX);
    let mut overlay = store.load(bead_id).unwrap().unwrap();
    overlay.pr_number = Some(9007);
    overlay.branch = Some("factory/bead-dedup-r10-v2".into());
    store.save(&overlay).unwrap();

    let summary3 = run_tick(&deps, 2, 0).expect("tick 3 should succeed");
    assert_eq!(summary3.escalations_suppressed, 0);
    assert_eq!(summary3.beads_escalated, 1);
    let log3 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        log3.matches("ESCALATION_REQUIRED").count(),
        2,
        "ESCALATION_REQUIRED must re-emit after context change (sqlite)"
    );
    let (ctx_hash3, _, terminal3) = store
        .ledger_row(bead_id, "human_held_recovery_attempt_cap_reached")
        .unwrap();
    assert_ne!(
        ctx_hash3, ctx_hash,
        "context hash MUST change when pr_number/branch change"
    );
    assert_eq!(terminal3, 0);

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Permanent-notification-failure path through real SQLite: a transient
/// `escalation_should_emit` result followed by `mark_escalation_undeliverable`
/// must flip the ledger row to `terminal = 1`, and subsequent ticks must
/// suppress regardless of context or backoff window. Catches a regression
/// class where a fake's in-memory upsert mask hides an `INSERT ... ON CONFLICT`
/// ordering bug.
#[test]
fn sqlite_escalation_dedup_terminal_marker_survives_real_upsert() {
    let store = SqliteTestStore::new();

    // Simulate a notification that was first emitted, then classified as
    // permanent (e.g. `invalid issue format: "local-xxx"`).
    store
        .record_escalation_emit("bead-perma", "human_held_recovery_attempt_cap_reached", "deadbeef", 1000)
        .unwrap();
    let (_, _, terminal_before) = store
        .ledger_row("bead-perma", "human_held_recovery_attempt_cap_reached")
        .unwrap();
    assert_eq!(terminal_before, 0);

    store
        .mark_escalation_undeliverable("bead-perma", "human_held_recovery_attempt_cap_reached")
        .unwrap();
    let (_, _, terminal_after) = store
        .ledger_row("bead-perma", "human_held_recovery_attempt_cap_reached")
        .unwrap();
    assert_eq!(terminal_after, 1, "mark_escalation_undeliverable must flip terminal=1");

    // Should-suppress regardless of context hash or epoch:
    assert!(!store
        .escalation_should_emit(
            "bead-perma",
            "human_held_recovery_attempt_cap_reached",
            "totally-different-hash",
            10_000,
            3_600,
        )
        .unwrap());

    // A fresh mark on a non-existent (bead_id, reason) pair must insert a
    // terminal row, not error.
    store
        .mark_escalation_undeliverable("fresh-bead", "fresh-reason")
        .unwrap();
    let (_, last_epoch, terminal_fresh) = store
        .ledger_row("fresh-bead", "fresh-reason")
        .unwrap();
    assert_eq!(terminal_fresh, 1, "fresh terminal row must be inserted as terminal");
    assert_eq!(
        last_epoch, 0,
        "terminal-only upsert uses epoch 0 to avoid spurious backoff windows"
    );

    // Suppression crosses re-emit attempts: an `record_escalation_emit` AFTER
    // `mark_escalation_undeliverable` must NOT clear terminal (mirrors the
    // FakeStateStore invariant).
    store
        .record_escalation_emit("bead-perma", "human_held_recovery_attempt_cap_reached", "newhash", 5000)
        .unwrap();
    let (_, _, terminal_post) = store
        .ledger_row("bead-perma", "human_held_recovery_attempt_cap_reached")
        .unwrap();
    assert_eq!(
        terminal_post, 1,
        "record_escalation_emit after mark_escalation_undeliverable must NOT clear terminal"
    );
}
