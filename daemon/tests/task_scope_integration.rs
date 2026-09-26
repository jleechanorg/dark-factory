#[path = "common/mod.rs"]
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeTracker, FakeVcs};
use daemon::config::{Config, RepoConfig};
use daemon::errors::DaemonError;
use daemon::state::{BeadOverlay, OverlayState, SqliteStateStore, StateStore};
use daemon::tick::{run_tick, validate_task_scope, TickDeps};
use daemon::tools::Bead;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;

fn cfg(task_bead_id: Option<&str>) -> Config {
    Config {
        task_bead_id: task_bead_id.map(str::to_string),
        target_repo: "owner/production".into(),
        ao_project: Some("repo".into()),
        base_branch: "main".into(),
        stage: 1,
        max_workers: 40,
        max_batch: 15,
        fast_tick_secs: 1,
        slow_tick_secs: 1,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
        reroll_head_stability_window_secs: 30,
        reroll_death_confirm_secs: 5,
        held_recheck_cooldown_secs: 900,
        repos: HashMap::from([(
            "owner/production".into(),
            RepoConfig {
                ao_project: "repo".into(),
                push_remote: "origin".into(),
                local_checkout: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()),
            },
        )]),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

fn overlay(id: &str, state: OverlayState) -> BeadOverlay {
    BeadOverlay {
        bead_id: id.into(),
        state,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 7,
        spend_usd: 1.25,
        pr_number: None,
        branch: None,
        session_id: None,
        session_ao_project: None,
        is_adopted: false,
        spawn_failure_count: 0,
        transient_error_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: Some("owner/production".into()),
        attempt_started_at: None,
    }
}

fn sqlite_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "dark-factory-task-scope-{label}-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn logical_rows(conn: &Connection, excluded_bead_id: Option<&str>) -> Vec<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT * FROM bead_overlay WHERE (?1 IS NULL OR bead_id != ?1) ORDER BY bead_id")
        .unwrap();
    let columns = stmt.column_count();
    stmt.query_map([excluded_bead_id], |row| {
        let mut values = Vec::with_capacity(columns);
        for index in 0..columns {
            let value = row.get_ref(index)?;
            values.push(format!("{value:?}"));
        }
        Ok(values)
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn matching_checkout() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "dark-factory-task-checkout-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    assert!(std::process::Command::new("git").args(["-c", "user.email=test@example.invalid", "-c", "user.name=test", "commit", "--allow-empty", "-m", "base"]).current_dir(&path).status().unwrap().success());
    assert!(std::process::Command::new("git").args(["remote", "add", "origin", "https://github.com/owner/production.git"]).current_dir(&path).status().unwrap().success());
    path
}

#[test]
fn isolated_tick_dispatches_only_target_and_preserves_unrelated_sqlite_rows() {
    let path = sqlite_path("isolated");
    let store = SqliteStateStore::open(&path).unwrap();
    let target = "repair-target";
    let unrelated = "repair-unrelated";
    store.save(&overlay(target, OverlayState::Queued)).unwrap();
    store.save(&overlay(unrelated, OverlayState::Queued)).unwrap();
    let mut active = overlay("unrelated-active", OverlayState::Dispatched);
    active.branch = Some("factory/unrelated-active-r1".into());
    active.session_id = Some("unrelated-session".into());
    store.save(&active).unwrap();
    for index in 0..745 {
        let mut held = overlay(&format!("operator-held-{index:03}"), OverlayState::HumanHeld);
        held.park_reason = Some("operator_scope_hold_20260907_three_prs".into());
        store.save(&held).unwrap();
    }

    let inspect = Connection::open(&path).unwrap();
    let before = logical_rows(&inspect, Some(target));
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().extend([
        Bead { id: target.into(), title: target.into(), description: "target_repo: owner/production".into(), ..Bead::default() },
        Bead { id: unrelated.into(), title: unrelated.into(), ..Bead::default() },
    ]);
    tracker.ready_ids.borrow_mut().replace([target.into(), unrelated.into()].into_iter().collect());
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    sessions.set_worktree_remote("https://github.com/owner/production.git");
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"STANDARD_PATH","justification":"scoped test"}"#.into(),
    ));
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), "base-sha".into());
    let cfg = cfg(Some(target));
    let checkout = matching_checkout();
    let mut cfg = cfg;
    cfg.repos.get_mut("owner/production").unwrap().local_checkout = Some(checkout.clone());
    let telemetry = path.with_extension("jsonl");
    let deps = TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry, vendor_health: None };

    let summary = run_tick(&deps, 0, 0).unwrap();
    assert_eq!(summary.beads_dispatched, 1);
    let session_calls = sessions.calls.borrow();
    assert_eq!(
        session_calls.iter().filter(|call| *call == &format!("spawn({target})")).count(),
        1,
        "expected exactly one spawn call for target: {session_calls:#?}"
    );
    assert_eq!(
        session_calls.iter().filter(|call| call.starts_with("spawn(")).count(),
        1,
        "expected exactly one spawn call across all beads: {session_calls:#?}"
    );
    assert!(
        !session_calls.iter().any(|call| call == &format!("spawn({unrelated})")),
        "unrelated bead was spawned: {session_calls:#?}"
    );
    assert!(
        session_calls.iter().all(|call| {
            !call.contains("unrelated")
                && !call.contains("operator-held")
                && !call.starts_with("attach(")
                && !call.starts_with("stop(")
        }),
        "unexpected session calls: {session_calls:#?}"
    );
    let target_session_id = store.load(target).unwrap().unwrap().session_id.unwrap();
    assert!(
        session_calls
            .iter()
            .filter(|call| call.starts_with("check_session_health("))
            .all(|call| *call == format!("check_session_health({target_session_id})")),
        "check_session_health called for non-target session: {session_calls:#?}"
    );
    assert!(
        !tracker.calls.borrow().iter().any(|call| call.starts_with("create_bead")),
        "unexpected create_bead in tracker: {:#?}",
        tracker.calls.borrow()
    );
    assert!(
        tracker.calls.borrow().iter().all(|call| !call.contains("unrelated") && !call.contains("operator-held")),
        "tracker called with unrelated identity: {:#?}",
        tracker.calls.borrow()
    );
    assert!(
        scm.calls.borrow().iter().all(|call| !call.contains("unrelated") && !call.contains("operator-held")),
        "scm called with unrelated identity: {:#?}",
        scm.calls.borrow()
    );
    assert!(
        !scm.calls.borrow().iter().any(|call| call.contains("labeled_")),
        "scm called with labeled_: {:#?}",
        scm.calls.borrow()
    );
    assert!(
        llm.calls.borrow().iter().all(|call| !call.contains("unrelated") && !call.contains("operator-held")),
        "llm called with unrelated identity: {:#?}",
        llm.calls.borrow()
    );

    let after = logical_rows(&inspect, Some(target));
    assert_eq!(before, after, "scoped execution changed an unrelated row");
    assert_eq!(store.load(unrelated).unwrap().unwrap().state, OverlayState::Queued);
    assert_eq!(store.load("unrelated-active").unwrap().unwrap().state, OverlayState::Dispatched);

    for suffix in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", path.display())); }
    let _ = std::fs::remove_file(telemetry);
    let _ = std::fs::remove_dir_all(checkout);
}

