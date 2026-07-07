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
// Gate 6 note (beads jleechan-3rf + jleechan-qdw hardening): gate 6 is now
// "`/er` returns PASS" (spec §4.2.5 item 6), not just the LOC floor. Stage 1's
// `tick::skeptic_evidence` has no wired `/er` data source yet (only the
// Skeptic's `pass|warn|fail` judge call is wired) so it honestly reports
// `ErVerdict::Absent`, which makes gate 6 `Unknown` rather than a guessed
// `Green`. Unknown-only reports are verifier-incomplete, not defect verdicts:
// the bead stays ATTESTED for retry and must not reach READY, park HUMAN_HELD,
// or enter the re-roll lane without an actual Red gate.
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeStateStore, FakeTracker, FakeVcs};
use daemon::config::Config;
use daemon::er_runner;
use daemon::errors::DaemonError;
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::tick::{combine_dual_verdict, run_tick, TickDeps};
use daemon::tools::{Bead, Issue, Llm, Permission, PrComment, PrSnapshot, Scm};
use daemon::verifier::SkepticVerdict;

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
        ao_project: None,
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
fn one_full_tick_cycle_keeps_unknown_only_gate_attested() {
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
    let telemetry_log = telemetry_dir.join(format!("daemon-{}.jsonl", std::process::id()));
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
    assert_eq!(
        summary1.beads_created, 1,
        "one bead should be created from the new issue"
    );
    assert_eq!(
        summary1.beads_routed, 1,
        "the freshly created bead should be routed"
    );
    assert_eq!(
        summary1.beads_dispatched, 1,
        "the routed bead should be dispatched"
    );

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
    // honestly defaults to `Absent` rather than a guessed `Pass`. An Unknown
    // gate still prevents READY, but without any Red gate it must remain a
    // transient/incomplete verifier state, not a Stage-1 HUMAN_HELD park.
    assert_eq!(
        summary2.beads_ready, 0,
        "gate 6 (/er) is Unknown with no wired /er source, so this PR must not reach READY"
    );
    assert_eq!(
        summary2.beads_parked_human_held, 0,
        "an Unknown-only gate report must not park the bead HUMAN_HELD"
    );

    let final_overlay = store
        .load("fake-bead-1")
        .unwrap()
        .expect("overlay should still exist");
    assert_eq!(
        final_overlay.state,
        OverlayState::Attested,
        "final overlay state must stay ATTESTED while gate 6 (/er) is Unknown-only"
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
    // module doc comment above), so the tick records a transient Unknown-only
    // assessment instead of `READY_FOR_MERGE`, `REROLL_VERDICT_RECORDED`, or
    // `PARKED_HUMAN_HELD`.
    for required in [
        "INTAKE_BEAD_CREATED",
        "TASK_ROUTED",
        "TASK_DISPATCHED",
        "PR_OPENED",
        "GATE_ASSESSMENT",
        "GATE_ASSESSMENT_TRANSIENT_UNKNOWN",
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
    assert!(
        !event_types.iter().any(|e| e == "REROLL_VERDICT_RECORDED"),
        "Unknown-only gate reports must not enter the re-roll lane: {event_types:?}"
    );
    assert!(
        !event_types.iter().any(|e| e == "PARKED_HUMAN_HELD"),
        "Unknown-only gate reports must not park HUMAN_HELD: {event_types:?}"
    );

    // Schema order (plan Task 10 Step 1: ">=5 schema-ordered events"): the
    // lifecycle events must appear in causal order, not merely be present. The
    // first-occurrence index of each stage must strictly increase — a bead is
    // created before it is routed, routed before dispatched, its PR opens
    // before the gates are assessed, and the transient Unknown event is emitted
    // only after assessment. Asserting first-occurrence indices (rather than exact
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
        "GATE_ASSESSMENT_TRANSIENT_UNKNOWN",
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
    let scm = FakeScm::new();
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

    let err =
        run_tick(&deps, 0, 0).expect_err("stage != 1 must be rejected, never silently executed");
    match err {
        daemon::errors::DaemonError::Config(msg) => {
            assert!(
                msg.contains("stage=3"),
                "error should name the offending stage: {msg}"
            );
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

    let summary =
        run_tick(&deps, 0, 0).expect("tick should succeed even on a routing parse failure");
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
    assert_eq!(
        spawn_call_count, 0,
        "an unroutable bead must never be dispatched"
    );

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn run_tick_emits_dispatched_only_for_actual_dispatch_successes() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().extend([
        Bead {
            id: "bead-0".into(),
            title: "first bead".into(),
            description: String::new(),
            file_tree_summary: String::new(),
            external_ref: None,
        },
        Bead {
            id: "bead-1".into(),
            title: "second bead".into(),
            description: String::new(),
            file_tree_summary: String::new(),
            external_ref: None,
        },
    ]);
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"STANDARD_PATH","justification":"scripted"}"#.into(),
    ));
    let store = FakeStateStore::new();
    for bead_id in ["bead-0", "bead-1"] {
        store
            .save(&BeadOverlay {
                bead_id: bead_id.into(),
                state: OverlayState::Queued,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: None,
                session_id: None,
            })
            .unwrap();
    }
    store.fail_save_for("bead-0", OverlayState::Dispatching);
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_tick_dispatch_isolation.jsonl");
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
        0,
        0,
    )
    .expect("tick should isolate one dispatch failure and continue");

    assert_eq!(summary.beads_dispatched, 1);
    assert_eq!(
        store.load("bead-0").unwrap().unwrap().state,
        OverlayState::Queued
    );
    assert_eq!(
        store.load("bead-1").unwrap().unwrap().state,
        OverlayState::Dispatched
    );

    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let dispatched_beads: Vec<_> = events
        .iter()
        .filter(|e| e["eventType"] == "TASK_DISPATCHED")
        .map(|e| e["beadId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        dispatched_beads,
        vec!["bead-1".to_string()],
        "TASK_DISPATCHED must describe actual successes, not ready-list prefix count"
    );

    let failure = events
        .iter()
        .find(|e| e["eventType"] == "BEAD_DISPATCH_TRANSIENT_ERROR")
        .expect("dispatch failure telemetry should be emitted");
    assert_eq!(failure["beadId"], "bead-0");
    assert_eq!(failure["context"]["phase"], "save_dispatching");

    let spawn_calls: Vec<_> = sessions
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("spawn("))
        .cloned()
        .collect();
    assert_eq!(
        spawn_calls,
        vec!["spawn(bead-1)".to_string()],
        "pre-spawn failure for bead-0 must not call spawn, but bead-1 should still dispatch"
    );

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
    scm.remote_branches
        .insert("factory/bead-1-r1".into(), Some(now_epoch));

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
    let scm = FakeScm::new();
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
    assert!(
        logs.contains("BUDGET_WARNING"),
        "log must contain BUDGET_WARNING event: {}",
        logs
    );

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
    scm.remote_branches
        .insert("factory/bead-silent-r1".into(), None);

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
    vcs.heads.insert(
        "factory/bead-local-ahead-r1".into(),
        "local-head-ahead".into(),
    );
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
    vcs.heads.insert(
        "factory/bead-diverged-r1".into(),
        "local-head-diverged".into(),
    );
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

    assert_eq!(
        summary.beads_created, 1,
        "manual bead should be auto-created/initialized in DB"
    );
    assert_eq!(summary.beads_routed, 1, "manual bead should be routed");
    assert_eq!(
        summary.beads_dispatched, 1,
        "manual bead should be dispatched"
    );

    let final_overlay = store.load("manual-bead-123").unwrap().unwrap();
    assert_eq!(final_overlay.state, OverlayState::Dispatched);
    assert_eq!(
        final_overlay.branch.as_deref(),
        Some("factory/manual-bead-123-r1")
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn newly_intaken_bead_dispatch_uses_real_tracker_title() {
    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 8123,
        title: "Wire a durable Linux trigger".into(),
        body: "systemd user unit acceptance criteria".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#8123".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_new_intake_prompt_{}.jsonl",
        std::process::id()
    ));
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
        0,
        0,
    )
    .expect("newly-intaken bead should route and dispatch");

    assert_eq!(summary.beads_created, 1);
    assert_eq!(summary.beads_dispatched, 1);
    let prompts = sessions.spawn_prompts.borrow();
    assert_eq!(
        prompts.as_slice(),
        &[(
            "fake-bead-1".to_string(),
            "Wire a durable Linux trigger (owner/repo)".to_string()
        )],
        "new intake must dispatch the real tracker title, not an empty stub prompt"
    );

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
    store
        .branches
        .borrow_mut()
        .push("fix/rewards-box-not-showing-8020-v2".into());
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
    store
        .branches
        .borrow_mut()
        .push("fix/rewards-box-not-showing-8020-v2".into());
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
    let scm = FakeScm::new();
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
    let scm = FakeScm::new();
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
    assert_eq!(
        summary.beads_escalated, 2,
        "beads at or above the cap must be explicitly escalated"
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
    assert_eq!(
        log.matches("ESCALATION_REQUIRED").count(),
        2,
        "both capped HUMAN_HELD beads must emit escalation telemetry; got: {log}"
    );
    assert!(
        log.contains("human_held_recovery_attempt_cap_reached"),
        "escalation telemetry must name the recovery cap; got: {log}"
    );
    let first_comment_count = {
        let tracker_calls = tracker.calls.borrow();
        assert!(
            tracker_calls.iter().any(|call| {
                call.contains("comment_external(owner/repo#9001,")
                    && call.contains("Escalation required")
            }),
            "at-cap PR must receive an escalation comment; calls: {tracker_calls:?}"
        );
        tracker_calls
            .iter()
            .filter(|call| call.contains("Escalation required"))
            .count()
    };
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
        2,
        0,
    )
    .expect("second tick should not repeat capped HUMAN_HELD escalation");
    assert_eq!(
        summary2.beads_escalated, 0,
        "already-escalated HUMAN_HELD beads must not emit again"
    );
    let second_comment_count = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.contains("Escalation required"))
        .count();
    assert_eq!(
        second_comment_count, first_comment_count,
        "second tick must not post duplicate escalation comments"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn capped_human_held_comment_failure_retries_before_recording_escalation() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-held-retry".into(),
        BeadOverlay {
            bead_id: "bead-held-retry".into(),
            state: OverlayState::HumanHeld,
            attempt: 10,
            reroll_count: 0,
            autonomy_secs: 7,
            spend_usd: 0.0,
            pr_number: Some(9003),
            branch: Some("factory/bead-held-retry-r10".into()),
            session_id: None,
        },
    );
    *tracker.fail_next_comment.borrow_mut() = Some("transient comment failure".into());

    let telemetry_log = std::env::temp_dir().join("afd_recover_human_held_retry_comment.jsonl");
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

    let summary = run_tick(&deps, 1, 0).expect("comment failure should not abort tick");
    assert_eq!(
        summary.beads_escalated, 0,
        "failed notification must not record a completed escalation"
    );
    assert!(
        store
            .load_rejection("bead-held-retry", u32::MAX)
            .unwrap()
            .is_none(),
        "sentinel must stay absent so the next tick retries notification"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("ESCALATION_NOTIFICATION_FAILED"),
        "notification failure must be visible in telemetry; got: {log}"
    );

    let summary2 = run_tick(&deps, 2, 0).expect("second tick should retry escalation");
    assert_eq!(summary2.beads_escalated, 1);
    assert!(
        store
            .load_rejection("bead-held-retry", u32::MAX)
            .unwrap()
            .is_some(),
        "sentinel must be recorded after successful retry"
    );
    let comment_count = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.contains("comment_external(owner/repo#9003,"))
        .count();
    assert_eq!(comment_count, 2, "second tick must retry the failed comment");

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn capped_human_held_candidate_lookup_failure_retries_before_recording_escalation() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-held-fallback".into(),
        BeadOverlay {
            bead_id: "bead-held-fallback".into(),
            state: OverlayState::HumanHeld,
            attempt: 10,
            reroll_count: 0,
            autonomy_secs: 7,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-held-fallback-r10".into()),
            session_id: None,
        },
    );
    tracker.candidates.borrow_mut().push(Bead {
        id: "bead-held-fallback".into(),
        title: "held fallback".into(),
        description: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#9004".into()),
    });
    *tracker.fail_next_fetch_candidates.borrow_mut() =
        Some("transient candidate lookup failure".into());

    let telemetry_log = std::env::temp_dir().join("afd_recover_human_held_retry_lookup.jsonl");
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

    let summary = run_tick(&deps, 1, 0).expect("lookup failure should not abort tick");
    assert_eq!(summary.beads_escalated, 0);
    assert!(
        store
            .load_rejection("bead-held-fallback", u32::MAX)
            .unwrap()
            .is_none(),
        "sentinel must stay absent when the fallback target lookup fails"
    );

    let summary2 = run_tick(&deps, 2, 0).expect("second tick should retry lookup and comment");
    assert_eq!(summary2.beads_escalated, 1);
    assert!(
        store
            .load_rejection("bead-held-fallback", u32::MAX)
            .unwrap()
            .is_some(),
        "sentinel must be recorded only after fallback comment succeeds"
    );
    let calls = tracker.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| call.contains("comment_external(owner/repo#9004,")),
        "retry must post to the fallback external_ref; calls: {calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn capped_human_held_missing_comment_target_does_not_record_escalation() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-held-missing-target".into(),
        BeadOverlay {
            bead_id: "bead-held-missing-target".into(),
            state: OverlayState::HumanHeld,
            attempt: 10,
            reroll_count: 0,
            autonomy_secs: 7,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-held-missing-target-r10".into()),
            session_id: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_recover_human_held_missing_target.jsonl");
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

    let summary = run_tick(&deps, 1, 0).expect("missing target should not abort tick");
    assert_eq!(summary.beads_escalated, 0);
    assert!(
        store
            .load_rejection("bead-held-missing-target", u32::MAX)
            .unwrap()
            .is_none(),
        "sentinel must stay absent until an operator-facing target exists"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("ESCALATION_NOTIFICATION_FAILED"),
        "missing notification target must be visible in telemetry; got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn er_runner_capped_unknown_only_gate_report_escalates_and_parks_at_recovery_cap() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass skeptic green".into()));
    let store = QdwAttemptStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    store.inner.overlays.borrow_mut().insert(
        "er-capped-unknown".into(),
        BeadOverlay {
            bead_id: "er-capped-unknown".into(),
            state: OverlayState::Attested,
            attempt: 4,
            reroll_count: 0,
            autonomy_secs: 120,
            spend_usd: 0.0,
            pr_number: Some(9101),
            branch: Some("factory/er-capped-unknown-r4".into()),
            session_id: Some("session-er-capped".into()),
        },
    );
    store
        .inner
        .branches
        .borrow_mut()
        .push("factory/er-capped-unknown-r4".into());
    store.inner.branch_beads.borrow_mut().insert(
        "factory/er-capped-unknown-r4".into(),
        "er-capped-unknown".into(),
    );
    store
        .er_attempts
        .borrow_mut()
        .insert("er-capped-unknown".into(), (er_runner::MAX_ER_RUNNER_ATTEMPTS, Some(1)));

    scm.pr_snapshots.insert(
        9101,
        PrSnapshot {
            pr_number: 9101,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "head9101".into(),
            body: String::new(),
            comments: Vec::new(),
            files: Vec::new(),
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_er_capped_unknown_escalates.jsonl");
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
        0,
        0,
    )
    .expect("unknown-only capped /er report should escalate without aborting tick");

    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(summary.beads_escalated, 1);
    assert_eq!(summary.beads_parked_human_held, 1);
    let overlay = store.load("er-capped-unknown").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::HumanHeld,
        "capped Unknown-only gate report is parked for inspection"
    );
    assert_eq!(
        overlay.attempt, 10,
        "capped /er escalation must park at the HUMAN_HELD recovery cap so recovery cannot churn it"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("ER_RUNNER_CAPPED"),
        "runner cap must be recorded; telemetry was:\n{telemetry}"
    );
    assert!(
        telemetry.contains("ESCALATION_REQUIRED")
            && telemetry.contains("unknown_only_gate_report_with_er_runner_capped"),
        "Unknown-only capped gate report must emit explicit escalation; telemetry was:\n{telemetry}"
    );
    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().any(|call| {
            call.contains("comment_external(owner/repo#9101,")
                && call.contains("Escalation required")
        }),
        "PR must receive an escalation comment; calls: {tracker_calls:?}"
    );
    let first_comment_count = tracker_calls
        .iter()
        .filter(|call| call.contains("Escalation required"))
        .count();
    drop(tracker_calls);

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
    .expect("second tick should not recover or repeat capped /er escalation");
    assert_eq!(summary2.beads_recovered_from_held, 0);
    assert_eq!(
        summary2.beads_escalated, 0,
        "already-escalated capped /er bead must not emit again through HUMAN_HELD recovery"
    );
    let second_comment_count = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.contains("Escalation required"))
        .count();
    assert_eq!(
        second_comment_count, first_comment_count,
        "second tick must not post duplicate capped /er escalation comments"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn er_runner_capped_unknown_only_comment_failure_retries_before_parking() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass skeptic green".into()));
    let store = QdwAttemptStore::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    store.inner.overlays.borrow_mut().insert(
        "er-capped-retry".into(),
        BeadOverlay {
            bead_id: "er-capped-retry".into(),
            state: OverlayState::Attested,
            attempt: 4,
            reroll_count: 0,
            autonomy_secs: 120,
            spend_usd: 0.0,
            pr_number: Some(9102),
            branch: Some("factory/er-capped-retry-r4".into()),
            session_id: Some("session-er-capped-retry".into()),
        },
    );
    store
        .inner
        .branches
        .borrow_mut()
        .push("factory/er-capped-retry-r4".into());
    store.inner.branch_beads.borrow_mut().insert(
        "factory/er-capped-retry-r4".into(),
        "er-capped-retry".into(),
    );
    store
        .er_attempts
        .borrow_mut()
        .insert("er-capped-retry".into(), (er_runner::MAX_ER_RUNNER_ATTEMPTS, Some(1)));
    *tracker.fail_next_comment.borrow_mut() = Some("transient comment failure".into());

    scm.pr_snapshots.insert(9102, qdw_green_snapshot(9102, Vec::new()));

    let telemetry_log = std::env::temp_dir().join("afd_er_capped_unknown_retry_comment.jsonl");
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

    let summary = run_tick(&deps, 0, 0).expect("comment failure should not abort tick");
    assert_eq!(summary.beads_escalated, 0);
    assert_eq!(summary.beads_parked_human_held, 0);
    let overlay = store.load("er-capped-retry").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::Attested);
    assert_eq!(overlay.attempt, 4);
    assert!(
        store
            .load_rejection("er-capped-retry", u32::MAX)
            .unwrap()
            .is_none(),
        "sentinel must stay absent after failed notification"
    );

    let summary2 = run_tick(&deps, 1, 0).expect("second tick should retry and park");
    assert_eq!(summary2.beads_escalated, 1);
    assert_eq!(summary2.beads_parked_human_held, 1);
    let overlay = store.load("er-capped-retry").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(overlay.attempt, 10);
    assert!(
        store
            .load_rejection("er-capped-retry", u32::MAX)
            .unwrap()
            .is_some(),
        "sentinel must be recorded after successful retry"
    );
    let comment_count = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.contains("comment_external(owner/repo#9102,"))
        .count();
    assert_eq!(comment_count, 2, "second tick must retry the failed comment");

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
    store
        .branches
        .borrow_mut()
        .push("factory/att-bead-r1".into());
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
    store
        .branches
        .borrow_mut()
        .push("factory/slow-ci-r1".into());
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
    let scm = FakeScm::new();
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
    store
        .branches
        .borrow_mut()
        .push("factory/att-active-r1".into());
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

