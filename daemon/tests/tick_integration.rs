// Task 10 Step 1: Layer-2 integration test. Real call stack — `run_tick`
// wires the actual `intake`, `router`, `dispatch`, `verifier`, `state`, and
// `telemetry` modules together exactly as `main.rs` does; fakes are used ONLY
// at the five tool-boundary traits (`tests/common/mod.rs`), matching the
// evidence-floor discipline the design doc calls out for this task.
//
// Scenario (plan Task 10 Step 1, verbatim): one new `factory`-labeled issue
// from a write-tier collaborator -> intake creates a bead -> router judges
// SMALL_PATH -> dispatch spawns a session and opens a branch -> the fake PR
// "appears" (scripted onto the overlay + `PrSnapshot`) -> verifier reports
// all-green. `run_tick` is driven twice: tick 0 does intake+route+dispatch,
// tick 1 (fast tier always runs; slow tier is due again since our ratio
// forces it) promotes DISPATCHED->ATTESTED and assesses the gates.
//
// Gate 6 note (bead jleechan-3rf hardening): gate 6 is now "`/er` returns
// PASS" (spec §4.2.5 item 6), not just the LOC floor. Stage 1's
// `tick::skeptic_evidence` has no wired `/er` data source yet (only the
// Skeptic's `pass|warn|fail` judge call is wired) so it honestly reports
// `ErVerdict::Absent`, which makes gate 6 `Unknown` rather than a guessed
// `Green`. That means this all-other-gates-green scenario parks
// HUMAN_HELD instead of reaching READY — an honest reflection of the real
// gap (wiring a real `/er` invocation into Stage 1 is tracked separately),
// not a regression in this test's scenario.
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeStateStore, FakeTracker, FakeVcs};
use daemon::config::Config;
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::tick::{run_tick, TickDeps};
use daemon::tools::{Issue, Permission, PrSnapshot};

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
        base_branch: "main".into(),
        stage: 1,
        max_workers: 30,
        max_batch: 15,
        // fast_tick_secs == slow_tick_secs so the slow tier is due on every
        // `run_tick` call in this test (ratio == 1) — both driven ticks
        // exercise intake/route/dispatch AND the fast-tier verifier pass.
        fast_tick_secs: 60,
        slow_tick_secs: 60,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
    }
}