#[test]
fn invalid_scope_fails_before_tracker_or_scm_calls_and_unscoped_default_is_empty() {
    let path = sqlite_path("invalid");
    let store = SqliteStateStore::open(&path).unwrap();
    let tracker = FakeTracker::new();
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let vcs = FakeVcs::new();
    let scoped_cfg = cfg(Some("bad/id"));
    let telemetry = path.with_extension("jsonl");
    let deps = TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &scoped_cfg, telemetry_log: &telemetry, vendor_health: None };
    let error = run_tick(&deps, 0, 0).unwrap_err();
    assert!(matches!(error, DaemonError::Config(_)));
    assert!(tracker.calls.borrow().is_empty());
    assert!(scm.calls.borrow().is_empty());
    assert!(sessions.calls.borrow().is_empty());
    assert_eq!(cfg(None).task_bead_id, None);
    for suffix in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{}{suffix}", path.display())); }
}

#[test]
fn active_and_ready_scopes_run_without_open_tracker_candidates() {
    for state in [OverlayState::Dispatched, OverlayState::Ready] {
        let target = "lifecycle-target";
        let unrelated = "lifecycle-unrelated";
        let path = sqlite_path(match state {
            OverlayState::Dispatched => "active",
            OverlayState::Ready => "ready",
            _ => unreachable!(),
        });
        let store = SqliteStateStore::open(&path).unwrap();
        let mut target_overlay = overlay(target, state);
        target_overlay.branch = Some("factory/lifecycle-target-r1".into());
        target_overlay.session_id = Some("lifecycle-target-session".into());
        target_overlay.session_ao_project = Some("repo".into());
        store.save(&target_overlay).unwrap();
        let mut unrelated_overlay = overlay(unrelated, OverlayState::Dispatched);
        unrelated_overlay.branch = Some("factory/lifecycle-unrelated-r1".into());
        unrelated_overlay.session_id = Some("lifecycle-unrelated-session".into());
        unrelated_overlay.session_ao_project = Some("repo".into());
        store.save(&unrelated_overlay).unwrap();

        let inspect = Connection::open(&path).unwrap();
        let before = logical_rows(&inspect, Some(target));
        let tracker = FakeTracker::new();
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let llm = FakeLlm::new();
        let vcs = FakeVcs::new();
        let mut scoped_cfg = cfg(Some(target));
        scoped_cfg.fast_tick_secs = 1;
        scoped_cfg.slow_tick_secs = 600;
        let telemetry = path.with_extension("jsonl");
        let deps = TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &scoped_cfg,
            telemetry_log: &telemetry,
            vendor_health: None,
        };

        run_tick(&deps, 2, 0).unwrap();
        assert_eq!(store.load(target).unwrap().unwrap().state, state);
        assert_eq!(store.load(unrelated).unwrap().unwrap().state, OverlayState::Dispatched);
        assert!(tracker.calls.borrow().is_empty(), "{state:?} scope queried open candidates");
        assert!(sessions.calls.borrow().iter().all(|call| !call.contains(unrelated)));
        assert!(scm.calls.borrow().iter().all(|call| !call.contains(unrelated)));
        assert!(llm.calls.borrow().iter().all(|call| !call.contains(unrelated)));
        assert_eq!(before, logical_rows(&inspect, Some(target)));

        let _ = std::fs::remove_file(&telemetry);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}