// jleechan-qdw: per-bead / per-tick isolation in the fast tier. A
// transient `gh`/GraphQL/network hiccup fetching ONE bead's PR snapshot
// must not abort the entire tick — the next bead must still reach
// READY. Companion regression `qdw_ci_pending_snapshot_failure_does_not_park_near_timebox_bead`
// covers the same isolation in the active-overlay / ci_pending path.
//
// Non-ignored: a regression here would silently let one bead's
// transient snapshot failure block every other in-flight bead.
#[test]
fn qdw_per_bead_isolation_snapshot_failure_does_not_abort_fast_tier() {
    let store = FakeStateStore::new();
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    // Honest all-green fixture for PR 102 so it can actually reach READY:
    //   * LLM returns "pass" so `skeptic_evidence` parses to SkepticVerdict::Pass (gate 7 Green)
    //   * `/er PASS` comment so `parse_er_verdict` returns Pass (gate 6 Green)
    //   * empty `files` so the non-test LOC floor is below EVIDENCE_FLOOR_LOC (Green)
    //   * no production paths so `is_production = false` (Partial /er is allowed)
    *llm.response.borrow_mut() = Some(Ok("pass".to_string()));

    for (bead, pr) in [("qdw-bead-101", 101u64), ("qdw-bead-102", 102u64)] {
        store
            .save(&BeadOverlay {
                bead_id: bead.into(),
                state: OverlayState::Attested,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(pr),
                branch: Some(format!("factory/{bead}-r1")),
                session_id: Some("sess-1".into()),
            })
            .unwrap();
        store
            .register_branch(bead, &format!("factory/{bead}-r1"))
            .unwrap();
    }
    let fresh_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.pr_snapshots.insert(
        102,
        PrSnapshot {
            pr_number: 102,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "deadbeef".into(),
            body: String::new(),
            comments: vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
            }],
            files: Vec::new(),
            // Fresh epoch so the wedge-detection check (>=30 min stale)
            // does not park bead 102 — the qdw fix targets per-bead
            // isolation, not the wedge heuristic.
            updated_at_epoch: fresh_epoch,
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
        },
    );
    // PR 101 deliberately has no scripted entry — `pr_snapshot(101)`
    // returns Err and without qdw's per-bead catch, the tick aborts.

    let telemetry_log =
        std::env::temp_dir().join(format!("afd_qdw_per_bead_{}.jsonl", std::process::id()));
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
        0,
        0,
    )
    .expect("tick must not abort when one bead's pr_snapshot errors");

    // Strong assertions: bead 102 reaches READY in the same tick.
    assert_eq!(
        summary.gates_assessed, 1,
        "only bead 102's gates should be assessed (bead 101's snapshot errored); got {summary:?}"
    );
    assert_eq!(
        summary.beads_ready, 1,
        "bead 102 must reach READY in the same tick as bead 101's snapshot failure; got {summary:?}"
    );

    let b102 = store.load("qdw-bead-102").unwrap().unwrap();
    assert_eq!(
        b102.state,
        OverlayState::Ready,
        "bead 102 must reach READY when its fixture is honestly all-green"
    );

    let b101 = store.load("qdw-bead-101").unwrap().unwrap();
    assert_eq!(
        b101.state,
        OverlayState::Attested,
        "bead 101 must stay ATTESTED on a single transient snapshot failure (no false-park); got {:?}",
        b101.state
    );

    // Telemetry assertion: at least one BEAD_SNAPSHOT_TRANSIENT_ERROR event
    // was emitted for the errored bead (qdw acceptance: per-bead catch
    // must be observable via the telemetry stream, not silent).
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let saw_bste = telemetry
        .lines()
        .filter(|l| l.contains("BEAD_SNAPSHOT_TRANSIENT_ERROR"))
        .count();
    assert!(
        saw_bste >= 1,
        "expected at least one BEAD_SNAPSHOT_TRANSIENT_ERROR event for the errored bead; telemetry was:\n{telemetry}"
    );
    let saw_phase_fast_tier = telemetry.lines().any(|l| {
        l.contains("BEAD_SNAPSHOT_TRANSIENT_ERROR") && l.contains("\"phase\":\"fast_tier\"")
    });
    assert!(
        saw_phase_fast_tier,
        "BEAD_SNAPSHOT_TRANSIENT_ERROR must carry phase=fast_tier for the snapshot fetch site"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-qdw: regression for the audit-found gap. The previous
// implementation collapsed `ci_pending_for_attested`'s `Err` to `false`,
// which let the active-overlay timebox-park branch false-park a near-
// timebox `Attested` bead on a single transient snapshot error.
//
// Two `Attested` beads are seeded:
//   * bead 901: autonomy_secs = autonomy_timebox_secs - 1 (one second
//     short of the timebox). PR 901 has no scripted snapshot —
//     `pr_snapshot(901)` returns `Err`.
//   * bead 902: healthy. PR 902 is honestly all-green.
//
// Invariants this test pins:
//   1. Bead 901 stays `Attested` (NOT `HumanHeld`) even though it is
//      one second from the timebox — a transient snapshot failure
//      must not bump the autonomy clock AND must not trigger
//      timebox-park / wedge-check this tick.
//   2. Bead 902 reaches `Ready` in the same tick (per-bead isolation:
//      one bead's snapshot error must not stop the healthy bead).
//   3. Telemetry contains a `BEAD_SNAPSHOT_TRANSIENT_ERROR` event with
//      `phase: "ci_pending"` for bead 901.
#[test]
fn qdw_ci_pending_snapshot_failure_does_not_park_near_timebox_bead() {
    let store = FakeStateStore::new();
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let vcs = FakeVcs::new();
    let cfg = test_cfg();
    *llm.response.borrow_mut() = Some(Ok("pass".to_string()));

    // Bead 901: near timebox, no scripted PR snapshot (errors).
    store
        .save(&BeadOverlay {
            bead_id: "qdw-near-timebox".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: cfg.autonomy_timebox_secs - 1,
            spend_usd: 0.0,
            pr_number: Some(901),
            branch: Some("factory/qdw-near-timebox-r1".into()),
            session_id: Some("sess-1".into()),
        })
        .unwrap();
    store
        .register_branch("qdw-near-timebox", "factory/qdw-near-timebox-r1")
        .unwrap();

    // Bead 902: healthy, all-green PR snapshot.
    store
        .save(&BeadOverlay {
            bead_id: "qdw-healthy".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(902),
            branch: Some("factory/qdw-healthy-r1".into()),
            session_id: Some("sess-1".into()),
        })
        .unwrap();
    store
        .register_branch("qdw-healthy", "factory/qdw-healthy-r1")
        .unwrap();

    let fresh_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.pr_snapshots.insert(
        902,
        PrSnapshot {
            pr_number: 902,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "cafebabe".into(),
            body: String::new(),
            comments: vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
            }],
            files: Vec::new(),
            updated_at_epoch: fresh_epoch,
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
        },
    );

    let telemetry_log =
        std::env::temp_dir().join(format!("afd_qdw_ci_pending_{}.jsonl", std::process::id()));
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
        // elapsed_secs=1 (non-zero) so the pre-fix `Err=>false` path
        // would have bumped autonomy_secs to timebox and parked the
        // bead this very tick — proving the regression existed. With
        // the qdw `SnapshotUnavailable` branch in place, the bump is
        // skipped and autonomy_secs stays exactly timebox - 1.
        0,
        1,
    )
    .expect("tick must not abort when one overlay's ci_pending snapshot errors");

    // Invariant 1: bead 901 was NOT false-parked (must stay Attested)
    // AND its autonomy_secs must stay exactly timebox - 1 (the bump
    // path is gated by `ci_pending_for_attested` returning
    // `SnapshotUnavailable`, which now short-circuits to `continue`
    // before the elapsed_secs bump runs).
    let b901 = store.load("qdw-near-timebox").unwrap().unwrap();
    assert_eq!(
        b901.autonomy_secs,
        cfg.autonomy_timebox_secs - 1,
        "near-timebox bead's autonomy_secs must stay exactly timebox - 1 (no bump on snapshot-unavailable); got {}",
        b901.autonomy_secs
    );
    assert_eq!(
        b901.state,
        OverlayState::Attested,
        "near-timebox bead must NOT be false-parked on a transient snapshot error; got {:?}",
        b901.state
    );

    // Invariant 2: bead 902 reached Ready (per-bead isolation holds in
    // the fast tier too).
    let b902 = store.load("qdw-healthy").unwrap().unwrap();
    assert_eq!(
        b902.state,
        OverlayState::Ready,
        "healthy bead must still reach Ready in the same tick as the snapshot-failing bead; got {:?}",
        b902.state
    );
    assert!(
        summary.beads_ready >= 1,
        "summary must report at least one bead reaching Ready despite the other bead's snapshot error; got {summary:?}"
    );

    // Invariant 3: telemetry records the ci_pending-phase transient error
    // for bead 901 with the expected phase discriminator.
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let saw_ci_pending_phase = telemetry.lines().any(|l| {
        l.contains("BEAD_SNAPSHOT_TRANSIENT_ERROR")
            && l.contains("\"beadId\":\"qdw-near-timebox\"")
            && l.contains("\"phase\":\"ci_pending\"")
    });
    assert!(
        saw_ci_pending_phase,
        "expected BEAD_SNAPSHOT_TRANSIENT_ERROR with phase=ci_pending for bead 901; telemetry was:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

struct QdwSequenceLlm {
    replies: std::cell::RefCell<Vec<String>>,
}

impl QdwSequenceLlm {
    fn new(replies: Vec<&str>) -> Self {
        Self {
            replies: std::cell::RefCell::new(
                replies.into_iter().map(|reply| reply.to_string()).collect(),
            ),
        }
    }
}

impl Llm for QdwSequenceLlm {
    fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
        let mut replies = self.replies.borrow_mut();
        if replies.is_empty() {
            return Err(DaemonError::Tool {
                tool: "llm".into(),
                rc: 1,
                stderr: "no scripted QDW LLM reply".into(),
            });
        }
        Ok(replies.remove(0))
    }
}