#[test]
fn one_full_tick_cycle_drives_bead_from_intake_to_ready() {
    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 7,
        title: "Add the widget".into(),
        body: "please add a widget".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#7".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    // `create_bead_result` defaults to `Ok("fake-bead-1")` (see
    // `tests/common/mod.rs`), which is also the id `dispatch_ready` will
    // register a branch under: `factory/fake-bead-1-r1`.
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_dir = std::env::temp_dir().join("afd_tick_integration_test");
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    let telemetry_log = telemetry_dir.join(format!(
        "daemon-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    // --- Tick 1: intake -> route SMALL_PATH -> dispatch ---
    let summary1 = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        0,
        0,
    )
    .expect("tick 1 should succeed");
    assert_eq!(summary1.beads_created, 1, "one bead should be created from the new issue");
    assert_eq!(summary1.beads_routed, 1, "the freshly created bead should be routed");
    assert_eq!(summary1.beads_dispatched, 1, "the routed bead should be dispatched");

    let overlay_after_tick1 = store
        .load("fake-bead-1")
        .unwrap()
        .expect("overlay should exist after dispatch");
    assert_eq!(overlay_after_tick1.state, OverlayState::Dispatched);
    assert_eq!(
        overlay_after_tick1.branch.as_deref(),
        Some("factory/fake-bead-1-r1")
    );

    let spawn_calls: Vec<_> = sessions
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("spawn("))
        .cloned()
        .collect();
    assert_eq!(spawn_calls, vec!["spawn(fake-bead-1)".to_string()]);

    // --- The fake PR "appears": scripted directly onto the overlay + PrSnapshot ---
    let mut overlay = overlay_after_tick1;
    overlay.pr_number = Some(101);
    store.save(&overlay).unwrap();
    scm.pr_snapshots.insert(
        101,
        PrSnapshot {
            pr_number: 101,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
        },
    );
    // The router call already happened in tick 1; re-script the same `FakeLlm`
    // for tick 2's Skeptic gate call (`pass|warn|fail` grammar, not JSON).
    *llm.response.borrow_mut() = Some(Ok("pass".into()));

    // --- Tick 2: fast tier promotes DISPATCHED->ATTESTED, then assesses gates ---
    let summary2 = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        1,
        0,
    )
    .expect("tick 2 should succeed");
    assert_eq!(
        summary2.gates_assessed, 1,
        "the ATTESTED bead should be gate-assessed this tick"
    );
    // Gate 6 (evidence review) is `Unknown` in this scenario: Stage 1's
    // `skeptic_evidence` has no wired `/er` source yet, so `er_verdict`
    // honestly defaults to `Absent` rather than a guessed `Pass`. An
    // `Unknown` gate forces `all_green=false` (same as a `Red` gate — see
    // `verifier::GateReport` doc comment), so the bead parks HUMAN_HELD
    // under the Stage-1 substitution rule instead of reaching READY.
    assert_eq!(
        summary2.beads_ready, 0,
        "gate 6 (/er) is Unknown with no wired /er source, so this PR must not reach READY"
    );
    assert_eq!(
        summary2.beads_parked_human_held, 1,
        "an Unknown gate 6 must park the bead HUMAN_HELD, not silently pass it"
    );

    let final_overlay = store
        .load("fake-bead-1")
        .unwrap()
        .expect("overlay should still exist");
    assert_eq!(
        final_overlay.state,
        OverlayState::HumanHeld,
        "final overlay state must be HUMAN_HELD while gate 6 (/er) is Unknown"
    );

    // --- Telemetry: >=5 events in schema order, real JSONL on disk ---
    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.len() >= 5,
        "expected >= 5 telemetry events, got {}: {events:#?}",
        events.len()
    );

    let event_types: Vec<String> = events
        .iter()
        .map(|e| e["eventType"].as_str().unwrap().to_string())
        .collect();
    // Gate 6 (/er) is Unknown in this scenario (no wired /er source — see the
    // module doc comment above), so the Stage-1 substitution rule fires:
    // `REROLL_VERDICT_RECORDED` + `PARKED_HUMAN_HELD` instead of
    // `READY_FOR_MERGE`.
    for required in [
        "INTAKE_BEAD_CREATED",
        "TASK_ROUTED",
        "TASK_DISPATCHED",
        "PR_OPENED",
        "GATE_ASSESSMENT",
        "REROLL_VERDICT_RECORDED",
        "PARKED_HUMAN_HELD",
        "TICK",
    ] {
        assert!(
            event_types.iter().any(|e| e == required),
            "expected a {required} telemetry event, got: {event_types:?}"
        );
    }
    assert!(
        !event_types.iter().any(|e| e == "READY_FOR_MERGE"),
        "gate 6 (/er) is Unknown, so READY_FOR_MERGE must never be emitted: {event_types:?}"
    );

    // Schema order (plan Task 10 Step 1: ">=5 schema-ordered events"): the
    // lifecycle events must appear in causal order, not merely be present. The
    // first-occurrence index of each stage must strictly increase — a bead is
    // created before it is routed, routed before dispatched, its PR opens
    // before the gates are assessed, and it is only parked HUMAN_HELD after
    // assessment. Asserting first-occurrence indices (rather than exact
    // adjacency) keeps the check robust to the interleaved `TICK` summary event
    // while still failing loudly if the pipeline ever emits out of order.
    let first_index = |needle: &str| -> usize {
        event_types
            .iter()
            .position(|e| e == needle)
            .unwrap_or_else(|| panic!("{needle} not found for ordering check: {event_types:?}"))
    };
    let ordered = [
        "INTAKE_BEAD_CREATED",
        "TASK_ROUTED",
        "TASK_DISPATCHED",
        "PR_OPENED",
        "GATE_ASSESSMENT",
        "REROLL_VERDICT_RECORDED",
        "PARKED_HUMAN_HELD",
    ];
    for pair in ordered.windows(2) {
        assert!(
            first_index(pair[0]) < first_index(pair[1]),
            "{} must be emitted before {} (schema order); event stream: {event_types:?}",
            pair[0],
            pair[1],
        );
    }

    // Every event must carry all 7 schema-mandated top-level keys (spec §4.2.9).
    for ev in &events {
        for key in [
            "timestamp",
            "beadId",
            "attemptId",
            "lifecycleState",
            "eventType",
            "metrics",
            "context",
        ] {
            assert!(ev.get(key).is_some(), "event missing {key}: {ev:#?}");
        }
    }

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn run_tick_rejects_non_stage_1_or_2_config() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 3;
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_tick_integration_stage2_unused.jsonl");

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    let err = run_tick(&deps, 0, 0).expect_err("stage != 1 must be rejected, never silently executed");
    match err {
        daemon::errors::DaemonError::Config(msg) => {
            assert!(msg.contains("stage=3"), "error should name the offending stage: {msg}");
        }
        other => panic!("expected DaemonError::Config, got {other:?}"),
    }
}