#[test]
fn task_scope_dispatching_after_crash_fails_closed_and_redrive_recovers() {
    let target = "crash-target";
    let unrelated_disp = "unrelated-dispatching";
    let unrelated_queued = "unrelated-queued";
    let unrelated_held = "unrelated-held";

    let path = sqlite_path("crash-dispatching");
    let store = SqliteStateStore::open(&path).unwrap();

    store.save(&overlay(target, OverlayState::Dispatching)).unwrap();
    store.save(&overlay(unrelated_disp, OverlayState::Dispatching)).unwrap();
    store.save(&overlay(unrelated_queued, OverlayState::Queued)).unwrap();
    let mut held = overlay(unrelated_held, OverlayState::HumanHeld);
    held.park_reason = Some("operator_scope_hold_20260907_three_prs".into());
    store.save(&held).unwrap();

    let inspect = Connection::open(&path).unwrap();
    let initial_all_rows = logical_rows(&inspect, None);
    let initial_unrelated_rows = logical_rows(&inspect, Some(target));

    let tracker = FakeTracker::new();
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    sessions.set_worktree_remote("https://github.com/owner/production.git");
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"STANDARD_PATH","justification":"scoped test"}"#.into(),
    ));
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), "base-sha".into());

    let checkout = matching_checkout();
    let mut scoped_cfg = cfg(Some(target));
    scoped_cfg
        .repos
        .get_mut("owner/production")
        .unwrap()
        .local_checkout = Some(checkout.clone());
    let telemetry = path.with_extension("jsonl");
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &scoped_cfg,
        telemetry_log: &telemetry,
        vendor_health: None,
    };

    let expected_diag = format!(
        "task_bead_id {target:?} is in orphaned/ambiguous DISPATCHING state after crash; redrive by resetting state to QUEUED or park explicitly"
    );

    // 1. Startup validation boundary fails closed with exact diagnostic
    let startup_err = validate_task_scope(&store, &scoped_cfg, &tracker, target).unwrap_err();
    match startup_err {
        DaemonError::Config(msg) => assert_eq!(msg, expected_diag),
        other => panic!("expected DaemonError::Config, got {other:?}"),
    }
    assert_eq!(
        logical_rows(&inspect, None),
        initial_all_rows,
        "startup boundary failure must leave all rows untouched"
    );

    // 2. run_tick boundary fails closed with exact diagnostic without touching rows or calling adapters
    let tick_err = run_tick(&deps, 0, 0).unwrap_err();
    match tick_err {
        DaemonError::Config(msg) => assert_eq!(msg, expected_diag),
        other => panic!("expected DaemonError::Config, got {other:?}"),
    }
    assert!(scm.calls.borrow().is_empty());
    assert!(tracker.calls.borrow().is_empty());
    assert!(sessions.calls.borrow().is_empty());
    assert_eq!(
        logical_rows(&inspect, None),
        initial_all_rows,
        "tick boundary failure must leave all rows untouched"
    );

    // 3. Exact redrive: reset state to QUEUED and ensure candidate is admitted in tracker
    let mut redriven = store.load(target).unwrap().unwrap();
    redriven.state = OverlayState::Queued;
    store.save(&redriven).unwrap();

    tracker.candidates.borrow_mut().push(Bead {
        id: target.into(),
        title: target.into(),
        description: "target_repo: owner/production".into(),
        ..Bead::default()
    });
    tracker.ready_ids.borrow_mut().replace([target.into()].into_iter().collect());

    // Startup boundary now succeeds
    assert!(validate_task_scope(&store, &scoped_cfg, &tracker, target).is_ok());

    // run_tick now succeeds and dispatches target
    let summary = run_tick(&deps, 0, 0).unwrap();
    assert_eq!(summary.beads_dispatched, 1);
    assert_eq!(store.load(target).unwrap().unwrap().state, OverlayState::Dispatched);

    // Unrelated rows remain completely untouched
    assert_eq!(
        store.load(unrelated_disp).unwrap().unwrap().state,
        OverlayState::Dispatching
    );
    assert_eq!(
        store.load(unrelated_queued).unwrap().unwrap().state,
        OverlayState::Queued
    );
    assert_eq!(
        store.load(unrelated_held).unwrap().unwrap().state,
        OverlayState::HumanHeld
    );
    assert_eq!(
        logical_rows(&inspect, Some(target)),
        initial_unrelated_rows,
        "redrive and dispatch of target must leave all unrelated rows completely untouched"
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_file(&telemetry);
    let _ = std::fs::remove_dir_all(checkout);
}

