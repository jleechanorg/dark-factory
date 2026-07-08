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
use daemon::tools::{Bead, Issue, LabeledPr, Llm, Permission, PrComment, PrSnapshot, Scm};
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
                is_adopted: false,
                spawn_failure_count: 0,
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
    assert!(logs.contains("feat/wa-3004-hook-refactor"), "logs: {}", logs);

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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
        store.bead_id_for_branch("feature/already-open-pr-702").unwrap(),
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
        store.bead_id_for_branch("factory/existing-bead-r1").unwrap(),
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
        tracker_calls.iter().all(|call| !call.starts_with("create_bead(")),
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
        session_calls.iter().all(|call| {
            !call.starts_with("spawn(") && !call.starts_with("attach(")
        }),
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
/// red-gate reroll on an adopted PR pushes an append-only fix commit to the
/// EXISTING contributor branch via `run_tick`'s real intake -> verifier ->
/// reroll wiring (not a direct `reroll::execute` call — this proves the
/// `is_adopted` flag actually survives the round trip through
/// `tick::run_slow_tier`'s adoption block, `StateStore::save`/`load`, and
/// back into `tick::run_fast_tier`'s reroll dispatch). The PR stays open,
/// branch registry is unchanged, and no force-push/rebase/close_pr ever
/// happens.
#[test]
fn adopted_red_pr_stage2_reroll_pushes_fix_commit_leaves_pr_open() {
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
    let vcs = FakeVcs::new();
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
        "a successful append-only push must not park the bead"
    );

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::Attested);
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

    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls
            .iter()
            .any(|c| c.starts_with("push_fix_commit(alice/my-cool-feature,")),
        "adopted stage-2 reroll must push a fix commit to the existing branch: {vcs_calls:?}"
    );
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("create_branch_at(")),
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
            c.contains("comment_external(owner/repo#706") && c.contains("remediation commit")
        }),
        "adopted stage-2 success should post a status comment: {tracker_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// bead jleechan-tfs1, requirement (d), full pipeline (Stage 2): when the
/// append-only push genuinely can't land (scripted as a non-fast-forward
/// rejection — the real-world case is a remote that diverged or a base
/// conflict needing a rebase), the bead must be parked `HUMAN_HELD` with an
/// escalation comment actually posted on the PR (via
/// `tick::post_scm_comment_by_bead_id` -> `Tracker::comment_external`) —
/// not a silent failure, and never a force-push/rebase fallback.
#[test]
fn adopted_red_pr_stage2_reroll_append_only_conflict_parks_human_held_with_escalation() {
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
    let vcs = FakeVcs::new();
    vcs.fail_push_fix_commit_for("alice/my-conflicted-feature");
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
    .expect("adopted red PR stage2 append-only failure should park, not error");

    assert_eq!(summary.beads_parked_human_held, 1);

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.pr_number,
        Some(707),
        "PR number must be left in place even on a failed remediation attempt"
    );
    assert_eq!(overlay.branch.as_deref(), Some("alice/my-conflicted-feature"));

    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls
            .iter()
            .any(|c| c.starts_with("push_fix_commit(alice/my-conflicted-feature,")),
        "must have attempted the append-only push before parking: {vcs_calls:?}"
    );
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("create_branch_at(")),
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
    assert_eq!(summary0.beads_created, 1, "PR must be adopted as a new bead");
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
    assert_eq!(at_cap.attempt, 10, "bead must have reached the recovery cap");
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
                is_adopted: false,
                spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
            is_adopted: false,
            spawn_failure_count: 0,
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
    let _env_guard = EnvVarGuard::set(&[("PATH", &new_path), ("DARK_FACTORY_CODER_DEFAULT", "minimax")]);

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
            unresolved_thread_count: 0,
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
            }],
            files: vec![],
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "green".into(),
            ci_pending: false,
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
    let _env_guard = EnvVarGuard::set(&[("PATH", &new_path), ("DARK_FACTORY_CODER_DEFAULT", "minimax")]);

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
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/real-repo-bead-asym-r1".into());
    store
        .branch_beads
        .borrow_mut()
        .insert("factory/real-repo-bead-asym-r1".into(), "real-repo-bead-asym".into());

    scm.pr_snapshots.insert(
        556,
        PrSnapshot {
            pr_number: 556,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
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
                },
                PrComment {
                    author: "jleechan".into(),
                    body: "Looks good, sign-off from me.".into(),
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