#[test]
fn run_tick_never_calls_dispatch_when_router_parses_no_verdict() {
    // A bead whose router reply is unparseable prose must never be dispatched
    // — it is parked HUMAN_HELD instead (ZFC: no silent default verdict).
    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 9,
        title: "Ambiguous task".into(),
        body: "not sure what this needs".into(),
        author_login: "bob".into(),
        external_ref: "owner/repo#9".into(),
    });
    scm.permissions.insert("bob".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("I think this looks fine, hard to say".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_tick_integration_parse_fail.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    let summary = run_tick(&deps, 0, 0).expect("tick should succeed even on a routing parse failure");
    assert_eq!(summary.beads_created, 1);
    assert_eq!(summary.beads_routed, 0);
    assert_eq!(summary.beads_dispatched, 0);
    assert_eq!(summary.beads_parked_human_held, 1);

    let spawn_call_count = sessions
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("spawn("))
        .count();
    assert_eq!(spawn_call_count, 0, "an unroutable bead must never be dispatched");

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_autonomy_increment_and_timebox_envelope() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.autonomy_timebox_secs = 3600; // 1 hour for test

    // Seed recent commit time for the branch to bypass wedge detection
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.remote_branches.insert("factory/bead-1-r1".into(), Some(now_epoch));

    // Pre-seed a Dispatched bead with 50 minutes of autonomy
    store.overlays.borrow_mut().insert(
        "bead-1".into(),
        BeadOverlay {
            bead_id: "bead-1".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 3000, // 50 mins
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-1-r1".into()),
            session_id: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_autonomy.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = FakeVcs::new();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    // Run tick with 300 seconds (5 minutes) elapsed
    let _summary = run_tick(&deps, 1, 300).expect("tick should succeed");
    
    // Check autonomy_secs has incremented by 300 to 3300
    let o = store.load("bead-1").unwrap().unwrap();
    assert_eq!(o.autonomy_secs, 3300);
    assert_eq!(o.state, OverlayState::Dispatched); // should still be Dispatched, below timebox

    // Now run tick with another 400 seconds (total 3700, exceeding 3600 timebox)
    let summary2 = run_tick(&deps, 2, 400).expect("tick should succeed");
    let o2 = store.load("bead-1").unwrap().unwrap();
    assert_eq!(o2.autonomy_secs, 3700);
    assert_eq!(o2.state, OverlayState::HumanHeld); // should be parked HumanHeld
    assert_eq!(summary2.beads_parked_human_held, 1);

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_autonomy_budget_warning_crossing() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.autonomy_timebox_secs = 1000; // 80% is 800

    // Seed bead below 80% threshold (e.g. 750 secs)
    store.overlays.borrow_mut().insert(
        "bead-2".into(),
        BeadOverlay {
            bead_id: "bead-2".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 750,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-2-r1".into()),
            session_id: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_warning.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = FakeVcs::new();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    // Run tick with 60 seconds elapsed (new autonomy_secs = 810, crossing 800)
    let _ = run_tick(&deps, 1, 60).unwrap();

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(logs.contains("BUDGET_WARNING"), "log must contain BUDGET_WARNING event: {}", logs);

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_wedge_detection_dispatched_coder_silent() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    // Pre-seed a Dispatched bead with autonomy_secs >= 1800 (e.g. 1900)
    store.overlays.borrow_mut().insert(
        "bead-silent".into(),
        BeadOverlay {
            bead_id: "bead-silent".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 1900,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-silent-r1".into()),
            session_id: None,
        },
    );

    // Script scm remote branch to return None (does not exist)
    scm.remote_branches.insert("factory/bead-silent-r1".into(), None);

    let telemetry_log = std::env::temp_dir().join("afd_test_wedge_silent.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = FakeVcs::new();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    // Run tick
    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(summary.beads_parked_human_held, 1);

    let o = store.load("bead-silent").unwrap().unwrap();
    assert_eq!(o.state, OverlayState::HumanHeld);

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(logs.contains("PARKED_HUMAN_HELD"), "logs: {}", logs);
    assert!(logs.contains("coder_silent"), "logs: {}", logs);

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_wedge_detection_attested_session_stalled() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let mut sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    // Pre-seed Attested bead with session_id
    store.overlays.borrow_mut().insert(
        "bead-stalled".into(),
        BeadOverlay {
            bead_id: "bead-stalled".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 500,
            spend_usd: 0.0,
            pr_number: Some(42),
            branch: Some("factory/bead-stalled-r1".into()),
            session_id: Some("session-abc123yz".into()),
        },
    );

    // Mock PR snapshot with updated_at_epoch older than 30 minutes
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.pr_snapshots.insert(
        42,
        PrSnapshot {
            pr_number: 42,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch - 2000, // older than 1800s
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
        },
    );

    // Script sessions to be quiescent (stalled/dead)
    sessions.quiescent = true;

    let telemetry_log = std::env::temp_dir().join("afd_test_wedge_stalled.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = FakeVcs::new();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    // Run tick
    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(summary.beads_parked_human_held, 1);

    let o = store.load("bead-stalled").unwrap().unwrap();
    assert_eq!(o.state, OverlayState::HumanHeld);

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(logs.contains("session_stalled"), "logs: {}", logs);

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Regression test for bead `jleechan-ubas` ("False-positive PARKED_HUMAN_HELD
/// with new commits present"). When the AO worker session reports
/// `is_quiescent=true`, but the PR's remote head SHA has advanced past the
/// local branch head SHA, the daemon MUST emit
/// `COMMITS_OBSERVED_AFTER_STALL` and stay in `ATTESTED` instead of
/// flipping to `HUMAN_HELD`. Without this guard, the verifier would park
/// beads whose coder sessions were forked/terminated externally (or whose
/// AO state lost sync with the actual PR progress) even though real commits
/// were still landing.
#[test]
fn test_wedge_detection_attested_session_not_stalled_if_remote_ahead() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let mut sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-ubas".into(),
        BeadOverlay {
            bead_id: "bead-ubas".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 500,
            spend_usd: 0.0,
            pr_number: Some(99),
            branch: Some("factory/bead-ubas-r1".into()),
            session_id: Some("session-ubas-1".into()),
        },
    );

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // PR snapshot's remote head_sha DIFFERS from FakeVcs's local head_sha —
    // this is the structural difference from the regression-prevention
    // test below: the bead SHOULD stay ATTESTED, not be parked.
    scm.pr_snapshots.insert(
        99,
        PrSnapshot {
            pr_number: 99,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "remote-head-advanced".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch - 2000,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
        },
    );

    sessions.quiescent = true; // sessions report dead

    let mut vcs = FakeVcs::new();
    // Local branch SHA is older than the remote head SHA, signalling that
    // the daemon's local checkout hasn't been updated but the PR has new
    // commits on remote. `is_remote_ahead` is scripted to return true for
    // this (branch, remote_sha) pair, matching the post-ubas strict check
    // (a strict ancestor predicate, not just SHA inequality).
    vcs.heads
        .insert("factory/bead-ubas-r1".into(), "local-head-stale".into());
    vcs.remote_ahead.insert(
        ("factory/bead-ubas-r1".into(), "remote-head-advanced".into()),
        true,
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_ubas_commits_observed.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 0,
        "remote-ahead bead must NOT be parked (bead jleechan-ubas regression)"
    );

    let o = store.load("bead-ubas").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Attested,
        "remote-ahead bead must stay in ATTESTED so the next fast-tier tick \
         can re-run gate assessment against the live PR state"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        logs.contains("COMMITS_OBSERVED_AFTER_STALL"),
        "telemetry must record the COMMITS_OBSERVED_AFTER_STALL signal: {logs}"
    );
    assert!(
        !logs.contains("\"reason\":\"session_stalled\""),
        "must NOT emit session_stalled when remote is ahead of local: {logs}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Companion regression test: when the local branch head matches the
/// remote head SHA, the original "session stalled" park path MUST still
/// fire (i.e. the ubas guard is a positive guard, not a blanket skip).
#[test]
fn test_wedge_detection_still_parks_when_local_matches_remote() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let mut sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-genuinely-stalled".into(),
        BeadOverlay {
            bead_id: "bead-genuinely-stalled".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 500,
            spend_usd: 0.0,
            pr_number: Some(101),
            branch: Some("factory/bead-genuinely-stalled-r1".into()),
            session_id: Some("session-stuck-1".into()),
        },
    );

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let same_sha = "identical-head-no-progress".to_string();
    scm.pr_snapshots.insert(
        101,
        PrSnapshot {
            pr_number: 101,
            head_sha: same_sha.clone(),
            updated_at_epoch: now_epoch - 2000,
            ci_pending: false,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            body: "".into(),
            comments: vec![],
            files: vec![],
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
        },
    );

    sessions.quiescent = true;

    let mut vcs = FakeVcs::new();
    vcs.heads
        .insert("factory/bead-genuinely-stalled-r1".into(), same_sha);

    let telemetry_log = std::env::temp_dir().join("afd_test_ubas_genuine_stall.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "matching-SHA + quiescent session MUST still park — the ubas guard \
         applies only when remote is ahead, not when both sides agree"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Companion regression for bead `jleechan-ubas` review thread
/// ("Require remote-ahead proof before bypassing the stall"). When the local
/// branch has unpushed commits — i.e. `local_head` is NOT a strict ancestor
/// of the remote head SHA — the daemon MUST still park the bead in
/// `HUMAN_HELD`. The previous "remote != local" inequality check would
/// also fire here and silently mask a real stall, so this test pins the
/// stronger `is_remote_ahead`-based predicate.
#[test]
fn test_wedge_detection_still_parks_when_local_is_ahead_of_remote() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let mut sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-local-ahead".into(),
        BeadOverlay {
            bead_id: "bead-local-ahead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 500,
            spend_usd: 0.0,
            pr_number: Some(202),
            branch: Some("factory/bead-local-ahead-r1".into()),
            session_id: Some("session-local-ahead-1".into()),
        },
    );

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Remote PR head SHA differs from the local branch head, but
    // `is_remote_ahead` is scripted to return false (the daemon's
    // stall-bypass guard runs that predicate, not raw inequality). The
    // previous weaker check would have incorrectly bypassed the park here.
    scm.pr_snapshots.insert(
        202,
        PrSnapshot {
            pr_number: 202,
            head_sha: "remote-head-stale".into(),
            updated_at_epoch: now_epoch - 2000,
            ci_pending: false,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            body: "".into(),
            comments: vec![],
            files: vec![],
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
        },
    );

    sessions.quiescent = true;

    let mut vcs = FakeVcs::new();
    vcs.heads
        .insert("factory/bead-local-ahead-r1".into(), "local-head-ahead".into());
    // remote_ahead stays at its Default (false) — i.e. local is NOT a strict
    // ancestor of the remote head, so the bypass guard must NOT fire.

    let telemetry_log = std::env::temp_dir().join("afd_test_ubas_local_ahead.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "local-ahead bead MUST still park — the ubas guard requires \
         is_remote_ahead=true (strict ancestor predicate), not just SHA inequality"
    );

    let o = store.load("bead-local-ahead").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::HumanHeld,
        "local-ahead bead must end up HUMAN_HELD, not stay ATTESTED behind a bypass"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !logs.contains("COMMITS_OBSERVED_AFTER_STALL"),
        "must NOT emit COMMITS_OBSERVED_AFTER_STALL when local is ahead of remote: {logs}"
    );
    assert!(
        logs.contains("session_stalled"),
        "must emit session_stalled as the park reason: {logs}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Companion regression for bead `jleechan-ubas` review thread: when the
/// local branch and the remote PR head have diverged (both advanced from
/// base, but on different lines) the SHA inequality `remote != local` is
/// true but `is_remote_ahead` is false. The daemon MUST still park the
/// bead in `HUMAN_HELD` — the weaker predicate would silently mask a
/// real stall and let the daemon assess a green-but-diverged PR.
#[test]
fn test_wedge_detection_still_parks_when_branches_have_diverged() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let mut sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-diverged".into(),
        BeadOverlay {
            bead_id: "bead-diverged".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 500,
            spend_usd: 0.0,
            pr_number: Some(203),
            branch: Some("factory/bead-diverged-r1".into()),
            session_id: Some("session-diverged-1".into()),
        },
    );

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.pr_snapshots.insert(
        203,
        PrSnapshot {
            pr_number: 203,
            head_sha: "remote-head-diverged".into(),
            updated_at_epoch: now_epoch - 2000,
            ci_pending: false,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            body: "".into(),
            comments: vec![],
            files: vec![],
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
        },
    );

    sessions.quiescent = true;

    let mut vcs = FakeVcs::new();
    vcs.heads
        .insert("factory/bead-diverged-r1".into(), "local-head-diverged".into());
    // remote_ahead stays at its Default (false) — diverged branches are
    // neither remote-ahead nor remote-behind, so the bypass guard must
    // NOT fire.

    let telemetry_log = std::env::temp_dir().join("afd_test_ubas_diverged.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "diverged bead MUST still park — SHA inequality is too weak a guard"
    );

    let o = store.load("bead-diverged").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::HumanHeld,
        "diverged bead must end up HUMAN_HELD, not stay ATTESTED"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !logs.contains("COMMITS_OBSERVED_AFTER_STALL"),
        "must NOT emit COMMITS_OBSERVED_AFTER_STALL for diverged branches: {logs}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_manual_bead_input_auto_queued_and_dispatched() {
    let scm = FakeScm::new(); // no issues in SCM
    let tracker = FakeTracker::new();
    // Pre-seed a manual bead in the tracker candidates list (like `br list`)
    tracker.candidates.borrow_mut().push(daemon::tools::Bead {
        id: "manual-bead-123".into(),
        title: "Test manual bead".into(),
        description: "manually created".into(),
        file_tree_summary: "".into(),
        external_ref: None, // manual beads have no external_ref
    });

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new(); // empty database initially
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    
    let telemetry_log = std::env::temp_dir().join("afd_manual_bead_test.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
    };

    // Run tick 1: should detect manual bead in tracker, see no overlay,
    // initialize QUEUED overlay, route it, and dispatch it!
    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");
    
    assert_eq!(summary.beads_created, 1, "manual bead should be auto-created/initialized in DB");
    assert_eq!(summary.beads_routed, 1, "manual bead should be routed");
    assert_eq!(summary.beads_dispatched, 1, "manual bead should be dispatched");

    let final_overlay = store.load("manual-bead-123").unwrap().unwrap();
    assert_eq!(final_overlay.state, OverlayState::Dispatched);
    assert_eq!(final_overlay.branch.as_deref(), Some("factory/manual-bead-123-r1"));

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn drive_existing_pr_pending_ci_does_not_reach_ready() {
    // Regression: pending CI buckets must not yield READY (callpath 2026-07-06).
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();

    store.overlays.borrow_mut().insert(
        "drive-bead".into(),
        BeadOverlay {
            bead_id: "drive-bead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(8060),
            branch: Some("fix/rewards-box-not-showing-8020-v2".into()),
            session_id: None,
        },
    );
    store.branches.borrow_mut().push("fix/rewards-box-not-showing-8020-v2".into());
    store.branch_beads.borrow_mut().insert(
        "fix/rewards-box-not-showing-8020-v2".into(),
        "drive-bead".into(),
    );

    scm.pr_snapshots.insert(
        8060,
        PrSnapshot {
            pr_number: 8060,
            ci_success: false, // pending/fail checks on head
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "abc".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "unknown".into(),
            coderabbit_status: "approved".into(),
            ci_pending: true,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_pending_ci_test.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        1,
        0,
    )
    .expect("tick should succeed");

    assert_eq!(summary.gates_assessed, 0);
    assert_eq!(summary.beads_ready, 0, "pending CI must not reach READY");
    assert_eq!(summary.beads_parked_human_held, 0);

    let overlay = store.load("drive-bead").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::Attested);

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn drive_existing_pr_failed_ci_parks_human_held() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();

    store.overlays.borrow_mut().insert(
        "drive-bead".into(),
        BeadOverlay {
            bead_id: "drive-bead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(8060),
            branch: Some("fix/rewards-box-not-showing-8020-v2".into()),
            session_id: None,
        },
    );
    store.branches.borrow_mut().push("fix/rewards-box-not-showing-8020-v2".into());
    store.branch_beads.borrow_mut().insert(
        "fix/rewards-box-not-showing-8020-v2".into(),
        "drive-bead".into(),
    );

    scm.pr_snapshots.insert(
        8060,
        PrSnapshot {
            pr_number: 8060,
            ci_success: false, // failed checks on head
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "abc".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "red".into(),
            coderabbit_status: "approved".into(),
            ci_pending: false,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_failed_ci_test.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        1,
        0,
    )
    .expect("tick should succeed");

    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(summary.beads_ready, 0, "failed CI must not reach READY");
    assert_eq!(summary.beads_parked_human_held, 1);

    let overlay = store.load("drive-bead").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-gib: automated HUMAN_HELD exit — recover_human_held requeues
// beads whose attempt is below MAX_HUMAN_HELD_RECOVERY_ATTEMPT (=10),
// zeros autonomy_secs, and emits a RECOVERED_FROM_HELD telemetry event.
#[test]
fn recover_human_held_requeues_queued_bead_with_attempt_below_max() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    // Pre-seed a HUMAN_HELD bead with attempt=2 (below the 10 cap)
    store.overlays.borrow_mut().insert(
        "bead-held".into(),
        BeadOverlay {
            bead_id: "bead-held".into(),
            state: OverlayState::HumanHeld,
            attempt: 2,
            reroll_count: 0,
            autonomy_secs: 9999,
            spend_usd: 0.0,
            pr_number: Some(4242),
            branch: Some("factory/bead-held-r2".into()),
            session_id: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_recover_human_held_under.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        // tick_index=1 with fast_tick_secs==slow_tick_secs==60 means
        // the slow tier fires (ratio=1, every tick), which is where
        // the recovery step lives.
        1,
        0,
    )
    .expect("tick should succeed");

    let overlay = store.load("bead-held").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Queued,
        "bead must be requeued to QUEUED by automated HUMAN_HELD exit"
    );
    assert_eq!(overlay.attempt, 3, "attempt must increment by 1");
    assert_eq!(overlay.autonomy_secs, 0, "autonomy_secs must reset to 0");
    assert_eq!(
        summary.beads_recovered_from_held, 1,
        "summary counter must reflect the recovery"
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("RECOVERED_FROM_HELD"),
        "RECOVERED_FROM_HELD event must be emitted; got: {log}"
    );
    assert!(
        log.contains("\"prior_state\":\"HUMAN_HELD\""),
        "telemetry metadata must record the prior HUMAN_HELD state; got: {log}"
    );
    assert!(
        log.contains("\"pr_number\":4242"),
        "telemetry metadata must carry the recovered PR number; got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn recover_human_held_does_not_touch_bead_at_or_above_max_attempt() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    // Pre-seed a HUMAN_HELD bead with attempt=10 (the cap)
    store.overlays.borrow_mut().insert(
        "bead-held-cap".into(),
        BeadOverlay {
            bead_id: "bead-held-cap".into(),
            state: OverlayState::HumanHeld,
            attempt: 10,
            reroll_count: 0,
            autonomy_secs: 7,
            spend_usd: 0.0,
            pr_number: Some(9001),
            branch: Some("factory/bead-held-cap-r10".into()),
            session_id: None,
        },
    );
    // Also seed one above the cap (defensive — matches the shell overlay)
    store.overlays.borrow_mut().insert(
        "bead-held-over".into(),
        BeadOverlay {
            bead_id: "bead-held-over".into(),
            state: OverlayState::HumanHeld,
            attempt: 11,
            reroll_count: 0,
            autonomy_secs: 7,
            spend_usd: 0.0,
            pr_number: Some(9002),
            branch: Some("factory/bead-held-over-r11".into()),
            session_id: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_recover_human_held_at_cap.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        1,
        0,
    )
    .expect("tick should succeed");

    assert_eq!(
        summary.beads_recovered_from_held, 0,
        "beads at or above the cap must NOT be recovered"
    );

    let cap = store.load("bead-held-cap").unwrap().unwrap();
    assert_eq!(
        cap.state,
        OverlayState::HumanHeld,
        "at-cap bead must stay HUMAN_HELD for human review"
    );
    let over = store.load("bead-held-over").unwrap().unwrap();
    assert_eq!(over.state, OverlayState::HumanHeld);

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !log.contains("RECOVERED_FROM_HELD"),
        "no RECOVERED_FROM_HELD events must be emitted when both beads are at/above the cap; got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-gib: stop the autonomy clock during ci_pending. ATTESTED beads
// whose PR has ci_pending=true must NOT have autonomy_secs incremented,
// because CI wait time is wall-clock time the operator (or CI itself) owns,
// not coder session time we are budgeting against.
#[test]
fn attested_ci_pending_does_not_bump_autonomy_secs() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.autonomy_timebox_secs = 3600; // 1h timebox so the bead survives the tick
    let vcs = FakeVcs::new();

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.remote_branches
        .insert("factory/att-bead-r1".into(), Some(now_epoch));

    // Pre-seed an ATTESTED bead with autonomy_secs=500 and a PR whose ci_pending=true
    store.overlays.borrow_mut().insert(
        "att-bead".into(),
        BeadOverlay {
            bead_id: "att-bead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 500,
            spend_usd: 0.0,
            pr_number: Some(7000),
            branch: Some("factory/att-bead-r1".into()),
            session_id: None,
        },
    );
    scm.pr_snapshots.insert(
        7000,
        PrSnapshot {
            pr_number: 7000,
            ci_success: false,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "abc".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch,
            ci_status: "unknown".into(),
            coderabbit_status: "approved".into(),
            ci_pending: true,
        },
    );
    store.branches.borrow_mut().push("factory/att-bead-r1".into());
    store
        .branch_beads
        .borrow_mut()
        .insert("factory/att-bead-r1".into(), "att-bead".into());

    let telemetry_log = std::env::temp_dir().join("afd_attested_ci_pending.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let _summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        1,
        // Pretend 600s elapsed since the last tick; ci_pending must freeze the clock.
        600,
    )
    .expect("tick should succeed");

    let overlay = store.load("att-bead").unwrap().unwrap();
    assert_eq!(
        overlay.autonomy_secs, 500,
        "ci_pending=true must freeze the autonomy clock (no bump)"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-54ky (sub-fix for jleechan-gib): an ATTESTED bead whose CI has
// been pending for hours must NOT timebox-park to HUMAN_HELD. The 3h
// timebox is supposed to bound coder session cost, not CI queue latency;
// pausing the clock while ci_pending=true means a healthy PR that's
// waiting on slow CI keeps its attempt slot instead of being silently
// killed and then requiring shell recover-held churn.
#[test]
fn attested_ci_pending_does_not_timebox_park() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.autonomy_timebox_secs = 600; // 10 min — would park in one tick if clock runs
    let vcs = FakeVcs::new();

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.remote_branches
        .insert("factory/slow-ci-r1".into(), Some(now_epoch));

    store.overlays.borrow_mut().insert(
        "slow-ci".into(),
        BeadOverlay {
            bead_id: "slow-ci".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            // autonomy_secs is already 590s; one 600s tick would push it past 600.
            autonomy_secs: 590,
            spend_usd: 0.0,
            pr_number: Some(7100),
            branch: Some("factory/slow-ci-r1".into()),
            session_id: None,
        },
    );
    scm.pr_snapshots.insert(
        7100,
        PrSnapshot {
            pr_number: 7100,
            ci_success: false,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "ghi".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch,
            ci_status: "unknown".into(),
            coderabbit_status: "approved".into(),
            ci_pending: true,
        },
    );
    store.branches.borrow_mut().push("factory/slow-ci-r1".into());
    store
        .branch_beads
        .borrow_mut()
        .insert("factory/slow-ci-r1".into(), "slow-ci".into());

    let telemetry_log = std::env::temp_dir().join("afd_ci_pending_no_timebox.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        1,
        600, // 10-minute tick; ci_pending=true means clock should NOT advance
    )
    .expect("tick should succeed");

    assert_eq!(
        summary.beads_parked_human_held, 0,
        "ci_pending=true must NOT park the bead on the timebox envelope"
    );
    let overlay = store.load("slow-ci").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Attested,
        "ATTESTED + ci_pending=true must stay ATTESTED while CI runs"
    );
    assert_eq!(
        overlay.autonomy_secs, 590,
        "ci_pending=true must freeze the autonomy clock at its current value"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-54ky / jleechan-gib: a non-green recoverable PR must re-enter
// the dispatch loop via the automated HUMAN_HELD exit (the slow-tier
// recovery step), without requiring shell `recover-held` churn. We model
// the loop end-to-end: a pre-seeded HUMAN_HELD bead (from a prior
// non-green gates assessment, attempt=1) is requeued by the slow tier
// and its `attempt` advances to 2, which is exactly what shell
// `recover-held` would have done if anyone ran it. The slow tier now
// does it for them.
#[test]
fn non_green_bead_reenters_loop_via_automated_human_held_exit() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    // Prior tick parked this bead HUMAN_HELD after a non-green gates
    // assessment (e.g. CodeRabbit CHANGES_REQUESTED). attempt=1 means
    // there's plenty of headroom under the 10-cap.
    store.overlays.borrow_mut().insert(
        "non-green".into(),
        BeadOverlay {
            bead_id: "non-green".into(),
            state: OverlayState::HumanHeld,
            attempt: 1,
            reroll_count: 0,
            // leftover autonomy_secs from the prior session — must be
            // reset on recovery, matching shell `recover-held`.
            autonomy_secs: 4321,
            spend_usd: 0.0,
            pr_number: Some(5050),
            branch: Some("factory/non-green-r1".into()),
            session_id: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_non_green_reentry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        // slow-tier-due (ratio=1, every tick fires both tiers)
        1,
        0,
    )
    .expect("tick should succeed");

    // Recovery happened in slow tier BEFORE the slow tier's
    // intake/route/dispatch pass even looked at the bead, so we expect:
    //   - state -> QUEUED
    //   - attempt -> 2
    //   - autonomy_secs -> 0
    //   - RECOVERED_FROM_HELD telemetry event with prior_state + pr_number
    let overlay = store.load("non-green").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Queued,
        "automated HUMAN_HELD exit must requeue the bead"
    );
    assert_eq!(
        overlay.attempt, 2,
        "attempt must advance; this is the attempt counter, not a NEW bead"
    );
    assert_eq!(
        overlay.autonomy_secs, 0,
        "autonomy_secs must reset (matches shell `recover-held`); the next dispatch starts fresh"
    );
    assert_eq!(overlay.pr_number, Some(5050));
    assert_eq!(
        summary.beads_recovered_from_held, 1,
        "summary must reflect the recovery (no shell `recover-held` was invoked)"
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("RECOVERED_FROM_HELD"),
        "RECOVERED_FROM_HELD event must be emitted by the Rust tick (no shell caller); got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn attested_ci_not_pending_does_bump_autonomy_secs() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.autonomy_timebox_secs = 3600;
    let vcs = FakeVcs::new();

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.remote_branches
        .insert("factory/att-active-r1".into(), Some(now_epoch));

    store.overlays.borrow_mut().insert(
        "att-active".into(),
        BeadOverlay {
            bead_id: "att-active".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: Some(7001),
            branch: Some("factory/att-active-r1".into()),
            session_id: None,
        },
    );
    scm.pr_snapshots.insert(
        7001,
        PrSnapshot {
            pr_number: 7001,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "def".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch,
            ci_status: "success".into(),
            coderabbit_status: "approved".into(),
            ci_pending: false,
        },
    );
    store.branches.borrow_mut().push("factory/att-active-r1".into());
    store
        .branch_beads
        .borrow_mut()
        .insert("factory/att-active-r1".into(), "att-active".into());

    let telemetry_log = std::env::temp_dir().join("afd_attested_ci_not_pending.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let _summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        1,
        300,
    )
    .expect("tick should succeed");

    let overlay = store.load("att-active").unwrap().unwrap();
    assert_eq!(
        overlay.autonomy_secs, 400,
        "ci_pending=false must allow the autonomy clock to advance"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Bead `jleechan-qdw` regression test: a single bead's per-tick
/// `gh` failure must NOT abort the whole tick. Two ATTESTED beads are
/// loaded; bead A's PR has no scripted `pr_snapshot` (so `gh` returns
/// the FakeScm "no scripted snapshot" Tool error), bead B's PR has a
/// scripted all-green snapshot. The previous (pre-qdw) code had `?`
/// propagation in three places (the wedge-detection `pr_snapshot`,
/// the wedge-detection `remote_branch_last_commit`, and the per-bead
/// `pr_snapshot` inside the fast tier) — any one of which would
/// `?`-propagate a single transient gh failure to `main.rs:277` and
/// kill the daemon. The qdw fix softens each into a `match`/`if let
/// Some(...)` so the loop continues. This test exercises the
/// ATTESTED-pr_snapshot path (the most common one) and asserts the
/// tick completes, bead-B reaches READY, and a `BEAD_TICK_ERROR` /
/// `WEDGE_SNAPSHOT_ERROR` telemetry event was emitted for bead-A.
#[test]
fn test_per_bead_isolation_lets_tick_complete_after_one_failure() {
    use daemon::state::BeadOverlay;
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_dir = std::env::temp_dir().join("afd_tick_qdw_isolation_test");
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    let telemetry_log = telemetry_dir.join(format!(
        "daemon-{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);

    // Seed two ATTESTED beads with PR numbers. Bead A's PR has NO
    // scripted snapshot (FakeScm::pr_snapshot returns Tool(rc=1,
    // "no scripted snapshot ...")), bead B's PR has an all-green
    // snapshot.
    store
        .save(&BeadOverlay {
            bead_id: "bead-A".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(9001),
            branch: Some("factory/bead-A-r1".into()),
            session_id: None,
        })
        .unwrap();
    store
        .save(&BeadOverlay {
            bead_id: "bead-B".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(9002),
            branch: Some("factory/bead-B-r1".into()),
            session_id: None,
        })
        .unwrap();
    // Register the branches in the store so `run_fast_tier` discovers
    // them via `owned_branches`. The first argument is the bead id
    // (kept in `branch_beads` for `bead_id_for_branch` lookup); the
    // second is the full branch name. Both beads are ATTESTED, so
    // `increment_active_autonomy` also pulls them as overlays — but
    // ATTESTED is in the active set, so the wedge check on bead-A's
    // missing pr_snapshot now hits the new `match` block in tick.rs
    // instead of the old `?`.
    store
        .register_branch("bead-A", "factory/bead-A-r1")
        .unwrap();
    store
        .register_branch("bead-B", "factory/bead-B-r1")
        .unwrap();

    // Script bead-B's snapshot (all-green, no comments, no files —
    // passes the gates that don't require a real PR diff). The
    // `updated_at_epoch` MUST be set to the current epoch so the
    // wedge detection in `increment_active_autonomy`'s ATTESTED branch
    // (which checks `now_epoch.saturating_sub(updated_at_epoch) >=
    // 1800`) does NOT trip and park bead-B before the fast-tier
    // assessment ever runs. This is realistic — a freshly-attested PR
    // has a current `updated_at` from gh.
    scm.pr_snapshots.insert(
        9002,
        PrSnapshot {
            pr_number: 9002,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "deadbeef".into(),
            body: String::new(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
        },
    );
    // The skeptic gate (gate 7) is the only LLM-dependent gate that
    // fires from the fast tier. Script a `pass` verdict so the
    // Stage-1 fast tier considers bead-B's gates all-green; the test
    // is about isolation, not gate logic.
    *llm.response.borrow_mut() = Some(Ok("pass ready to merge".into()));

    let summary = run_tick(
        &TickDeps {
            scm: &scm,
            tracker: &tracker,
            sessions: &sessions,
            llm: &llm,
            store: &store,
            vcs: &vcs,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
        },
        0,
        0,
    )
    .expect("tick must NOT fail even when one bead's gh call fails");

    // Bead A's pr_snapshot was never scripted -> Tool(rc=1) -> the
    // per-bead path returns Err, the loop continues. Bead B's
    // all-green snapshot drives one gate assessment. We assert ">= 1"
    // rather than "== 1" because the order in which the two beads are
    // processed depends on HashMap iteration of the branch registry —
    // both orderings prove the structural fix (the loop completes
    // despite one bead's failure).
    //
    // Note: bead-B will park HUMAN_HELD, not reach READY, because
    // gate 6 (/er verdict) is `Unknown` in Stage 1 (no wired `/er`
    // source yet, per the same reasoning as
    // `one_full_tick_cycle_drives_bead_from_intake_to_ready` above).
    // An `Unknown` gate forces `all_green=false`, parking the bead
    // under the Stage-1 substitution rule. The point of this test is
    // that the LOOP COMPLETES and bead-B is reached despite bead-A's
    // failure — not that bead-B reaches READY.
    assert!(
        summary.gates_assessed >= 1,
        "bead-B should be gate-assessed despite bead-A's failure; got gates_assessed={}",
        summary.gates_assessed
    );
    // Bead B should have been processed (the structural proof is that
    // its state moved past ATTESTED — into HUMAN_HELD via the
    // Stage-1 substitution rule, not stuck at ATTESTED). Bead A
    // stays ATTESTED — its pr_snapshot failed, no state mutation.
    let overlay_b = store.load("bead-B").unwrap().expect("bead-B overlay");
    assert_ne!(
        overlay_b.state,
        OverlayState::Attested,
        "bead-B must be processed (HUMAN_HELD, not stuck at ATTESTED); got {:?}",
        overlay_b.state
    );
    assert_eq!(
        overlay_b.state,
        OverlayState::HumanHeld,
        "bead-B should reach HUMAN_HELD via Stage-1 substitution (gate 6 /er is Unknown); got {:?}",
        overlay_b.state
    );

    // A BEAD_TICK_ERROR telemetry event must have been emitted for
    // bead-A. The pre-qdw code would have crashed before emitting
    // anything, so the existence of this event is the structural
    // proof that the per-bead isolation is wired.
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("BEAD_TICK_ERROR"),
        "BEAD_TICK_ERROR must be emitted for bead-A; full log:\n{telemetry}"
    );
    assert!(
        telemetry.contains("bead-A"),
        "bead-A must appear in the BEAD_TICK_ERROR event; full log:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}
