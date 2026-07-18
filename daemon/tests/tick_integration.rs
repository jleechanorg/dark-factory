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
use daemon::tools::{Bead, Issue, LabeledPr, Llm, Permission, PrComment, PrHeadBranch, PrSnapshot, Scm};
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
        repos: std::collections::HashMap::new(),
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
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            head_committed_epoch: 0,
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
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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

/// jleechan-coder-silent-false-parks-h92r regression test: reproduces the
/// LIVE 2026-07-17 bug this bead tracks. All 6 active dispatch lanes were
/// parked `PARKED_HUMAN_HELD reason=coder_silent` while their coders were
/// demonstrably working (transcripts growing, real commits landing) — the
/// wedge-detection sweep's ONLY liveness signal was "no remote branch commit
/// in 30 minutes", which is not evidence of silence for a coder mid-edit
/// that simply hasn't pushed yet. Same setup as
/// `test_wedge_detection_dispatched_coder_silent` (remote branch has no
/// commit at all), but this time the coder's own transcript directory was
/// modified moments ago — the bead must NOT be parked, and a
/// `CODER_ACTIVE_GRACE` telemetry event must record why.
#[test]
fn test_wedge_detection_dispatched_coder_silent_saved_by_transcript_activity() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-active".into(),
        BeadOverlay {
            bead_id: "bead-active".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 1900,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-active-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );

    // Same as the false-park scenario: the remote branch has received no
    // commit at all, which alone would trigger the silence park.
    scm.remote_branches
        .insert("factory/bead-active-r1".into(), None);

    // But the coder's own transcript was modified 5 seconds ago — real,
    // ongoing local activity the old branch-only check couldn't see.
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // `test_cfg()` leaves `ao_project: None` and `target_repo: "owner/repo"`,
    // so `Config::resolve_repo` derives `ao_project = "repo"` (last path
    // segment) — the same derivation `SpawnSpec` tests already rely on.
    sessions.set_transcript_activity("repo", "factory/bead-active-r1", now_epoch - 5);

    let telemetry_log = std::env::temp_dir().join("afd_test_wedge_silent_grace.jsonl");
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

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 0,
        "a coder with recent transcript activity must NOT be parked"
    );

    let o = store.load("bead-active").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Dispatched,
        "bead must remain DISPATCHED, not be parked HUMAN_HELD"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(
        logs.contains("CODER_ACTIVE_GRACE"),
        "telemetry must record the grace decision: {}",
        logs
    );
    assert!(
        !logs.contains("PARKED_HUMAN_HELD"),
        "must not park: {}",
        logs
    );
    assert!(!logs.contains("coder_silent"), "must not park: {}", logs);

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Fail-closed companion to the grace test above: a STALE transcript
/// (last modified over 30 minutes ago) must not save a bead from parking —
/// only genuinely recent evidence should count. Guards against a naive "any
/// transcript record ever" implementation that would defeat the timeout.
#[test]
fn test_wedge_detection_dispatched_coder_silent_stale_transcript_still_parks() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-stale".into(),
        BeadOverlay {
            bead_id: "bead-stale".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 1900,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-stale-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );

    scm.remote_branches
        .insert("factory/bead-stale-r1".into(), None);

    // Transcript exists but was last modified 2 hours ago — no evidence of
    // CURRENT activity.
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    sessions.set_transcript_activity("repo", "factory/bead-stale-r1", now_epoch - 7200);

    let telemetry_log = std::env::temp_dir().join("afd_test_wedge_silent_stale.jsonl");
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

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "a stale transcript must not save a genuinely silent coder from parking"
    );

    let o = store.load("bead-stale").unwrap().unwrap();
    assert_eq!(o.state, OverlayState::HumanHeld);

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(logs.contains("PARKED_HUMAN_HELD"), "logs: {}", logs);
    assert!(logs.contains("coder_silent"), "logs: {}", logs);

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-5ia2 regression test: reproduces the LIVE bug this bead tracks.
/// Bead `jleechan-vj89`'s overlay was observed with `state=DISPATCHED`,
/// `branch=factory/jleechan-vj89-r1`, and a real, alive `session_id`
/// (`wa-3004`) whose ACTUAL live branch was `feat/wa-3004-hook-refactor` —
/// a completely unrelated, pre-existing task. This must be caught and
/// self-healed by the dispatch-integrity sweep on the very next tick,
/// independent of the 30-minute "coder silent" autonomy threshold (this
/// overlay's `autonomy_secs` is deliberately tiny — 5 — to prove the sweep
/// is NOT gated on that timer).
#[test]
fn test_dispatch_integrity_sweep_parks_session_branch_mismatch() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.set_session_branch("wa-3004", "feat/wa-3004-hook-refactor");
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "jleechan-vj89".into(),
        BeadOverlay {
            bead_id: "jleechan-vj89".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(8227),
            branch: Some("factory/jleechan-vj89-r1".into()),
            session_id: Some("wa-3004".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_dispatch_integrity_sweep.jsonl");
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

    let summary = run_tick(&deps, 1, 1).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "a branch-mismatched DISPATCHED row must be parked on the very next tick"
    );

    let o = store.load("jleechan-vj89").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::HumanHeld,
        "a corrupted DISPATCHED row must never be left silently trusted"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(logs.contains("PARKED_HUMAN_HELD"), "logs: {}", logs);
    assert!(logs.contains("session_branch_mismatch"), "logs: {}", logs);
    assert!(logs.contains("wa-3004"), "logs: {}", logs);
    assert!(
        logs.contains("feat/wa-3004-hook-refactor"),
        "logs: {}",
        logs
    );

    let recovery = run_tick(&deps, 2, 0).unwrap();
    assert_eq!(
        recovery.beads_recovered_from_held, 0,
        "a branch-mismatch hold retaining a live session must never auto-requeue"
    );
    let held = store.load("jleechan-vj89").unwrap().unwrap();
    assert_eq!(held.state, OverlayState::HumanHeld);
    assert_eq!(held.session_id.as_deref(), Some("wa-3004"));
    assert_eq!(
        held.park_reason.as_deref(),
        Some("session_branch_mismatch")
    );
    assert!(
        !sessions
            .calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("spawn(")),
        "recovery must not create an overlapping worker: {:?}",
        sessions.calls.borrow()
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Companion test: a DISPATCHED row whose session_id's live branch DOES
/// match must be left completely untouched by the integrity sweep (no
/// false positives on legitimate in-flight dispatches).
#[test]
fn test_dispatch_integrity_sweep_leaves_matching_branch_alone() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.set_session_branch("wa-4001", "factory/bead-ok-r1");
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-ok".into(),
        BeadOverlay {
            bead_id: "bead-ok".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-ok-r1".into()),
            session_id: Some("wa-4001".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_dispatch_integrity_ok.jsonl");
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

    let summary = run_tick(&deps, 1, 1).unwrap();
    assert_eq!(summary.beads_parked_human_held, 0);

    let o = store.load("bead-ok").unwrap().unwrap();
    assert_eq!(o.state, OverlayState::Dispatched);

    let _ = std::fs::remove_file(&telemetry_log);
}

/// bead jleechan-tfs1 amendment: an adversarial review found the
/// append-only guarantee for adopted-branch remediation was enforced ONLY
/// at the prompt level, with zero code-level detection if the spawned
/// coder session force-pushed anyway. This reproduces that gap and proves
/// the post-hoc detection backstop: a bead DISPATCHED on an adopted branch
/// whose current remote tip is no longer a descendant of the pre-session
/// HEAD SHA (i.e. history was rewritten) must be parked HUMAN_HELD with an
/// escalation comment naming both SHAs on the very next tick — never
/// silently left DISPATCHED as if remediation were proceeding normally.
#[test]
fn test_dispatch_integrity_sweep_detects_force_push_on_adopted_branch() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-adopted-rewrite".into(),
        BeadOverlay {
            bead_id: "bead-adopted-rewrite".into(),
            state: OverlayState::Dispatched,
            attempt: 2,
            reroll_count: 1,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(999),
            branch: Some("alice/my-cool-feature".into()),
            session_id: Some("session-xyz".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some("pre-session-sha-abc123".into()),
            park_reason: None,
            target_repo: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_force_push_detection.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut vcs = FakeVcs::new();
    // The branch's CURRENT remote tip after the (simulated) force-push:
    vcs.heads
        .insert("alice/my-cool-feature".into(), "rewritten-sha-999".into());
    // The pre-session SHA is NOT an ancestor of that new tip — i.e. it was
    // dropped from history by a force-push/rebase:
    vcs.ancestor_pairs.insert(
        (
            "pre-session-sha-abc123".to_string(),
            "rewritten-sha-999".to_string(),
        ),
        false,
    );

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

    let summary = run_tick(&deps, 1, 1).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "a history-rewrite on an adopted branch must be parked on the very next tick"
    );

    let o = store.load("bead-adopted-rewrite").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::HumanHeld,
        "a bead whose adopted branch was force-pushed must never be left DISPATCHED as if remediation is proceeding normally"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(logs.contains("PARKED_HUMAN_HELD"), "logs: {}", logs);
    assert!(
        logs.contains("adopted_branch_history_rewrite_detected"),
        "logs: {}",
        logs
    );
    assert!(logs.contains("pre-session-sha-abc123"), "logs: {}", logs);
    assert!(logs.contains("rewritten-sha-999"), "logs: {}", logs);

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Companion to the force-push detection test: an ordinary append-only
/// remediation commit (the pre-session SHA IS still an ancestor of the
/// branch's current tip) must NOT be parked HUMAN_HELD by the new sweep —
/// only a genuine history rewrite should trigger it.
#[test]
fn test_dispatch_integrity_sweep_allows_fast_forward_adopted_commit() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-adopted-ff".into(),
        BeadOverlay {
            bead_id: "bead-adopted-ff".into(),
            state: OverlayState::Dispatched,
            attempt: 2,
            reroll_count: 1,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(998),
            branch: Some("bob/another-feature".into()),
            session_id: Some("session-ff".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some("pre-session-sha-def456".into()),
            park_reason: None,
            target_repo: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_force_push_ok.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut vcs = FakeVcs::new();
    vcs.heads
        .insert("bob/another-feature".into(), "new-commit-sha-777".into());
    // No entry in ancestor_pairs for this (ancestor, descendant) pair -> the
    // fake defaults to `true` (no rewrite), matching a real ancestor check.

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

    let _summary = run_tick(&deps, 1, 1).unwrap();

    let o = store.load("bead-adopted-ff").unwrap().unwrap();
    assert_ne!(
        o.state,
        OverlayState::HumanHeld,
        "an ordinary append-only remediation commit must not be parked HUMAN_HELD"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !logs.contains("adopted_branch_history_rewrite_detected"),
        "logs: {}",
        logs
    );

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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch - 2000, // older than 1800s
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            head_committed_epoch: 0,
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
    assert_eq!(
        o.session_id, None,
        "positive terminal proof must be persisted with the recoverable hold"
    );

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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
            head_sha: "remote-head-advanced".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch - 2000,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            head_committed_epoch: 0,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            head_committed_epoch: 0,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            head_committed_epoch: 0,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
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
fn factory_labeled_existing_pr_is_adopted_and_verified_without_spawn() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 701,
        title: "Existing factory PR".into(),
        body: "already has a branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#701".into(),
        head_ref_name: "feature/already-open-pr".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.pr_snapshots.insert(
        701,
        qdw_green_snapshot(
            701,
            vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
        ),
    );

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_existing_pr_adoption.jsonl");
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
    .expect("factory-labeled PR should be adopted and verified");

    assert_eq!(summary.beads_created, 1);
    assert_eq!(summary.beads_dispatched, 0);
    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(summary.beads_ready, 1);

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::Ready);
    assert_eq!(overlay.pr_number, Some(701));
    assert_eq!(overlay.branch.as_deref(), Some("feature/already-open-pr"));
    assert_eq!(
        store.bead_id_for_branch("feature/already-open-pr").unwrap(),
        Some("fake-bead-1".into())
    );

    let session_calls = sessions.calls.borrow();
    assert!(
        !session_calls.iter().any(|call| call.starts_with("spawn(")),
        "existing PR adoption must not spawn a new session; calls: {session_calls:?}"
    );
    assert!(
        !session_calls.iter().any(|call| call.starts_with("attach(")),
        "existing PR adoption must not attach a remediation session during initial verification; calls: {session_calls:?}"
    );
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("EXISTING_PR_ADOPTED") && telemetry.contains("READY_FOR_MERGE"),
        "expected adoption and ready events, got:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn factory_labeled_existing_pr_second_tick_reuses_tracking_bead_without_spawn() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 702,
        title: "Existing factory PR".into(),
        body: "already has a branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#702".into(),
        head_ref_name: "feature/already-open-pr-702".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.pr_snapshots.insert(
        702,
        qdw_green_snapshot(
            702,
            vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
        ),
    );

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_existing_pr_adoption_idempotent.jsonl");
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

    let first = run_tick(&deps, 0, 0).expect("first tick adopts PR");
    let second = run_tick(&deps, 1, 0).expect("second tick reuses adoption");

    assert_eq!(first.beads_created, 1);
    assert_eq!(second.beads_created, 0);
    assert_eq!(second.beads_dispatched, 0);
    assert_eq!(
        store
            .bead_id_for_branch("feature/already-open-pr-702")
            .unwrap(),
        Some("fake-bead-1".into())
    );
    let create_calls: Vec<_> = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.starts_with("create_bead("))
        .cloned()
        .collect();
    assert_eq!(
        create_calls.len(),
        1,
        "second tick must not duplicate the tracking bead: {create_calls:?}"
    );
    let session_calls = sessions.calls.borrow();
    assert!(
        !session_calls.iter().any(|call| call.starts_with("spawn(")),
        "repeated adoption must not spawn a session; calls: {session_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn factory_labeled_existing_pr_without_session_is_not_parked_as_stalled() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 703,
        title: "Existing factory PR waiting on review".into(),
        body: "already has a branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#703".into(),
        head_ref_name: "feature/already-open-pr-703".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    let mut stale_unknown = qdw_green_snapshot(703, Vec::new());
    stale_unknown.updated_at_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(1900);
    scm.pr_snapshots.insert(703, stale_unknown);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_existing_pr_no_session_waiting.jsonl");
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

    run_tick(&deps, 0, 0).expect("first tick adopts PR");
    run_tick(&deps, 1, 1).expect("second tick must not false-park no-session PR");

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::Attested);
    assert_eq!(overlay.session_id, None);
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("EXISTING_PR_WAITING"),
        "expected no-session waiting telemetry, got:\n{telemetry}"
    );
    assert!(
        !telemetry.contains("\"reason\":\"session_stalled\""),
        "existing PR without session must not be parked as session_stalled:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn factory_labeled_pr_branch_collision_is_refused_without_stealing_mapping() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 704,
        title: "Branch collision PR".into(),
        body: "tries to reuse an owned branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#704".into(),
        head_ref_name: "factory/existing-bead-r1".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    store
        .save(&BeadOverlay {
            bead_id: "existing-bead".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(111),
            branch: Some("factory/existing-bead-r1".into()),
            session_id: Some("sess-existing".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        })
        .unwrap();
    store
        .register_branch("existing-bead", "factory/existing-bead-r1")
        .unwrap();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_existing_pr_collision.jsonl");
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
    .expect("branch collision should escalate without failing the tick");

    assert_eq!(summary.beads_escalated, 1);
    assert_eq!(
        store
            .bead_id_for_branch("factory/existing-bead-r1")
            .unwrap(),
        Some("existing-bead".into()),
        "collision must not steal the branch registry key"
    );
    let refused_overlay = store.load("fake-bead-1").unwrap();
    assert!(
        refused_overlay.is_none(),
        "refused adoption must not create an overlay for the colliding PR; overlay={refused_overlay:?}, calls={:?}",
        store.calls.borrow()
    );
    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().any(|call| {
            call.contains("comment_external(owner/repo#704")
                && call.contains("already registered to bead `existing-bead`")
        }),
        "collision must be escalated on the original PR: {tracker_calls:?}"
    );
    let store_calls = store.calls.borrow();
    assert!(
        !store_calls
            .iter()
            .any(|call| call == "register_branch(fake-bead-1,factory/existing-bead-r1)"),
        "colliding adoption must not register the candidate bead: {store_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-sniw re-review gap #1: `daemon/tests/intake.rs`'s
/// `fork_factory_pr_is_skipped_with_escalation_comment` only proves
/// `intake::normalize_labeled_prs` returns an empty `Vec` for a fork PR — it
/// stops at the intake unit boundary and never proves the *tick-level*
/// guarantee that `run_tick` (and specifically `run_slow_tier`'s
/// `deps.store.register_branch` call at the single call site gated by the
/// `intake::normalize_labeled_prs(...)` loop) never registers a branch for a
/// fork-origin PR. A fork PR is filtered out one layer earlier inside
/// `intake::normalize_labeled_prs` (via `same_repo_pr`), so it never even
/// produces an `ExistingPrIntake` for `run_slow_tier`'s branch-collision loop
/// to see — this test proves that end-to-end through the real `run_tick`
/// entry point, not just at the `intake` unit boundary.
#[test]
fn fork_labeled_pr_never_registers_branch_at_tick_level() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 811,
        title: "Fork PR trying to adopt".into(),
        body: "opened from a fork, must never enter the branch registry".into(),
        author_login: "mallory".into(),
        external_ref: "owner/repo#811".into(),
        head_ref_name: "factory/fork-pr-r1".into(),
        is_cross_repository: true,
        head_repo_full_name: Some("mallory-fork/repo".into()),
        head_repo_owner_login: Some("mallory-fork".into()),
    });
    scm.permissions.insert("mallory".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_fork_pr_tick_level.jsonl");
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
    .expect("fork PR intake must not fail the tick");

    assert_eq!(
        summary.beads_created, 0,
        "fork PR must never be adopted as a bead"
    );
    assert_eq!(
        summary.beads_escalated, 0,
        "fork PR is filtered before run_slow_tier's own branch-collision escalation counter fires; \
         the escalation comment comes from intake::normalize_labeled_prs directly"
    );
    assert_eq!(
        store.bead_id_for_branch("factory/fork-pr-r1").unwrap(),
        None,
        "fork PR's branch must never appear in the branch-ownership registry"
    );
    let store_calls = store.calls.borrow();
    assert!(
        !store_calls
            .iter()
            .any(|call| call.starts_with("register_branch(")),
        "register_branch must never be called for a fork-origin PR: {store_calls:?}"
    );
    assert!(
        !store_calls.iter().any(|call| call.starts_with("save(")),
        "no overlay may be created/saved for a fork-origin PR: {store_calls:?}"
    );
    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls
            .iter()
            .all(|call| !call.starts_with("create_bead(")),
        "fork PR must not create an adopted bead at the tick level: {tracker_calls:?}"
    );
    assert!(
        tracker_calls.iter().any(|call| {
            call.contains("comment_external(owner/repo#811")
                && call.contains("fork/cross-repository PR adoption is not supported")
                && call.contains("jleechan-tfs1")
        }),
        "fork PR must still receive the intake-level escalation comment: {tracker_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn adopted_non_green_pr_parks_human_held_with_v1_escalation() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 705,
        title: "Adopted red PR".into(),
        body: "already has a non-green branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#705".into(),
        head_ref_name: "feature/adopted-red-pr".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    let mut snapshot = qdw_green_snapshot(
        705,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    snapshot.ci_success = false;
    snapshot.ci_status = "failure".into();
    scm.pr_snapshots.insert(705, snapshot);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_existing_pr_adoption_red.jsonl");
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
    .expect("non-green adopted PR should park with escalation");

    assert_eq!(summary.beads_created, 1);
    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(summary.beads_parked_human_held, 1);
    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(overlay.session_id, None);
    assert_eq!(overlay.pr_number, Some(705));
    assert_eq!(overlay.branch.as_deref(), Some("feature/adopted-red-pr"));
    assert_eq!(
        store.bead_id_for_branch("feature/adopted-red-pr").unwrap(),
        Some("fake-bead-1".into())
    );
    let session_calls = sessions.calls.borrow();
    assert!(
        session_calls
            .iter()
            .all(|call| { !call.starts_with("spawn(") && !call.starts_with("attach(") }),
        "adopted non-green PR must not fabricate remediation sessions: {session_calls:?}"
    );
    let branch_calls = store.calls.borrow();
    assert!(
        branch_calls
            .iter()
            .all(|call| !call.contains("factory/fake-bead-1-r2")),
        "adopted non-green PR must not fabricate replacement branches: {branch_calls:?}"
    );
    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().any(|call| {
            call.contains("comment_external(owner/repo#705")
                && call.contains("adopted PR is not green")
                && call.contains("jleechan-tfs1")
        }),
        "adopted red PR must receive the v1 escalation comment: {tracker_calls:?}"
    );
    assert!(
        tracker_calls.iter().all(|call| !call.contains("close")),
        "original PR must not be closed by adopted non-green handling: {tracker_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// bead jleechan-tfs1, requirement (a) + (c), full pipeline (Stage 2): a
/// red-gate reroll on an adopted PR spawns a remediation coder session on the
/// EXISTING contributor branch via `run_tick`'s real intake -> verifier ->
/// reroll wiring (not a direct `reroll::execute` call — this proves the
/// `is_adopted` flag actually survives the round trip through
/// `tick::run_slow_tier`'s adoption block, `StateStore::save`/`load`, and
/// back into `tick::run_fast_tier`'s reroll dispatch). The PR stays open and
/// branch registry is unchanged.
#[test]
fn adopted_red_pr_stage2_reroll_spawns_remediation_session_leaves_pr_open() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 706,
        title: "Adopted red PR (stage 2)".into(),
        body: "already has a non-green branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#706".into(),
        head_ref_name: "alice/my-cool-feature".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    let mut snapshot = qdw_green_snapshot(
        706,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    snapshot.ci_success = false;
    snapshot.ci_status = "failure".into();
    scm.pr_snapshots.insert(706, snapshot);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2; // Stage 2: actually execute reroll() rather than just recording the verdict
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("alice/my-cool-feature".into(), "pre-session-sha-abc123".into());
    let telemetry_log = std::env::temp_dir().join("afd_adopted_stage2_success.jsonl");
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
    .expect("adopted red PR stage2 reroll should succeed");

    assert_eq!(summary.beads_created, 1);
    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(
        summary.beads_parked_human_held, 0,
        "a successful remediation-session spawn must not park the bead"
    );

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    // run_fast_tier does not revisit a bead after the reroll branch in the
    // same tick, so quiescence-gated promotion happens on a later tick.
    assert_eq!(overlay.state, OverlayState::Dispatched);
    assert_eq!(overlay.attempt, 2);
    assert_eq!(
        overlay.pr_number,
        Some(706),
        "PR must stay open under the same number"
    );
    assert_eq!(overlay.branch.as_deref(), Some("alice/my-cool-feature"));
    assert!(overlay.is_adopted);

    assert_eq!(
        store.bead_id_for_branch("alice/my-cool-feature").unwrap(),
        Some("fake-bead-1".into())
    );
    assert_eq!(
        store.branches.borrow().as_slice(),
        &["alice/my-cool-feature"],
        "no fabricated replacement branch should be registered"
    );

    let session_prompts = sessions.spawn_prompts.borrow();
    assert_eq!(
        session_prompts.len(),
        1,
        "adopted stage-2 reroll must spawn exactly one remediation session: {session_prompts:?}"
    );
    let (spawned_bead_id, prompt) = &session_prompts[0];
    assert_eq!(spawned_bead_id, "fake-bead-1");
    assert!(
        prompt.contains("CI check-run(s) not all success"),
        "spawn prompt must include the red-gate feedback: {prompt}"
    );
    assert!(
        prompt.contains("alice/my-cool-feature"),
        "spawn prompt must target the adopted branch: {prompt}"
    );

    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")),
        "adopted stage-2 reroll must never fabricate a replacement branch: {vcs_calls:?}"
    );
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.contains("force") && !c.contains("rebase")),
        "adopted stage-2 reroll must never force-push or rebase: {vcs_calls:?}"
    );

    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().all(|c| !c.contains("close")),
        "original PR must never be closed by adopted stage-2 remediation: {tracker_calls:?}"
    );
    assert!(
        tracker_calls.iter().any(|c| {
            c.contains("comment_external(owner/repo#706") && c.contains("remediation coder session")
        }),
        "adopted stage-2 success should post a status comment: {tracker_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// bead jleechan-tfs1, requirement (d), full pipeline (Stage 2): when the
/// remediation coder session cannot be spawned, the bead must be parked
/// `HUMAN_HELD` with an escalation comment actually posted on the PR (via
/// `tick::post_scm_comment_by_bead_id` -> `Tracker::comment_external`) —
/// not a silent failure.
#[test]
fn adopted_red_pr_stage2_reroll_spawn_failure_parks_human_held_with_escalation() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 707,
        title: "Adopted red PR (stage 2 conflict)".into(),
        body: "already has a non-green branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#707".into(),
        head_ref_name: "alice/my-conflicted-feature".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    let mut snapshot = qdw_green_snapshot(
        707,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    snapshot.ci_success = false;
    snapshot.ci_status = "failure".into();
    scm.pr_snapshots.insert(707, snapshot);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2;
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("alice/my-conflicted-feature".into(), "pre-session-sha-abc123".into());
    sessions.fail_spawn_for("fake-bead-1");
    let telemetry_log = std::env::temp_dir().join("afd_adopted_stage2_conflict.jsonl");
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
    .expect("adopted red PR stage2 spawn failure should park, not error");

    assert_eq!(summary.beads_parked_human_held, 1);

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.pr_number,
        Some(707),
        "PR number must be left in place even on a failed remediation attempt"
    );
    assert_eq!(
        overlay.branch.as_deref(),
        Some("alice/my-conflicted-feature")
    );

    let session_calls = sessions.calls.borrow();
    assert!(
        session_calls.iter().any(|c| c == "spawn(fake-bead-1)"),
        "must have attempted the remediation-session spawn before parking: {session_calls:?}"
    );

    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")),
        "a failed append-only push must never fall back to fabricating a branch: {vcs_calls:?}"
    );
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.contains("force") && !c.contains("rebase")),
        "a failed append-only push must never force-push or rebase as a fallback: {vcs_calls:?}"
    );

    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().all(|c| !c.contains("close")),
        "original PR must never be closed even when remediation fails: {tracker_calls:?}"
    );
    assert!(
        tracker_calls.iter().any(|c| {
            c.contains("comment_external(owner/repo#707")
                && (c.contains("human held") || c.contains("re-roll held"))
        }),
        "a failed append-only push must post an escalation comment on the PR, not fail silently: {tracker_calls:?}"
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

/// jleechan-drive-pr-branch-binding-pcpr red-proof: a manually-created
/// "drive an existing PR" bead (`external_ref` names a currently-OPEN PR in
/// the daemon's configured repo) must dispatch onto that PR's own head
/// branch, not the generated `factory/<bead>-r1` one. Live incident
/// 2026-07-17: beads `jleechan-af-drive-pr288-gd2x` / `...pr289-inoy` parked
/// `session_branch_mismatch` because AO correctly reused the session
/// already bound to the PR's real branch while dispatch had requested a
/// different, freshly fabricated branch.
#[test]
fn drive_existing_pr_bead_dispatches_onto_pr_head_branch_not_generated_branch() {
    let mut scm = FakeScm::new();
    scm.open_pr_head_refs.insert(
        ("owner/repo".to_string(), 288),
        PrHeadBranch::SameRepo("factory/jleechan-xa99-reconciliation-rebased".to_string()),
    );
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "jleechan-af-drive-pr288-gd2x".into(),
        title: "drive PR #288".into(),
        description: "existing_pr: 288".into(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#288".into()),
    });
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"drive existing PR"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_drive_pr_branch_binding_{}.jsonl",
        std::process::id()
    ));
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

    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");
    assert_eq!(summary.beads_dispatched, 1, "the drive-PR bead must dispatch");

    let final_overlay = store
        .load("jleechan-af-drive-pr288-gd2x")
        .unwrap()
        .expect("overlay must exist");
    // `test_cfg()` runs the fast tier in the same `run_tick(0, 0)` call
    // (fast_tick_secs == slow_tick_secs), and setting `pr_number` at
    // dispatch time (below) makes this bead eligible for further same-tick
    // gate-assessment progress — so the state may already have moved past
    // `Dispatched`. The branch/adoption/pr_number bindings this test is
    // actually proving are stable across that further progress; assert
    // those directly instead of pinning the exact downstream state.
    assert_eq!(
        final_overlay.branch.as_deref(),
        Some("factory/jleechan-xa99-reconciliation-rebased"),
        "must bind to the PR's own head branch, not factory/jleechan-af-drive-pr288-gd2x-r1"
    );
    assert!(
        final_overlay.is_adopted,
        "drive-PR dispatch must mark is_adopted so a later reroll takes the \
         append-only remediation path instead of fabricating a replacement \
         branch and closing PR #288"
    );
    assert_eq!(final_overlay.pr_number, Some(288));

    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let dispatched = events
        .iter()
        .find(|e| e["eventType"] == "TASK_DISPATCHED")
        .expect("TASK_DISPATCHED event must be emitted");
    assert_eq!(
        dispatched["context"]["branch"], "factory/jleechan-xa99-reconciliation-rebased"
    );
    assert_eq!(
        dispatched["context"]["branch_mode"], "pr_head",
        "telemetry must record which branch-binding mode fired: {dispatched:#?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Complementary case: the SAME `external_ref` shape (`owner/repo#N`), but
/// the PR is closed/missing — `FakeScm` has no scripted entry for it, so
/// `open_pr_head_ref_for_repo` returns `Ok(PrHeadBranch::NotFound)` (the
/// fail-safe default). Dispatch must fall back to the ordinary
/// generated-branch path exactly as before this bead, not treat every
/// `external_ref` as a drive-PR bead.
#[test]
fn bead_with_external_ref_but_no_open_pr_falls_back_to_generated_branch() {
    let scm = FakeScm::new(); // no open PRs scripted at all
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "bead-closed-pr-ref".into(),
        title: "issue-tracked bead".into(),
        description: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#999".into()),
    });
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"ordinary work"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_no_open_pr_generated_branch_{}.jsonl",
        std::process::id()
    ));
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

    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");
    assert_eq!(summary.beads_dispatched, 1);

    let final_overlay = store.load("bead-closed-pr-ref").unwrap().unwrap();
    assert_eq!(
        final_overlay.branch.as_deref(),
        Some("factory/bead-closed-pr-ref-r1"),
        "no confirmed-open PR means the generated-branch path must fire, unchanged"
    );
    assert!(!final_overlay.is_adopted);

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Codex cross-model review of PR #305 red-proof: an OPEN PR whose head
/// lives on a FORK must NEVER be bound to by branch name — the base repo
/// has no such branch, so binding would create an unrelated same-named
/// branch there and silently never update the actual PR being driven.
/// `FakeScm` scripts `PrHeadBranch::Fork` for PR #501, simulating a real
/// `gh api repos/owner/repo/pulls/501` response whose `head.repo.full_name`
/// is a fork (or a deleted fork, where GitHub omits `head.repo` entirely).
#[test]
fn drive_pr_bead_with_fork_head_falls_back_to_generated_branch_not_fork_head() {
    let mut scm = FakeScm::new();
    scm.open_pr_head_refs
        .insert(("owner/repo".to_string(), 501), PrHeadBranch::Fork);
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "jleechan-fork-pr-bead-e2e".into(),
        title: "drive PR whose head is on a fork".into(),
        description: "existing_pr: 501".into(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#501".into()),
    });
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"drive existing PR"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_fork_pr_generated_fallback_{}.jsonl",
        std::process::id()
    ));
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

    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");
    assert_eq!(summary.beads_dispatched, 1);

    let final_overlay = store.load("jleechan-fork-pr-bead-e2e").unwrap().unwrap();
    assert_eq!(
        final_overlay.branch.as_deref(),
        Some("factory/jleechan-fork-pr-bead-e2e-r1"),
        "a fork PR's head branch name must NEVER be bound to — must fall back to the          generated branch, exactly like an ordinary create-new-work bead"
    );
    assert!(
        !final_overlay.is_adopted,
        "fork fallback must not take the append-only adopted-remediation path —          it never actually bound to the PR's own branch"
    );

    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let dispatched = events
        .iter()
        .find(|e| e["eventType"] == "TASK_DISPATCHED")
        .expect("TASK_DISPATCHED event must be emitted");
    assert_eq!(
        dispatched["context"]["branch_mode"], "generated_fork_fallback",
        "telemetry must distinguish a fork-blocked drive-PR bead from an ordinary          generated dispatch: {dispatched:#?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn remote_credentials_never_reach_tick_telemetry_or_escalation_comments() {
    const SECRET: &str = "SYNTHETIC_REMOTE_CREDENTIAL_SENTINEL";
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "credential-redaction-bead".into(),
        title: "Verify remote credential redaction".into(),
        description: "synthetic integration fixture".into(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#291".into()),
    });
    let sessions = FakeSessions::new();
    sessions.set_worktree_remote(&format!(
        "https://user:{SECRET}@github.com/wrong-owner/wrong-repo.git"
    ));
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log =
        std::env::temp_dir().join(format!("afd_remote_redaction_{}.jsonl", std::process::id()));
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
    .expect("remote mismatch should park safely without failing the tick");

    assert_eq!(summary.beads_dispatched, 0);
    assert_eq!(summary.beads_parked_human_held, 1);
    assert_eq!(summary.beads_escalated, 1);

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(telemetry.contains("<redacted-git-remote>"));
    assert!(!telemetry.contains(SECRET));

    let tracker_calls = tracker.calls.borrow();
    let comment = tracker_calls
        .iter()
        .find(|call| call.starts_with("comment_external(owner/repo#291,"))
        .expect("remote mismatch must post an escalation comment");
    assert!(comment.contains("<redacted-git-remote>"));
    assert!(!comment.contains(SECRET));

    let overlay = store.load("credential-redaction-bead").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("worktree_remote_mismatch")
    );
    assert!(sessions
        .calls
        .borrow()
        .iter()
        .any(|call| call == "stop(fake-session-1)"));

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-3wh0: file:line-cited regression guard for the *actual* root
/// cause of the 15-orphan-bead defect. This is not a bug in `create_bead`
/// (that trait method has always required a non-optional `external_ref: &str`
/// — see `every_create_bead_call_across_both_intake_paths_carries_nonempty_external_ref`
/// in `intake.rs` for that coverage). The orphans instead come through this
/// "manual bead adoption" branch of `run_tick` (tick.rs, the block that logs
/// `INTAKE_BEAD_CREATED` with `serde_json::json!({"manual": true})`): when a
/// `factory`-labeled bead already exists in `br` (created directly by an
/// operator/agent, bypassing the daemon entirely) but has no local
/// `BeadOverlay` row yet, the daemon adopts it into its own tracking state
/// WITHOUT ever calling `Tracker::create_bead`.
///
/// That is legitimate, intentional behavior (`Bead.external_ref: Option<...>`
/// is documented "None = manual bead" in `tools.rs`) — the defect is that
/// this path was undocumented/untested against regression, so a future
/// refactor could silently start (a) calling `create_bead` here with an
/// empty ref, which would just create MORE orphans, or (b) fabricating a
/// synthetic external_ref for a bead the daemon never actually verified
/// against GitHub. This test locks in both invariants: adopting a manual
/// bead must never invoke `create_bead`, and must never mutate the bead's
/// external_ref away from what the tracker reported.
#[test]
fn manual_bead_adoption_never_calls_create_bead_or_fabricates_external_ref() {
    let scm = FakeScm::new(); // no issues/PRs in SCM -- this bead did not come through GitHub intake
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(daemon::tools::Bead {
        id: "manual-bead-999".into(),
        title: "Orphan-shaped manual bead".into(),
        description: "created directly via `br create`, no --external-ref".into(),
        file_tree_summary: "".into(),
        external_ref: None, // exactly the jleechan-3wh0 orphan shape
    });

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();

    let telemetry_log = std::env::temp_dir().join("afd_manual_bead_external_ref_test.jsonl");
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

    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");
    assert_eq!(
        summary.beads_created, 1,
        "manual bead should get a local overlay row"
    );

    let calls = tracker.calls.borrow();
    assert!(
        calls.iter().all(|c| !c.starts_with("create_bead(")),
        "manual-bead adoption must NEVER call create_bead — it adopts a bead \
         that already exists in `br`, it does not create a new one. A \
         create_bead( call here means this path started fabricating \
         beads/refs, which is the jleechan-3wh0 orphan-defect shape: {calls:?}"
    );

    // The tracker's own candidate record (what a real `br list` would report)
    // must remain untouched -- nothing in the tick loop should have called
    // `br update --external-ref` to synthesize a fake linkage for a bead the
    // daemon never independently verified against GitHub.
    let candidate = tracker
        .candidates
        .borrow()
        .iter()
        .find(|b| b.id == "manual-bead-999")
        .cloned()
        .expect("candidate should still be present");
    assert_eq!(
        candidate.external_ref, None,
        "manual bead's external_ref must not be silently fabricated by tick processing"
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
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].0, "fake-bead-1");
    // jleechan-if09 (PR #247) + jleechan-bqdv Stage C: the default dispatch
    // arm renders through `build_coder_prompt`, the enriched coder contract
    // (title + description + repo/remote/branch/push-command instructions),
    // not the bare bead title. This test's original intent — the REAL
    // tracker title reaches the coder, not an empty stub — is preserved as a
    // containment check, plus the tracker-supplied description.
    assert!(
        prompts[0].1.contains("Wire a durable Linux trigger (owner/repo)"),
        "new intake must dispatch the real tracker title, not an empty stub prompt: {}",
        prompts[0].1
    );
    assert!(
        prompts[0].1.contains("systemd user unit acceptance criteria"),
        "tracker-supplied description must reach the coder prompt: {}",
        prompts[0].1
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
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
            head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
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
            head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: Some("transient_spawn_retry_cap_exceeded".into()),
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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

/// jleechan-sniw re-review gap #2: the generic HUMAN_HELD cap/escalation
/// coverage (`recover_human_held_does_not_touch_bead_at_or_above_max_attempt`
/// above) pre-dates this PR and only proves the cap mechanism in isolation by
/// directly seeding an overlay at `attempt=10`/`attempt=11`. It never proves
/// that a bead which arrived via the *adopted-PR intake overlay* (this PR's
/// `run_slow_tier` PR-adoption path, `daemon/src/tick.rs` ~line 597-636) can
/// actually walk itself up to the recovery cap and escalate through repeated
/// real ticks. This test adopts a factory-labeled PR whose PR snapshot is
/// permanently non-green (CI never turns green), drives `run_tick` for
/// enough ticks to organically cycle
/// HUMAN_HELD -> (recovery) QUEUED -> (re-adoption) ATTESTED -> (gate
/// assessment, still red) HUMAN_HELD until `attempt` reaches
/// `MAX_HUMAN_HELD_RECOVERY_ATTEMPT`, and asserts: (a) recovery stops at the
/// cap, (b) a real escalation comment is posted via
/// `post_scm_comment_by_bead_id`, and (c) `escalation_already_recorded`
/// dedup logic (the `ESCALATION_SENTINEL_ATTEMPT` rejection-table row)
/// prevents a duplicate escalation comment on a later tick past the cap.
#[test]
fn adopted_pr_that_never_goes_green_escalates_at_recovery_cap_and_dedups() {
    const PR_NUMBER: u64 = 909;
    const BRANCH: &str = "factory/never-green-r1";
    const BEAD_ID: &str = "fake-bead-1";

    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: PR_NUMBER,
        title: "Adopted PR that never goes green".into(),
        body: "CI stays red across every tick".into(),
        author_login: "alice".into(),
        external_ref: format!("owner/repo#{PR_NUMBER}"),
        head_ref_name: BRANCH.into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    let mut snapshot = qdw_green_snapshot(
        PR_NUMBER,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    snapshot.ci_success = false;
    snapshot.ci_status = "failure".into();
    scm.pr_snapshots.insert(PR_NUMBER, snapshot);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_adopted_pr_never_green_cap.jsonl");
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

    // Tick 0: adopt the labeled PR (real intake path — creates the bead,
    // registers the branch) and, in the same tick, run gate assessment
    // against the permanently-red PR snapshot. Stage-1 substitution rule
    // parks it HUMAN_HELD immediately at attempt=1 (first-ever overlay
    // starts at attempt=1, not 0 — matches `adopted_non_green_pr_parks_
    // human_held_with_v1_escalation`).
    let summary0 = run_tick(&deps, 0, 0).expect("tick 0 (adoption + park) should succeed");
    assert_eq!(
        summary0.beads_created, 1,
        "PR must be adopted as a new bead"
    );
    assert_eq!(summary0.gates_assessed, 1);
    assert_eq!(summary0.beads_parked_human_held, 1);
    assert_eq!(summary0.beads_escalated, 0);
    let overlay0 = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(overlay0.state, OverlayState::HumanHeld);
    assert_eq!(overlay0.attempt, 1);
    assert_eq!(overlay0.pr_number, Some(PR_NUMBER));
    assert_eq!(overlay0.branch.as_deref(), Some(BRANCH));

    // Ticks 1..=9: each tick's `run_recovery_step` requeues the still-
    // HUMAN_HELD bead (attempt < 10), the same tick's re-adoption pass
    // (`run_slow_tier`) immediately re-attests it against the still-open PR,
    // and `run_fast_tier`'s gate assessment sees the same permanently-red
    // snapshot and re-parks it HUMAN_HELD — all organically, no manual state
    // mutation. After this loop `attempt` must equal 10 (the cap).
    for tick_index in 1..=9u64 {
        let summary = run_tick(&deps, tick_index, 0)
            .unwrap_or_else(|e| panic!("tick {tick_index} should succeed: {e:?}"));
        assert_eq!(
            summary.beads_recovered_from_held, 1,
            "tick {tick_index}: bead below the cap must be recovered from HUMAN_HELD"
        );
        assert_eq!(
            summary.beads_escalated, 0,
            "tick {tick_index}: bead below the cap must not escalate yet"
        );
        let overlay = store.load(BEAD_ID).unwrap().unwrap();
        assert_eq!(
            overlay.state,
            OverlayState::HumanHeld,
            "tick {tick_index}: bead must re-park HUMAN_HELD after re-adoption + failed gate assessment"
        );
        assert_eq!(
            overlay.attempt,
            (tick_index as u32) + 1,
            "tick {tick_index}: attempt must advance by exactly one recovery cycle"
        );
    }
    let at_cap = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(
        at_cap.attempt, 10,
        "bead must have reached the recovery cap"
    );
    assert_eq!(at_cap.state, OverlayState::HumanHeld);

    // Tick 10: attempt (10) is no longer `< MAX_HUMAN_HELD_RECOVERY_ATTEMPT`
    // (10), so recovery MUST stop retrying and instead escalate: a real
    // escalation comment is posted through `post_scm_comment_by_bead_id`,
    // and the escalation sentinel row is recorded.
    let summary_cap = run_tick(&deps, 10, 0).expect("cap tick should succeed");
    assert_eq!(
        summary_cap.beads_recovered_from_held, 0,
        "bead at the cap must NOT be recovered again"
    );
    assert_eq!(
        summary_cap.beads_escalated, 1,
        "bead at the cap must be escalated exactly once"
    );
    let capped_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(
        capped_overlay.state,
        OverlayState::HumanHeld,
        "escalation must leave the bead parked HUMAN_HELD for human review"
    );
    assert_eq!(capped_overlay.attempt, 10);

    let escalation_comment_count = |tracker: &FakeTracker| {
        tracker
            .calls
            .borrow()
            .iter()
            .filter(|call| {
                call.contains(&format!("comment_external(owner/repo#{PR_NUMBER}"))
                    && call.contains("Escalation required")
                    && call.contains(&format!("bead `{BEAD_ID}` is HUMAN_HELD at attempt 10"))
                    && call.contains("max automated recovery attempts: 10")
            })
            .count()
    };
    assert_eq!(
        escalation_comment_count(&tracker),
        1,
        "a real escalation comment must be posted via post_scm_comment_by_bead_id: {:?}",
        tracker.calls.borrow()
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("ESCALATION_REQUIRED")
            && log.contains("human_held_recovery_attempt_cap_reached"),
        "cap escalation telemetry must be emitted; got: {log}"
    );

    // Tick 11: dedup check. The bead is still HUMAN_HELD at attempt 10, so
    // `run_recovery_step` finds it again via `human_held_at_or_above_attempt`,
    // but `escalation_already_recorded` (the `ESCALATION_SENTINEL_ATTEMPT`
    // rejection-table row written by tick 10's `record_escalation`) must
    // suppress a second escalation comment.
    let summary_dedup = run_tick(&deps, 11, 0).expect("dedup tick should succeed");
    assert_eq!(
        summary_dedup.beads_escalated, 0,
        "an already-escalated capped bead must not escalate again"
    );
    assert_eq!(summary_dedup.beads_recovered_from_held, 0);
    assert_eq!(
        escalation_comment_count(&tracker),
        1,
        "dedup must prevent a second escalation comment on a later tick past the cap: {:?}",
        tracker.calls.borrow()
    );
    let final_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(final_overlay.state, OverlayState::HumanHeld);
    assert_eq!(final_overlay.attempt, 10);

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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
    assert_eq!(
        comment_count, 2,
        "second tick must retry the failed comment"
    );

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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
fn capped_human_held_missing_comment_target_records_local_escalation_fallback() {
    // jleechan-baaf-followup / 2026-07-09 live incident: 45 beads were found
    // stuck forever with `ESCALATION_NOTIFICATION_FAILED` /
    // `"config: no SCM comment target found for bead <id>"` and ZERO durable
    // trace anywhere else — no GitHub comment, no sentinel row, no operator-
    // visible marker. Every subsequent tick re-attempted and re-failed
    // identically because `record_escalation` (the sentinel write that makes
    // `escalation_already_recorded` return `true`) was only ever called on
    // the success path. A bead with no `pr_number` and no matching
    // `fetch_candidates()` entry can NEVER get an SCM target — retrying is
    // pure waste, and silence means an operator has to know to grep
    // `daemon.jsonl` for a needle they don't know exists.
    //
    // This test asserts the fallback: when no SCM comment target exists at
    // all (as opposed to a transient tracker/API error — see the sibling
    // `_retries_before_recording_escalation` tests above, which must keep
    // retrying), the daemon must still leave a durable, human-visible record
    // — a `park_reason` on the bead's own `bead_overlay` row, plus a
    // distinct `ESCALATED_LOCALLY` telemetry event — and must record the
    // escalation sentinel so the tick loop stops silently re-attempting
    // forever.
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );
    // No candidates registered on the tracker either — this bead's source
    // issue/PR has fallen out of the live query window entirely, exactly
    // like the 45 orphaned beads found live.

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
    assert_eq!(
        summary.beads_escalated_locally, 1,
        "a local-only fallback escalation must be counted distinctly from an SCM-posted one"
    );
    assert!(
        store
            .load_rejection("bead-held-missing-target", u32::MAX)
            .unwrap()
            .is_some(),
        "sentinel must be recorded even without an SCM target, or the daemon retries forever"
    );
    let overlay_after = store.load("bead-held-missing-target").unwrap().unwrap();
    assert!(
        overlay_after
            .park_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("escalation")),
        "bead_overlay.park_reason must carry a durable, queryable escalation marker; got: {:?}",
        overlay_after.park_reason
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("ESCALATED_LOCALLY"),
        "a distinct local-escalation telemetry event must be emitted; got: {log}"
    );

    // Second tick: no infinite retry storm. The sentinel from tick 1 must
    // suppress a second local-escalation attempt.
    let summary2 = run_tick(&deps, 2, 0).expect("second tick should not re-escalate");
    assert_eq!(
        summary2.beads_escalated_locally, 0,
        "an already-recorded local escalation must not fire again every tick"
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
    store.er_attempts.borrow_mut().insert(
        "er-capped-unknown".into(),
        (er_runner::MAX_ER_RUNNER_ATTEMPTS, Some(1)),
    );

    scm.pr_snapshots.insert(
        9101,
        PrSnapshot {
            pr_number: 9101,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
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
            head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
    store.er_attempts.borrow_mut().insert(
        "er-capped-retry".into(),
        (er_runner::MAX_ER_RUNNER_ATTEMPTS, Some(1)),
    );
    *tracker.fail_next_comment.borrow_mut() = Some("transient comment failure".into());

    scm.pr_snapshots
        .insert(9102, qdw_green_snapshot(9102, Vec::new()));

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
    assert_eq!(
        comment_count, 2,
        "second tick must retry the failed comment"
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
            head_sha: "abc".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch,
            ci_status: "unknown".into(),
            coderabbit_status: "approved".into(),
            ci_pending: true,
            head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
            head_sha: "ghi".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch,
            ci_status: "unknown".into(),
            coderabbit_status: "approved".into(),
            ci_pending: true,
            head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: Some(
                "gate assessment not all-green (stage 1: recorded, not executed)".into(),
            ),
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
            head_sha: "def".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: now_epoch,
            ci_status: "success".into(),
            coderabbit_status: "approved".into(),
            ci_pending: false,
            head_committed_epoch: 0,
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
                is_adopted: false,
                spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef".into(),
            body: String::new(),
            comments: vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
            files: Vec::new(),
            // Fresh epoch so the wedge-detection check (>=30 min stale)
            // does not park bead 102 — the qdw fix targets per-bead
            // isolation, not the wedge heuristic.
            updated_at_epoch: fresh_epoch,
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
            head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            unresolved_thread_count: Some(0),
            head_sha: "cafebabe".into(),
            body: String::new(),
            comments: vec![PrComment {
                author: "dark-factory-er".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
            files: Vec::new(),
            updated_at_epoch: fresh_epoch,
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
            head_committed_epoch: 0,
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

    fn labeled_prs(&self, _label: &str) -> Result<Vec<LabeledPr>, DaemonError> {
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

    fn labeled_prs(&self, _label: &str) -> Result<Vec<LabeledPr>, DaemonError> {
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
        unresolved_thread_count: Some(0),
        head_sha: format!("sha-{pr}"),
        body: String::new(),
        comments,
        files: Vec::new(),
        updated_at_epoch: fresh_epoch,
        ci_status: "green".into(),
        coderabbit_status: "green".into(),
        ci_pending: false,
        head_committed_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
                created_at_epoch: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
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
                created_at_epoch: 0,
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

// --- PR#163 finding 4: exercise the REAL (non-"owner/repo") skeptic_evidence
// combination path through `run_tick`. Every other test in this file uses
// `target_repo: "owner/repo"`, which is exactly `skeptic_evidence`'s
// `is_test_repo` bypass — structurally, nothing in this suite could catch a
// regression in the real 3-subsystem combine, which is exactly how finding
// 1's sign-off deadlock shipped undetected. `codex`/`claude` are real
// subprocess CLIs installed on dev machines' PATH; this test shadows them
// with fast, deterministic fake scripts (prepended to PATH) so it stays fast
// and hermetic instead of ever invoking a real reviewer tool.

/// Restores a fixed set of env vars to their pre-test values on drop, even on
/// panic. Used to isolate `PATH` (and the coder-vendor env vars) for the
/// single test below that must exercise `skeptic_evidence`'s real (non-test)
/// dispatch path without touching genuinely-installed `codex`/`claude` CLIs.
struct EnvVarGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvVarGuard {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((*k, std::env::var(k).ok()));
            // SAFETY: serialized by REAL_TARGET_REPO_TEST_LOCK below — this
            // is the only test in the binary that mutates these keys (every
            // other test drives all five tool-boundary traits through fakes
            // and never reaches real subprocess dispatch), so there is no
            // concurrent reader/writer of PATH or the coder-vendor env vars.
            unsafe { std::env::set_var(k, v) };
        }
        Self { saved }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                // SAFETY: see `set` above.
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

/// Guards every test in this file that needs to mutate process-wide env vars
/// (`PATH`, `DARK_FACTORY_CODER_DEFAULT`) so a future second such test can't
/// race this one.
static REAL_TARGET_REPO_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write a fake reviewer script named `name` into `dir` that ignores its
/// arguments, prints `reply` to stdout, and exits 0 — a fast, deterministic
/// stand-in for the real `codex`/`claude` CLIs `skeptic_evidence` shells out
/// to on a real (non-`is_test_repo`) target repo.
#[cfg(unix)]
fn write_fake_reviewer(dir: &std::path::Path, name: &str, reply: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{reply}'\n"))
        .unwrap_or_else(|e| panic!("failed to write fake reviewer {name}: {e}"));
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

#[test]
#[cfg(unix)]
fn real_target_repo_skeptic_gate_resolves_from_dual_llm_without_gha_or_signoff() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_reviewers_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    write_fake_reviewer(&fake_bin_dir, "codex", "pass");
    write_fake_reviewer(&fake_bin_dir, "claude", "pass");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // Fix the coder vendor so the reviewer priority list (and therefore
    // which two fake binaries get dispatched) is deterministic regardless
    // of the ambient environment.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "minimax"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/myrepo".into(); // NOT "owner/repo" -> is_test_repo == false

    store.overlays.borrow_mut().insert(
        "real-repo-bead".into(),
        BeadOverlay {
            bead_id: "real-repo-bead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(555),
            branch: Some("factory/real-repo-bead-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/real-repo-bead-r1".into());
    store
        .branch_beads
        .borrow_mut()
        .insert("factory/real-repo-bead-r1".into(), "real-repo-bead".into());

    scm.pr_snapshots.insert(
        555,
        PrSnapshot {
            pr_number: 555,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef555".into(),
            body: String::new(),
            // Deliberately NO github-actions/skeptic comment and NO human
            // sign-off comment — the exact scenario that permanently
            // deadlocked gate 7 pre-fix (finding 1). An `/er PASS` comment
            // is present purely so gate 6 resolves and `er_runner::maybe_run`
            // is a no-op (`AlreadyPosted`) — this test only exercises gate 7.
            comments: vec![PrComment {
                author: "some-reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_real_target_repo_skeptic_{}.jsonl",
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
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    // Mutation-test sanity (manually verified, not automated): finding 1 is
    // fixed in two layers — (a) `tick.rs::skeptic_evidence` only wraps the
    // dual-reviewer verdict in the `subsystem:` grammar when real gha/
    // sign-off evidence exists, and (b) `verifier::parse_skeptic_verdict`
    // treats `sign-off` as optional even when the grammar IS used. Because
    // this test's scenario has no gha/sign-off evidence at all, layer (a)
    // alone is enough to resolve gate 7 — reverting ONLY layer (b) does not
    // fail this test. Reverting layer (a) as well (always wrapping in the
    // subsystem grammar, restoring the exact pre-fix combine) makes gate 7
    // resolve to `Unknown`: `run_fast_tier` takes its "Unknown-only" branch
    // and emits `GATE_ASSESSMENT_TRANSIENT_UNKNOWN` instead of promoting the
    // bead to READY, so `beads_ready` drops to 0 and `overlay.state` stays
    // `Attested`. Confirmed by temporarily reverting both layers together
    // (2026-07-07): this test fails as described; restoring both layers
    // makes it pass again.
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        summary.beads_ready, 1,
        "PR#163 finding 1 regression: with two independent reviewer \
         subprocesses both returning 'pass' and NO gha/sign-off comment, \
         gate 7 must resolve to Green and the bead must reach READY. \
         summary={summary:?}\ntelemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"all_green\":true"),
        "GATE_ASSESSMENT must report all_green:true; telemetry:\n{telemetry}"
    );
    assert!(
        !telemetry.contains("GATE_ASSESSMENT_TRANSIENT_UNKNOWN"),
        "must not fall into the Unknown-only branch when gate 7 correctly \
         resolves from the dual-LLM verdict alone; telemetry:\n{telemetry}"
    );

    let overlay = store
        .load("real-repo-bead")
        .unwrap()
        .expect("overlay must still exist");
    assert_eq!(overlay.state, OverlayState::Ready);

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// --- PR#163 finding (round 3): residual asymmetric deadlock -------------
//
// The test above proves gate 7 resolves when NEITHER gha nor sign-off has
// evidence. It structurally cannot catch the round-3 residual: `tick.rs`'s
// `!has_gha_evidence && !has_signoff_evidence` bypass only fires when BOTH
// are absent. The moment a PR comment trips the (deliberately loose)
// sign-off heuristic — any non-bot comment containing "sign-off",
// "signoff", "verdict: pass", or "/skeptic pass" — `skeptic_evidence`
// unconditionally wraps the dual-LLM verdict in the full 3-subsystem
// grammar, padding the still-absent `gha` subsystem with the literal
// `"verdict: absent"` placeholder. Before the round-3 fix,
// `verifier::parse_skeptic_verdict` still hard-required `gha`
// (`gha_verdict?`), so this asymmetric case — gha absent, sign-off
// present, dual-LLM real evidence — silently re-deadlocked gate 7 to
// `Unknown` even though both independent LLM reviewers passed. This test
// drives that exact scenario end-to-end through `run_tick` against a real
// (non-`owner/repo`) target repo and asserts the bead reaches READY.
#[test]
#[cfg(unix)]
fn real_target_repo_skeptic_gate_resolves_from_dual_llm_with_signoff_but_no_gha() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_reviewers_asym_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    write_fake_reviewer(&fake_bin_dir, "codex", "pass");
    write_fake_reviewer(&fake_bin_dir, "claude", "pass");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // Fix the coder vendor so the reviewer priority list (and therefore
    // which two fake binaries get dispatched) is deterministic regardless
    // of the ambient environment.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "minimax"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/myrepo".into(); // NOT "owner/repo" -> is_test_repo == false

    store.overlays.borrow_mut().insert(
        "real-repo-bead-asym".into(),
        BeadOverlay {
            bead_id: "real-repo-bead-asym".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(556),
            branch: Some("factory/real-repo-bead-asym-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/real-repo-bead-asym-r1".into());
    store.branch_beads.borrow_mut().insert(
        "factory/real-repo-bead-asym-r1".into(),
        "real-repo-bead-asym".into(),
    );

    scm.pr_snapshots.insert(
        556,
        PrSnapshot {
            pr_number: 556,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef556".into(),
            body: String::new(),
            // No gha/skeptic-workflow comment at all (this target repo has
            // no equivalent CI workflow), but ONE human-looking comment
            // trips the loose sign-off heuristic ("sign-off" substring,
            // non-bot author) — the exact asymmetric scenario round 3
            // proved was still deadlocked. An `/er PASS` comment is present
            // purely so gate 6 resolves; this test only exercises gate 7.
            comments: vec![
                PrComment {
                    author: "some-reviewer".into(),
                    body: "/er PASS".into(),
                    created_at_epoch: 0,
                },
                PrComment {
                    author: "jleechan".into(),
                    body: "Looks good, sign-off from me.".into(),
                    created_at_epoch: 0,
                },
            ],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_real_target_repo_skeptic_asym_{}.jsonl",
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
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    // Mutation-test sanity (manually verified 2026-07-07): reverting ONLY
    // `verifier::parse_skeptic_verdict`'s `gha` optionality (restoring
    // `gha_verdict?` as a hard requirement) makes this test fail —
    // `beads_ready` drops to 0, `all_green` is false, and telemetry emits
    // `GATE_ASSESSMENT_TRANSIENT_UNKNOWN`, reproducing the round-3
    // deadlock exactly. Restoring the fix makes it pass again. Unlike the
    // "without_gha_or_signoff" test above, `tick.rs`'s bypass branch does
    // NOT fire here (sign-off evidence IS present), so this test exercises
    // the full 3-subsystem grammar combine path in `parse_skeptic_verdict`,
    // not just the early-return bypass.
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        summary.beads_ready, 1,
        "PR#163 finding (round 3) regression: with two independent \
         reviewer subprocesses both returning 'pass', NO gha comment, and \
         a comment that trips the loose sign-off heuristic, gate 7 must \
         still resolve to Green and the bead must reach READY (not \
         deadlock on the absent gha subsystem). summary={summary:?}\n\
         telemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"all_green\":true"),
        "GATE_ASSESSMENT must report all_green:true; telemetry:\n{telemetry}"
    );
    assert!(
        !telemetry.contains("GATE_ASSESSMENT_TRANSIENT_UNKNOWN"),
        "must not fall into the Unknown-only branch when gate 7 correctly \
         resolves from gate-7 + sign-off evidence (gha absent); \
         telemetry:\n{telemetry}"
    );

    let overlay = store
        .load("real-repo-bead-asym")
        .unwrap()
        .expect("overlay must still exist");
    assert_eq!(overlay.state, OverlayState::Ready);

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// --- jleechan-baaf: 3rd-vendor fallback gap -----------------------------
//
// Live-verified 2026-07-09: `skeptic_evidence`'s dual-reviewer dispatch only
// ever tries the first TWO vendors in the coder-exclusion-filtered
// `priority` list (`vendor1 = priority[0]`, `vendor2 = priority[1]`). When
// the coder vendor is `minimax`, `priority = [codex, claude, agy]`, so only
// `codex` and `claude` are ever dispatched — `agy` (`priority[2]`) is never
// attempted even when it is healthy and would have produced a usable
// verdict. Tonight `agy` returns empty stdout and `codex` is
// quota-exhausted, so every bead whose first two reviewers are both broken
// permanently fails gate 7 with "both reviewers failed to produce a
// parseable verdict" even though a third, healthy vendor was available and
// simply never dispatched.
//
// This test drives that exact scenario end-to-end through `run_tick`
// against a real (non-`owner/repo`) target repo: fake `codex` and `claude`
// binaries that both print unparseable garbage (simulating "broken tool" /
// "empty output"), and a fake `agy` binary that prints a valid `pass`
// verdict. Before the fix, `vendor1`/`vendor2` are hardcoded to
// `priority[0]`/`priority[1]` (`codex`/`claude`), so `agy` is never
// dispatched, `combine_dual_verdict(None, None, ..)` returns `Err`, and the
// bead stays `ATTESTED` (never reaches `READY`). After the fix, when both
// dispatched vendors fail to parse, `skeptic_evidence` must fall back to
// the third `priority` member (if any) before giving up.
#[test]
#[cfg(unix)]
fn real_target_repo_skeptic_gate_falls_back_to_third_vendor_when_first_two_fail() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_reviewers_3rdvendor_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    // vendor1 (codex) and vendor2 (claude) both "succeed" as processes but
    // produce output `parse_skeptic_verdict` cannot parse — matching
    // tonight's live incident (agy empty stdout, codex quota-exhausted
    // error text) without depending on nonzero exit codes.
    write_fake_reviewer(&fake_bin_dir, "codex", "not a verdict at all");
    write_fake_reviewer(&fake_bin_dir, "claude", "still not a verdict");
    // vendor3 (agy) is healthy and would have produced a usable verdict —
    // the bug is that it is never dispatched.
    write_fake_reviewer(&fake_bin_dir, "agy", "pass");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // Fix the coder vendor so priority = [codex, claude, agy] deterministically.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "minimax"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/myrepo".into(); // NOT "owner/repo" -> is_test_repo == false

    store.overlays.borrow_mut().insert(
        "real-repo-bead-3rdvendor".into(),
        BeadOverlay {
            bead_id: "real-repo-bead-3rdvendor".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(557),
            branch: Some("factory/real-repo-bead-3rdvendor-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/real-repo-bead-3rdvendor-r1".into());
    store.branch_beads.borrow_mut().insert(
        "factory/real-repo-bead-3rdvendor-r1".into(),
        "real-repo-bead-3rdvendor".into(),
    );

    scm.pr_snapshots.insert(
        557,
        PrSnapshot {
            pr_number: 557,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef557".into(),
            body: String::new(),
            comments: vec![PrComment {
                author: "some-reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_real_target_repo_skeptic_3rdvendor_{}.jsonl",
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
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        summary.beads_ready, 1,
        "jleechan-baaf regression: when the first two dispatched reviewer \
         vendors (codex, claude) both fail to produce a parseable verdict \
         but a third vendor (agy) is available in `priority` and would \
         succeed, `skeptic_evidence` must fall back to it instead of \
         propagating a total-outage Err. summary={summary:?}\n\
         telemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"all_green\":true"),
        "GATE_ASSESSMENT must report all_green:true once the third vendor's \
         verdict is used; telemetry:\n{telemetry}"
    );

    let overlay = store
        .load("real-repo-bead-3rdvendor")
        .unwrap()
        .expect("overlay must still exist");
    assert_eq!(
        overlay.state,
        OverlayState::Ready,
        "bead must reach READY via the third vendor's verdict, not stay \
         ATTESTED on a false total-outage"
    );

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// jleechan-wzgl: GATE_ASSESSMENT must serialize the full per-gate report
// (all 7 gates, verdict + reason) AND the gate-7 reviewer vendor identity,
// not just the aggregate `all_green` boolean. Before this fix, diagnosing
// which of the 7 gates failed for bead jleechan-93ft required a manual
// GitHub REST sweep even though `verifier::assess` already computed the
// per-gate array; and it was impossible to tell from telemetry which
// vendor produced the gate-7 skeptic verdict, which the af-e2e mission's
// "ironclad" exit criterion E3 needs to confirm the reviewer was non-self
// and genuinely ran (not self-certified).
//
// This scenario reuses the third-vendor-fallback fixture above (codex and
// claude both produce unparseable output, agy is the one that actually
// resolves gate 7) specifically because it makes vendor provenance
// observable: the fix must report `agy` as the contributing reviewer, not
// `codex`/`claude` (which were dispatched but never produced a usable
// verdict) and not a placeholder.
#[test]
#[cfg(unix)]
fn gate_assessment_telemetry_reports_full_gate_report_and_skeptic_vendor() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_reviewers_wzgl_gate_report_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    write_fake_reviewer(&fake_bin_dir, "codex", "not a verdict at all");
    write_fake_reviewer(&fake_bin_dir, "claude", "still not a verdict");
    write_fake_reviewer(&fake_bin_dir, "agy", "pass");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "minimax"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/myrepo".into(); // NOT "owner/repo" -> is_test_repo == false

    store.overlays.borrow_mut().insert(
        "wzgl-gate-report-bead".into(),
        BeadOverlay {
            bead_id: "wzgl-gate-report-bead".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(558),
            branch: Some("factory/wzgl-gate-report-bead-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/wzgl-gate-report-bead-r1".into());
    store.branch_beads.borrow_mut().insert(
        "factory/wzgl-gate-report-bead-r1".into(),
        "wzgl-gate-report-bead".into(),
    );

    scm.pr_snapshots.insert(
        558,
        PrSnapshot {
            pr_number: 558,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef558".into(),
            body: String::new(),
            comments: vec![PrComment {
                author: "some-reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_wzgl_gate_report_{}.jsonl",
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
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    assert_eq!(summary.beads_ready, 1, "bead should reach READY via the agy fallback verdict");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let gate_assessment_line = telemetry
        .lines()
        .find(|line| line.contains("\"eventType\":\"GATE_ASSESSMENT\""))
        .unwrap_or_else(|| panic!("no GATE_ASSESSMENT line found; telemetry:\n{telemetry}"));
    let parsed: serde_json::Value = serde_json::from_str(gate_assessment_line)
        .unwrap_or_else(|e| panic!("GATE_ASSESSMENT line is not valid JSON: {e}\nline: {gate_assessment_line}"));
    let context = &parsed["context"];

    // jleechan-wzgl (PR #239 review round 1): `gates` MUST be a
    // `{gate_name: verdict}` OBJECT using the PR #235/jleechan-l4ki
    // canonical 7-gate vocabulary — `daemon/scripts/auto-merge-guard.sh`'s
    // `latest_assessment_no_red` predicate does `for k, v in g.items()` on
    // this exact field and would crash on `list.items()` if it were an
    // array, which is why this is asserted as an object, not a length-7
    // array like round 1 checked.
    let gates = context["gates"]
        .as_object()
        .unwrap_or_else(|| panic!("GATE_ASSESSMENT context.gates must be a {{gate_name: verdict}} object, not an array; context:\n{context}"));
    const CANONICAL_GATE_KEYS: [&str; 7] = [
        "ci_green",
        "no_conflicts",
        "coderabbit",
        "bugbot",
        "comments_resolved",
        "evidence_review",
        "skeptic",
    ];
    assert_eq!(
        gates.len(),
        7,
        "GATE_ASSESSMENT must report all 7 per-gate results, not just all_green; context:\n{context}"
    );
    for key in CANONICAL_GATE_KEYS {
        assert!(
            gates.contains_key(key),
            "GATE_ASSESSMENT gates dict must use the PR #235/jleechan-l4ki \
             canonical vocabulary (daemon/factory-overlay.sh REQUIRED_KEYS); \
             missing key {key:?}; context:\n{context}"
        );
    }

    let skeptic_reviewers = context["skeptic_reviewers"]
        .as_array()
        .unwrap_or_else(|| panic!("GATE_ASSESSMENT context.skeptic_reviewers must be an array; context:\n{context}"));
    let skeptic_reviewers: Vec<&str> = skeptic_reviewers
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        skeptic_reviewers,
        vec!["agy"],
        "GATE_ASSESSMENT must report the gate-7 reviewer vendor that actually \
         produced the verdict (agy, the 3rd-vendor fallback), not the first \
         two dispatched vendors (codex/claude) that failed to parse, and not \
         a placeholder; context:\n{context}"
    );

    // jleechan-wzgl (PR #239 review round 1): `pr_number` must be present
    // in context — without it, `auto-merge-guard.sh`'s
    // `grep -E "\"pr_number\": *$pr[,}]"` never matches this line at all,
    // and the dict-shape/vocabulary fix above stays permanently dormant.
    assert_eq!(
        context["pr_number"].as_u64(),
        Some(558),
        "GATE_ASSESSMENT context must carry pr_number so auto-merge-guard.sh's \
         grep-by-PR-number match path is reachable; context:\n{context}"
    );

    // jleechan-wzgl (PR #239 review round 2, team-lead request): don't just
    // assert our own shape expectations — pipe the ACTUAL emitted line
    // through auto-merge-guard.sh's REAL predicate (the exact python
    // heredoc it runs, extracted the same way
    // tests/scripts/test_auto_merge_guard_gate_vocabulary.sh does) and
    // confirm the match path is genuinely non-dormant: it parses without
    // crashing and reports a non-blocking verdict for this all-green
    // scenario.
    let guard_script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/auto-merge-guard.sh");
    let guard_src = std::fs::read_to_string(&guard_script)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", guard_script.display()));
    let predicate_block: String = guard_src
        .lines()
        .skip(40) // 0-indexed: line 41 (1-indexed) of auto-merge-guard.sh
        .take(29) // lines 41..=69 inclusive, mirroring test_auto_merge_guard_gate_vocabulary.sh's `sed -n '41,69p'`
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        predicate_block.contains("g.items()"),
        "extracted predicate block drifted from auto-merge-guard.sh's actual \
         line range 41-69 (line numbers may have shifted); block:\n{predicate_block}"
    );

    use std::io::Write as _;
    let mut child = std::process::Command::new("python3")
        .arg("-c")
        .arg(&predicate_block)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn python3 for auto-merge-guard.sh predicate");
    child
        .stdin
        .take()
        .expect("child stdin must be piped")
        .write_all(gate_assessment_line.as_bytes())
        .expect("failed to write GATE_ASSESSMENT line to predicate stdin");
    let output = child
        .wait_with_output()
        .expect("python3 predicate failed to run to completion");
    let predicate_stdout = String::from_utf8_lossy(&output.stdout);
    let predicate_stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "auto-merge-guard.sh's real predicate must accept the emitted \
         GATE_ASSESSMENT line (dict-shaped gates, canonical vocab) for this \
         all-green scenario; stdout={predicate_stdout}\nstderr={predicate_stderr}\n\
         line={gate_assessment_line}"
    );
    assert!(
        predicate_stdout.contains("no-fail"),
        "expected a non-blocking 'no-fail' verdict from auto-merge-guard.sh's \
         predicate for this all-green scenario; got: {predicate_stdout}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

/// jleechan-9xrs Stage D end-to-end regression: a bead whose `target_repo`
/// is a "test-repo" placeholder (`owner/repo`), dispatched under a daemon
/// whose GLOBAL `cfg.target_repo` is a DIFFERENT, non-test-pattern repo,
/// must have its ENTIRE verification loop (skeptic gate's `is_test_repo`
/// classification + snapshot fetch, gate assessment's snapshot fetch, and
/// the PARKED_HUMAN_HELD escalation comment) target the bead's OWN repo —
/// never `cfg.target_repo`. See
/// docs/multirepo-dispatch-investigation-2026-07-11.md Stage D.
///
/// This is a strong regression pin: before the Stage D fix, `is_test_repo`
/// was computed from `cfg.target_repo` (here a real-looking, non-test
/// string), so `skeptic_evidence` would have taken the REAL dual-reviewer
/// dispatch branch (spawning `codex`/`claude`/`agy` subprocesses) instead of
/// the mock `FakeLlm` path — this test's `FakeLlm` script would never be
/// consulted, and the test would hang/fail against a `PATH` with no
/// scripted reviewer binaries.
#[test]
fn cross_repo_bead_verification_loop_uses_its_own_repo_not_cfg_target_repo() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();

    let mut cfg = test_cfg();
    // Deliberately NOT "owner/repo" / "fake-*" / "test-*" -- if is_test_repo
    // were (incorrectly) computed from this, skeptic_evidence would try to
    // spawn real reviewer subprocesses.
    cfg.target_repo = "myorg/global-real-repo".into();

    let pr = 4242;
    let bead_id = "bxrs-cross-repo";
    store.overlays.borrow_mut().insert(
        bead_id.into(),
        BeadOverlay {
            bead_id: bead_id.into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some(format!("factory/{bead_id}-r1")),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            // The bead's OWN resolved repo: a test-pattern placeholder,
            // deliberately DIFFERENT from cfg.target_repo above.
            target_repo: Some("owner/repo".into()),
        },
    );
    store
        .branches
        .borrow_mut()
        .push(format!("factory/{bead_id}-r1"));
    store
        .branch_beads
        .borrow_mut()
        .insert(format!("factory/{bead_id}-r1"), bead_id.into());

    // CI is red -> gate assessment is not all-green -> Stage 1 parks
    // HUMAN_HELD and posts an escalation comment via
    // post_scm_comment_by_bead_id.
    let mut snapshot = qdw_green_snapshot(pr, vec![]);
    snapshot.ci_success = false;
    snapshot.ci_status = "failure".into();
    scm.pr_snapshots.insert(pr, snapshot);

    let telemetry_log =
        std::env::temp_dir().join("afd_9xrs_cross_repo_verification_loop.jsonl");
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
    .expect("run_tick should succeed for a cross-repo bead");

    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(summary.beads_parked_human_held, 1);
    let overlay = store.load(bead_id).unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);

    // 1. EVERY snapshot fetch in the whole verification loop (the
    //    active-overlay wedge-detection fetch in `run_tick`, plus
    //    skeptic_evidence + verifier::assess in `run_fast_tier`) must have
    //    gone through `pr_snapshot_for_repo` with the bead's OWN repo --
    //    never the plain (cfg-bound) `pr_snapshot`. This positive assertion
    //    on its own is not sufficient: a stray missed call site that still
    //    calls plain `pr_snapshot(pr)` logs a call string containing
    //    neither "owner/repo" nor "global-real-repo" (FakeScm's plain
    //    `pr_snapshot` call-log format carries no repo string at all), so
    //    it would silently pass both the positive check above and a
    //    "doesn't contain global-real-repo" negative check. The explicit
    //    "zero plain pr_snapshot(...) calls" assertion below closes that
    //    gap.
    let scm_calls = scm.calls.borrow();
    assert!(
        scm_calls
            .iter()
            .any(|c| c == &format!("pr_snapshot_for_repo(owner/repo,{pr})")),
        "expected pr_snapshot_for_repo with the bead's own repo, got: {scm_calls:?}"
    );
    assert!(
        !scm_calls.iter().any(|c| c.contains("global-real-repo")),
        "verification loop must never fall back to cfg.target_repo for a \
         bead with an explicit target_repo, got: {scm_calls:?}"
    );
    assert!(
        !scm_calls.iter().any(|c| c.starts_with("pr_snapshot(")),
        "found a call to the plain (cfg-bound) pr_snapshot -- every \
         verification-loop snapshot fetch must go through \
         pr_snapshot_for_repo instead, got: {scm_calls:?}"
    );

    // 2. The skeptic prompt must embed the bead's own repo, not
    //    cfg.target_repo -- AND `skeptic_evidence` reaching the mock LLM
    //    path AT ALL proves `is_test_repo` was computed from the bead's own
    //    repo, not cfg.target_repo (which matches no test pattern here).
    //
    //    `er_runner::maybe_run` ALSO calls the mock LLM unconditionally
    //    (its dispatch is gated on `Llm::is_real()`, which `FakeLlm`
    //    defaults to `false`, independent of `is_test_repo`) -- so a naive
    //    "at least one judge() call happened" assertion cannot distinguish
    //    the fix from the bug: reverting `is_test_repo` back to
    //    `cfg.target_repo` still produces exactly one judge() call (from
    //    er_runner alone) if the real `codex`/`claude`/`agy` binaries
    //    happen to be on `PATH` and return a parseable verdict, silently
    //    passing this test while `skeptic_evidence` spawned REAL reviewer
    //    subprocesses instead of using the mock. Two independent checks
    //    close that gap: (a) an EXACT call count of 2 (skeptic +
    //    er_runner -- one fewer than expected if skeptic took the real
    //    subprocess branch instead), and (b) inspecting the
    //    skeptic-specific prompt (identified by its unique "Stage-1
    //    Skeptic" marker, distinct from er_runner's "/er (evidence
    //    review)" marker) for the bead's own repo.
    let llm_calls = llm.calls.borrow();
    assert_eq!(
        llm_calls.len(),
        2,
        "expected exactly 2 mock LLM calls (skeptic_evidence + \
         er_runner::maybe_run); a count of 1 means skeptic_evidence took \
         the REAL dual-reviewer subprocess branch instead of the mock \
         path, i.e. is_test_repo was computed from cfg.target_repo (not \
         the bead's own repo). got: {llm_calls:?}"
    );
    let skeptic_prompt = llm_calls
        .iter()
        .find(|c| c.contains("Stage-1 Skeptic"))
        .unwrap_or_else(|| panic!("expected a Stage-1 Skeptic prompt among judge() calls, got: {llm_calls:?}"));
    assert!(
        skeptic_prompt.contains("owner/repo"),
        "skeptic prompt must embed the bead's own repo, got: {skeptic_prompt:?}"
    );
    assert!(
        !skeptic_prompt.contains("global-real-repo"),
        "skeptic prompt must not leak cfg.target_repo, got: {skeptic_prompt:?}"
    );

    // 3. The PARKED_HUMAN_HELD escalation comment's ext_ref must target the
    //    bead's own repo -- this is the twa0/mdgr escalation cross-repo
    //    class Stage D closes.
    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls
            .iter()
            .any(|c| c.starts_with(&format!("comment_external(owner/repo#{pr}"))),
        "escalation comment must target the bead's own repo, got: {tracker_calls:?}"
    );
    assert!(
        !tracker_calls.iter().any(|c| c.contains("global-real-repo")),
        "escalation comment must not leak cfg.target_repo, got: {tracker_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// --- jleechan-bkru: 4th-vendor (gemini) fallback gap --------------------
//
// Live incident 2026-07-09 (bead jleechan-93ft, worldarchitect.ai PR #7888):
// codex, claude, AND agy went simultaneously non-functional (codex quota
// exhausted multi-day, claude weekly limit hit multi-day, agy quota
// exhausted + a separate session-continuity bug even when fresh). With
// `priority = [codex, claude, agy]`, once ALL THREE fail to parse,
// `skeptic_evidence` had zero remaining vendors to fall back to and
// permanently failed gate 7, even though a `gemini` CLI reviewer was live
// and produced a real, parseable verdict in manual testing.
//
// This test adds a 4th vendor (`gemini`) to `priority` and asserts that
// when the first three dispatched/fallback vendors (codex, claude, agy)
// all fail to produce a parseable verdict, `skeptic_evidence` falls back
// to the 4th (`gemini`) before giving up — generalizing the jleechan-baaf
// single `priority[2]` fallback into a loop over all remaining vendors.
#[test]
#[cfg(unix)]
fn bkru_skeptic_gate_falls_back_to_fourth_vendor_when_first_three_fail() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_reviewers_4thvendor_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    // vendor1 (codex), vendor2 (claude), vendor3 (agy) all "succeed" as
    // processes but produce output `parse_skeptic_verdict` cannot parse —
    // matching tonight's live triple-outage.
    write_fake_reviewer(&fake_bin_dir, "codex", "not a verdict at all");
    write_fake_reviewer(&fake_bin_dir, "claude", "still not a verdict");
    write_fake_reviewer(&fake_bin_dir, "agy", "also not a verdict");
    // vendor4 (gemini) is healthy and would have produced a usable verdict
    // — the bug (pre-fix) is that `priority[3]` is never reached.
    write_fake_reviewer(&fake_bin_dir, "gemini", "pass");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // Fix the coder vendor so priority = [codex, claude, agy, gemini]
    // deterministically.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "minimax"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = FakeVcs::new();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/myrepo".into(); // NOT "owner/repo" -> is_test_repo == false

    store.overlays.borrow_mut().insert(
        "real-repo-bead-4thvendor".into(),
        BeadOverlay {
            bead_id: "real-repo-bead-4thvendor".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(558),
            branch: Some("factory/real-repo-bead-4thvendor-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/real-repo-bead-4thvendor-r1".into());
    store.branch_beads.borrow_mut().insert(
        "factory/real-repo-bead-4thvendor-r1".into(),
        "real-repo-bead-4thvendor".into(),
    );

    scm.pr_snapshots.insert(
        558,
        PrSnapshot {
            pr_number: 558,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef558".into(),
            body: String::new(),
            comments: vec![PrComment {
                author: "some-reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_real_target_repo_skeptic_4thvendor_{}.jsonl",
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
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        summary.beads_ready, 1,
        "jleechan-bkru regression: when the first THREE dispatched/fallback \
         reviewer vendors (codex, claude, agy) all fail to produce a \
         parseable verdict but a fourth vendor (gemini) is available in \
         `priority` and would succeed, `skeptic_evidence` must fall back \
         to it instead of propagating a total-outage Err. \
         summary={summary:?}\ntelemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"all_green\":true"),
        "GATE_ASSESSMENT must report all_green:true once the fourth \
         vendor's verdict is used; telemetry:\n{telemetry}"
    );

    let overlay = store
        .load("real-repo-bead-4thvendor")
        .unwrap()
        .expect("overlay must still exist");
    assert_eq!(
        overlay.state,
        OverlayState::Ready,
        "bead must reach READY via the fourth vendor's verdict, not stay \
         ATTESTED on a false total-outage"
    );

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// jleechan follow-up to #198 (fix/dispatch-batch-isolation): #198 fixed the
// batch-abort bug in `dispatch_ready`'s transient-spawn-failure path (a
// requeue no longer aborts the rest of the batch), but left the requeue path
// uncapped. A bead whose `Sessions::spawn` deterministically fails every
// tick (e.g. the target project pinned at its AO session cap) cycled
// `Queued -> Dispatching -> transient failure -> Queued` forever: it never
// reaches `DISPATCHED`, so `query_active_overlays`'s `DISPATCHED`/`ATTESTED`
// scope never sees it, `autonomy_secs` never accumulates, and the 30-minute
// wedge-detection net never fires — a livelock with zero telemetry signal.
// These three tests exercise the new `overlay.spawn_failure_count` counter +
// `dispatch::MAX_TRANSIENT_SPAWN_RETRY` cap end-to-end through the real
// `run_tick` call stack (same evidence-floor discipline as the rest of this
// file — fakes only at the five tool-boundary traits).

/// Below the cap: the bead must stay QUEUED and retriable. Also asserts the
/// dedicated `spawn_failure_count` counter is what advances — NOT `attempt`
/// (reusing `attempt` here would corrupt both the `factory/<id>-r<n>` branch
/// numbering in `dispatch_ready` and the unrelated
/// `MAX_HUMAN_HELD_RECOVERY_ATTEMPT` cap in `run_recovery_step`).
#[test]
fn transient_spawn_failures_below_cap_stay_retriable_and_do_not_park() {
    const BEAD_ID: &str = "fake-bead-1";

    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 4242,
        title: "Bead whose spawn fails a few times".into(),
        body: "transient tool hiccup, not a permanent block".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#4242".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.fail_spawn_for(BEAD_ID);
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_spawn_retry_below_cap_{}.jsonl",
        std::process::id()
    ));
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

    // Three consecutive transient spawn failures — well under the cap
    // (MAX_TRANSIENT_SPAWN_RETRY == 15, dispatch.rs private const).
    for tick_index in 0..3u64 {
        let summary = run_tick(&deps, tick_index, 0)
            .unwrap_or_else(|e| panic!("tick {tick_index} should succeed: {e:?}"));
        assert_eq!(summary.beads_dispatched, 0, "tick {tick_index}");
        assert_eq!(
            summary.beads_parked_human_held, 0,
            "tick {tick_index}: three transient failures is well under the cap; must not park"
        );
        assert_eq!(summary.beads_escalated, 0, "tick {tick_index}");
    }

    let overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Queued,
        "bead must remain retriable (QUEUED), not parked, below the cap"
    );
    assert_eq!(
        overlay.spawn_failure_count, 3,
        "spawn_failure_count must track the three transient failures"
    );
    assert_eq!(
        overlay.attempt, 1,
        "plain transient spawn retries must NOT bump `attempt` — that field is reserved for \
         the branch/re-roll suffix and the HUMAN_HELD recovery cap"
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !log.contains("PARKED_HUMAN_HELD"),
        "no park should have happened below the cap; got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// At/above the cap: `MAX_TRANSIENT_SPAWN_RETRY + 1` consecutive transient
/// spawn failures must park the bead HUMAN_HELD (never let it silently cycle
/// QUEUED<->DISPATCHING forever), post a real escalation comment, and emit
/// the `PARKED_HUMAN_HELD` / `ESCALATION_REQUIRED` telemetry with reason
/// `transient_spawn_retry_cap_exceeded`.
#[test]
fn transient_spawn_retry_cap_exceeded_parks_human_held_with_escalation() {
    // MAX_TRANSIENT_SPAWN_RETRY (daemon::dispatch, `pub(crate)`) == 15;
    // hardcoded here the same way this file already hardcodes
    // MAX_HUMAN_HELD_RECOVERY_ATTEMPT (== 10, private in tick.rs) elsewhere,
    // since neither is reachable from an external test crate.
    const MAX_TRANSIENT_SPAWN_RETRY: u32 = 15;
    const BEAD_ID: &str = "fake-bead-1";
    const EXTERNAL_REF: &str = "owner/repo#8123";

    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 8123,
        title: "Bead whose spawn deterministically fails".into(),
        body: "target project pinned at its AO session cap".into(),
        author_login: "alice".into(),
        external_ref: EXTERNAL_REF.into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.fail_spawn_for(BEAD_ID);
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_spawn_retry_cap_exceeded_{}.jsonl",
        std::process::id()
    ));
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

    // Tick 0: intake creates + routes the bead, then dispatch attempts
    // spawn — the first transient failure. Must requeue (not park).
    let summary0 = run_tick(&deps, 0, 0).expect("tick 0 should succeed");
    assert_eq!(summary0.beads_created, 1);
    assert_eq!(summary0.beads_routed, 1);
    assert_eq!(summary0.beads_dispatched, 0);
    assert_eq!(summary0.beads_parked_human_held, 0);
    let overlay0 = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(overlay0.state, OverlayState::Queued);
    assert_eq!(overlay0.spawn_failure_count, 1);

    // Ticks 1..=14: fourteen more transient spawn failures (15 total).
    // `spawn_failure_count > MAX_TRANSIENT_SPAWN_RETRY` requires STRICTLY
    // greater than 15, so the bead must stay retriable through exactly 15
    // consecutive failures — the "still under the cap" half of the bound.
    for tick_index in 1..=14u64 {
        let summary = run_tick(&deps, tick_index, 0)
            .unwrap_or_else(|e| panic!("tick {tick_index} should succeed: {e:?}"));
        assert_eq!(
            summary.beads_dispatched, 0,
            "tick {tick_index}: spawn keeps failing transiently"
        );
        assert_eq!(
            summary.beads_parked_human_held, 0,
            "tick {tick_index}: bead must not be parked before the cap is exceeded"
        );
        let overlay = store.load(BEAD_ID).unwrap().unwrap();
        assert_eq!(
            overlay.state,
            OverlayState::Queued,
            "tick {tick_index}: bead below the cap must stay QUEUED — this is exactly the \
             livelock this fix closes if it regresses"
        );
        assert_eq!(overlay.spawn_failure_count, (tick_index as u32) + 1);
    }
    let at_cap = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(at_cap.spawn_failure_count, MAX_TRANSIENT_SPAWN_RETRY);
    assert_eq!(at_cap.state, OverlayState::Queued);

    // Tick 15: the 16th consecutive transient spawn failure pushes
    // spawn_failure_count to 16 (> 15). The bead must park HUMAN_HELD
    // instead of requeuing again, and tick.rs must post a real escalation
    // comment + emit telemetry.
    let summary_cap = run_tick(&deps, 15, 0).expect("cap tick should succeed");
    assert_eq!(summary_cap.beads_dispatched, 0);
    assert_eq!(
        summary_cap.beads_parked_human_held, 1,
        "the cap-exceeding failure must park the bead HUMAN_HELD"
    );
    assert_eq!(
        summary_cap.beads_escalated, 1,
        "the cap-exceeding failure must escalate exactly once"
    );
    let capped_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(capped_overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        capped_overlay.spawn_failure_count,
        MAX_TRANSIENT_SPAWN_RETRY + 1
    );

    let escalation_comment_count = |tracker: &FakeTracker| {
        tracker
            .calls
            .borrow()
            .iter()
            .filter(|call| {
                call.contains(&format!("comment_external({EXTERNAL_REF}"))
                    && call.contains("Escalation required")
                    && call.contains(&format!("bead `{BEAD_ID}`"))
                    && call.contains("consecutive times")
            })
            .count()
    };
    assert_eq!(
        escalation_comment_count(&tracker),
        1,
        "a real escalation comment must be posted via post_scm_comment_by_bead_id: {:?}",
        tracker.calls.borrow()
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("PARKED_HUMAN_HELD") && log.contains("transient_spawn_retry_cap_exceeded"),
        "park telemetry must record the transient_spawn_retry_cap_exceeded reason; got: {log}"
    );
    assert!(
        log.contains("ESCALATION_REQUIRED"),
        "cap escalation telemetry must be emitted; got: {log}"
    );

    // Tick 16: dedup check. `run_recovery_step` runs before `run_slow_tier`
    // every tick and will find this bead again via
    // `human_held_at_or_above_attempt` (its `attempt` is still 1, untouched
    // by the spawn-retry counter, so it's actually BELOW
    // MAX_HUMAN_HELD_RECOVERY_ATTEMPT and gets auto-recovered to QUEUED —
    // then, in the very same tick, dispatch is retried and spawn fails
    // again). Because `spawn_failure_count` was NOT reset by recovery, this
    // must re-trip the cap and re-park immediately rather than granting a
    // fresh 15-retry budget, and `escalation_already_recorded` must prevent
    // a second escalation comment.
    let summary_dedup = run_tick(&deps, 16, 0).expect("dedup tick should succeed");
    assert_eq!(
        summary_dedup.beads_escalated, 0,
        "an already-escalated capped bead must not escalate again"
    );
    assert_eq!(
        escalation_comment_count(&tracker),
        1,
        "dedup must prevent a second escalation comment on a later tick past the cap: {:?}",
        tracker.calls.borrow()
    );
    let final_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(
        final_overlay.state,
        OverlayState::HumanHeld,
        "a bead whose spawn is permanently broken must not be left cycling QUEUED<->DISPATCHING \
         forever even across the automated HUMAN_HELD recovery step"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Real progress (a spawn that finally succeeds) must reset the counter to
/// zero — a bead that flakes twice then recovers should not start its next
/// unrelated flaky streak already halfway to the cap.
#[test]
fn spawn_failure_count_resets_after_a_successful_dispatch() {
    const BEAD_ID: &str = "fake-bead-1";

    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 5150,
        title: "Bead that flakes twice then spawns fine".into(),
        body: "transient hiccup clears up".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#5150".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.fail_spawn_for(BEAD_ID);
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_spawn_retry_reset_{}.jsonl",
        std::process::id()
    ));
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

    for tick_index in 0..2u64 {
        run_tick(&deps, tick_index, 0)
            .unwrap_or_else(|e| panic!("tick {tick_index} should succeed: {e:?}"));
    }
    let flaky_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(flaky_overlay.state, OverlayState::Queued);
    assert_eq!(flaky_overlay.spawn_failure_count, 2);

    // The underlying transient condition clears — spawn now succeeds.
    sessions.fail_spawn_for.borrow_mut().clear();
    let summary = run_tick(&deps, 2, 0).expect("recovery tick should succeed");
    assert_eq!(summary.beads_dispatched, 1);

    let dispatched_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(dispatched_overlay.state, OverlayState::Dispatched);
    assert_eq!(
        dispatched_overlay.spawn_failure_count, 0,
        "a confirmed successful dispatch must reset the transient-failure counter"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-w28n: follow-up to the three `MAX_TRANSIENT_SPAWN_RETRY` tests
// above. `DaemonError::Deferred` (AO's own admission-control queue, hit when
// the target project is pinned at its active-session cap) was classified as
// `is_transient() == true` by `errors.rs` from the start, which meant it fell
// into the SAME counter-incrementing `Err(err) if err.is_transient()` arm as
// genuine `Tool`/`Timeout` spawn failures. Sustained cap saturation is the
// live, expected steady state (not a bug) — worldarchitect.ai routinely sits
// at its session cap — so every dispatch cycle for every queued bead would
// increment `spawn_failure_count`, and after `MAX_TRANSIENT_SPAWN_RETRY`
// cycles the entire backlog would spuriously park HUMAN_HELD with escalation
// comments. These two tests prove the fix: `Deferred` now has its own
// `"spawn_deferred"` phase and NEVER touches the counter.

/// N=20 consecutive `Deferred` spawn "failures" (backpressure, not failure)
/// for the same bead, driven across 20 separate simulated dispatch cycles via
/// the real `run_tick` call stack. N is intentionally chosen above
/// `MAX_TRANSIENT_SPAWN_RETRY` (15) to prove the cap simply does not apply to
/// this path — if the fix regressed and `Deferred` fell back into the
/// general transient arm, this bead would park HUMAN_HELD partway through
/// the loop and the test would fail loudly.
#[test]
fn deferred_spawn_backpressure_never_increments_counter_or_parks_across_repeated_cycles() {
    const BEAD_ID: &str = "fake-bead-1";
    const N: u64 = 20;

    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 9001,
        title: "Bead whose target project sits at its AO session cap".into(),
        body: "sustained admission-control backpressure, not a failure".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#9001".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.fail_spawn_deferred_for(BEAD_ID);
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_spawn_deferred_below_cap_{}.jsonl",
        std::process::id()
    ));
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

    for tick_index in 0..N {
        let summary = run_tick(&deps, tick_index, 0)
            .unwrap_or_else(|e| panic!("tick {tick_index} should succeed: {e:?}"));
        assert_eq!(summary.beads_dispatched, 0, "tick {tick_index}");
        assert_eq!(
            summary.beads_parked_human_held, 0,
            "tick {tick_index}: Deferred backpressure must never park a bead, no matter how \
             many consecutive cycles it persists for"
        );
        assert_eq!(summary.beads_escalated, 0, "tick {tick_index}");

        let overlay = store.load(BEAD_ID).unwrap().unwrap();
        assert_eq!(
            overlay.state,
            OverlayState::Queued,
            "tick {tick_index}: a Deferred bead must stay QUEUED and retriable"
        );
        assert_eq!(
            overlay.spawn_failure_count, 0,
            "tick {tick_index}: Deferred must NEVER increment spawn_failure_count, even after \
             {tick_index} consecutive cycles well past MAX_TRANSIENT_SPAWN_RETRY (15)"
        );
    }

    let final_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(final_overlay.state, OverlayState::Queued);
    assert_eq!(final_overlay.spawn_failure_count, 0);

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !log.contains("PARKED_HUMAN_HELD"),
        "no park should ever happen for sustained Deferred backpressure; got: {log}"
    );

    let deferred_phase_count = log
        .lines()
        .filter(|line| {
            line.contains("BEAD_DISPATCH_TRANSIENT_ERROR")
                && line.contains("\"phase\":\"spawn_deferred\"")
        })
        .count();
    assert_eq!(
        deferred_phase_count, N as usize,
        "exactly {N} \"spawn_deferred\"-phase telemetry events must reach daemon.jsonl (one per \
         tick) — a Deferred spawn outcome that never reaches telemetry reproduces the \
         zero-telemetry-signal gap this fix closes; got log: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Mixed-batch independence: in the SAME dispatch batch, bead A hits
/// sustained `Deferred` backpressure (must never park, counter stays 0)
/// while bead B hits genuine `Tool` transient failures and DOES eventually
/// park HUMAN_HELD once `spawn_failure_count` exceeds
/// `MAX_TRANSIENT_SPAWN_RETRY`. Proves the two paths are independent: A's
/// backpressure must never contaminate B's counter (or vice versa), and B
/// parking must never affect A.
#[test]
fn mixed_batch_deferred_backpressure_and_genuine_transient_failures_are_independent() {
    const MAX_TRANSIENT_SPAWN_RETRY: u32 = 15;
    const BEAD_A: &str = "fake-bead-a";
    const BEAD_B: &str = "fake-bead-b";
    const EXTERNAL_REF_A: &str = "owner/repo#9101";
    const EXTERNAL_REF_B: &str = "owner/repo#9102";

    // `FakeTracker::create_bead` always returns the single hardcoded id
    // "fake-bead-1" (see tests/common/mod.rs), so two `scm.issues` entries
    // cannot be used to mint two DISTINCT bead ids in one test. Instead,
    // pre-seed both beads directly as already-tracked (mirrors
    // `capped_human_held_candidate_lookup_failure_retries_before_recording_escalation`'s
    // pattern below): `run_slow_tier`'s "leftover from a prior tick" loop
    // (tick.rs, `for bead in tracker_candidates { ... }`) picks up every
    // `tracker.candidates` entry each tick and routes/dispatches it if its
    // overlay is QUEUED, exactly like a real `br list` would on tick N+1.
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: BEAD_A.into(),
        title: "Bead A: sustained Deferred backpressure".into(),
        description: "target project pinned at its AO session cap".into(),
        file_tree_summary: String::new(),
        external_ref: Some(EXTERNAL_REF_A.into()),
    });
    tracker.candidates.borrow_mut().push(Bead {
        id: BEAD_B.into(),
        title: "Bead B: genuine deterministic tool spawn failure".into(),
        description: "distinct from bead A's backpressure — a real transient error".into(),
        file_tree_summary: String::new(),
        external_ref: Some(EXTERNAL_REF_B.into()),
    });

    let sessions = FakeSessions::new();
    sessions.fail_spawn_deferred_for(BEAD_A);
    sessions.fail_spawn_for(BEAD_B);
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_spawn_mixed_batch_{}.jsonl",
        std::process::id()
    ));
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

    // Ticks 0..=14: fifteen consecutive cycles. Bead A must stay QUEUED with
    // a permanently-zero counter; bead B accumulates spawn_failure_count but
    // must stay under the cap (>15 required to trip it).
    for tick_index in 0..=14u64 {
        let summary = run_tick(&deps, tick_index, 0)
            .unwrap_or_else(|e| panic!("tick {tick_index} should succeed: {e:?}"));
        assert_eq!(
            summary.beads_parked_human_held, 0,
            "tick {tick_index}: bead B must not be parked before the cap is exceeded, and bead \
             A must never be parked at all"
        );

        let overlay_a = store.load(BEAD_A).unwrap().unwrap();
        assert_eq!(
            overlay_a.state,
            OverlayState::Queued,
            "tick {tick_index}: bead A (Deferred) must stay QUEUED"
        );
        assert_eq!(
            overlay_a.spawn_failure_count, 0,
            "tick {tick_index}: bead A's counter must stay at 0 regardless of bead B's failures \
             in the same batch"
        );

        let overlay_b = store.load(BEAD_B).unwrap().unwrap();
        assert_eq!(
            overlay_b.state,
            OverlayState::Queued,
            "tick {tick_index}: bead B (genuine transient) must stay QUEUED below the cap"
        );
        assert_eq!(overlay_b.spawn_failure_count, (tick_index as u32) + 1);
    }

    let a_at_fifteen = store.load(BEAD_A).unwrap().unwrap();
    assert_eq!(a_at_fifteen.spawn_failure_count, 0);
    let b_at_cap = store.load(BEAD_B).unwrap().unwrap();
    assert_eq!(b_at_cap.spawn_failure_count, MAX_TRANSIENT_SPAWN_RETRY);
    assert_eq!(b_at_cap.state, OverlayState::Queued);

    // Tick 15: bead B's 16th consecutive genuine transient failure exceeds
    // the cap and must park HUMAN_HELD + escalate. Bead A, in the very same
    // batch, must remain completely unaffected.
    let summary_cap = run_tick(&deps, 15, 0).expect("cap tick should succeed");
    assert_eq!(
        summary_cap.beads_parked_human_held, 1,
        "exactly one bead (B) should park on this tick"
    );
    assert_eq!(summary_cap.beads_escalated, 1);

    let a_final = store.load(BEAD_A).unwrap().unwrap();
    assert_eq!(
        a_final.state,
        OverlayState::Queued,
        "bead A must remain QUEUED even after bead B parks HUMAN_HELD in the same batch"
    );
    assert_eq!(a_final.spawn_failure_count, 0);

    let b_final = store.load(BEAD_B).unwrap().unwrap();
    assert_eq!(b_final.state, OverlayState::HumanHeld);
    assert_eq!(b_final.spawn_failure_count, MAX_TRANSIENT_SPAWN_RETRY + 1);

    let escalation_comment_count_for =
        |tracker: &FakeTracker, external_ref: &str, bead_id: &str| {
            tracker
                .calls
                .borrow()
                .iter()
                .filter(|call| {
                    call.contains(&format!("comment_external({external_ref}"))
                        && call.contains("Escalation required")
                        && call.contains(&format!("bead `{bead_id}`"))
                })
                .count()
        };
    assert_eq!(
        escalation_comment_count_for(&tracker, EXTERNAL_REF_B, BEAD_B),
        1,
        "bead B must get exactly one real escalation comment: {:?}",
        tracker.calls.borrow()
    );
    assert_eq!(
        escalation_comment_count_for(&tracker, EXTERNAL_REF_A, BEAD_A),
        0,
        "bead A (Deferred backpressure) must NEVER be escalated: {:?}",
        tracker.calls.borrow()
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("\"phase\":\"spawn_deferred\""),
        "bead A's Deferred outcomes must still reach telemetry via BEAD_DISPATCH_TRANSIENT_ERROR; \
         got: {log}"
    );
    assert!(
        log.contains("PARKED_HUMAN_HELD") && log.contains("transient_spawn_retry_cap_exceeded"),
        "bead B's cap-exceeded park telemetry must be present; got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-eazj: reproduces the exact mechanism that made GitHub issue
/// jleechanorg/worldarchitect.ai#8171 vanish with zero telemetry, ever, even
/// though it carried the `factory` label and was OPEN.
///
/// #8171's shape: factory-labeled, no existing bead, no existing branch.
/// That shape alone always worked fine in isolation — the actual gap was
/// upstream in the *batch*: `intake::normalize`'s per-candidate loop used to
/// `return Err(e)` the instant ANY earlier candidate in the same
/// `scm.labeled_issues` fetch hit a non-duplicate `create_bead` error (e.g. a
/// malformed body/title `br create` rejects for unrelated reasons). That
/// early return aborted the whole `normalize()` call before later candidates
/// in the batch — including one shaped exactly like #8171 — were ever even
/// looked at, so NOTHING was ever logged for them: not ADOPTED, not
/// SKIPPED_*, not even ERRORED. Depending on `DaemonError` variant this also
/// either crash-loops the whole daemon process (non-transient -> `main()`
/// calls `std::process::exit(1)`) or silently retries forever hitting the
/// exact same wall every tick (transient `DaemonError::Tool` -> backoff and
/// retry, re-fetching the same ordered candidate list and getting stuck on
/// the same earlier failing candidate again).
///
/// This test seeds two factory-labeled issues in one fetch batch: issue #1
/// (`owner/repo#1`) is scripted to fail `create_bead` with a generic,
/// non-duplicate `br` tool error — standing in for "whatever other candidate
/// was ahead of #8171 in the batch and broke". Issue #2 (`owner/repo#8171`)
/// matches #8171's real shape (factory label, no bead, no branch, write-tier
/// author) and would have adopted cleanly on its own. The fix under test:
/// BOTH issues must resolve to exactly one of the five jleechan-eazj verdict
/// events (ADOPTED/SKIPPED_DUPLICATE/SKIPPED_FORK/SKIPPED_INELIGIBLE/ERRORED)
/// in the SAME tick, and the tick itself must succeed rather than aborting.
#[test]
fn earlier_candidate_create_bead_error_does_not_silence_later_candidate_matching_issue_8171() {
    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 1,
        title: "some other malformed candidate".into(),
        body: "triggers a create_bead failure unrelated to #8171".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#1".into(),
    });
    scm.issues.push(Issue {
        number: 8171,
        title: "[autor] Drive PR #8061 (Nocturna docs) to 4-green NON_PRODUCTION".into(),
        body: "matches issue #8171's real shape".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#8171".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    // Only owner/repo#1's create_bead call fails; owner/repo#8171's succeeds
    // via the default `create_bead_result` (`Ok("fake-bead-1")`).
    *tracker.create_bead_fail_for_ref.borrow_mut() = Some((
        "owner/repo#1".to_string(),
        "br: create failed: malformed candidate".to_string(),
    ));

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_dir = std::env::temp_dir().join("afd_tick_integration_test");
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    let telemetry_log = telemetry_dir.join(format!("daemon-8171-{}.jsonl", std::process::id()));
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

    let summary = run_tick(&deps, 0, 0).expect(
        "the tick must SUCCEED despite issue #1's create_bead error -- one candidate's failure \
         must never abort the batch (this is exactly the mechanism that silenced #8171)",
    );
    assert_eq!(
        summary.beads_created, 1,
        "only owner/repo#8171 should produce a bead; owner/repo#1 errored"
    );

    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    // owner/repo#1: exactly one ERRORED verdict, naming the real reason.
    let issue1_events: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| e["beadId"] == "owner/repo#1")
        .collect();
    assert_eq!(
        issue1_events.len(),
        1,
        "owner/repo#1 must get exactly one verdict event, got: {issue1_events:#?}"
    );
    assert_eq!(issue1_events[0]["eventType"], "ERRORED");
    let reason = issue1_events[0]["context"]["reason"]
        .as_str()
        .expect("ERRORED context must carry a real reason string");
    assert!(
        reason.contains("malformed candidate"),
        "ERRORED reason must be the actual error, not a generic message: {reason}"
    );

    // owner/repo#8171: exactly one ADOPTED verdict (INTAKE_BEAD_CREATED),
    // proving issue #1's earlier failure did not silence it.
    let issue_8171_events: Vec<&serde_json::Value> = events
        .iter()
        .filter(|e| {
            e["context"]["external_ref"] == "owner/repo#8171" || e["beadId"] == "fake-bead-1"
        })
        .filter(|e| e["eventType"] == "INTAKE_BEAD_CREATED")
        .collect();
    assert_eq!(
        issue_8171_events.len(),
        1,
        "owner/repo#8171 (matching the real issue's shape) must get exactly one ADOPTED \
         verdict in the same tick as the errored candidate ahead of it, got full event stream: \
         {events:#?}"
    );

    // grep-ability: the operator's actual diagnostic step was `grep 8171
    // daemon.jsonl`. Assert the raw JSONL text contains "8171" somewhere,
    // matching both the ERRORED external_ref format and this assertion's
    // intent.
    assert!(
        body.contains("8171"),
        "raw telemetry JSONL must be grep-able for the issue number, got: {body}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-cq8r: per-bead isolation for the re-roll engine's error path,
/// in the SAME fast-tier loop `qdw_per_bead_isolation_snapshot_failure_does_not_abort_fast_tier`
/// above covers for `pr_snapshot` failures. Before this fix, `tick.rs`'s
/// `match crate::reroll::execute(...) { ... Err(e) => return Err(e) }` arm
/// propagated ANY re-roll-engine failure -- including a transient LLM call
/// failure inside the circuit-breaker comparator (`same_underlying_issue`)
/// -- straight out of `run_fast_tier`, aborting the ENTIRE fast tier for
/// every OTHER in-flight bead in the same tick, not just the one bead whose
/// comparator call failed.
///
/// Two beads are seeded directly at attempt=2 (already past a first
/// re-roll, both `is_adopted` to skip session/quiescence mocking and reach
/// the circuit-breaker comparator via the shortest real path) with a
/// stored attempt-1 rejection from the SAME reviewer as this tick's
/// (scripted) red-gate reviewer, so `reroll::execute` actually invokes
/// `same_underlying_issue` for both:
///   * bead A's PRIOR rejection text carries a marker the scripted `Llm`
///     recognizes and answers with a reply containing no JSON object at
///     all (the exact malformed-reply shape jleechan-cq8r found) --
///     `same_underlying_issue` returns `Err(ComparatorUnparseable)`.
///   * bead B's PRIOR rejection text carries no such marker -- the
///     scripted `Llm` answers normally (`sameUnderlyingIssue: false`), the
///     breaker does not fire, and the adopted append-only remediation path
///     completes successfully.
///
/// Invariants this test pins:
///   1. Bead B reaches a successful re-roll (attempt bumped to 3, still
///      `Attested`) in the SAME tick as bead A's comparator failure --
///      per-bead isolation, not a tick-wide abort.
///   2. Bead A is left `ReRoll` (as `reroll::execute` already persisted it
///      before the failing comparator call) rather than crashing the tick.
///   3. Telemetry records a `BEAD_PROCESSING_TRANSIENT_ERROR` event with
///      `phase: "reroll_execute"` for bead A.
struct IsoRerollLlm;

impl Llm for IsoRerollLlm {
    fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
        if prompt.contains("Circuit-Breaker Semantic Comparator") {
            if prompt.contains("BEAD-A-PRIOR-MARKER") {
                Ok("the model babbled without any JSON object at all".to_string())
            } else {
                Ok(r#"{"sameUnderlyingIssue": false}"#.to_string())
            }
        } else {
            Ok("pass".to_string())
        }
    }
}

#[test]
fn cq8r_per_bead_isolation_reroll_comparator_failure_does_not_abort_fast_tier() {
    let mut scm = FakeScm::new();
    let mut snap_a = qdw_green_snapshot(
        801,
        vec![PrComment { author: "dark-factory-er".into(), body: "/er PASS".into(), created_at_epoch: 0 }],
    );
    snap_a.ci_success = false;
    snap_a.ci_status = "failure".into();
    scm.pr_snapshots.insert(801, snap_a);

    let mut snap_b = qdw_green_snapshot(
        802,
        vec![PrComment { author: "dark-factory-er".into(), body: "/er PASS".into(), created_at_epoch: 0 }],
    );
    snap_b.ci_success = false;
    snap_b.ci_status = "failure".into();
    scm.pr_snapshots.insert(802, snap_b);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = IsoRerollLlm;
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2;
    let mut vcs = FakeVcs::new();
    // jleechan-tfs1 amendment (#209): execute_adopted now captures
    // pre_session_head_sha via Vcs::remote_head_sha before spawning, and
    // fails closed (Held) if that lookup errors. Bead B (the only bead in
    // this test that reaches the real adopted-remediation dispatch path,
    // since bead A exits early on the comparator failure) needs a scripted
    // head for its branch or it never reaches its expected `Attested`
    // outcome — this test predates #209 and only scripted PR snapshots.
    vcs.heads
        .insert("bob/cq8r-bead-b-branch".into(), "bead-b-head-sha".into());

    for (bead_id, pr, branch, prior_text) in [
        ("cq8r-bead-a", 801u64, "alice/cq8r-bead-a-branch", "BEAD-A-PRIOR-MARKER"),
        ("cq8r-bead-b", 802u64, "bob/cq8r-bead-b-branch", "bead-b-prior-text"),
    ] {
        store
            .save(&BeadOverlay {
                bead_id: bead_id.into(),
                state: OverlayState::Attested,
                attempt: 2,
                reroll_count: 1,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: Some(pr),
                branch: Some(branch.into()),
                session_id: None,
                is_adopted: true,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: None,
            })
            .unwrap();
        store.register_branch(bead_id, branch).unwrap();
        store
            .save_rejection(bead_id, 1, "verifier", "deadbeefdeadbeef", prior_text)
            .unwrap();
    }

    let telemetry_log =
        std::env::temp_dir().join(format!("afd_cq8r_iso_{}.jsonl", std::process::id()));
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
    .expect("bead A's comparator failure must not abort the tick");

    assert_eq!(
        summary.gates_assessed, 2,
        "both beads' gates should be assessed in this tick; got {summary:?}"
    );

    let bead_a = store.load("cq8r-bead-a").unwrap().unwrap();
    assert_eq!(
        bead_a.state,
        OverlayState::ReRoll,
        "bead A stays ReRoll (persisted by reroll::execute before the comparator failed); got {:?}",
        bead_a.state
    );
    assert_eq!(
        bead_a.attempt, 2,
        "bead A's attempt must not advance on a failed comparator call"
    );

    let bead_b = store.load("cq8r-bead-b").unwrap().unwrap();
    // jleechan-tfs1 amendment (#209): adopted-PR remediation now dispatches a
    // real coder session and lands in Dispatched (not an immediate Attested)
    // — the fast-tier quiescence-gated DISPATCHED -> ATTESTED promotion
    // re-verifies on a later tick once the coder session finishes. This test
    // predates that change; the invariant under test (bead A's comparator
    // failure does not block bead B's re-roll in the same tick) still holds
    // — bead B reaches Dispatched, not HumanHeld or untouched.
    assert_eq!(
        bead_b.state,
        OverlayState::Dispatched,
        "bead B must progress its re-roll (to Dispatched, real coder session) in the SAME tick as bead A's comparator failure; got {:?}",
        bead_b.state
    );
    assert_eq!(
        bead_b.attempt, 3,
        "bead B's successful append-only re-roll dispatch must advance its attempt counter"
    );

    // jleechan-tfs1 amendment (#209): execute_adopted no longer fabricates a
    // commit via Vcs::push_fix_commit — it dispatches a real coder session
    // via Sessions::spawn instead. This test predates that change.
    let session_calls = sessions.calls.borrow();
    assert!(
        session_calls.iter().any(|c| c == "spawn(cq8r-bead-b)"),
        "bead B's re-roll must actually dispatch a real coder session: {session_calls:?}"
    );
    assert!(
        session_calls.iter().all(|c| c != "spawn(cq8r-bead-a)"),
        "bead A must never reach the spawn dispatch after its comparator call failed: {session_calls:?}"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let saw_bpte = telemetry.lines().any(|l| {
        l.contains("BEAD_PROCESSING_TRANSIENT_ERROR")
            && l.contains("cq8r-bead-a")
            && l.contains("\"phase\":\"reroll_execute\"")
    });
    assert!(
        saw_bpte,
        "expected a BEAD_PROCESSING_TRANSIENT_ERROR/phase=reroll_execute event for bead A; telemetry was:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// --- jleechan-x8tf: run_slow_tier PR-existence probe must target the ---
// --- bead's OWN resolved repo, not unconditionally `cfg.target_repo` ---
//
// Stage D (jleechan-9xrs, PR #250) swept the verification loop's repo call
// sites but did NOT touch this one: the intake `created`-bead loop in
// `run_slow_tier` parses a repo out of the bead's `external_ref` via the
// local `parse_external_ref` helper, discards it (`_`), and unconditionally
// probes `deps.cfg.target_repo` via `gh pr view --repo <cfg.target_repo>` to
// decide whether the bead already has an open PR. For any bead whose
// `external_ref`/`target_repo:` body field names a DIFFERENT repo than the
// daemon's global default, this silently checks the WRONG repo's PR list —
// flagged during Stage D review as a risk to Stage E's two-repo E2E
// acceptance proof (a dark-factory fixture bead's probe could silently
// check worldarchitect.ai instead).
//
// This gates on `Llm::is_real()` (see `tick.rs` around the `parse_external_ref`
// call), so these tests use a small local `is_real()==true` `Llm` fake — NOT
// `common::FakeLlm`, which always reports `is_real() == false` (see the
// `real_target_repo_skeptic_gate_...` test's comment above for why that
// distinction matters). `cfg.target_repo` is kept at the file's usual
// `"owner/repo"` test-repo convention so `skeptic_evidence`'s SEPARATE
// `is_test_repo` gate (driven by repo name, not `Llm::is_real()`) still takes
// the mock-LLM path and this test stays fast/hermetic apart from the one
// `gh` shell-out under test.
struct RealLlmForRepoProbe {
    response: std::cell::RefCell<Option<Result<String, String>>>,
}

impl Llm for RealLlmForRepoProbe {
    fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
        match self.response.borrow().as_ref() {
            Some(Ok(t)) => Ok(t.clone()),
            Some(Err(e)) => Err(DaemonError::Parse(e.clone())),
            None => Ok(String::new()),
        }
    }
    fn is_real(&self) -> bool {
        true
    }
}

/// Write a fake `gh` script into `dir` that answers `pr view <num> --repo
/// <repo> --json number` by (a) recording the value passed to `--repo` into
/// `capture_file` and (b) printing a valid `{"number": ...}` JSON payload so
/// the caller's `.is_ok()` check succeeds regardless of which repo was
/// probed — this test only cares WHICH repo `run_slow_tier` asked about, not
/// simulating a real PR-not-found case.
#[cfg(unix)]
fn write_fake_gh_capturing_repo_arg(
    dir: &std::path::Path,
    capture_file: &std::path::Path,
    expect_num: u64,
) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh");
    let script = format!(
        "#!/bin/sh\n\
         prev=\"\"\n\
         match=0\n\
         repo_val=\"\"\n\
         for arg in \"$@\"; do\n\
           if [ \"$arg\" = \"{expect_num}\" ]; then\n\
             match=1\n\
           fi\n\
           if [ \"$prev\" = \"--repo\" ]; then\n\
             repo_val=\"$arg\"\n\
           fi\n\
           prev=\"$arg\"\n\
         done\n\
         if [ $match -eq 1 ] && [ -n \"$repo_val\" ]; then\n\
           printf '%s' \"$repo_val\" > \"{capture}\"\n\
         fi\n\
         echo '{{\"number\": 1}}'\n",
        expect_num = expect_num,
        capture = capture_file.display()
    );
    std::fs::write(&path, script)
        .unwrap_or_else(|e| panic!("failed to write fake gh: {e}"));
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// The fix: a bead whose `external_ref` names a DIFFERENT repo than
/// `cfg.target_repo` must have its PR-existence probe target ITS OWN repo,
/// not silently fall back to the global config repo.
#[test]
#[cfg(unix)]
fn run_slow_tier_pr_existence_probe_targets_bead_own_repo_not_global_cfg() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_x8tf_probe_own_repo_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let capture_file = fake_bin_dir.join("captured_repo.txt");
    write_fake_gh_capturing_repo_arg(&fake_bin_dir, &capture_file, 42);

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);
    let _env_guard = EnvVarGuard::set(&[("PATH", &new_path)]);

    let mut scm = FakeScm::new();
    // external_ref names a DIFFERENT repo than cfg.target_repo ("owner/repo"
    // below) — exactly the multi-repo-fixture-vs-global-default scenario
    // Stage E's two-repo E2E proof depends on.
    scm.issues.push(Issue {
        number: 42,
        title: "Fixture bead in a different repo".into(),
        body: "please fix the thing".into(),
        author_login: "alice".into(),
        external_ref: "jleechanorg/dark-factory-holdouts#42".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = RealLlmForRepoProbe {
        response: std::cell::RefCell::new(Some(Ok(
            r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
        ))),
    };
    let store = FakeStateStore::new();
    let cfg = test_cfg(); // target_repo == "owner/repo"
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_x8tf_probe_own_repo_{}.jsonl",
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
    .expect("tick should succeed");

    assert_eq!(summary.beads_created, 1, "one bead should be created");

    let captured = std::fs::read_to_string(&capture_file).unwrap_or_else(|e| {
        panic!(
            "expected the fake gh to have recorded a --repo arg, but reading \
             {capture_file:?} failed: {e} (gh pr view was never called with \
             --repo at all)"
        )
    });
    assert_eq!(
        captured, "jleechanorg/dark-factory-holdouts",
        "run_slow_tier's PR-existence probe must target the bead's OWN \
         resolved repo (from external_ref), not cfg.target_repo \
         (\"owner/repo\"); captured --repo arg: {captured:?}"
    );

    let overlay = store
        .load("fake-bead-1")
        .unwrap()
        .expect("overlay must exist");
    assert_eq!(
        overlay.target_repo.as_deref(),
        Some("jleechanorg/dark-factory-holdouts"),
        "overlay must carry the bead's own resolved target_repo"
    );

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

/// Legacy-behavior regression: a bead whose resolved repo happens to MATCH
/// `cfg.target_repo` (the pre-multi-repo, single-repo default — every other
/// test in this file uses this shape) must still probe that same repo,
/// unchanged, after the fix.
#[test]
#[cfg(unix)]
fn run_slow_tier_pr_existence_probe_unchanged_for_single_repo_legacy_bead() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_x8tf_probe_legacy_repo_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let capture_file = fake_bin_dir.join("captured_repo.txt");
    write_fake_gh_capturing_repo_arg(&fake_bin_dir, &capture_file, 43);

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);
    let _env_guard = EnvVarGuard::set(&[("PATH", &new_path)]);

    let mut scm = FakeScm::new();
    // external_ref's repo prefix MATCHES cfg.target_repo — the single-repo
    // shape every other test in this file already exercises.
    scm.issues.push(Issue {
        number: 43,
        title: "Same-repo bead".into(),
        body: "please fix the other thing".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#43".into(),
    });
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = RealLlmForRepoProbe {
        response: std::cell::RefCell::new(Some(Ok(
            r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
        ))),
    };
    let store = FakeStateStore::new();
    let cfg = test_cfg(); // target_repo == "owner/repo"
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_x8tf_probe_legacy_repo_{}.jsonl",
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
    .expect("tick should succeed");

    assert_eq!(summary.beads_created, 1, "one bead should be created");

    let captured = std::fs::read_to_string(&capture_file).unwrap_or_else(|e| {
        panic!(
            "expected the fake gh to have recorded a --repo arg, but reading \
             {capture_file:?} failed: {e} (gh pr view was never called with \
             --repo at all)"
        )
    });
    assert_eq!(
        captured, "owner/repo",
        "single-repo (legacy default) beads must keep probing \
         cfg.target_repo's value unchanged; captured --repo arg: {captured:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}