struct QdwPostErRefetchScm {
    target_pr: u64,
    target_snapshot: PrSnapshot,
    healthy_pr: u64,
    healthy_snapshot: PrSnapshot,
    target_calls: std::cell::RefCell<u32>,
}

impl QdwPostErRefetchScm {
    fn new(
        target_pr: u64,
        target_snapshot: PrSnapshot,
        healthy_pr: u64,
        healthy_snapshot: PrSnapshot,
    ) -> Self {
        Self {
            target_pr,
            target_snapshot,
            healthy_pr,
            healthy_snapshot,
            target_calls: std::cell::RefCell::new(0),
        }
    }
}

impl Scm for QdwPostErRefetchScm {
    fn labeled_issues(&self, _label: &str) -> Result<Vec<Issue>, DaemonError> {
        Ok(Vec::new())
    }

    fn collaborator_permission(&self, _login: &str) -> Result<Permission, DaemonError> {
        Ok(Permission::None)
    }

    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
        if pr == self.target_pr {
            let mut calls = self.target_calls.borrow_mut();
            *calls += 1;
            if *calls >= 5 {
                return Err(DaemonError::Tool {
                    tool: "gh".into(),
                    rc: 1,
                    stderr: "scripted post_er_refetch outage".into(),
                });
            }
            return Ok(self.target_snapshot.clone());
        }
        if pr == self.healthy_pr {
            return Ok(self.healthy_snapshot.clone());
        }
        Err(DaemonError::Tool {
            tool: "gh".into(),
            rc: 1,
            stderr: format!("no scripted snapshot for pr {pr}"),
        })
    }

    fn close_pr(&self, _pr: u64, _comment: &str) -> Result<(), DaemonError> {
        Ok(())
    }

    fn remote_branch_last_commit(&self, _branch: &str) -> Result<Option<u64>, DaemonError> {
        Ok(None)
    }
}