#[test]
fn task_scope_quota_watchdog_resumes_target_and_preserves_unrelated_due_wake() {
    let target = "target-quota-bead";
    let target_session = "sess-target-quota";
    let unrelated = "unrelated-quota-bead";
    let unrelated_session = "sess-unrelated-quota";

    daemon::health::quota_watchdog::clear(target);
    daemon::health::quota_watchdog::clear(unrelated);

    let path = sqlite_path("quota-watchdog-scope");
    let store = SqliteStateStore::open(&path).unwrap();

    let mut target_overlay = overlay(target, OverlayState::Dispatched);
    target_overlay.branch = Some("factory/target-quota-bead-r1".into());
    target_overlay.session_id = Some(target_session.into());
    target_overlay.session_ao_project = Some("repo".into());
    store.save(&target_overlay).unwrap();

    let mut unrelated_overlay = overlay(unrelated, OverlayState::Dispatched);
    unrelated_overlay.branch = Some("factory/unrelated-quota-bead-r1".into());
    unrelated_overlay.session_id = Some(unrelated_session.into());
    unrelated_overlay.session_ao_project = Some("repo".into());
    store.save(&unrelated_overlay).unwrap();

    // Arm quota watchdog for both target and unrelated beads with a reset time in the past
    daemon::health::quota_watchdog::record_quota_reset(target, target_session, 1);
    daemon::health::quota_watchdog::record_quota_reset(unrelated, unrelated_session, 1);
    assert_eq!(daemon::health::quota_watchdog::recorded_reset_at(target), Some(1));
    assert_eq!(daemon::health::quota_watchdog::recorded_reset_at(unrelated), Some(1));

    let tracker = FakeTracker::new();
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let vcs = FakeVcs::new();
    let mut scoped_cfg = cfg(Some(target));
    scoped_cfg.fast_tick_secs = 1;
    scoped_cfg.slow_tick_secs = 1;

    let telemetry = path.with_extension("jsonl");
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &scoped_cfg,
        telemetry_log: &telemetry,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 0).unwrap();
    assert_eq!(summary.quota_watchdog_wakes, 1, "only target wake should fire");

    let calls = sessions.calls.borrow();
    assert!(
        calls.contains(&format!("wake_pane({target_session})")),
        "wake_pane must be called for target session: {calls:?}"
    );
    assert!(
        !calls.contains(&format!("wake_pane({unrelated_session})")),
        "wake_pane must NOT be called for unrelated session: {calls:?}"
    );

    // Target ledger entry was consumed
    assert_eq!(
        daemon::health::quota_watchdog::recorded_reset_at(target),
        None,
        "target quota watchdog entry must be consumed"
    );
    // Unrelated ledger entry is retained
    assert_eq!(
        daemon::health::quota_watchdog::recorded_reset_at(unrelated),
        Some(1),
        "unrelated quota watchdog entry must be retained"
    );

    daemon::health::quota_watchdog::clear(target);
    daemon::health::quota_watchdog::clear(unrelated);

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let _ = std::fs::remove_file(&telemetry);
}