struct QdwAssessRefetchScm {
    pr: u64,
    snapshot: PrSnapshot,
    calls: std::cell::RefCell<u32>,
    close_calls: std::cell::RefCell<Vec<u64>>,
}

impl QdwAssessRefetchScm {
    fn new(pr: u64, snapshot: PrSnapshot) -> Self {
        Self {
            pr,
            snapshot,
            calls: std::cell::RefCell::new(0),
            close_calls: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl Scm for QdwAssessRefetchScm {
    fn labeled_issues(&self, _label: &str) -> Result<Vec<Issue>, DaemonError> {
        Ok(Vec::new())
    }

    fn collaborator_permission(&self, _login: &str) -> Result<Permission, DaemonError> {
        Ok(Permission::None)
    }

    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
        if pr != self.pr {
            return Err(DaemonError::Tool {
                tool: "gh".into(),
                rc: 1,
                stderr: format!("no scripted snapshot for pr {pr}"),
            });
        }
        let mut calls = self.calls.borrow_mut();
        *calls += 1;
        // This scenario must fail inside `verifier::assess`, after fast-tier
        // checks have already fetched enough SCM state to keep the bead in the
        // gate path: PR opened, ci_pending, er_runner, and post-/er refetch.
        if *calls >= 5 {
            return Err(DaemonError::Tool {
                tool: "gh".into(),
                rc: 1,
                stderr: "scripted assess refetch outage".into(),
            });
        }
        Ok(self.snapshot.clone())
    }

    fn close_pr(&self, pr: u64, _comment: &str) -> Result<(), DaemonError> {
        self.close_calls.borrow_mut().push(pr);
        Ok(())
    }

    fn remote_branch_last_commit(&self, _branch: &str) -> Result<Option<u64>, DaemonError> {
        Ok(None)
    }
}

struct QdwAttemptStore {
    inner: FakeStateStore,
    er_attempts: std::cell::RefCell<std::collections::HashMap<String, (u32, Option<u64>)>>,
}

impl QdwAttemptStore {
    fn new() -> Self {
        Self {
            inner: FakeStateStore::new(),
            er_attempts: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }
}

impl StateStore for QdwAttemptStore {
    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
        self.inner.load(bead_id)
    }

    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
        self.inner.save(overlay)
    }

    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError> {
        self.inner.register_branch(bead_id, branch)
    }

    fn owned_branches(&self) -> Result<Vec<String>, DaemonError> {
        self.inner.owned_branches()
    }

    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError> {
        self.inner.bead_id_for_branch(branch)
    }

    fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.inner.list_active_overlays()
    }

    fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), DaemonError> {
        self.inner.bump_autonomy_secs(bead_id, delta_secs)
    }

    fn increment_active_autonomy(
        &self,
        elapsed_secs: u64,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.inner.increment_active_autonomy(elapsed_secs)
    }

    fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.inner.recover_human_held(max_attempt)
    }

    fn human_held_at_or_above_attempt(
        &self,
        max_attempt: u32,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.inner.human_held_at_or_above_attempt(max_attempt)
    }

    fn save_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
        reviewer: &str,
        feedback_hash: &str,
        feedback_text: &str,
    ) -> Result<(), DaemonError> {
        self.inner
            .save_rejection(bead_id, attempt, reviewer, feedback_hash, feedback_text)
    }

    fn load_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<Option<(String, String)>, DaemonError> {
        self.inner.load_rejection(bead_id, attempt)
    }

    fn er_runner_attempt(&self, bead_id: &str) -> Result<(u32, Option<u64>), DaemonError> {
        Ok(self
            .er_attempts
            .borrow()
            .get(bead_id)
            .copied()
            .unwrap_or((0, None)))
    }

    fn incr_er_runner_attempt(&self, bead_id: &str, now_epoch: u64) -> Result<u32, DaemonError> {
        let mut attempts = self.er_attempts.borrow_mut();
        let next = attempts
            .get(bead_id)
            .map(|(count, _)| count + 1)
            .unwrap_or(1);
        attempts.insert(bead_id.to_string(), (next, Some(now_epoch)));
        Ok(next)
    }

    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        self.inner.reconcile_dispatching()
    }
}

fn qdw_green_snapshot(pr: u64, comments: Vec<PrComment>) -> PrSnapshot {
    let fresh_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    PrSnapshot {
        pr_number: pr,
        ci_success: true,
        mergeable: true,
        coderabbit_approved: true,
        bugbot_error_count: 0,
        unresolved_thread_count: 0,
        head_sha: format!("sha-{pr}"),
        body: String::new(),
        comments,
        files: Vec::new(),
        updated_at_epoch: fresh_epoch,
        ci_status: "green".into(),
        coderabbit_status: "green".into(),
        ci_pending: false,
    }
}

// jleechan-qdw: regression for the latest PR #184 inline blocker. After the
// daemon posts a fresh `/er PASS`, the post-/er snapshot refetch can still
// fail transiently. The old code emitted `BEAD_SNAPSHOT_TRANSIENT_ERROR` but
// then fell through into `verifier::assess`, which performs another
// `pr_snapshot(pr)`; that second outage became Unknown/all_green=false and
// Stage 1 false-parked the otherwise posted/PASS bead to HUMAN_HELD.
//
// This pins the intended retry behavior:
//   * the affected bead stays ATTESTED after post_er_refetch fails;
//   * no assessment/reroll/park logic runs for that bead this tick;
//   * a later healthy bead still reaches READY in the same tick.
#[test]
fn qdw_post_er_refetch_failure_skips_bead_without_false_park() {
    let store = QdwAttemptStore::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = QdwSequenceLlm::new(vec![
        "pass",     // affected bead: Skeptic gate
        "/er PASS", // affected bead: posted /er verdict
        "pass",     // healthy bead: Skeptic gate
    ]);
    let vcs = FakeVcs::new();
    let cfg = test_cfg();

    store
        .save(&BeadOverlay {
            bead_id: "qdw-post-refetch-fails".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(1901),
            branch: Some("factory/qdw-post-refetch-fails-r1".into()),
            session_id: Some("sess-1".into()),
        })
        .unwrap();
    store
        .register_branch(
            "qdw-post-refetch-fails",
            "factory/qdw-post-refetch-fails-r1",
        )
        .unwrap();
    store
        .save(&BeadOverlay {
            bead_id: "qdw-post-refetch-healthy".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(1902),
            branch: Some("factory/qdw-post-refetch-healthy-r1".into()),
            session_id: Some("sess-2".into()),
        })
        .unwrap();
    store
        .register_branch(
            "qdw-post-refetch-healthy",
            "factory/qdw-post-refetch-healthy-r1",
        )
        .unwrap();

    let scm = QdwPostErRefetchScm::new(
        1901,
        qdw_green_snapshot(1901, Vec::new()),
        1902,
        qdw_green_snapshot(
            1902,
            vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
            }],
        ),
    );
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_qdw_post_er_refetch_{}.jsonl",
        std::process::id()
    ));
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
        0,
        0,
    )
    .expect("post_er_refetch outage must not abort the tick");

    let affected = store.load("qdw-post-refetch-fails").unwrap().unwrap();
    assert_eq!(
        affected.state,
        OverlayState::Attested,
        "post_er_refetch outage must leave the affected bead ATTESTED for retry, not HUMAN_HELD"
    );
    let healthy = store.load("qdw-post-refetch-healthy").unwrap().unwrap();
    assert_eq!(
        healthy.state,
        OverlayState::Ready,
        "later healthy bead must still reach READY after the affected bead is skipped"
    );
    assert_eq!(
        summary.gates_assessed, 1,
        "only the healthy bead should be assessed after post_er_refetch fails; got {summary:?}"
    );
    assert_eq!(
        summary.beads_parked_human_held, 0,
        "post_er_refetch outage must not park any bead HUMAN_HELD; got {summary:?}"
    );
    assert_eq!(
        *scm.target_calls.borrow(),
        5,
        "affected PR should stop immediately after the failed post_er_refetch call; an assess() fall-through would add another pr_snapshot call"
    );
    let (er_attempt_count, _) = store
        .er_runner_attempt("qdw-post-refetch-fails")
        .expect("test store must expose /er runner attempts");
    assert_eq!(
        er_attempt_count, 1,
        "post_er_refetch outage happens after the /er comment posts, so the affected bead must record one er_runner_attempt"
    );

    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().any(|call| {
            call.contains("comment_external(owner/repo#1901,") && call.contains("/er PASS")
        }),
        "affected PR should receive the posted /er PASS comment before post_er_refetch fails; calls were: {tracker_calls:?}"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.lines().any(|line| {
            line.contains("BEAD_SNAPSHOT_TRANSIENT_ERROR")
                && line.contains("\"beadId\":\"qdw-post-refetch-fails\"")
                && line.contains("\"phase\":\"post_er_refetch\"")
        }),
        "expected post_er_refetch transient telemetry for affected bead; telemetry was:\n{telemetry}"
    );
    assert!(
        telemetry.lines().any(|line| {
            line.contains("\"beadId\":\"qdw-post-refetch-fails\"")
                && line.contains("\"eventType\":\"ER_RUNNER_POSTED\"")
        }),
        "affected bead must emit ER_RUNNER_POSTED before post_er_refetch fails; telemetry was:\n{telemetry}"
    );
    assert!(
        !telemetry.lines().any(|line| {
            line.contains("\"beadId\":\"qdw-post-refetch-fails\"")
                && line.contains("\"eventType\":\"GATE_ASSESSMENT\"")
        }),
        "affected bead must not enter gate assessment after post_er_refetch outage; telemetry was:\n{telemetry}"
    );
    assert!(
        !telemetry.lines().any(|line| {
            line.contains("\"beadId\":\"qdw-post-refetch-fails\"")
                && line.contains("\"eventType\":\"REROLL_VERDICT_RECORDED\"")
        }),
        "affected bead must not record a reroll verdict after post_er_refetch outage; telemetry was:\n{telemetry}"
    );
    assert!(
        !telemetry.lines().any(|line| {
            line.contains("\"beadId\":\"qdw-post-refetch-fails\"")
                && line.contains("\"eventType\":\"PARKED_HUMAN_HELD\"")
        }),
        "affected bead must not emit PARKED_HUMAN_HELD after post_er_refetch outage; telemetry was:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-qdw: Unknown gates are verifier outages, not defect verdicts. If
// `verifier::assess` cannot re-fetch SCM state after earlier fast-tier reads
// succeeded, the bead must stay ATTESTED for retry. In stage 2 this is
// especially important: treating Unknown as Red would execute the re-roll
// engine and close a healthy PR.
#[test]
fn qdw_assess_refetch_failure_stays_attested_and_never_closes_pr() {
    let store = QdwAttemptStore::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let vcs = FakeVcs::new();
    let mut cfg = test_cfg();
    cfg.stage = 2;

    store
        .save(&BeadOverlay {
            bead_id: "qdw-assess-refetch-fails".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(2001),
            branch: Some("factory/qdw-assess-refetch-fails-r1".into()),
            session_id: Some("sess-assess".into()),
        })
        .unwrap();
    store
        .register_branch(
            "qdw-assess-refetch-fails",
            "factory/qdw-assess-refetch-fails-r1",
        )
        .unwrap();

    let scm = QdwAssessRefetchScm::new(
        2001,
        qdw_green_snapshot(
            2001,
            vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
            }],
        ),
    );
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_qdw_assess_refetch_{}.jsonl",
        std::process::id()
    ));
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
        0,
        0,
    )
    .expect("Unknown-only gate assessment must not abort the tick");

    let overlay = store.load("qdw-assess-refetch-fails").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Attested,
        "assess refetch outage must leave the bead ATTESTED for retry"
    );
    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(summary.beads_ready, 0);
    assert_eq!(summary.beads_parked_human_held, 0);
    assert!(
        scm.close_calls.borrow().is_empty(),
        "stage 2 Unknown-only gate reports must not execute reroll/close_pr"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.lines().any(|line| {
            line.contains("\"beadId\":\"qdw-assess-refetch-fails\"")
                && line.contains("\"eventType\":\"GATE_ASSESSMENT_TRANSIENT_UNKNOWN\"")
        }),
        "expected transient Unknown gate telemetry; telemetry was:\n{telemetry}"
    );
    assert!(
        !telemetry.lines().any(|line| {
            line.contains("\"beadId\":\"qdw-assess-refetch-fails\"")
                && line.contains("\"eventType\":\"REROLL_VERDICT_RECORDED\"")
        }),
        "Unknown-only gate report must not emit reroll verdict; telemetry was:\n{telemetry}"
    );
    assert!(
        !telemetry.lines().any(|line| {
            line.contains("\"beadId\":\"qdw-assess-refetch-fails\"")
                && line.contains("\"eventType\":\"PARKED_HUMAN_HELD\"")
        }),
        "Unknown-only gate report must not park HUMAN_HELD; telemetry was:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-qdw: helper-level coverage for the dual-reviewer combining
// logic that feeds `skeptic_evidence`. The previous code returned
// `Ok(None)` when BOTH reviewers failed, which let gate 7 = Unknown
// cascade into a Stage-1 false-park. `combine_dual_verdict` now
// returns `Err` in that case so `run_fast_tier`'s per-bead catch
// keeps the bead ATTESTED. These tests pin both the new failure
// contract and the preserved single-pass success contract.
#[test]
fn qdw_combine_dual_verdict_returns_err_when_both_reviewers_fail() {
    let r = combine_dual_verdict(None, None, "qdw-bead-x", 7);
    assert!(
        matches!(r, Err(daemon::errors::DaemonError::Tool { ref tool, .. }) if tool == "skeptic_evidence"),
        "expected Err(DaemonError::Tool {{ tool: \"skeptic_evidence\", .. }}) when both reviewers fail, got {r:?}"
    );
}

#[test]
fn qdw_combine_dual_verdict_preserves_single_pass_success() {
    // Single-pass success: one reviewer returns Pass, the other failed.
    let r = combine_dual_verdict(Some(SkepticVerdict::Pass), None, "b", 1);
    assert!(
        matches!(r, Ok(Some(SkepticVerdict::Pass))),
        "single-pass Pass must be preserved; got {r:?}"
    );
    let r = combine_dual_verdict(None, Some(SkepticVerdict::Pass), "b", 1);
    assert!(
        matches!(r, Ok(Some(SkepticVerdict::Pass))),
        "single-pass Pass must be preserved when only the second reviewer returned it; got {r:?}"
    );
    // Both reviewers Pass — same outcome, stronger signal.
    let r = combine_dual_verdict(
        Some(SkepticVerdict::Pass),
        Some(SkepticVerdict::Pass),
        "b",
        1,
    );
    assert!(
        matches!(r, Ok(Some(SkepticVerdict::Pass))),
        "double Pass must still produce Pass; got {r:?}"
    );
}

#[test]
fn qdw_combine_dual_verdict_fail_beats_missing_or_warn() {
    // One reviewer Fail + other missing — Fail wins (single-pass Fail preserved).
    let r = combine_dual_verdict(Some(SkepticVerdict::Fail("bad".into())), None, "b", 1);
    assert!(
        matches!(r, Ok(Some(SkepticVerdict::Fail(_)))),
        "Fail from one reviewer must be preserved when the other fails; got {r:?}"
    );
    // One Fail + one Pass — Fail wins.
    let r = combine_dual_verdict(
        Some(SkepticVerdict::Fail("bad".into())),
        Some(SkepticVerdict::Pass),
        "b",
        1,
    );
    assert!(
        matches!(r, Ok(Some(SkepticVerdict::Fail(_)))),
        "Fail must beat Pass; got {r:?}"
    );
    // Both Fail — Fail merged with " && ".
    let r = combine_dual_verdict(
        Some(SkepticVerdict::Fail("a".into())),
        Some(SkepticVerdict::Fail("b".into())),
        "bead",
        9,
    );
    match r {
        Ok(Some(SkepticVerdict::Fail(reason))) => {
            assert!(reason.contains("a") && reason.contains("b"))
        }
        other => panic!("expected merged Fail, got {other:?}"),
    }
    // One Warn + one Fail — Fail wins (Fail beats Warn).
    let r = combine_dual_verdict(
        Some(SkepticVerdict::Warn("soft".into())),
        Some(SkepticVerdict::Fail("hard".into())),
        "b",
        1,
    );
    assert!(
        matches!(r, Ok(Some(SkepticVerdict::Fail(_)))),
        "Fail must beat Warn; got {r:?}"
    );
}

#[test]
fn qdw_combine_dual_verdict_one_fail_one_none_is_not_both_failed() {
    // Single-reviewer Fail is a real signal (do NOT escalate to Err).
    let r = combine_dual_verdict(Some(SkepticVerdict::Fail("reason".into())), None, "b", 1);
    match r {
        Ok(Some(SkepticVerdict::Fail(_))) => {}
        other => {
            panic!("expected Ok(Some(Fail)), not Err — single Fail is a real signal; got {other:?}")
        }
    }
}
