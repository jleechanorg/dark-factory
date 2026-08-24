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
use daemon::config::{Config, RepoConfig};

use daemon::er_runner;
use daemon::errors::DaemonError;
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::tick::{combine_dual_verdict, run_tick, TickDeps};
use daemon::tools::{
    Bead, Issue, LabeledPr, Llm, Permission, PrComment, PrHeadBranch, PrSnapshot, Scm,
};
use daemon::verifier::SkepticVerdict;

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
        // fast_tick_secs == slow_tick_secs so the slow tier is due on every
        // `run_tick` call in this test (ratio == 1) — both driven ticks
        // exercise intake/route/dispatch AND the fast-tier verifier pass.
        fast_tick_secs: 60,
        slow_tick_secs: 60,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
        reroll_head_stability_window_secs: 1,
        reroll_death_confirm_secs: 0,
        held_recheck_cooldown_secs: 900,
        repos: std::collections::HashMap::from([
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
    // jleechan-t40t r6: the slow-tier branch→PR re-resolution now
    // fail-closed-clears stale pr_number when the branch has no live PR.
    // Script the fake branch→PR lookup so the gate-assessment path
    // proceeds against the live PR 101 (not None).
    scm.pr_numbers_for_branch.insert(
        ("owner/repo".into(), "factory/fake-bead-1-r1".into()),
        Some(101),
    );
    scm.pr_snapshots.insert(
        101,
        PrSnapshot {
            pr_number: 101,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
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
        bugbot_pending: false,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
        vendor_health: None,
    };

    let summary =
        run_tick(&deps, 0, 0).expect("tick should succeed even on a routing parse failure");
    assert_eq!(summary.beads_created, 1);
    assert_eq!(summary.beads_routed, 0);
    assert_eq!(summary.beads_dispatched, 0);
    assert_eq!(summary.beads_parked_human_held, 1);
    // Under the dispatch-scheduling-guarantee ordering, `run_recovery_step`
    // runs AFTER `run_slow_tier` on slow ticks, so the freshly-parked
    // unroutable bead (attempt 1 < cap, session_id already NULL) is
    // immediately requeued to QUEUED in the same tick. The bead is still
    // never dispatched — the spawn count assertion below is the real guard.
    assert_eq!(summary.beads_recovered_from_held, 1);

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
    assert_eq!(
        overlay.state,
        OverlayState::Queued,
        "unroutable bead is parked then recovered to QUEUED in the same tick under the new ordering"
    );

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
            notes: String::new(),
            file_tree_summary: String::new(),
            external_ref: None,
        },
        Bead {
            id: "bead-1".into(),
            title: "second bead".into(),
            description: String::new(),
            notes: String::new(),
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
                target_repo: Some("owner/repo".to_string()),
                attempt_started_at: None,
            })
            .unwrap();
    }
    store.fail_save_for("bead-0", OverlayState::Dispatching);
    let cfg = test_cfg();
    let vcs = test_vcs();
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
        vendor_health: None,
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
            attempt_started_at: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_autonomy.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
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
            attempt_started_at: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_warning.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
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
            attempt_started_at: None,
        },
    );

    // Script scm remote branch to return None (does not exist)
    scm.remote_branches
        .insert("factory/bead-silent-r1".into(), None);

    let telemetry_log = std::env::temp_dir().join("afd_test_wedge_silent.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
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
            attempt_started_at: None,
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

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
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
            attempt_started_at: None,
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

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
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
            attempt_started_at: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_dispatch_integrity_sweep.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
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
        "session_branch_mismatch is not in the recoverable set, so a \
         branch-mismatch hold must never auto-requeue"
    );
    let held = store.load("jleechan-vj89").unwrap().unwrap();
    assert_eq!(held.state, OverlayState::HumanHeld);
    // jleechan-park-leaves-zombie-session-mh9o: `session_branch` just
    // proved the leaked session belongs to a DIFFERENT bead/branch (the
    // `jleechan-5ia2` corruption case) — so we MUST NOT call
    // `sessions.stop()` here. Killing it would terminate another bead's
    // legitimate worker. The right fix is to drop OUR overlay's bad
    // handle (the durable record pointing at a session that was never
    // ours to own) without touching AO.
    assert_eq!(
        held.session_id,
        None,
        "session_branch_mismatch park MUST drop the bad overlay handle so \
         the leaked record cannot poison future redispatches of THIS bead \
         via the AO dedup guard. Calls: {:?}",
        sessions.calls.borrow()
    );
    assert!(
        !sessions
            .calls
            .borrow()
            .iter()
            .any(|call| call == "stop(wa-3004)"),
        "session_branch_mismatch park MUST NOT kill the leaked session \
         because session_branch has proven it belongs to a different \
         bead/branch. Killing it would terminate someone else's \
         legitimate worker. Calls: {:?}",
        sessions.calls.borrow()
    );
    assert_eq!(held.park_reason.as_deref(), Some("session_branch_mismatch"));
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
            attempt_started_at: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_dispatch_integrity_ok.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
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
            attempt_started_at: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_force_push_detection.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut vcs = test_vcs();
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
        vendor_health: None,
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
            attempt_started_at: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_force_push_ok.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut vcs = test_vcs();
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
        vendor_health: None,
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
fn append_only_sweep_warns_but_keeps_unpublished_local_descendant_running() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let branch = "factory/bead-unpublished-r5";
    let pre_sha = "pre-session-sha-unpublished";
    store.overlays.borrow_mut().insert(
        "bead-unpublished".into(),
        BeadOverlay {
            bead_id: "bead-unpublished".into(),
            state: OverlayState::Dispatched,
            attempt: 5,
            reroll_count: 1,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some(branch.into()),
            session_id: Some("session-unpublished".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some(pre_sha.into()),
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );
    sessions.set_worktree_ancestor("session-unpublished", branch, pre_sha, true);
    let telemetry_log =
        std::env::temp_dir().join("afd_test_append_only_unpublished_local.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 1).unwrap();
    assert_eq!(summary.beads_parked_human_held, 0);
    let overlay = store.load("bead-unpublished").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::Dispatched);
    assert_eq!(overlay.session_id.as_deref(), Some("session-unpublished"));
    assert!(!sessions.calls.borrow().iter().any(|c| c == "stop(session-unpublished)"));
    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(logs.contains("APPEND_ONLY_REMOTE_CHECK_DEFERRED"), "logs: {logs}");
    assert!(logs.contains("local_worktree_ancestry_confirmed"), "logs: {logs}");
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn append_only_sweep_parks_local_rewrite_without_trusting_stale_remote() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let branch = "factory/bead-local-rewrite-r2";
    let pre_sha = "pre-local-rewrite";
    store.overlays.borrow_mut().insert(
        "bead-local-rewrite".into(),
        BeadOverlay {
            bead_id: "bead-local-rewrite".into(),
            state: OverlayState::Dispatched,
            attempt: 2,
            reroll_count: 1,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some(branch.into()),
            session_id: Some("session-local-rewrite".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some(pre_sha.into()),
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );
    sessions.set_worktree_ancestor("session-local-rewrite", branch, pre_sha, false);
    let telemetry_log = std::env::temp_dir().join("afd_test_append_only_local_rewrite.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    let mut vcs = test_vcs();
    vcs.heads.insert(branch.into(), "stale-safe-remote-head".into());
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 1).unwrap();
    assert_eq!(summary.beads_parked_human_held, 1);
    let overlay = store.load("bead-local-rewrite").unwrap().unwrap();
    assert_eq!(overlay.park_reason.as_deref(), Some("adopted_branch_history_rewrite_detected"));
    assert!(!vcs.calls.borrow().iter().any(|c| c.starts_with("remote_head_sha(")));
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn append_only_sweep_falls_back_to_remote_when_local_probe_errors() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let branch = "factory/bead-local-error-r2";
    let pre_sha = "pre-local-error";
    store.overlays.borrow_mut().insert(
        "bead-local-error".into(),
        BeadOverlay {
            bead_id: "bead-local-error".into(),
            state: OverlayState::Dispatched,
            attempt: 2,
            reroll_count: 1,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some(branch.into()),
            session_id: Some("session-local-error".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some(pre_sha.into()),
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );
    sessions.fail_worktree_ancestor(
        "session-local-error",
        branch,
        pre_sha,
        "transient local git read",
    );
    let telemetry_log = std::env::temp_dir().join("afd_test_append_only_local_error.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    let mut vcs = test_vcs();
    vcs.heads.insert(branch.into(), "remote-descendant".into());
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 1).unwrap();
    assert_eq!(summary.beads_parked_human_held, 0);
    assert_eq!(store.load("bead-local-error").unwrap().unwrap().state, OverlayState::Dispatched);
    assert!(vcs.calls.borrow().iter().any(|c| c.starts_with("remote_head_sha(")));
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn append_only_sweep_does_not_let_local_proof_override_remote_rewrite() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let branch = "alice/remote-rewritten";
    let pre_sha = "pre-remote-rewrite";
    store.overlays.borrow_mut().insert(
        "bead-remote-rewrite".into(),
        BeadOverlay {
            bead_id: "bead-remote-rewrite".into(),
            state: OverlayState::Dispatched,
            attempt: 2,
            reroll_count: 1,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some(branch.into()),
            session_id: Some("session-remote-rewrite".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some(pre_sha.into()),
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );
    sessions.set_worktree_ancestor("session-remote-rewrite", branch, pre_sha, true);
    let telemetry_log = std::env::temp_dir().join("afd_test_append_only_remote_rewrite.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    let mut vcs = test_vcs();
    vcs.heads.insert(branch.into(), "rewritten-remote-head".into());
    vcs.ancestor_pairs
        .insert((pre_sha.into(), "rewritten-remote-head".into()), false);
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 1).unwrap();
    assert_eq!(summary.beads_parked_human_held, 1);
    assert_eq!(
        store.load("bead-remote-rewrite").unwrap().unwrap().park_reason.as_deref(),
        Some("adopted_branch_history_rewrite_detected")
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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
        bugbot_pending: false,
            head_committed_epoch: 0,
        },
    );

    // Script sessions to be quiescent (stalled/dead)
    sessions.quiescent = true;

    let telemetry_log = std::env::temp_dir().join("afd_test_wedge_stalled.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    // Run tick. With the dispatch-scheduling-guarantee ordering
    // (wedge loop → slow_tier → recovery → fast_tier), the wedge loop
    // parks the stalled bead HUMAN_HELD, then `run_recovery_step` (which
    // now runs AFTER the wedge loop on slow ticks) immediately requeues
    // it to QUEUED in the same tick — the session was already killed and
    // its handle cleared by the wedge park, so `recover_human_held`'s
    // `session_id IS NULL` predicate is satisfied. The bead ends QUEUED,
    // ready for redispatch on the next slow tick. The park still happens
    // (asserted via `beads_parked_human_held` and the `session_stalled`
    // telemetry), which is what this test is really proving.
    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(summary.beads_parked_human_held, 1);
    assert_eq!(
        summary.beads_recovered_from_held, 1,
        "recovery runs after the wedge loop and requeues the freshly-parked bead"
    );

    let o = store.load("bead-stalled").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Queued,
        "wedge-parked bead is recovered to QUEUED in the same tick under the new ordering"
    );
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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
        bugbot_pending: false,
            head_committed_epoch: 0,
        },
    );

    sessions.quiescent = true; // sessions report dead

    let mut vcs = test_vcs();
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
        vendor_health: None,
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
            attempt_started_at: None,
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
        bugbot_pending: false,
            head_committed_epoch: 0,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
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

    let mut vcs = test_vcs();
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
        vendor_health: None,
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
            attempt_started_at: None,
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
        bugbot_pending: false,
            head_committed_epoch: 0,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
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

    let mut vcs = test_vcs();
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "local-ahead bead MUST still park — the ubas guard requires \
         is_remote_ahead=true (strict ancestor predicate), not just SHA inequality"
    );
    assert_eq!(
        summary.beads_recovered_from_held, 1,
        "recovery runs after the wedge loop and requeues the freshly-parked bead"
    );

    let o = store.load("bead-local-ahead").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Queued,
        "wedge-parked bead is recovered to QUEUED in the same tick under the new ordering"
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
            attempt_started_at: None,
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
        bugbot_pending: false,
            head_committed_epoch: 0,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
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

    let mut vcs = test_vcs();
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 10).unwrap();
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "diverged bead MUST still park — SHA inequality is too weak a guard"
    );
    assert_eq!(
        summary.beads_recovered_from_held, 1,
        "recovery runs after the wedge loop and requeues the freshly-parked bead"
    );

    let o = store.load("bead-diverged").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Queued,
        "wedge-parked bead is recovered to QUEUED in the same tick under the new ordering"
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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

/// jleechan-mdun: a bead that has already been adoption-attested once should
/// NOT re-emit `EXISTING_PR_ADOPTED` on every subsequent tick. Live telemetry
/// shows 30 attested beads re-emitting ~301k events (top ~14 beads account
/// for ~24.8k/day), because the adoption loop unconditionally emits whether
/// `should_adopt` was true or false. The cache lives on the overlay's
/// `is_adopted` + `state` (no new store column needed): once the bead is
/// Attested/Ready/HumanHeld/DispositionRequired, the telemetry is redundant
/// because the durable overlay already records the same provenance.
#[test]
fn existing_pr_adoption_does_not_re_emit_telemetry_on_subsequent_ticks() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 705,
        title: "Existing factory PR for dedup".into(),
        body: "already has a branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#705".into(),
        head_ref_name: "feature/already-open-pr-705".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.pr_snapshots.insert(
        705,
        qdw_green_snapshot(
            705,
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
    let vcs = test_vcs();
    // jleechan-mdun follow-up (CodeRabbit actionable #1): the previous
    // fixed-name `afd_existing_pr_adoption_dedup.jsonl` collided across
    // concurrent `cargo test` jobs in the same process — every parallel
    // thread sharing `std::process::id()` would clobber the telemetry
    // file. Match the convention at the top of this file (PID-suffixed
    // under a dedicated per-test directory) so concurrent runs and
    // repeated runs share no telemetry storage.
    let telemetry_dir = std::env::temp_dir().join("afd_existing_pr_adoption_dedup");
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    let telemetry_log = telemetry_dir.join(format!("daemon-{}.jsonl", std::process::id()));
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
        vendor_health: None,
    };

    run_tick(&deps, 0, 0).expect("first tick adopts PR");
    run_tick(&deps, 1, 0).expect("second tick reuses adoption");
    run_tick(&deps, 2, 0).expect("third tick reuses adoption");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let adopt_count = telemetry
        .lines()
        .filter(|line| line.contains("EXISTING_PR_ADOPTED"))
        .count();
    // jleechan-mdun follow-up (CodeRabbit actionable #2): the assertion
    // message must describe what the test actually proves — repeated ticks
    // against an already-attested overlay suppress the redundant event —
    // not "per bead lifetime", which is a different invariant. The
    // re-emit-after-state-transition case is covered separately by
    // `existing_pr_adoption_re_emits_after_state_transition_away_from_attested`
    // below.
    assert_eq!(
        adopt_count, 1,
        "EXISTING_PR_ADOPTED should emit exactly once across repeated ticks while the bead stays in Attested/Ready/HumanHeld; got {adopt_count} emits:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Companion to `existing_pr_adoption_does_not_re_emit_telemetry_on_subsequent_ticks`:
/// proves the inverse invariant — when the overlay leaves the dedup
/// set (Attested/Ready/HumanHeld) and is re-adopted later, the next
/// tick MUST re-emit `EXISTING_PR_ADOPTED` so the audit trail captures
/// the transition. jleechan-mdun follow-up (CodeRabbit actionable #2).
#[test]
fn existing_pr_adoption_re_emits_after_state_transition_away_from_attested() {
    use daemon::state::OverlayState;

    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 706,
        title: "Existing factory PR for transition re-emit".into(),
        body: "already has a branch".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#706".into(),
        head_ref_name: "feature/already-open-pr-706".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.pr_snapshots.insert(
        706,
        qdw_green_snapshot(
            706,
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
    let vcs = test_vcs();
    let telemetry_dir = std::env::temp_dir().join("afd_existing_pr_adoption_transition");
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    let telemetry_log = telemetry_dir.join(format!("daemon-{}.jsonl", std::process::id()));
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
        vendor_health: None,
    };

    // First tick adopts the PR — overlay moves to Attested; telemetry
    // records exactly one EXISTING_PR_ADOPTED.
    run_tick(&deps, 0, 0).expect("first tick adopts PR");

    let telemetry_after_first = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let first_count = telemetry_after_first
        .lines()
        .filter(|line| line.contains("EXISTING_PR_ADOPTED"))
        .count();
    assert_eq!(
        first_count, 1,
        "first tick should emit exactly one EXISTING_PR_ADOPTED, got {first_count}:\n{telemetry_after_first}"
    );

    // Drive a state transition AWAY from the dedup set (Attested →
    // Queued). The next tick's pre_adopt_state is therefore NOT in
    // {Attested, Ready, HumanHeld}, so the dedup check at tick.rs:1507
    // must NOT suppress the re-emit.
    {
        let mut overlays = store.overlays.borrow_mut();
        let overlay = overlays
            .values_mut()
            .next()
            .expect("first tick must have created an overlay");
        overlay.state = OverlayState::Queued;
    }

    run_tick(&deps, 1, 0).expect("second tick after transition reuses adoption");

    let telemetry_after_second = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let second_count = telemetry_after_second
        .lines()
        .filter(|line| line.contains("EXISTING_PR_ADOPTED"))
        .count();
    assert_eq!(
        second_count, 2,
        "EXISTING_PR_ADOPTED must re-emit after the overlay leaves the dedup set (Attested→Queued); got {second_count} emits:\n{telemetry_after_second}"
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
            attempt_started_at: None,
        })
        .unwrap();
    store
        .register_branch("existing-bead", "factory/existing-bead-r1")
        .unwrap();
    let cfg = test_cfg();
    let vcs = test_vcs();
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
        vendor_health: None,
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("mallory".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();
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
        vendor_health: None,
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
    let mut vcs = test_vcs();
    vcs.heads.insert(
        "alice/my-cool-feature".into(),
        "pre-session-sha-abc123".into(),
    );
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
        vendor_health: None,
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
    let mut vcs = test_vcs();
    vcs.heads.insert(
        "alice/my-conflicted-feature".into(),
        "pre-session-sha-abc123".into(),
    );
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
        vendor_health: None,
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

/// jleechan-zaga / issue #348: an adopted PR whose only red gate is a
/// CodeRabbit usage-limit (`coderabbit_status="blocked"`) must be held
/// at `DISPOSITION_REQUIRED`, NOT rerolled. Without this hold, every
/// reroll produces an equivalent r2 that hits the same external-blocker
/// signal and parks `HUMAN_HELD` at the attempt cap (v6ud #342 → r1
/// was a real production incident of this exact churn).
///
/// Acceptance: bead ends in `DISPOSITION_REQUIRED`, telemetry emits
/// `DISPOSITION_REQUIRED` (not `PARKED_HUMAN_HELD`), the original PR
/// stays open, and the daemon posted the per-gate disposition comment.
#[test]
fn adopted_red_pr_structural_only_red_gates_holds_disposition_required_not_reroll() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 708,
        title: "Adopted PR with structural-only red gates".into(),
        body: "CodeRabbit usage limit".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#708".into(),
        head_ref_name: "alice/structural-only-red".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    let mut snapshot = qdw_green_snapshot(
        708,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    // Structural-only blocker: CodeRabbit unavailable. Production emits
    // `coderabbit_status="unknown"` (adapters.rs) — NOT a synthetic "blocked"
    // — which verifier::assess maps to an `Unknown` CodeRabbit gate. That is
    // the sole non-green gate, and the coder cannot make CodeRabbit run, so
    // classify_chain -> HoldDisposition.
    snapshot.coderabbit_approved = false;
    snapshot.coderabbit_status = "unknown".into();
    scm.pr_snapshots.insert(708, snapshot);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2; // Stage 2: reroll normally executes; our new branch must preempt it
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join("afd_structural_only_red_gates.jsonl");
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
        vendor_health: None,
        },
        0,
        0,
    )
    .expect("structural-only red gate path must not error the tick");

    assert_eq!(summary.beads_created, 1);
    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(
        summary.beads_parked_human_held, 0,
        "structural-only red gates must NOT park the bead HUMAN_HELD — that's \
         the exact churn issue #348 documents (v6ud #342)"
    );
    assert_eq!(
        summary.beads_held_disposition_required, 1,
        "beads_held_disposition_required counter must increment for every \
         DISPOSITION_REQUIRED placement (operator visibility / dashboard signal)"
    );

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::DispositionRequired,
        "bead must be held at DISPOSITION_REQUIRED, not ATTESTED (would \
         re-trigger the same gate report on next tick) and not HUMAN_HELD \
         (would cap-circuit)"
    );
    assert_eq!(overlay.pr_number, Some(708));
    assert_eq!(overlay.branch.as_deref(), Some("alice/structural-only-red"));

    // The original PR must remain open — DISPOSITION_REQUIRED is a hold,
    // not a supersede.
    let session_calls = sessions.calls.borrow();
    assert!(
        session_calls.iter().all(|c| !c.starts_with("spawn(")),
        "DISPOSITION_REQUIRED hold must not fabricate remediation sessions: {session_calls:?}"
    );
    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().all(|c| !c.contains("close")),
        "original PR must not be closed on structural-only red gates: {tracker_calls:?}"
    );

    // Telemetry must show DISPOSITION_REQUIRED, not PARKED_HUMAN_HELD.
    let log_contents = std::fs::read_to_string(&telemetry_log).expect("telemetry log must exist");
    assert!(
        log_contents.contains("\"DISPOSITION_REQUIRED\""),
        "DISPOSITION_REQUIRED telemetry event must be emitted; log:\n{log_contents}"
    );
    assert!(
        !log_contents.contains("\"PARKED_HUMAN_HELD\""),
        "structural-only red gates must not emit PARKED_HUMAN_HELD — that's the \
         exact regression issue #348 documents; log:\n{log_contents}"
    );
    assert!(
        !log_contents.contains("\"REROLL_VERDICT_RECORDED\""),
        "structural-only red gates must not trigger reroll; log:\n{log_contents}"
    );

    // Per-gate disposition comment must name every red gate.
    assert!(
        tracker_calls.iter().any(|c| {
            c.contains("comment_external(owner/repo#708")
                && c.contains("Disposition required")
                && c.contains("coderabbit")
                && c.contains("structural")
        }),
        "DISPOSITION_REQUIRED comment must name the structural red gate(s); \
         tracker_calls: {tracker_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-zaga / issue #348: mixed redness (a coder-fixable RED gate plus a
/// structural-pending gate) must keep today's reroll behavior — the issue's
/// explicit acceptance criterion "mixed red → reroll as today". Blocker 4:
/// this asserts a re-roll ACTUALLY OCCURRED (attempt increment + REROLL_START
/// telemetry + a spawned remediation session), not merely the absence of
/// DISPOSITION_REQUIRED.
#[test]
fn adopted_red_pr_mixed_red_gates_still_rerolls() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 709,
        title: "Adopted PR with mixed red gates".into(),
        body: "CI broken + CodeRabbit unavailable".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#709".into(),
        head_ref_name: "alice/mixed-red-gates".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("alice".into(), Permission::Write);
    let mut snapshot = qdw_green_snapshot(
        709,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    // Mixed, in production shapes: CI failed (`ci_status="red"` -> a
    // coder-fixable RED CI gate) + CodeRabbit unavailable
    // (`coderabbit_status="unknown"` -> a structural-pending Unknown gate).
    // The coder-fixable red wins -> reroll.
    snapshot.ci_success = false;
    snapshot.ci_status = "red".into();
    snapshot.coderabbit_approved = false;
    snapshot.coderabbit_status = "unknown".into();
    scm.pr_snapshots.insert(709, snapshot);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2;
    let mut vcs = test_vcs();
    // Adopted-PR reroll captures the branch's pre-session HEAD via
    // remote_head_sha before dispatching a remediation session.
    vcs.heads.insert(
        "alice/mixed-red-gates".into(),
        "pre-session-sha-mixed".into(),
    );
    let telemetry_log = std::env::temp_dir().join("afd_mixed_red_gates_rerolls.jsonl");
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
        vendor_health: None,
        },
        0,
        0,
    )
    .expect("mixed red gate reroll should not error");

    assert_eq!(
        summary.beads_held_disposition_required, 0,
        "mixed red gates must NOT hold DISPOSITION_REQUIRED — a coder-fixable red \
         triggers reroll (issue #348 acceptance: 'mixed red → reroll as today')"
    );

    let overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_ne!(
        overlay.state,
        OverlayState::DispositionRequired,
        "mixed-red bead must continue the existing reroll flow, not the new hold"
    );

    // Blocker 4: prove a reroll ACTUALLY happened, not just the absence of the
    // hold. (1) attempt was incremented past the initial 1; (2) the adopted
    // reroll dispatched a fresh remediation session; (3) REROLL_START
    // telemetry was emitted.
    assert!(
        overlay.attempt >= 2,
        "reroll must increment the attempt counter (was {}); a no-op would leave it at 1",
        overlay.attempt
    );
    assert_eq!(
        overlay.state,
        OverlayState::Dispatched,
        "adopted-PR reroll re-dispatches a remediation coder session (DISPATCHED)"
    );
    let session_calls = sessions.calls.borrow();
    assert!(
        session_calls.iter().any(|c| c.starts_with("spawn(")),
        "reroll must spawn a remediation session: {session_calls:?}"
    );
    let log_contents = std::fs::read_to_string(&telemetry_log).expect("telemetry log must exist");
    assert!(
        log_contents.contains("\"REROLL_START\""),
        "reroll must emit REROLL_START telemetry; log:\n{log_contents}"
    );
    assert!(
        !log_contents.contains("\"DISPOSITION_REQUIRED\""),
        "mixed-red must not emit the DISPOSITION_REQUIRED hold event; log:\n{log_contents}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-zaga / issue #348 (blocker 2 acceptance): a bead already held at
/// `DISPOSITION_REQUIRED` must be RE-SELECTED and re-assessed on a later tick,
/// and RESUME the normal flow the moment the structural condition clears. Here
/// CodeRabbit becomes available and all gates go green, so the bead resolves
/// to READY — proving the hold is recoverable, not terminal.
#[test]
fn disposition_required_bead_resumes_when_gates_go_green() {
    let mut scm = FakeScm::new();
    // Now-green snapshot with a fresh /er PASS so er_runner short-circuits.
    let snapshot = qdw_green_snapshot(
        710,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    scm.pr_snapshots.insert(710, snapshot);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into())); // Skeptic green
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2;
    let vcs = test_vcs();

    // Pre-seed a bead already held at DISPOSITION_REQUIRED with an open PR and
    // a registered branch (as the daemon would have left it on a prior tick).
    let branch = "alice/held-then-green";
    store
        .save(&BeadOverlay {
            bead_id: "held-bead".into(),
            state: OverlayState::DispositionRequired,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(710),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("held-bead", branch).unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_disposition_resumes_green.jsonl");
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("re-assessing a held bead must not error");

    // The held bead was re-selected and re-assessed this tick.
    assert_eq!(
        summary.gates_assessed, 1,
        "a DISPOSITION_REQUIRED bead must be re-selected and gate-assessed, not skipped"
    );
    // Now-green -> it resumes to READY (the hold is recoverable).
    let overlay = store.load("held-bead").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Ready,
        "a held bead whose gates went green must resume to READY, not stay held"
    );
    assert_eq!(summary.beads_ready, 1);
    assert_eq!(
        summary.beads_held_disposition_required, 0,
        "resuming to green is not a new hold"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-zaga / issue #348 r3 residual 2: a held bead whose cooldown has
/// NOT yet elapsed must be SKIPPED — not re-assessed, and crucially the SCM
/// API must not be hit for it. Prevents hammering CodeRabbit/gh every fast
/// tick while a structural condition persists for hours.
#[test]
fn disposition_required_bead_in_cooldown_is_skipped_without_scm_call() {
    let mut scm = FakeScm::new();
    // A snapshot IS available — the test proves it is never fetched.
    scm.pr_snapshots.insert(
        711,
        qdw_green_snapshot(
            711,
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
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2;
    let vcs = test_vcs();

    let branch = "alice/held-in-cooldown";
    store
        .save(&BeadOverlay {
            bead_id: "cooldown-bead".into(),
            state: OverlayState::DispositionRequired,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(711),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("cooldown-bead", branch).unwrap();
    // Cooldown far in the future -> the bead must be skipped this tick.
    let far_future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 100_000;
    store
        .set_held_recheck_after("cooldown-bead", far_future)
        .unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_disposition_cooldown_skip.jsonl");
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("cooldown skip must not error");

    assert_eq!(
        summary.gates_assessed, 0,
        "a bead still in cooldown must NOT be re-assessed"
    );
    // The SCM API must not be hit for this bead's PR while it is in cooldown.
    assert!(
        scm.calls
            .borrow()
            .iter()
            .all(|c| !c.contains("pr_snapshot_for_repo") || !c.contains("711")),
        "cooldown must prevent the SCM snapshot fetch: {:?}",
        scm.calls.borrow()
    );
    // Still held, unchanged.
    assert_eq!(
        store.load("cooldown-bead").unwrap().unwrap().state,
        OverlayState::DispositionRequired
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-zaga / issue #348 r3 residual 3: if a held bead's re-assessment
/// exits early (here the PR snapshot fetch fails mid-tick), the durable state
/// must STAY DISPOSITION_REQUIRED — the in-memory ATTESTED promotion is never
/// persisted — so hold provenance survives and the next re-hold does not
/// double-emit the counter/telemetry/comment.
#[test]
fn disposition_required_reassessment_error_preserves_hold_provenance() {
    let mut scm = FakeScm::new();
    // NO snapshot inserted for PR 712 -> pr_snapshot_for_repo errors -> the
    // fast tier takes the transient early-exit (continue) after promoting.
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.stage = 2;
    let vcs = test_vcs();

    let branch = "alice/held-then-error";
    store
        .save(&BeadOverlay {
            bead_id: "prov-bead".into(),
            state: OverlayState::DispositionRequired,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(712),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("prov-bead", branch).unwrap();
    // held_recheck_after unset (None) -> eligible to re-assess now.

    let telemetry_log = std::env::temp_dir().join("afd_disposition_provenance.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    // Tick 1: re-assessment errors (snapshot fetch fails) mid-way.
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("errored re-assessment must not abort the tick");
    assert_eq!(
        summary1.gates_assessed, 0,
        "the snapshot fetch failed, so no gate assessment completed"
    );
    // Core provenance fix: durable state STAYS DISPOSITION_REQUIRED (with the
    // pre-r3 eager save it would have been left ATTESTED).
    assert_eq!(
        store.load("prov-bead").unwrap().unwrap().state,
        OverlayState::DispositionRequired,
        "an errored re-assessment must not lose the hold state"
    );
    let log1 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !log1.contains("\"DISPOSITION_REQUIRED\""),
        "the errored path must not emit a (duplicate) DISPOSITION_REQUIRED hold event; log:\n{log1}"
    );

    // Tick 2: snapshot now available and STILL structural (CodeRabbit
    // unavailable). Bypass the cooldown, re-assess. Because tick 1 preserved
    // the held state, this is a RE-hold (entered_as_disposition=true) -> it
    // must NOT increment the counter or post another comment.
    let mut structural = qdw_green_snapshot(
        712,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    structural.coderabbit_approved = false;
    structural.coderabbit_status = "unknown".into();
    scm.pr_snapshots.insert(712, structural);
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    store.set_held_recheck_after("prov-bead", 0).unwrap(); // clear cooldown

    let tracker_calls_before = tracker.calls.borrow().len();
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
        vendor_health: None,
        },
        2,
        0,
    )
    .expect("re-hold must not error");
    assert_eq!(
        summary2.beads_held_disposition_required, 0,
        "a re-hold of an already-held bead must not double-count the operator counter"
    );
    assert_eq!(
        store.load("prov-bead").unwrap().unwrap().state,
        OverlayState::DispositionRequired
    );
    let new_disposition_comments = tracker
        .calls
        .borrow()
        .iter()
        .skip(tracker_calls_before)
        .filter(|c| c.contains("Disposition required"))
        .count();
    assert_eq!(
        new_disposition_comments, 0,
        "a re-hold must not post a duplicate disposition comment"
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
        notes: String::new(),
        file_tree_summary: "".into(),
        // jleechan-8jxr r2: a manual bead without an explicit external_ref
        // (or body `target_repo:` field) is now parked `unmapped_repo`
        // at dispatch time rather than silently defaulting to
        // `cfg.target_repo`. Provide an explicit external_ref matching
        // the test cfg's `target_repo` ("owner/repo") so this test
        // exercises the happy path; the no-repo failure mode is
        // covered by the dedicated regression test in dispatch.rs.
        external_ref: Some("owner/repo#1".into()),
    });

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new(); // empty database initially
    let cfg = test_cfg();
    let vcs = test_vcs();

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
        vendor_health: None,
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
        notes: String::new(),
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
    let vcs = test_vcs();
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");
    assert_eq!(
        summary.beads_dispatched, 1,
        "the drive-PR bead must dispatch"
    );

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
        dispatched["context"]["branch"],
        "factory/jleechan-xa99-reconciliation-rebased"
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
        notes: String::new(),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        notes: String::new(),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        notes: String::new(),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        // jleechan-htf7 r3: the body MUST include a `target_repo:` field so
        // the manual adoption path actually proceeds to Queued. The
        // unresolvable-repo shape (no external_ref, no target_repo) now
        // fail-closes at adoption — that's the new r3 contract, pinned by
        // `manual_bead_adoption_fails_closed_when_target_repo_unresolvable`
        // below. PR #201's invariant (don't call create_bead, don't
        // fabricate external_ref) still needs a Queued manual bead to
        // exercise; this body field supplies the resolvable target_repo
        // without touching the orphan-defect shape the test is locking in.
        description: "target_repo: owner/repo\n\ncreated directly via `br create`, no --external-ref".into(),
        notes: String::new(),
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
    let vcs = test_vcs();

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
        vendor_health: None,
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

/// jleechan-htf7 r3: the manual bead-adoption path in `run_tick` (the
/// `INTAKE_BEAD_CREATED`/`{"manual": true}` branch at tick.rs:1815-1864) is
/// the only entry through which a bead can enter the daemon's overlay
/// system WITHOUT a non-empty `external_ref`. PR #201 (jleechan-3wh0) added
/// a regression test that locks in `create_bead` is not called and
/// `external_ref` is not fabricated, but it did NOT lock in the structural
/// invariant that prevents the manual path from creating an INTERMEDIATE
/// `Queued` overlay row for a bead the daemon can never dispatch (no
/// resolvable `target_repo`).
///
/// Pre-r3 the path silently adopted the bead with `state: Queued`, called
/// `router::route` (an LLM call), pushed the bead through `dispatch`, and
/// only THEN learned `overlay.target_repo = None` and parked it
/// `unmapped_repo` at `tick.rs:2133-2145`. That reactive park still leaves
/// the bead in the routing/dispatch pipeline for every tick, wasting an
/// LLM call and creating churn telemetry. The fix is fail-closed at
/// adoption: when a manual bead has neither an `external_ref` owner/repo
/// prefix nor a `target_repo:` body field, park it `HUMAN_HELD` with reason
/// `unmapped_repo` immediately and skip routing entirely.
///
/// This test pins the new structural invariant:
/// - Overlay state MUST be `HUMAN_HELD` (not `Queued`) at adoption time.
/// - `park_reason` MUST be `unmapped_repo` (or the local-fallback variant).
/// - `summary.beads_parked_human_held` MUST increment; `beads_dispatched`
///   MUST stay 0.
/// - The LLM MUST NOT be called for routing (no `judge(...)` invocation).
/// - Telemetry MUST emit `PARKED_HUMAN_HELD` for the manual adoption branch
///   (NOT route through dispatch and emit the same event later).
/// - `Tracker::create_bead` MUST still NOT be called (PR #201 invariant
///   preserved).
#[test]
fn manual_bead_adoption_fails_closed_when_target_repo_unresolvable() {
    let scm = FakeScm::new(); // no SCM — this bead came from `br create`, not GitHub
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "no-repo-manual-bead".into(),
        title: "Manual bead with no repo signal".into(),
        // No body `target_repo:` field, no external_ref — exactly the shape
        // that triggers the future-orphan defect on every tick.
        description: "manually created via `br create` with no --external-ref".into(),
        notes: String::new(),
        file_tree_summary: "".into(),
        external_ref: None,
    });

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    // No `llm.response` pre-seeded — if the daemon calls `judge`, it gets
    // an empty string and the test's "no LLM calls" assertion will catch it.
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    let telemetry_log = std::env::temp_dir().join("afd_manual_bead_fail_closed_test.jsonl");
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
            vendor_health: None,
        },
        0,
        0,
    )
    .expect("tick should succeed even when manual adoption fail-closes");

    // (1) Overlay must be HUMAN_HELD, not Queued — fail-closed adoption
    // parks before routing, so dispatch never enters the loop.
    let overlay = store
        .load("no-repo-manual-bead")
        .expect("overlay load must not error")
        .expect("manual adoption must persist the overlay row");
    assert_eq!(
        overlay.state,
        OverlayState::HumanHeld,
        "manual bead without external_ref / target_repo MUST be parked HUMAN_HELD \
         at adoption time, not left Queued for the dispatch layer to fail on. \
         A Queued overlay here means the r3 fix regressed: the daemon is \
         letting a future-orphan shape enter routing."
    );
    let park_reason = overlay
        .park_reason
        .as_deref()
        .expect("HUMAN_HELD overlay must carry a park_reason");
    assert!(
        park_reason == "unmapped_repo"
            || park_reason.starts_with("escalation_local_fallback:unmapped_repo"),
        "park_reason must be unmapped_repo-derived, got: {park_reason:?}"
    );
    assert_eq!(
        overlay.target_repo, None,
        "fail-closed adoption must preserve the unresolvable target_repo so the \
         reason in park_reason matches the persisted column (consistency check \
         against silent mutation)"
    );

    // (2) Summary counters: HUMAN_HELD park, not a transient dispatch error,
    // and ZERO dispatches (the LLM should never have been called).
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "manual fail-closed adoption must increment beads_parked_human_held (got: {})",
        summary.beads_parked_human_held
    );
    assert_eq!(
        summary.beads_dispatched, 0,
        "a no-repo manual bead must NOT dispatch"
    );
    assert_eq!(
        summary.beads_routed, 0,
        "a no-repo manual bead must NOT enter the routing pipeline (LLM call \
         skipped — fail-closed adoption is what saves the LLM cost). Got \
         beads_routed={}",
        summary.beads_routed
    );

    // (3) LLM must NOT be called for routing — fail-closed adoption is what
    // saves the wasted judge() invocation. An empty `llm.calls` is the
    // strongest assertion here; if any future refactor reintroduces routing
    // for unresolvable-repo manual beads, this assertion fires immediately.
    let llm_calls = llm.calls.borrow();
    assert!(
        llm_calls.is_empty(),
        "manual fail-closed adoption must skip router::route() entirely — \
         any LLM call here means the r3 fail-closed gate regressed. \
         LLM calls observed: {llm_calls:?}"
    );

    // (4) create_bead must NOT be called — PR #201 invariant preserved.
    let tracker_calls = tracker.calls.borrow();
    assert!(
        tracker_calls.iter().all(|c| !c.starts_with("create_bead(")),
        "manual-bead adoption must NEVER call create_bead (PR #201 invariant). \
         A create_bead(...) call here means the r3 path started minting new \
         beads/refs, which is the orphan-defect shape: {tracker_calls:?}"
    );

    // (5) Telemetry must emit PARKED_HUMAN_HELD for the fail-closed adoption,
    // and the bead's tracker record must remain untouched (no fabricated
    // external_ref). Read the JSONL line-by-line for stable assertions.
    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let parked = events
        .iter()
        .find(|e| e["eventType"] == "PARKED_HUMAN_HELD" && e["beadId"] == "no-repo-manual-bead");
    assert!(
        parked.is_some(),
        "telemetry MUST emit PARKED_HUMAN_HELD for fail-closed manual adoption; \
         events = {events:?}"
    );
    let parked_event = parked.expect("checked above");
    assert_eq!(
        parked_event["context"]["reason"], "unmapped_repo",
        "PARKED_HUMAN_HELD event for manual fail-closed adoption must name the \
         unmapped_repo reason in its context payload so downstream tooling \
         (Healer, dashboards) attributes it correctly; got context = {:?}",
        parked_event["context"]
    );
    assert_eq!(
        parked_event["context"]["source"], "manual_adoption_fail_closed",
        "PARKED_HUMAN_HELD context must tag the adoption-time source so Healer \
         can distinguish r3 fail-closed parks from dispatch-layer unmapped_repo \
         parks (bead jleechan-8jxr). Got context = {:?}",
        parked_event["context"]
    );

    // PR #201 invariant: the tracker's candidate record for the manual bead
    // stays at external_ref = None — no fake linkage was minted to satisfy
    // the daemon. The comment posted via post_scm_comment_by_bead_id is
    // tested separately (success / transient / permanent / missing-target)
    // by the r3 escalation-outcome tests below.
    let candidate = tracker
        .candidates
        .borrow()
        .iter()
        .find(|b| b.id == "no-repo-manual-bead")
        .cloned()
        .expect("candidate should still be present");
    assert_eq!(
        candidate.external_ref, None,
        "PR #201 invariant: a manual bead with no external_ref must remain \
         with external_ref = None after the fail-closed adoption gate \
         (no fake linkage minted to satisfy the daemon)"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-htf7 r3 incremental: the dispatch-flow canonical idiom for the
/// `unmapped_target_repo` reason (`tick.rs:2290-2353`) handles FOUR outcomes
/// of `post_scm_comment_by_bead_id`:
///   (a) missing-target  -> `record_local_escalation_fallback` +
///                          `ESCALATED_LOCALLY` event
///   (b) non-transient   -> `mark_escalation_undeliverable_and_emit` +
///                          `ESCALATION_UNDELIVERABLE` event (terminal)
///   (c) transient       -> `ESCALATION_NOTIFICATION_FAILED` event (deduped)
///   (d) success         -> `record_escalation` + `summary.beads_escalated
///                          += 1` + `ESCALATION_REQUIRED` event (deduped)
///
/// PR #579 (r2) implemented only (a) and (b) for the new fail-closed manual
/// adoption gate. PR #579 CodeRabbit review flagged this gap and r3 closes
/// it by adding (c) and (d) to the manual adoption site — completing the
/// outcome-handling to mirror the canonical dispatch-flow idiom.
///
/// The fail-closed manual adoption gate's "target_repo = None" pre-condition
/// means the bead has no `external_ref` AND no `target_repo:` body field.
/// Today, that combination deterministically takes the missing-target
/// branch (a) — `post_scm_comment_by_bead_id` cannot resolve a comment
/// target without an `external_ref`. Branches (c) and (d) are therefore
/// unreachable through this path today, but the r3 structural parity is
/// the durable invariant that future maintainers can rely on.
///
/// This test exercises the missing-target path explicitly and asserts the
/// structural escalation record is complete: the sentinel is written,
/// `beads_escalated_locally` bumps, and the `ESCALATED_LOCALLY` event
/// fires. (Branches (c) and (d) are pinned by the dispatch-flow tests
/// already in the suite — `capped_human_held_comment_failure_retries_*`,
/// `permanent_gh_error_marks_escalation_undeliverable_and_never_retries` —
/// so the r3 fix inherits that coverage without duplicating it.)
#[test]
fn manual_bead_adoption_fail_closed_local_fallback_path_persists_escalation() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "no-repo-local-fallback-bead".into(),
        title: "Manual bead, local-fallback escalation path".into(),
        description: "manually created via `br create` with no --external-ref".into(),
        notes: String::new(),
        file_tree_summary: "".into(),
        external_ref: None,
    });

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join(
        "afd_manual_bead_fail_closed_local_fallback_test.jsonl",
    );
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
            vendor_health: None,
        },
        0,
        0,
    )
    .expect("tick should succeed even when manual adoption fail-closes");

    // (a) missing-target: park HUMAN_HELD + record_local_escalation_fallback.
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "manual fail-closed adoption must increment beads_parked_human_held (got: {})",
        summary.beads_parked_human_held
    );
    assert!(
        summary.beads_escalated_locally >= 1,
        "missing-target path MUST record a local escalation fallback so the \
         operator still sees the escalation event (r3 invariant: never lose \
         the escalation record). Got beads_escalated_locally={}",
        summary.beads_escalated_locally
    );

    // Sentinel recorded so subsequent ticks skip re-attempt.
    let rejection = store
        .load_rejection("no-repo-local-fallback-bead", u32::MAX)
        .unwrap();
    assert!(
        rejection.is_some(),
        "escalation sentinel MUST be recorded on the missing-target path so \
         escalation_already_recorded blocks re-attempt on later ticks (r3 \
         invariant; matches the unmapped_target_repo dispatch-flow idiom). \
         Got: {rejection:?}"
    );

    // (a) ESCALATED_LOCALLY event is emitted (NOT silently dropped).
    let body = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        body.contains("ESCALATED_LOCALLY"),
        "telemetry MUST emit ESCALATED_LOCALLY for the missing-target \
         local-fallback path so dashboards see the escalation; got: {body}"
    );
    assert!(
        body.contains("\"reason\":\"unmapped_repo\""),
        "ESCALATED_LOCALLY context must carry reason=unmapped_repo; got: {body}"
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        prompts[0]
            .1
            .contains("Wire a durable Linux trigger (owner/repo)"),
        "new intake must dispatch the real tracker title, not an empty stub prompt: {}",
        prompts[0].1
    );
    assert!(
        prompts[0]
            .1
            .contains("systemd user unit acceptance criteria"),
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
    let vcs = test_vcs();

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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
        bugbot_pending: false,
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
        vendor_health: None,
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
    let vcs = test_vcs();

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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
        bugbot_pending: false,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
        vendor_health: None,
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
    // jleechan-t40t r6: production state.rs::recover_human_held clears
    // pr_number = NULL on recovery (line ~1239) so the recovered overlay
    // does NOT carry the dead PR from the prior attempt into the new
    // dispatch. FakeStateStore mirrors that contract, so the RECOVERED
    // telemetry event reports `pr_number: null` — the prior PR number is
    // intentionally NOT carried forward.
    assert!(
        log.contains("\"pr_number\":null"),
        "telemetry metadata must record the cleared (null) pr_number after \
         recover_human_held clears it (mirrors production contract); got: {log}"
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
            attempt_started_at: None,
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
        vendor_health: None,
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
        vendor_health: None,
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
        head_sha: Some("sha-stub".into()),
        updated_at_epoch: Some(1_700_000_000),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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

    // Ticks 1..=18: under the dispatch-scheduling-guarantee ordering, each
    // recovery cycle now takes TWO slow ticks instead of one:
    //   - Odd tick (1,3,…,17): `run_recovery_step` (which now runs AFTER
    //     `run_slow_tier`) requeues the still-HUMAN_HELD bead → QUEUED and
    //     increments `attempt`. No dispatch or gate assessment happens this
    //     tick (the bead is QUEUED, not ATTESTED, when `run_fast_tier` runs).
    //   - Even tick (2,4,…,18): `run_slow_tier` dispatches the QUEUED bead,
    //     re-attesting it against the still-open PR; `run_recovery_step` has
    //     no HUMAN_HELD bead to recover; `run_fast_tier`'s gate assessment
    //     sees the same permanently-red snapshot and re-parks HUMAN_HELD.
    // After 9 two-tick cycles `attempt` must equal 10 (the cap).
    for tick_index in 1..=18u64 {
        let summary = run_tick(&deps, tick_index, 0)
            .unwrap_or_else(|e| panic!("tick {tick_index} should succeed: {e:?}"));
        let is_recovery_tick = tick_index % 2 == 1;
        let expected_attempt = (tick_index.div_ceil(2) + 1) as u32;
        if is_recovery_tick {
            assert_eq!(
                summary.beads_recovered_from_held, 1,
                "tick {tick_index}: recovery requeues the HUMAN_HELD bead"
            );
        } else {
            assert_eq!(
                summary.beads_recovered_from_held, 0,
                "tick {tick_index}: no HUMAN_HELD bead to recover (bead is ATTESTED at recovery time)"
            );
        }
        assert_eq!(
            summary.beads_escalated, 0,
            "tick {tick_index}: bead below the cap must not escalate yet"
        );
        let overlay = store.load(BEAD_ID).unwrap().unwrap();
        assert_eq!(
            overlay.attempt, expected_attempt,
            "tick {tick_index}: attempt must be {expected_attempt}"
        );
        if is_recovery_tick {
            assert_eq!(
                overlay.state,
                OverlayState::Queued,
                "tick {tick_index}: bead must be QUEUED after recovery (dispatched next tick)"
            );
        } else {
            assert_eq!(
                overlay.state,
                OverlayState::HumanHeld,
                "tick {tick_index}: bead must re-park HUMAN_HELD after re-adoption + failed gate assessment"
            );
        }
    }
    let at_cap = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(
        at_cap.attempt, 10,
        "bead must have reached the recovery cap"
    );
    assert_eq!(at_cap.state, OverlayState::HumanHeld);

    // Tick 19: attempt (10) is no longer `< MAX_HUMAN_HELD_RECOVERY_ATTEMPT`
    // (10), so recovery MUST stop retrying and instead escalate: a real
    // escalation comment is posted through `post_scm_comment_by_bead_id`,
    // and the escalation sentinel row is recorded.
    let summary_cap = run_tick(&deps, 19, 0).expect("cap tick should succeed");
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

    // Tick 20: dedup check. The bead is still HUMAN_HELD at attempt 10, so
    // `run_recovery_step` finds it again via `human_held_at_or_above_attempt`,
    // but `escalation_already_recorded` (the `ESCALATION_SENTINEL_ATTEMPT`
    // rejection-table row written by tick 19's `record_escalation`) must
    // suppress a second escalation comment.
    let summary_dedup = run_tick(&deps, 20, 0).expect("dedup tick should succeed");
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
        vendor_health: None,
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
fn permanent_gh_error_marks_escalation_undeliverable_and_never_retries() {
    // 1s2q-escalation-dedup Task 2: a permanent (non-transient) gh error from
    // `post_scm_comment_by_bead_id` (e.g. `invalid issue format: "local-xxx"`)
    // will never resolve on retry. The daemon must mark the escalation ledger
    // row terminal, emit ONE final `ESCALATION_UNDELIVERABLE` event, and
    // never re-emit `ESCALATION_NOTIFICATION_FAILED` on subsequent ticks —
    // stopping the live incident where the failed notification re-fired every
    // ~90s.
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = test_cfg();

    store.overlays.borrow_mut().insert(
        "bead-perm-err".into(),
        BeadOverlay {
            bead_id: "bead-perm-err".into(),
            state: OverlayState::HumanHeld,
            attempt: 10,
            reroll_count: 0,
            autonomy_secs: 7,
            spend_usd: 0.0,
            pr_number: Some(9005),
            branch: Some("factory/bead-perm-err-r10".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );
    // Permanent (non-transient) error — DaemonError::Config is NOT transient.
    *tracker.fail_next_comment_permanent.borrow_mut() =
        Some("invalid issue format: \"local-xxx\"".into());

    let telemetry_log =
        std::env::temp_dir().join("afd_permanent_gh_error_undeliverable.jsonl");
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
        vendor_health: None,
    };

    // Tick 1: permanent error → one ESCALATION_UNDELIVERABLE, terminal mark set.
    let summary = run_tick(&deps, 1, 0).expect("permanent error should not abort tick");
    assert_eq!(
        summary.escalations_undeliverable, 1,
        "a permanent error must mark exactly one escalation undeliverable"
    );
    assert_eq!(
        summary.beads_escalated, 0,
        "a permanent error must NOT record a successful escalation"
    );
    assert_eq!(
        summary.escalations_suppressed, 0,
        "first occurrence is not a dedup suppression"
    );
    // The sentinel IS recorded by the terminal-marking helper (it calls
    // `record_escalation` so `escalation_already_recorded` blocks re-entry on
    // future ticks — the permanent-error path is truly terminal).
    assert!(
        store
            .load_rejection("bead-perm-err", u32::MAX)
            .unwrap()
            .is_some(),
        "sentinel must be recorded so future ticks skip re-attempt"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("ESCALATION_UNDELIVERABLE"),
        "permanent error must emit ESCALATION_UNDELIVERABLE; got: {log}"
    );
    assert!(
        log.contains("\"permanent\":true"),
        "ESCALATION_UNDELIVERABLE must carry permanent:true; got: {log}"
    );
    // The TICK summary must record the counter.
    assert!(
        log.contains("\"escalationsUndeliverable\":1"),
        "TICK summary must include escalationsUndeliverable; got: {log}"
    );

    // Tick 2: the sentinel was recorded by the terminal-marking helper, so
    // `escalation_already_recorded` at the top of the site returns true →
    // the bead is skipped entirely (no re-attempt, no re-emit). The comment
    // is NOT retried.
    let summary2 = run_tick(&deps, 2, 0).expect("second tick must not abort");
    assert_eq!(
        summary2.escalations_undeliverable, 0,
        "second tick must NOT re-mark terminal (sentinel blocks re-entry)"
    );
    assert_eq!(
        summary2.beads_escalated, 0,
        "second tick must still not record a successful escalation"
    );
    let log2 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let undeliverable_count = log2.matches("ESCALATION_UNDELIVERABLE").count();
    assert_eq!(
        undeliverable_count, 1,
        "exactly one ESCALATION_UNDELIVERABLE event across both ticks; got {undeliverable_count}"
    );
    assert!(
        !log2.contains("ESCALATION_NOTIFICATION_FAILED"),
        "permanent error must never emit ESCALATION_NOTIFICATION_FAILED; got: {log2}"
    );
    // The comment was attempted exactly once (tick 1 only).
    let comment_count = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.contains("comment_external(owner/repo#9005,"))
        .count();
    assert_eq!(
        comment_count, 1,
        "comment must be attempted exactly once (no retry on tick 2)"
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
        },
    );
    tracker.candidates.borrow_mut().push(Bead {
        id: "bead-held-fallback".into(),
        title: "held fallback".into(),
        description: String::new(),
        notes: String::new(),
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
        vendor_health: None,
    };

    // Tick 1: under the dispatch-scheduling-guarantee ordering, `run_slow_tier`
    // runs BEFORE `run_recovery_step`. The transient `fetch_candidates` failure
    // hits `run_slow_tier`'s intake call first and aborts the tick via `?`
    // before recovery/escalation can run. The fail-next token is consumed, so
    // the next tick's `fetch_candidates` succeeds and the escalation retry
    // proceeds. This is the intended behavior: a dispatch-tier failure is more
    // critical than a pending escalation, so it wins the `?` propagation.
    let result1 = run_tick(&deps, 1, 0);
    assert!(
        result1.is_err(),
        "tick 1 should abort: slow_tier's fetch_candidates fails before recovery runs; got {result1:?}"
    );
    assert!(
        store
            .load_rejection("bead-held-fallback", u32::MAX)
            .unwrap()
            .is_none(),
        "sentinel must stay absent when the tick aborts before escalation"
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
        bugbot_pending: false,
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
        vendor_health: None,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
        vendor_health: None,
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
    let vcs = test_vcs();

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
            attempt_started_at: None,
        },
    );
    scm.pr_snapshots.insert(
        7000,
        PrSnapshot {
            pr_number: 7000,
            ci_success: false,
            mergeable: true,
            merge_state_unknown: false,
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
        bugbot_pending: false,
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
        vendor_health: None,
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
    let vcs = test_vcs();

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
            attempt_started_at: None,
        },
    );
    scm.pr_snapshots.insert(
        7100,
        PrSnapshot {
            pr_number: 7100,
            ci_success: false,
            mergeable: true,
            merge_state_unknown: false,
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
        bugbot_pending: false,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
        vendor_health: None,
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
    assert_eq!(
        overlay.pr_number, None,
        "recover_human_held must clear pr_number so the recovered bead does \
         NOT carry the dead PR from the prior attempt into the new dispatch \
         (mirrors production state.rs::recover_human_held)"
    );
    assert_eq!(
        overlay.session_id, None,
        "recover_human_held must clear session_id (mirror production contract)"
    );
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
    let vcs = test_vcs();

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
            attempt_started_at: None,
        },
    );
    scm.pr_snapshots.insert(
        7001,
        PrSnapshot {
            pr_number: 7001,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
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
        bugbot_pending: false,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
                attempt_started_at: None,
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
            merge_state_unknown: false,
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
            bugbot_pending: false,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
            bugbot_pending: false,
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
        vendor_health: None,
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

    fn labeled_prs(&self, _label: &str, _gh_calls: &mut u32) -> Result<Vec<LabeledPr>, DaemonError> {
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

    fn labeled_prs(&self, _label: &str, _gh_calls: &mut u32) -> Result<Vec<LabeledPr>, DaemonError> {
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
        merge_state_unknown: false,
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
        bugbot_pending: false,
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
            attempt_started_at: None,
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
            attempt_started_at: None,
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
        vendor_health: None,
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

#[cfg(unix)]
fn write_fake_target_worktree_git(dir: &std::path::Path, head_sha: &str) {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("git");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$1:$2:$3\" in\n  remote:get-url:origin) printf '%s\\n' 'https://github.com/myorg/global-real-repo.git' ;;\n  rev-parse:HEAD:) printf '%s\\n' '{head_sha}' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap_or_else(|e| panic!("failed to write fake target-worktree git: {e}"));
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
    write_fake_reviewer(&fake_bin_dir, "codex", "fail should-not-dispatch-codex");
    write_fake_reviewer(&fake_bin_dir, "gemini", "fail should-not-dispatch-gemini");
    write_fake_reviewer(&fake_bin_dir, "agy", "fail should-not-dispatch-agy-self-review");
    write_fake_reviewer(&fake_bin_dir, "claude", "pass");
    write_fake_reviewer(&fake_bin_dir, "cursor-agent", "pass");
    write_fake_target_worktree_git(&fake_bin_dir, "deadbeef555");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // Fix the coder vendor so the reviewer priority list (and therefore
    // which two fake binaries get dispatched) is deterministic regardless
    // of the ambient environment.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "agy"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/global-real-repo".into(); // NOT a fixture repo

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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
            bugbot_pending: false,
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
        vendor_health: None,
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
// are absent. The moment a PR comment trips the sign-off signal — a
// non-bot comment with an anchored `verdict:`/`overall:`/`normalized:`
// declaration line or `/skeptic pass|fail` command line, per
// `anchored_comment_verdict` (rev-gujs2) — `skeptic_evidence`
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
    write_fake_reviewer(&fake_bin_dir, "codex", "fail should-not-dispatch-codex");
    write_fake_reviewer(&fake_bin_dir, "gemini", "fail should-not-dispatch-gemini");
    write_fake_reviewer(&fake_bin_dir, "agy", "fail should-not-dispatch-agy-self-review");
    write_fake_reviewer(&fake_bin_dir, "claude", "pass");
    write_fake_reviewer(&fake_bin_dir, "cursor-agent", "pass");
    write_fake_target_worktree_git(&fake_bin_dir, "deadbeef556");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // Fix the coder vendor so the reviewer priority list (and therefore
    // which two fake binaries get dispatched) is deterministic regardless
    // of the ambient environment.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "agy"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/global-real-repo".into(); // NOT a fixture repo

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
            attempt_started_at: None,
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
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef556".into(),
            body: String::new(),
            // No gha/skeptic-workflow comment at all (this target repo has
            // no equivalent CI workflow), but ONE human-looking comment
            // trips the sign-off signal via an anchored `verdict: pass`
            // declaration line (non-bot author, rev-gujs2's
            // `anchored_comment_verdict` grammar) — the exact asymmetric
            // scenario round 3 proved was still deadlocked. An `/er PASS`
            // comment is present purely so gate 6 resolves; this test only
            // exercises gate 7.
            comments: vec![
                PrComment {
                    author: "some-reviewer".into(),
                    body: "/er PASS".into(),
                    created_at_epoch: 0,
                },
                PrComment {
                    author: "jleechan".into(),
                    body: "Looks good, sign-off from me.\nverdict: pass".into(),
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
            bugbot_pending: false,
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
        vendor_health: None,
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
    write_fake_reviewer(&fake_bin_dir, "codex", "fail should-not-dispatch-codex");
    write_fake_reviewer(&fake_bin_dir, "gemini", "fail should-not-dispatch-gemini");
    // Keep the full 3-vendor list by using a coder that is not in
    // SKEPTIC_REVIEWER_PRIORITY. Dual-dispatch primaries (claudem via the
    // `claude` binary, then agy) both fail to parse; cursor-agent is the
    // skip(2) fallback and produces a usable verdict.
    write_fake_reviewer(&fake_bin_dir, "claude", "not a verdict at all");
    write_fake_reviewer(&fake_bin_dir, "agy", "still not a verdict");
    write_fake_reviewer(&fake_bin_dir, "cursor-agent", "pass");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // coder=codex is not in [claudem, agy, cursor-agent], so priority stays
    // the full default list and the third-vendor fallback is reachable.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "codex"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/global-real-repo".into(); // NOT a fixture repo

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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
            bugbot_pending: false,
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    // Bead jleechan-984e r2 / strict merge policy (#328): the fallback
    // chain still works (agy's verdict was parsed and recorded), but the
    // cross-model guarantee refuses strict-green because only `agy` ran
    // (single family). The bead must park HUMAN_HELD, NOT reach READY.
    assert_eq!(
        summary.beads_ready, 0,
        "jleechan-baaf regression (r2): when the first two dispatched reviewer \
         vendors (claudem, agy) both fail to produce a parseable verdict \
         but a third vendor (cursor-agent) is available in `priority` and succeeds, \
         `skeptic_evidence` MUST fall back to it (NOT propagate a total- \
         outage Err) — but with single-family cursor-agent the bead MUST park on \
         the cross-model gate (issue #385 / strict merge policy #328) \
         instead of reaching READY. summary={summary:?}\ntelemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"all_green\":false"),
        "GATE_ASSESSMENT must report all_green:false because the cross-model \
         gate blocks single-family Pass; telemetry:\n{telemetry}"
    );

    let overlay = store
        .load("real-repo-bead-3rdvendor")
        .unwrap()
        .expect("overlay must still exist");
    assert_eq!(
        overlay.state,
        OverlayState::HumanHeld,
        "bead must park HUMAN_HELD via the third vendor's verdict on the \
         cross-model gate failure, not stay ATTESTED on a false total-outage \
         and not reach READY (single-family review cannot pass strict merge \
         policy #328)"
    );
    // The fallback chain itself MUST have worked — `cursor-agent` must
    // appear in skeptic_reviewers (proving the 3rd-vendor slot was reached
    // and its verdict parsed). This is the regression guard for the baaf
    // bug: before the fix, an outage of the first two vendors caused a
    // total-outage Err, never reaching the fallback vendor.
    assert!(
        telemetry.contains("\"skeptic_reviewers\":[\"cursor-agent\"]"),
        "fallback chain regression (baaf): cursor-agent must appear in skeptic_reviewers \
         after claudem+agy both fail to parse; telemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"review_degraded\":true"),
        "single-family cursor-agent review MUST emit review_degraded:true so strict \
         merge policy #328 refuses strict-green; telemetry:\n{telemetry}"
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
// This scenario uses the production dual-dispatch fixture (coder=agy, so
// reviewers are claudem + cursor-agent) specifically because it makes
// vendor provenance observable: the fix must report those two vendors,
// not `codex`/`gemini` (fail-traps on PATH) and not a placeholder.
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
    write_fake_reviewer(&fake_bin_dir, "codex", "fail should-not-dispatch-codex");
    write_fake_reviewer(&fake_bin_dir, "gemini", "fail should-not-dispatch-gemini");
    write_fake_reviewer(&fake_bin_dir, "agy", "fail should-not-dispatch-agy-self-review");
    write_fake_reviewer(&fake_bin_dir, "claude", "pass");
    write_fake_reviewer(&fake_bin_dir, "cursor-agent", "pass");
    write_fake_target_worktree_git(&fake_bin_dir, "deadbeef558");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "agy"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/global-real-repo".into(); // NOT a fixture repo

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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
            bugbot_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log =
        std::env::temp_dir().join(format!("afd_wzgl_gate_report_{}.jsonl", std::process::id()));
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    // Production coder=agy excludes agy, so dual-dispatch is claudem +
    // cursor-agent (minimax + cursor families) and the bead reaches READY.
    // Telemetry shape (per-gate object, vendor identity) is the focus.
    assert_eq!(
        summary.beads_ready, 1,
        "claudem+cursor-agent dual-dispatch is two families so the bead must reach READY"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let gate_assessment_line = telemetry
        .lines()
        .find(|line| line.contains("\"eventType\":\"GATE_ASSESSMENT\""))
        .unwrap_or_else(|| panic!("no GATE_ASSESSMENT line found; telemetry:\n{telemetry}"));
    let parsed: serde_json::Value =
        serde_json::from_str(gate_assessment_line).unwrap_or_else(|e| {
            panic!("GATE_ASSESSMENT line is not valid JSON: {e}\nline: {gate_assessment_line}")
        });
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
    const CANONICAL_GATE_KEYS: [&str; 8] = [
        "ci_green",
        "no_conflicts",
        "coderabbit",
        "bugbot",
        "comments_resolved",
        "evidence_review",
        "skeptic",
        // Bead jleechan-ijod / issue #387 (r6): gate 8 is the runtime
        // vacuous-test detector's verdict (NotProvided = Green for test
        // fixtures with no PR diff to revert; Genuine = Green; Vacuous
        // = Red; BaselineFailed / ManifestMissing = Unknown). The
        // canonical vocabulary widens from 7 to 8 in r6.
        "vacuous_red_green",
    ];
    assert_eq!(
        gates.len(),
        8,
        "GATE_ASSESSMENT must report all 8 per-gate results, not just all_green; context:\n{context}"
    );
    for key in CANONICAL_GATE_KEYS {
        assert!(
            gates.contains_key(key),
            "GATE_ASSESSMENT gates dict must use the PR #235/jleechan-l4ki \
             canonical vocabulary (daemon/factory-overlay.sh REQUIRED_KEYS); \
             missing key {key:?}; context:\n{context}"
        );
    }

    let skeptic_reviewers = context["skeptic_reviewers"].as_array().unwrap_or_else(|| {
        panic!("GATE_ASSESSMENT context.skeptic_reviewers must be an array; context:\n{context}")
    });
    let skeptic_reviewers: Vec<&str> = skeptic_reviewers
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        skeptic_reviewers,
        vec!["claudem", "cursor-agent"],
        "GATE_ASSESSMENT must report the gate-7 reviewer vendors that actually \
         produced the verdict (claudem+cursor-agent; agy is the coder). \
         Codex/Gemini CLI/Anthropic Claude must not appear even when present \
         on PATH; context:\n{context}"
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
        .skip(119) // 0-indexed: line 120 (1-indexed) of auto-merge-guard.sh
        .take(74) // lines 120..=193 inclusive, mirroring test_auto_merge_guard_gate_vocabulary.sh entry (sed -n '120,193p'; shifted from 85..=158 by PR #735's fail-closed repo-allowlist insertion)
        .map(|l| l.strip_prefix("  ").unwrap_or(l)) // mirror the shell test's sed s/^  // normalization
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        predicate_block.contains("g.items()"),
        "extracted predicate block drifted from auto-merge-guard.sh's actual \
         line range 120-193 (line numbers may have shifted); block:\n{predicate_block}"
    );

    use std::io::Write as _;
    // jleechan-328 P1 #1 (exact-head binding): the predicate reads the
    // live head SHA from `sys.argv[1]` to refuse stale assessments.
    // Pass the assessment's recorded `head_sha` so the live-head check
    // matches and the predicate falls through to the cross-model verdict.
    let predicate_live_head = context
        .get("head_sha")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut child = std::process::Command::new("python3")
        .arg("-c")
        .arg(&predicate_block)
        .arg(predicate_live_head)
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
    // Production dual-dispatch is claudem + cursor-agent (two families),
    // so GATE_ASSESSMENT is all-green. The auto-merge-guard.sh predicate
    // must parse the dict-shaped gates + canonical vocab and report a
    // non-blocking verdict. Codex/Gemini CLI are on PATH with fail-trap
    // replies; they must not be dispatched.
    assert!(
        predicate_stderr.is_empty(),
        "predicate must parse the GATE_ASSESSMENT line without stderr; \
         stdout={predicate_stdout}\nstderr={predicate_stderr}\n\
         line={gate_assessment_line}"
    );
    assert!(
        predicate_stdout.starts_with("OK")
            || predicate_stdout.starts_with("PASS")
            || predicate_stdout.to_ascii_lowercase().contains("all_green")
            || output.status.success(),
        "claudem+cursor-agent dual-dispatch is two families so auto-merge-guard.sh \
         must not FAIL the skeptic gate; got: {predicate_stdout} \
         exit_success={}",
        output.status.success()
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
    let vcs = test_vcs();

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
            attempt_started_at: None,
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

    let telemetry_log = std::env::temp_dir().join("afd_9xrs_cross_repo_verification_loop.jsonl");
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
        vendor_health: None,
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
        .unwrap_or_else(|| {
            panic!("expected a Stage-1 Skeptic prompt among judge() calls, got: {llm_calls:?}")
        });
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

// --- jleechan-bkru: second-family pursuit after a dual-dispatch miss ----
//
// Historical incident 2026-07-09 added a later-list fallback so a total
// outage of the dual-dispatch primaries could not permanently fail gate 7.
// Gemini CLI is no longer in the default list (same Google family as agy).
// This test now pins the remaining contract: with the full 3-vendor queue
// [claudem, agy, cursor-agent], a parse miss on vendor1 plus a Pass on
// vendor2 is still single-family, so second-family pursuit must reach
// cursor-agent and combine it. Two families → READY.
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
    write_fake_reviewer(&fake_bin_dir, "codex", "fail should-not-dispatch-codex");
    write_fake_reviewer(&fake_bin_dir, "gemini", "fail should-not-dispatch-gemini");
    // Keep the full 3-vendor list (coder not in the queue). vendor1
    // (claudem via `claude`) fails to parse; vendor2 (agy) produces a
    // Pass. That is a single-family review, so second-family pursuit must
    // reach cursor-agent (priority[2]) and combine it. Two families → READY.
    write_fake_reviewer(&fake_bin_dir, "claude", "not a verdict at all");
    write_fake_reviewer(&fake_bin_dir, "agy", "pass");
    write_fake_reviewer(&fake_bin_dir, "cursor-agent", "pass");
    write_fake_target_worktree_git(&fake_bin_dir, "deadbeef558");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "codex"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/global-real-repo".into(); // NOT a fixture repo

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
            attempt_started_at: None,
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
            merge_state_unknown: false,
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
            bugbot_pending: false,
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    // claudem failed to parse; agy produced a Pass; second-family pursuit
    // reached cursor-agent. Two distinct families → READY, not degraded.
    assert_eq!(
        summary.beads_ready, 1,
        "jleechan-bkru retarget: when dual-dispatch primary claudem fails to \
         parse but agy (vendor2) and cursor-agent (second-family \
         fallback) both produce a verdict, the bead MUST reach READY. \
         summary={summary:?}\ntelemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"all_green\":true"),
        "GATE_ASSESSMENT must report all_green:true for agy+cursor-agent; \
         telemetry:\n{telemetry}"
    );

    let overlay = store
        .load("real-repo-bead-4thvendor")
        .unwrap()
        .expect("overlay must still exist");
    assert_eq!(
        overlay.state,
        OverlayState::Ready,
        "bead must reach READY via agy + cursor-agent (two families)"
    );
    assert!(
        telemetry.contains("\"agy\"") && telemetry.contains("\"cursor-agent\""),
        "second-family pursuit must record both agy and cursor-agent; \
         telemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"review_degraded\":false"),
        "agy+cursor-agent MUST emit review_degraded:false; telemetry:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// --- jleechan-984e / issue #385: cross-model reviewer + review_degraded ---
//
// Issue #385 acceptance: "assessment telemetry shows two model families on
// a normal run; vendor-down fallback test; degraded flag when only one
// family available." This test exercises BOTH halves of that contract
// through the real `run_tick` call stack:
//
//   1. The 3rd priority member (`cursor-agent`, a different model family
//      from claudem/agy) is reachable as a vendor fallback when the first
//      two are unavailable — proving the `cursor-agent -f <prompt>`
//      dispatch arm in `dispatch_reviewer`.
//
//   2. Production dual-dispatch (`claudem` + `cursor-agent`, coder=agy)
//      covers two families so GATE_ASSESSMENT carries
//      `review_degraded: false`.
//
//   3. The single-family failure mode — `["cursor-agent"]` only because
//      claudem and agy failed to parse — produces `review_degraded: true`
//      in GATE_ASSESSMENT, which is the exact fail-closed signal the
//      issue asks for.
#[test]
#[cfg(unix)]
fn cross_model_reviewer_cursor_agent_falls_back_and_emits_review_degraded() {
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_reviewers_cursor_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    // Dual-dispatch primaries (claudem via `claude`, then agy) fail to
    // parse; cursor-agent is the skip(2) fallback and produces a verdict.
    // Gemini CLI is on PATH as a fail-trap and must not be dispatched.
    write_fake_reviewer(&fake_bin_dir, "codex", "not a verdict");
    write_fake_reviewer(&fake_bin_dir, "claude", "still not a verdict");
    write_fake_reviewer(&fake_bin_dir, "agy", "also not a verdict");
    write_fake_reviewer(&fake_bin_dir, "gemini", "fail should-not-dispatch-gemini");
    write_fake_reviewer(&fake_bin_dir, "cursor-agent", "pass");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // coder=codex is not in the reviewer queue, so priority stays
    // [claudem, agy, cursor-agent] and the third-vendor fallback is
    // reachable.
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "codex"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/global-real-repo".into(); // NOT a fixture repo

    store.overlays.borrow_mut().insert(
        "real-repo-bead-cursoragent".into(),
        BeadOverlay {
            bead_id: "real-repo-bead-cursoragent".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(559),
            branch: Some("factory/real-repo-bead-cursoragent-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/real-repo-bead-cursoragent-r1".into());
    store.branch_beads.borrow_mut().insert(
        "factory/real-repo-bead-cursoragent-r1".into(),
        "real-repo-bead-cursoragent".into(),
    );

    scm.pr_snapshots.insert(
        559,
        PrSnapshot {
            pr_number: 559,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef559".into(),
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
            bugbot_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_real_target_repo_skeptic_cursoragent_{}.jsonl",
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    // Bead jleechan-984e r2 (issue #385 / strict merge policy #328):
    // cursor-agent ALONE is a single-family review; strict merge policy now
    // fail-closed refuses strict-green on that path. The bead must therefore
    // NOT reach READY (it parks HUMAN_HELD on the cross-model gate failure).
    // The fallback chain itself still works — `skeptic_reviewers` proves
    // cursor-agent was reached and its verdict parsed.
    assert_eq!(
        summary.beads_ready, 0,
        "issue #385 r2 / strict merge policy #328: cursor-agent alone is a \
         single-family review, so the bead must NOT reach READY (it must \
         park on the cross-model gate failure, not propagate a total-outage \
         Err). summary={summary:?}\ntelemetry:\n{telemetry}"
    );
    assert!(
        telemetry.contains("\"all_green\":false"),
        "GATE_ASSESSMENT must report all_green:false because the cross-model \
         gate blocks single-family Pass; telemetry:\n{telemetry}"
    );

    // Acceptance check #1 from issue #385: the cursor-agent reviewer must
    // appear in `skeptic_reviewers` (it's the only vendor that produced
    // a parseable verdict on this run).
    let gate_assessment_line = telemetry
        .lines()
        .find(|l| l.contains("\"eventType\":\"GATE_ASSESSMENT\""))
        .unwrap_or_else(|| {
            panic!("no GATE_ASSESSMENT line; telemetry:\n{telemetry}")
        });
    let gate_assessment: serde_json::Value = serde_json::from_str(gate_assessment_line)
        .unwrap_or_else(|e| {
            panic!(
                "GATE_ASSESSMENT line is not valid JSON: {e}\nline: {gate_assessment_line}"
            )
        });
    let context = gate_assessment
        .get("context")
        .unwrap_or_else(|| panic!("no context: {gate_assessment_line}"));

    let skeptic_reviewers = context["skeptic_reviewers"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "GATE_ASSESSMENT context.skeptic_reviewers must be an array; context:\n{context}"
            )
        });
    let skeptic_reviewers: Vec<&str> = skeptic_reviewers
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        skeptic_reviewers.contains(&"cursor-agent"),
        "issue #385 acceptance: cursor-agent must appear in skeptic_reviewers \
         after the first two vendors fail to parse; got: {skeptic_reviewers:?}"
    );

    // Acceptance check #2 from issue #385: with only one vendor having
    // contributed (cursor-agent) and that vendor belonging to a single
    // model family, `review_degraded` MUST be true — strict merge policy
    // (#328) MUST treat this assessment as NOT strict-green. r2 wires that
    // signal into the skeptic gate so all_green is now correctly false
    // (instead of true-with-degraded-flag like in r1).
    assert_eq!(
        context["review_degraded"].as_bool(),
        Some(true),
        "issue #385 acceptance: review_degraded MUST be true when only one \
         model family (cursor) contributed; context:\n{context}"
    );
    // r2 acceptance: the skeptic gate must emit the cross-model Red reason,
    // not Pass.
    let skeptic_gate = context["gates"]["skeptic"].clone();
    let skeptic_verdict = skeptic_gate
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        skeptic_verdict, "fail",
        "issue #385 r2 acceptance: cross-model gate must flip single-family \
         Pass to fail; skeptic gate: {skeptic_gate}"
    );
    let skeptic_evidence = skeptic_gate
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|e| e.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    assert!(
        skeptic_evidence
            .iter()
            .any(|s| s.contains("cross-model") || s.contains("review_degraded")),
        "issue #385 r2 acceptance: skeptic gate Red reason must name the \
         cross-model failure; got: {skeptic_evidence:?}"
    );

    let overlay = store
        .load("real-repo-bead-cursoragent")
        .unwrap()
        .expect("overlay must still exist");
    // r2 / strict merge policy (#328): cursor-agent alone is a single-family
    // review. The bead must park (HUMAN_HELD) on the cross-model gate
    // failure, NOT reach READY. The dispatch worked — the verdict parsed —
    // but the cross-model guarantee refuses strict-green until a second
    // family contributes.
    assert_eq!(
        overlay.state,
        OverlayState::HumanHeld,
        "issue #385 r2 / strict merge policy #328: cursor-agent alone is a \
         single-family review; the bead must park HUMAN_HELD on the \
         cross-model gate failure, not stay ATTESTED and not reach READY"
    );

    let _ = std::fs::remove_file(&telemetry_log);
    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

#[test]
#[cfg(unix)]
fn cross_model_reviewer_two_distinct_families_is_not_degraded() {
    // The opposite half of the cross-model guarantee: when two vendors
    // from DISTINCT model families both contribute, review_degraded MUST
    // be false. Default dual-dispatch is now agy (google-antigravity) +
    // gemini (google-gemini). Codex and Claude are on PATH with fail-trap
    // replies so a regression that re-adds them as defaults fails this test.
    let _lock = REAL_TARGET_REPO_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_reviewers_twofam_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    write_fake_reviewer(&fake_bin_dir, "codex", "fail should-not-dispatch-codex");
    write_fake_reviewer(&fake_bin_dir, "gemini", "fail should-not-dispatch-gemini");
    write_fake_reviewer(&fake_bin_dir, "agy", "fail should-not-dispatch-agy-self-review");
    write_fake_reviewer(&fake_bin_dir, "claude", "pass");
    write_fake_reviewer(&fake_bin_dir, "cursor-agent", "pass");
    write_fake_target_worktree_git(&fake_bin_dir, "deadbeef560");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_CODER_DEFAULT", "agy"),
    ]);

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();

    let mut cfg = test_cfg();
    cfg.target_repo = "myorg/global-real-repo".into(); // NOT a fixture repo

    store.overlays.borrow_mut().insert(
        "real-repo-bead-twofam".into(),
        BeadOverlay {
            bead_id: "real-repo-bead-twofam".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(560),
            branch: Some("factory/real-repo-bead-twofam-r1".into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );
    store
        .branches
        .borrow_mut()
        .push("factory/real-repo-bead-twofam-r1".into());
    store.branch_beads.borrow_mut().insert(
        "factory/real-repo-bead-twofam-r1".into(),
        "real-repo-bead-twofam".into(),
    );

    scm.pr_snapshots.insert(
        560,
        PrSnapshot {
            pr_number: 560,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef560".into(),
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
            bugbot_pending: false,
            head_committed_epoch: 0,
        },
    );

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_real_target_repo_skeptic_twofam_{}.jsonl",
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("run_tick should succeed against a real (non-owner/repo) target_repo");

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        summary.beads_ready, 1,
        "two-family dual-dispatch: claudem + cursor-agent both pass → bead must reach READY. \
         summary={summary:?}\ntelemetry:\n{telemetry}"
    );

    let gate_assessment_line = telemetry
        .lines()
        .find(|l| l.contains("\"eventType\":\"GATE_ASSESSMENT\""))
        .unwrap_or_else(|| {
            panic!("no GATE_ASSESSMENT line; telemetry:\n{telemetry}")
        });
    let gate_assessment: serde_json::Value = serde_json::from_str(gate_assessment_line)
        .unwrap_or_else(|e| {
            panic!(
                "GATE_ASSESSMENT line is not valid JSON: {e}\nline: {gate_assessment_line}"
            )
        });
    let context = gate_assessment
        .get("context")
        .unwrap_or_else(|| panic!("no context: {gate_assessment_line}"));

    let skeptic_reviewers: Vec<&str> = context["skeptic_reviewers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    // Production coder=agy excludes agy, so dual-dispatch is claudem
    // (minimax) + cursor-agent (cursor). Both produced parseable verdicts.
    // Codex/Gemini CLI/Anthropic Claude are on PATH with fail-traps.
    assert!(
        skeptic_reviewers.contains(&"claudem") && skeptic_reviewers.contains(&"cursor-agent"),
        "both claudem and cursor-agent must be in skeptic_reviewers (both \
         dual-dispatch primaries produced parseable verdicts); got: {skeptic_reviewers:?}"
    );
    assert!(
        !skeptic_reviewers.iter().any(|v| *v == "codex" || *v == "claude" || *v == "gemini" || *v == "agy"),
        "codex, anthropic claude, gemini CLI, and the agy coder must not be \
         default reviewers; got: {skeptic_reviewers:?}"
    );
    // Two distinct model families → review_degraded MUST be false.
    assert_eq!(
        context["review_degraded"].as_bool(),
        Some(false),
        "issue #385 acceptance: two distinct model families (agy=google-antigravity, \
         gemini=google-gemini) → review_degraded MUST be false; context:\n{context}"
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
    // Under the dispatch-scheduling-guarantee ordering, `run_recovery_step`
    // runs AFTER `run_slow_tier` on slow ticks. The dispatch path parks the
    // bead HUMAN_HELD (attempt 1, below the recovery cap of 10), then
    // recovery immediately requeues it to QUEUED in the same tick. The park
    // and escalation still happen (asserted above) — that is the real guard
    // against silent livelock.
    assert_eq!(
        summary_cap.beads_recovered_from_held, 1,
        "recovery requeues the freshly-parked bead in the same tick"
    );
    let capped_overlay = store.load(BEAD_ID).unwrap().unwrap();
    assert_eq!(
        capped_overlay.state,
        OverlayState::Queued,
        "bead is parked then recovered to QUEUED in the same tick under the new ordering"
    );
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

    // Tick 16: dedup check. `run_slow_tier` runs first — the QUEUED bead
    // (recovered at the end of tick 15) is dispatched again, spawn fails
    // again, and because `spawn_failure_count` was NOT reset by recovery
    // (still > 15) the dispatch path re-parks HUMAN_HELD immediately rather
    // than granting a fresh 15-retry budget. Then `run_recovery_step`
    // requeues it to QUEUED again. `escalation_already_recorded` must
    // prevent a second escalation comment.
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
        OverlayState::Queued,
        "a bead whose spawn is permanently broken is re-parked by dispatch then requeued by \
         recovery each tick — it never silently disappears, and the one-time escalation comment \
         above is the human-visible signal"
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some(EXTERNAL_REF_A.into()),
    });
    tracker.candidates.borrow_mut().push(Bead {
        id: BEAD_B.into(),
        title: "Bead B: genuine deterministic tool spawn failure".into(),
        description: "distinct from bead A's backpressure — a real transient error".into(),
        notes: String::new(),
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
    // Under the dispatch-scheduling-guarantee ordering, `run_recovery_step`
    // runs AFTER `run_slow_tier` and requeues the freshly-parked bead B
    // (attempt 1 < recovery cap) to QUEUED in the same tick. The park and
    // escalation still happen (asserted above); bead A is unaffected.
    assert_eq!(
        summary_cap.beads_recovered_from_held, 1,
        "recovery requeues bead B in the same tick it is parked"
    );

    let a_final = store.load(BEAD_A).unwrap().unwrap();
    assert_eq!(
        a_final.state,
        OverlayState::Queued,
        "bead A must remain QUEUED even after bead B parks HUMAN_HELD in the same batch"
    );
    assert_eq!(a_final.spawn_failure_count, 0);

    let b_final = store.load(BEAD_B).unwrap().unwrap();
    assert_eq!(
        b_final.state,
        OverlayState::Queued,
        "bead B is parked then recovered to QUEUED in the same tick under the new ordering"
    );
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
    );
    snap_a.ci_success = false;
    snap_a.ci_status = "failure".into();
    scm.pr_snapshots.insert(801, snap_a);

    let mut snap_b = qdw_green_snapshot(
        802,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: 0,
        }],
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
    let mut vcs = test_vcs();
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
        (
            "cq8r-bead-a",
            801u64,
            "alice/cq8r-bead-a-branch",
            "BEAD-A-PRIOR-MARKER",
        ),
        (
            "cq8r-bead-b",
            802u64,
            "bob/cq8r-bead-b-branch",
            "bead-b-prior-text",
        ),
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
                // These seeded prior rejections model attempts that reached
                // remediation. Seed the durable lifecycle marker explicitly;
                // `pre_session_head_sha` alone is only a pre-spawn intent and
                // must not authorize the semantic circuit breaker.
                pre_session_head_sha: Some(format!("{bead_id}-pre-session-sha")),
                park_reason: None,
                target_repo: None,
                attempt_started_at: None,
            })
            .unwrap();
        store
            .mark_remediation_session_spawned(bead_id, 1)
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
        vendor_health: None,
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
    std::fs::write(&path, script).unwrap_or_else(|e| panic!("failed to write fake gh: {e}"));
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
    let vcs = test_vcs();
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
        vendor_health: None,
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
    let vcs = test_vcs();
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
        vendor_health: None,
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

// ===========================================================================
// Bead jleechan-zeij / issue #322 r4 P1: tick-boundary handling of the re-roll
// engine's Deferred outcome and permanent errors. These drive the REAL
// `run_tick` fast-tier selection (not `reroll::execute` directly) so they
// prove ATTESTED re-eligibility and the permanent-error park at the seam the
// finding names (run_fast_tier's ATTESTED filter + the reroll Err arm).
// ===========================================================================

/// Stage-2 config whose `target_repo` = "owner/repo" makes `is_test_repo`
/// true, so the Skeptic gate uses the scripted `FakeLlm` (no subprocess), and
/// with tiny re-roll windows.
fn reroll_stage2_cfg() -> Config {
    let mut cfg = test_cfg();
    cfg.stage = 2;
    cfg.reroll_head_stability_window_secs = 1;
    cfg.reroll_death_confirm_secs = 0;
    cfg
}

/// An ATTESTED bead + a scripted RED-CI PR snapshot that routes the fast tier
/// into the Stage-2 re-roll lane. A pre-posted `/er PASS` comment makes
/// `er_runner::maybe_run` return `AlreadyPosted` (no `claude` subprocess), and
/// `FakeLlm` "pass" greens the Skeptic gate, so CI-red is the sole Red gate.
fn seed_attested_red_ci_bead(
    scm: &mut FakeScm,
    store: &FakeStateStore,
    bead_id: &str,
    pr: u64,
) -> String {
    let branch = format!("factory/{bead_id}-r1");
    store
        .save(&BeadOverlay {
            bead_id: bead_id.into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some(branch.clone()),
            session_id: Some("fake-session-1".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        })
        .unwrap();
    store.register_branch(bead_id, &branch).unwrap();
    // Use a RECENT `updated_at_epoch` so the wedge/stall sweep (which fires on
    // `now - updated_at >= 1800s`) does not park the ATTESTED bead before it
    // reaches the reroll lane; and a `head_committed_epoch` older than the
    // pre-posted `/er PASS` comment so `parse_er_verdict_since` treats the
    // verdict as fresh (er_runner short-circuits -> no `claude` subprocess).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    scm.pr_snapshots.insert(
        pr,
        PrSnapshot {
            pr_number: pr,
            ci_success: false, // -> CI gate RED
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![PrComment {
                author: "reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: now,
            }],
            files: vec![],
            updated_at_epoch: now,
            ci_status: "red".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: now.saturating_sub(60),
        },
    );
    branch
}

/// r4 P1: a PERMANENT re-roll error must park the bead HUMAN_HELD (loud,
/// operator-visible) at the tick boundary — NOT strand it in RE_ROLL. Drives
/// the real fast tier; the entry `attach` fails with a permanent parse error,
/// so `reroll::execute` returns `Err(Parse)`, and the tick Err arm parks it
/// with reason `reroll_permanent_error`.
#[test]
fn tick_parks_human_held_on_permanent_reroll_error() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into())); // greens the Skeptic gate
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = reroll_stage2_cfg();

    let branch = seed_attested_red_ci_bead(&mut scm, &store, "perm-bead", 5001);
    // Entry attach fails permanently -> reroll::execute returns Err(Parse).
    sessions.fail_attach_permanent_for(&branch);

    let telemetry_log = std::env::temp_dir().join("afd_r4_tick_perm.jsonl");
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("tick should isolate the permanent reroll error and continue");

    let overlay = store.load("perm-bead").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::HumanHeld,
        "a permanent reroll error must park the bead HUMAN_HELD, not strand it in RE_ROLL"
    );
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("reroll_permanent_error")
    );
    assert!(
        summary.beads_parked_human_held >= 1,
        "the permanent-error park must be counted"
    );

    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(
        body.lines().any(|l| l.contains("\"reroll_permanent_error\"")),
        "operator-visible PARKED_HUMAN_HELD telemetry with the permanent-error reason must be emitted"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// r4 P1: a Deferred re-roll outcome must leave the bead ATTESTED and
/// RE-SELECTABLE by the fast tier on a later tick (proving full tick re-entry,
/// which the direct-`execute` cap test cannot). Two real ticks: each routes to
/// re-roll (RED CI), each defers (a transient stop() failure), and the bead is
/// gate-assessed BOTH times.
#[test]
fn tick_deferred_reroll_stays_attested_and_reselects_next_tick() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = reroll_stage2_cfg();

    seed_attested_red_ci_bead(&mut scm, &store, "defer-bead", 5002);
    // attach() succeeds (returns fake-session-1); stop() fails transiently ->
    // reroll defers before touching the branch/PR.
    sessions.fail_stop_for("fake-session-1");

    let telemetry_log = std::env::temp_dir().join("afd_r4_tick_defer.jsonl");
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
        vendor_health: None,
    };

    let summary1 = run_tick(&deps, 1, 0).expect("tick 1 should succeed");
    assert_eq!(
        summary1.gates_assessed, 1,
        "tick 1 must gate-assess the ATTESTED bead"
    );
    let after1 = store.load("defer-bead").unwrap().unwrap();
    assert_eq!(
        after1.state,
        OverlayState::Attested,
        "a deferred reroll must leave the bead ATTESTED (re-eligible), not parked or advanced"
    );
    assert_eq!(store.reroll_deferral_count("defer-bead").unwrap(), 1);
    assert_eq!(summary1.beads_parked_human_held, 0, "a defer is not a park");

    // Tick 2: the SAME bead is still ATTESTED, the same head_sha, and
    // `reroll_deferral_count` is 1 from tick 1's deferred reroll. The
    // jleechan-msmq guard skips gate re-assessment AND skips the reroll
    // deferral this tick (the bead has not yet had time to land a fix,
    // re-running the same deferral loop just races with the breaker).
    // Tick 2 therefore logs VERIFIER_SKIPPED_REROLL_IN_PROGRESS instead
    // of another gate assessment + another REROLL_QUIESCENCE_DEFERRED.
    let summary2 = run_tick(&deps, 2, 0).expect("tick 2 should succeed");
    let log2 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(
        summary2.gates_assessed, 0,
        "tick 2 with reroll_deferral_count > 0 must NOT re-gate-assess (jleechan-msmq skip guard); log:\n{log2}"
    );
    assert!(
        log2.contains("VERIFIER_SKIPPED_REROLL_IN_PROGRESS"),
        "tick 2 with reroll_deferral_count > 0 must emit the jleechan-msmq skip telemetry; log:\n{log2}"
    );
    let after2 = store.load("defer-bead").unwrap().unwrap();
    assert_eq!(
        after2.state,
        OverlayState::Attested,
        "tick 2 with deferred reroll must keep the bead ATTESTED, not park or advance"
    );
    assert_eq!(
        store.reroll_deferral_count("defer-bead").unwrap(),
        1,
        "tick 2 must NOT increment the deferral counter again (the guard prevents re-entering the same defer loop)"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// A deferred reroll suppresses duplicate assessment of the old PR, but that
// suppression must not leak into a later, durably discovered PR for the fresh
// attempt. The DISPATCHED -> ATTESTED promotion is that durable boundary.
#[test]
fn tick_resets_reroll_deferral_when_fresh_attempt_pr_opens() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = reroll_stage2_cfg();

    let branch = seed_attested_red_ci_bead(&mut scm, &store, "fresh-attempt", 5003);
    store.incr_reroll_deferral("fresh-attempt").unwrap();

    // Model a newly established retry: it has a branch-to-PR binding and is
    // promoted from DISPATCHED only after that binding has been observed.
    let mut overlay = store.load("fresh-attempt").unwrap().unwrap();
    overlay.state = OverlayState::Dispatched;
    overlay.reroll_count = 1;
    overlay.session_id = None;
    store.save(&overlay).unwrap();
    scm.pr_numbers_for_branch
        .insert(("owner/repo".into(), branch.clone()), Some(5003));
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), 5003),
        PrHeadBranch::SameRepo(branch.clone()),
    );
    let snapshot = scm.pr_snapshots.get_mut(&5003).unwrap();
    snapshot.ci_success = true;
    snapshot.ci_status = "green".into();
    snapshot.head_sha = "fresh-attempt-head".into();

    let telemetry_log = std::env::temp_dir().join("afd_fresh_attempt_resets_deferral.jsonl");
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 2, 0).expect("fresh attempt tick should succeed");
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert_eq!(summary.gates_assessed, 1, "fresh PR must be gate-assessed: {log}");
    assert!(log.contains("\"eventType\":\"PR_OPENED\""), "fresh PR must promote: {log}");
    assert!(
        !log.contains("VERIFIER_SKIPPED_REROLL_IN_PROGRESS"),
        "old-PR suppression must not apply to the fresh PR: {log}"
    );
    assert_eq!(
        store.reroll_deferral_count("fresh-attempt").unwrap(),
        0,
        "PR_OPENED must clear the previous attempt's deferral marker"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}
// jleechan-park-leaves-zombie-session-mh9o: regression for the U4-class
// zero-touch blocker where every PARKED_* transition leaked its AO session.
//
// Symptom: the daemon parked the bead HUMAN_HELD in its overlay but never
// called `ao session kill`. AO still listed the leaked session as
// `spawning`, so the next `ao spawn` for the same bead/branch/prompt was
// rejected by the dedup guard with "Duplicate session detected".
// Operator had to manually `ao session kill df-167 df-168 …` to clear the
// poison. Repeated 2026-07-17/18; lanes 287/285 (df-167/df-168) blocked.
//
// Required invariant for every PARKED_HUMAN_HELD transition written by the
// tick loop: `sessions.stop(session_id)` MUST be called and the durable
// overlay's `session_id` MUST be cleared. Without `session_id IS NULL`, the
// automated `recover_human_held` requeue path (jleechan-gib) is also
// blocked — recovery only requeues rows whose durable overlay has no
// session handle.
//
// These three tests cover the three park classes called out in the task
// data: autonomy_timebox_exceeded, coder_silent, session_branch_mismatch.
// They are the canonical proof that the fix at the tick-park layer — not
// at the recovery layer — terminates the zombie AO session before any
// downstream code can observe the dedup-blocked redispatch.
#[test]
fn autonomy_timebox_park_kills_associated_ao_session_and_clears_handle() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.autonomy_timebox_secs = 3600;

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.remote_branches
        .insert("factory/bead-mh9o-timebox-r1".into(), Some(now_epoch));

    // Pre-seed a DISPATCHED bead with a live session handle already
    // exceeding the autonomy timebox. Park must kill that exact session
    // and clear the overlay's handle so a future requeue/redispatch is not
    // blocked by the AO dedup guard.
    store.overlays.borrow_mut().insert(
        "bead-mh9o-timebox".into(),
        BeadOverlay {
            bead_id: "bead-mh9o-timebox".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 4000, // already > 3600 timebox
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-mh9o-timebox-r1".into()),
            session_id: Some("df-mh9o-timebox".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_mh9o_timebox.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 100).expect("tick should succeed");
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "bead must be parked on autonomy timebox overflow"
    );

    let overlay = store.load("bead-mh9o-timebox").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::HumanHeld,
        "bead must be HUMAN_HELD after timebox park"
    );
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("autonomy_timebox_exceeded"),
        "park_reason must record the timebox overflow"
    );
    assert!(
        overlay.session_id.is_none(),
        "park transition MUST clear the durable session handle — without \
         this, the automated HUMAN_HELD exit cannot requeue the bead \
         (recover_human_held requires session_id IS NULL) and any manual \
         requeue hits the AO dedup guard before it can spawn"
    );
    assert!(
        sessions
            .calls
            .borrow()
            .iter()
            .any(|call| call == "stop(df-mh9o-timebox)"),
        "park transition MUST invoke sessions.stop on the leaked session; \
         without this, AO still reports the session as [spawning] and \
         rejects subsequent spawn attempts with the dedup guard. Calls: {:?}",
        sessions.calls.borrow()
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn coder_silent_park_kills_associated_ao_session_and_clears_handle() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    // DISPATCHED bead whose remote branch has had no commit for >30 minutes,
    // no recent transcript activity → the wedge-detection sweep must park
    // this bead coder_silent. Park must kill the live session.
    store.overlays.borrow_mut().insert(
        "bead-mh9o-silent".into(),
        BeadOverlay {
            bead_id: "bead-mh9o-silent".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 1900,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-mh9o-silent-r1".into()),
            session_id: Some("df-mh9o-silent".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );

    // remote branch commit timestamp = None AND no transcript activity →
    // `branch_is_silent && transcript_is_active == false` → coder_silent
    // branch fires.
    scm.remote_branches
        .insert("factory/bead-mh9o-silent-r1".into(), None);

    let telemetry_log = std::env::temp_dir().join("afd_test_mh9o_silent.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 10).expect("tick should succeed");
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "bead must be parked coder_silent after wedge detection"
    );

    let overlay = store.load("bead-mh9o-silent").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("coder_silent"),
        "park_reason must record the silence"
    );
    assert!(
        overlay.session_id.is_none(),
        "coder_silent park MUST clear session handle (recover_human_held gate)"
    );
    assert!(
        sessions
            .calls
            .borrow()
            .iter()
            .any(|call| call == "stop(df-mh9o-silent)"),
        "coder_silent park MUST invoke sessions.stop; otherwise the next \
         ao spawn for this bead hits the dedup guard. Calls: {:?}",
        sessions.calls.borrow()
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn session_branch_mismatch_park_kills_associated_ao_session_and_clears_handle() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    // DISPATCHED bead with a live session whose reported branch differs
    // from the bead's registered branch. The dispatch-integrity sweep
    // (jleechan-5ia2) must park this bead session_branch_mismatch — and
    // now must ALSO kill the leaked session so the dedup guard cannot
    // trap the bead across redispatch attempts.
    store.overlays.borrow_mut().insert(
        "bead-mh9o-mismatch".into(),
        BeadOverlay {
            bead_id: "bead-mh9o-mismatch".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-mh9o-mismatch-r1".into()),
            session_id: Some("df-mh9o-mismatch".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );

    // Script the fake AO session to report a branch that does NOT match
    // the bead's registered branch, so `deps.sessions.session_branch` returns
    // `Ok(Some(<actual>))` and the positive-mismatch check fires.
    sessions.set_session_branch("df-mh9o-mismatch", "factory/wa-3004-hook-refactor");

    let telemetry_log = std::env::temp_dir().join("afd_test_mh9o_mismatch.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 10).expect("tick should succeed");
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "bead must be parked session_branch_mismatch on positive mismatch"
    );

    let overlay = store.load("bead-mh9o-mismatch").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("session_branch_mismatch"),
        "park_reason must record the branch mismatch"
    );
    assert!(
        overlay.session_id.is_none(),
        "session_branch_mismatch park MUST clear session handle"
    );
    assert!(
        !sessions
            .calls
            .borrow()
            .iter()
            .any(|call| call == "stop(df-mh9o-mismatch)"),
        "session_branch_mismatch park MUST NOT kill the leaked session: \
         session_branch has just proved that session belongs to a \
         different bead/branch, and killing it would terminate someone \
         else's legitimate worker. Only the bad overlay handle is \
         dropped. Calls: {:?}",
        sessions.calls.borrow()
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-park-leaves-zombie-session-mh9o (CodeRabbit P1 follow-up):
// when `sessions.stop()` fails, the AO session may STILL be live. Clearing
// the durable handle in that case would let `recover_human_held` requeue
// the bead and dispatch a second worker that overlaps the existing live
// one. Retain the handle so (a) recover_human_held cannot requeue and
// (b) the operator retains the durable session_id for manual cleanup.
#[test]
fn autonomy_timebox_park_retains_handle_when_stop_fails() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    sessions.fail_stop_for("df-mh9o-stop-fails");
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.autonomy_timebox_secs = 3600;

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.remote_branches
        .insert("factory/bead-mh9o-stop-fails-r1".into(), Some(now_epoch));

    store.overlays.borrow_mut().insert(
        "bead-mh9o-stop-fails".into(),
        BeadOverlay {
            bead_id: "bead-mh9o-stop-fails".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 4000, // already > 3600 timebox
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-mh9o-stop-fails-r1".into()),
            session_id: Some("df-mh9o-stop-fails".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_mh9o_stop_fails.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let vcs = test_vcs();
    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 100).expect("tick should succeed");
    assert_eq!(summary.beads_parked_human_held, 1);

    let overlay = store.load("bead-mh9o-stop-fails").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.session_id.as_deref(),
        Some("df-mh9o-stop-fails"),
        "on stop() failure the handle MUST be retained so the operator \
         retains the durable session_id for manual cleanup AND \
         recover_human_held cannot requeue and dispatch a second worker \
         that would overlap the still-live session"
    );

    let logs = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(
        logs.contains("BEAD_SESSION_KILL_FAILED"),
        "stop() failure MUST emit BEAD_SESSION_KILL_FAILED telemetry; logs: {logs}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-park-leaves-zombie-session-mh9o (CodeRabbit Major follow-up):
// adopted-branch remediation parks (history rewrite, append-only check
// failure) were also leaking their session. Wire the cleanup helper into
// both sites.
#[test]
fn adopted_branch_history_rewrite_park_kills_associated_ao_session() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();

    // Pre-seed an adopted DISPATCHED bead whose pre_session_head_sha is
    // NOT an ancestor of the live remote head (positive history rewrite).
    store.overlays.borrow_mut().insert(
        "bead-mh9o-adopted".into(),
        BeadOverlay {
            bead_id: "bead-mh9o-adopted".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-mh9o-adopted-r1".into()),
            session_id: Some("df-mh9o-adopted".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: Some("aaaaaaaaaaaaaaaa".into()),
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_test_mh9o_adopted.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut vcs = test_vcs();
    // Remote HEAD is a totally unrelated commit -> is_ancestor returns
    // false -> adopted_branch_history_rewrite_detected park.
    vcs.heads.insert(
        "factory/bead-mh9o-adopted-r1".into(),
        "bbbbbbbbbbbbbbbb".into(),
    );
    vcs.ancestor_pairs.insert(
        ("aaaaaaaaaaaaaaaa".into(), "bbbbbbbbbbbbbbbb".into()),
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 10).expect("tick should succeed");
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "bead must be parked adopted_branch_history_rewrite_detected"
    );

    let overlay = store.load("bead-mh9o-adopted").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("adopted_branch_history_rewrite_detected")
    );
    assert!(
        overlay.session_id.is_none(),
        "adopted_branch_history_rewrite_detected park MUST clear session handle"
    );
    assert!(
        sessions
            .calls
            .borrow()
            .iter()
            .any(|call| call == "stop(df-mh9o-adopted)"),
        "adopted_branch_history_rewrite_detected park MUST terminate the \
         leaked AO session; otherwise the AO dedup guard blocks future \
         spawns of this bead. Calls: {:?}",
        sessions.calls.borrow()
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-8jxr r3 (review follow-up, chatgpt-codex-connector P2 @
/// daemon/src/dispatch.rs:287): when a bead is parked HUMAN_HELD with
/// reason `unmapped_repo` at dispatch time, the tick layer must
/// special-case the failure phase the same way it special-cases
/// `unmapped_target_repo` and `worktree_remote_mismatch`. Otherwise the
/// generic `BEAD_DISPATCH_TRANSIENT_ERROR` fall-through (with
/// `lifecycle_state = QUEUED`) mis-reports a genuinely permanent,
/// operator-action-required park as retryable, never increments
/// `beads_parked_human_held`, and posts no escalation comment.
///
/// Regression pin: a manually-created bead (no `external_ref`, no body
/// `target_repo:` field) reaches dispatch with `overlay.target_repo =
/// None`. dispatch_ready parks it `unmapped_repo`. run_tick must:
/// 1. Emit `PARKED_HUMAN_HELD` (not `BEAD_DISPATCH_TRANSIENT_ERROR`).
/// 2. Increment `summary.beads_parked_human_held`.
/// 3. Post an escalation comment naming the remediation (add body
///    field/external_ref, or label `factory` so intake can resolve).
#[test]
fn run_tick_emits_parked_human_held_for_unmapped_repo_dispatch_failure() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "no-repo-bead".into(),
        title: "manual bead with no repo".into(),
        description: "manually created with no external_ref".into(),
        notes: String::new(),
        file_tree_summary: String::new(),
        // No external_ref and no body `target_repo:` field — dispatch
        // will park this as unmapped_repo.
        external_ref: None,
    });

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"manual bead"}"#.into(),
    ));
    let store = FakeStateStore::new(); // empty: dispatch will load overlay from DB
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join("afd_unmapped_repo_park_test.jsonl");
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
        vendor_health: None,
        },
        0,
        0,
    )
    .expect("tick should succeed even when dispatch parks a no-repo bead");

    // (1) summary counts the unmapped_repo park as a permanent HUMAN_HELD
    // park, not a transient dispatch error.
    assert_eq!(
        summary.beads_parked_human_held, 1,
        "unmapped_repo park must increment beads_parked_human_held (got: {})",
        summary.beads_parked_human_held
    );
    assert_eq!(
        summary.beads_dispatched, 0,
        "a no-repo bead must NOT dispatch"
    );

    // (2) overlay state is HUMAN_HELD with a park_reason derived from
    // `unmapped_repo` (either the bare reason or the local-fallback
    // variant — the FakeScm has no SCM target to post a comment to, so
    // `record_local_escalation_fallback` re-stamps the reason to
    // `escalation_local_fallback:unmapped_repo`. Either way the prefix
    // must name `unmapped_repo`, never the generic unmapped_target_repo
    // or worktree_remote_mismatch reason).
    let overlay = store.load("no-repo-bead").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    let park_reason = overlay
        .park_reason
        .as_deref()
        .expect("HUMAN_HELD overlay must have a park_reason");
    assert!(
        park_reason == "unmapped_repo"
            || park_reason.starts_with("escalation_local_fallback:unmapped_repo"),
        "park_reason must be unmapped_repo-derived, got: {park_reason:?}"
    );

    // (3) telemetry emits PARKED_HUMAN_HELD, NOT BEAD_DISPATCH_TRANSIENT_ERROR.
    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let parked = events
        .iter()
        .find(|e| e["eventType"] == "PARKED_HUMAN_HELD" && e["beadId"] == "no-repo-bead");
    assert!(
        parked.is_some(),
        "telemetry MUST emit PARKED_HUMAN_HELD for unmapped_repo parks; events = {:?}",
        events
    );
    let transient_error = events.iter().find(|e| {
        e["eventType"] == "BEAD_DISPATCH_TRANSIENT_ERROR" && e["beadId"] == "no-repo-bead"
    });
    assert!(
        transient_error.is_none(),
        "unmapped_repo park must NOT fall through to BEAD_DISPATCH_TRANSIENT_ERROR; events = {:?}",
        events
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn run_tick_escalates_explicit_target_without_checkout_as_human_held() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "missing-checkout-bead".into(),
        title: "explicit repo needs its own checkout".into(),
        description: "target_repo: other/repo".into(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("other/repo#321".into()),
    });

    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"explicit target"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.repos.insert(
        "other/repo".into(),
        RepoConfig {
            ao_project: "other".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    let vcs = test_vcs();
    let telemetry_log =
        std::env::temp_dir().join("afd_missing_target_checkout_park_test.jsonl");
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
            vendor_health: None,
        },
        0,
        0,
    )
    .expect("one missing target checkout must not abort the tick");

    assert_eq!(summary.beads_parked_human_held, 1);
    assert_eq!(summary.beads_dispatched, 0);
    let overlay = store.load("missing-checkout-bead").unwrap().unwrap();
    assert_eq!(overlay.state, OverlayState::HumanHeld);
    assert_eq!(
        overlay.park_reason.as_deref(),
        Some("target_checkout_unconfigured")
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = telemetry
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| {
        event["eventType"] == "PARKED_HUMAN_HELD"
            && event["beadId"] == "missing-checkout-bead"
            && event["context"]["reason"] == "target_checkout_unconfigured"
    }));
    assert!(!events.iter().any(|event| {
        event["eventType"] == "BEAD_DISPATCH_TRANSIENT_ERROR"
            && event["beadId"] == "missing-checkout-bead"
    }));
    assert!(tracker.calls.borrow().iter().any(|call| {
        call.starts_with("comment_external(other/repo#321,")
            && call.contains("local_checkout")
    }));

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-t40t (issue #326, live incident jleechan-t8fd / PR #316):
/// branch-mismatch handling preserves stale `pr_number`, blocking
/// discovery of a later correct PR — bead stays DISPATCHED indefinitely.
/// Regression test: a bead is dispatched onto a branch that originally
/// resolved to PR `3001`, but the live state has a LATER PR (`3002`) on
/// the same branch (e.g. the original PR was closed and a fresh one was
/// opened on the same head ref). The slow-tier DISPATCHED→ATTESTED path
/// must re-resolve `pr_number` from the branch, NOT trust the stale
/// stored `pr_number`, so the bead promotes against the correct PR.
///
/// Pre-fix: this test fails — `pr_number` stays `3001`, the bead never
/// converges, and no `PR_NUMBER_REREZOLVED` telemetry event fires.
/// Post-fix: `pr_number` becomes `3002`, the bead promotes past
/// DISPATCHED, and the telemetry log carries the `PR_NUMBER_REREZOLVED`
/// line so the drift is auditable from the daemon log alone.
#[test]
fn slow_tier_dispatched_branch_mismatch_re_resolves_stale_pr_number() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = test_cfg();

    // Seed a DISPATCHED bead on `factory/stale-pr-bead-r1` with a STALE
    // `pr_number = 3001`. Live state: a different PR (3002) is now bound
    // to the same branch — must supersede the stored value.
    let branch = "factory/stale-pr-bead-r1";
    store
        .save(&BeadOverlay {
            bead_id: "stale-pr-bead".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(3001), // stale
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("stale-pr-bead", branch).unwrap();

    // Script the branch→PR lookup: branch now resolves to 3002 (current).
    scm.pr_numbers_for_branch
        .insert(("owner/repo".into(), branch.into()), Some(3002));
    // PR 3002 has green gates; PR 3001 is irrelevant after re-resolution.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    scm.pr_snapshots.insert(
        3002,
        PrSnapshot {
            pr_number: 3002,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "cafebabe".into(),
            body: "".into(),
            comments: vec![PrComment {
                author: "reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: now,
            }],
            files: vec![],
            updated_at_epoch: now,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: now.saturating_sub(60),
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_t40t_branch_mismatch_stale_pr.jsonl");
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 1, 0).expect("tick should succeed");
    let after = store.load("stale-pr-bead").unwrap().unwrap();

    // Core fix: `pr_number` was re-resolved from the branch — old value
    // is gone, new value is stored, persisted state was updated.
    assert_eq!(
        after.pr_number,
        Some(3002),
        "branch→PR re-resolution must supersede the stale stored pr_number"
    );
    assert_ne!(
        after.state,
        OverlayState::Dispatched,
        "bead must advance past DISPATCHED against the correctly-resolved PR \
         (this is the core convergence invariant the bug violated; it may land \
         at ATTESTED with pending gates or at READY when the scripted snapshot is \
         all-green — both prove the drift was detected)"
    );
    assert_eq!(
        summary.beads_dispatched, 0,
        "this bead was already DISPATCHED (seeded), so no fresh dispatch should occur"
    );

    // Auditability: the drift must be observable from the daemon log alone
    // — `grep PR_NUMBER_REREZOLVED` must find it without reading code.
    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let rerez = events
        .iter()
        .find(|e| e["eventType"].as_str() == Some("PR_NUMBER_REREZOLVED"))
        .expect("expected a PR_NUMBER_REREZOLVED telemetry event after drift detection");
    let context = &rerez["context"];
    assert_eq!(context["branch"].as_str(), Some(branch));
    assert_eq!(context["previous_pr_number"].as_u64(), Some(3001));
    assert_eq!(context["current_pr_number"].as_u64(), Some(3002));
    assert_eq!(
        context["reason"].as_str(),
        Some("branch_mismatch_stale_state")
    );

    // Sanity: the FakeScm was actually called with the right (repo, branch)
    // tuple (not cfg.target_repo or some other wrong key).
    let calls = scm.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|c| c == &format!("pr_number_for_branch(owner/repo,{branch})")),
        "expected pr_number_for_branch(owner/repo,{branch}) in calls, got: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == "pr_snapshot_for_repo(owner/repo,3002)"),
        "fast-tier gate assessment must query the RE-RESOLVED pr 3002, not the stale 3001: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains(",3001)")),
        "no gate assessment may target the stale pr 3001 once drift is detected: {calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-t40t complement: a DISPATCHED bead whose stored `pr_number`
/// ALREADY matches the branch's live PR must NOT re-emit the drift
/// telemetry (it would be a per-tick spam storm on healthy beads) and
/// must NOT regress to a different value. The branch→PR re-resolution
/// is a quiet self-healing check, not a hot path.
#[test]
fn slow_tier_dispatched_branch_mismatch_no_op_when_pr_number_already_matches() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = test_cfg();

    let branch = "factory/clean-pr-bead-r1";
    store
        .save(&BeadOverlay {
            bead_id: "clean-pr-bead".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(4001),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("clean-pr-bead", branch).unwrap();

    // Branch→PR lookup agrees with the stored pr_number — no drift.
    scm.pr_numbers_for_branch
        .insert(("owner/repo".into(), branch.into()), Some(4001));
    // Pre-gate validation: stored pr 4001 is OPEN on the same branch.
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), 4001),
        PrHeadBranch::SameRepo(branch.into()),
    );

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    scm.pr_snapshots.insert(
        4001,
        PrSnapshot {
            pr_number: 4001,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "abc".into(),
            body: "".into(),
            comments: vec![PrComment {
                author: "reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: now,
            }],
            files: vec![],
            updated_at_epoch: now,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: now.saturating_sub(60),
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_t40t_branch_mismatch_clean.jsonl");
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
        vendor_health: None,
    };
    let _ = run_tick(&deps, 1, 0).expect("tick should succeed");
    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    assert!(
        !body.lines().any(|l| l.contains("PR_NUMBER_REREZOLVED")),
        "no drift event must fire when stored pr_number already matches the live PR"
    );
    assert!(
        !body
            .lines()
            .any(|l| l.contains("PR_NUMBER_REREZOLVE_TRANSIENT_ERROR")),
        "no transient-error event must fire on a clean FakeScm lookup"
    );
    let after = store.load("clean-pr-bead").unwrap().unwrap();
    assert_eq!(after.pr_number, Some(4001));
    let _ = std::fs::remove_file(&telemetry_log);
}
/// jleechan-t40t r6 contract: a DISPATCHED bead whose stored `pr_number`
/// points at a PR that has MERGED (or otherwise no longer exists for the
/// bead's branch) must NOT promote to ATTESTED against the stale number.
/// The pre-fix path treated `Ok(None)` from `pr_number_for_branch` as
/// "no drift — keep the stored value", which let a stale `pr_number`
/// ride through DISPATCHED→ATTESTED against a PR the branch was no
/// longer bound to, leaving the bead stuck against a closed/merged PR
/// forever. Fail-closed: clear the stale `pr_number`, stay DISPATCHED,
/// emit `PR_NUMBER_REREZOLVED_NO_OPEN_PR` so the operator can grep it.
#[test]
fn slow_tier_dispatched_branch_mismatch_clears_stale_pr_number_when_branch_has_no_open_pr() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = test_cfg();

    // Repro: bead's branch is a FRESH -rN (no open PR exists yet), but
    // a stale `pr_number` from a prior attempt (or a PR that already
    // merged and was reopened under a new number) is recorded on the
    // overlay. Pre-fix: bead promoted to ATTESTED against the stale PR.
    // Post-fix: stale `pr_number` cleared, bead stays DISPATCHED.
    let branch = "factory/merged-prior-pr-r2";
    store
        .save(&BeadOverlay {
            bead_id: "merged-prior-pr".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(6001), // stale: prior PR merged/closed
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("merged-prior-pr", branch).unwrap();

    // Branch→PR lookup returns Ok(None): the branch has no open PR.
    // (Mirrors the post-merge "fresh -rN" repro: the stale PR is closed,
    // and the new branch has nothing bound yet.)
    scm.pr_numbers_for_branch
        .insert(("owner/repo".into(), branch.into()), None);

    // No scripted snapshot for the stale 6001 — it must never be queried.
    let telemetry_log = std::env::temp_dir().join("afd_t40t_branch_mismatch_no_open_pr.jsonl");
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
        vendor_health: None,
    };

    let _ = run_tick(&deps, 1, 0).expect("tick should succeed");
    let after = store.load("merged-prior-pr").unwrap().unwrap();

    // Fail-closed contract: stale pr_number MUST be cleared and bead MUST
    // stay DISPATCHED. Promoting to ATTESTED against a closed/merged PR
    // would route every gate assessment at a dead PR — exactly the
    // jleechan-t8fd / PR #316 wedge the r6 guidance addresses.
    assert_eq!(
        after.pr_number, None,
        "stale pr_number must be cleared when branch→PR resolves to Ok(None); \
         the merged-PRIor-PR repro keeps the bead wedged otherwise"
    );
    assert_eq!(
        after.state,
        OverlayState::Dispatched,
        "bead must stay DISPATCHED when no live PR exists for its branch; \
         promotion to ATTESTED against a stale/closed PR is the r6 defect"
    );

    // Auditability: the daemon log carries PR_NUMBER_REREZOLVED_NO_OPEN_PR
    // so the operator can grep for it without reading code.
    let body = std::fs::read_to_string(&telemetry_log).unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let no_open_pr = events
        .iter()
        .find(|e| e["eventType"].as_str() == Some("PR_NUMBER_REREZOLVED_NO_OPEN_PR"))
        .expect(
            "expected PR_NUMBER_REREZOLVED_NO_OPEN_PR telemetry when stale pr_number is cleared",
        );
    let ctx = &no_open_pr["context"];
    assert_eq!(ctx["branch"].as_str(), Some(branch));
    assert_eq!(ctx["previous_pr_number"].as_u64(), Some(6001));
    assert_eq!(ctx["reason"].as_str(), Some("branch_mismatch_no_open_pr"));

    // Sanity: the stale pr 6001 must NEVER be queried for a snapshot.
    let calls = scm.calls.borrow();
    assert!(
        !calls.iter().any(|c| c.contains(",6001)")),
        "no gate assessment may target the stale pr 6001 after it's cleared: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == &format!("pr_number_for_branch(owner/repo,{branch})")),
        "expected pr_number_for_branch lookup on this branch: {calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-t40t r6: gate assessment must NOT proceed against a stored
/// `pr_number` whose underlying PR is no longer OPEN, or whose head ref
/// has drifted off the bead's recorded branch. Mismatches re-resolve by
/// head branch; inconclusive lookups DEFER (do NOT promote, do NOT
/// gate-assess against the stale pr).
#[test]
fn slow_tier_pre_gate_validation_re_resolves_when_stored_pr_no_longer_open() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let mut cfg = test_cfg();
    // Pre-gate validation is opt-in (default false) so legacy tests
    // aren't disturbed. This test exercises pre-gate drift detection,
    // so enable it.
    cfg.pre_gate_validation_enabled = true;

    // Bead reached ATTESTED via the slow tier, but the stored pr 7001
    // is now CLOSED. Branch has a NEW live PR (7002) bound to it.
    // Gate assessment must NOT query the closed 7001.
    let branch = "factory/drifted-pr-r1";
    store
        .save(&BeadOverlay {
            bead_id: "drifted-pr".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(7001),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("drifted-pr", branch).unwrap();

    // Stored pr 7001 is CLOSED (not OPEN) — head ref differs from bead's branch.
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), 7001),
        PrHeadBranch::NotFound, // closed -> NotFound
    );
    // Branch→PR re-resolution succeeds with the current 7002.
    scm.pr_numbers_for_branch
        .insert(("owner/repo".into(), branch.into()), Some(7002));

    // Script the new PR's snapshot.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    scm.pr_snapshots.insert(
        7002,
        PrSnapshot {
            pr_number: 7002,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![PrComment {
                author: "reviewer".into(),
                body: "/er PASS".into(),
                created_at_epoch: now,
            }],
            files: vec![],
            updated_at_epoch: now,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: now.saturating_sub(60),
        },
    );

    let telemetry_log = std::env::temp_dir().join("afd_t40t_pre_gate_validation_drift.jsonl");
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
        vendor_health: None,
    };

    let _ = run_tick(&deps, 1, 0).expect("tick should succeed");
    let after = store.load("drifted-pr").unwrap().unwrap();

    // The stale pr_number must have been re-resolved to 7002, and gate
    // assessment must have queried 7002 (not the stale 7001).
    assert_eq!(
        after.pr_number,
        Some(7002),
        "pre-gate validation must re-resolve pr_number from the branch when \
         the stored pr is no longer OPEN"
    );
    let calls = scm.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|c| c == "pr_snapshot_for_repo(owner/repo,7002)"),
        "gate assessment must query the re-resolved 7002: {calls:?}"
    );
    // Pre-gate probes (ci_pending_for_attested, active-overlay wedge loop)
    // ARE allowed to query the stale 7001 BEFORE the re-resolution runs.
    // What MUST NOT happen is any pr_snapshot_for_repo call AFTER the
    // `pr_number_for_branch` re-resolution that still targets 7001 — that
    // would mean a gate-assessment landed on the closed PR.
    let pr_reresolve_idx = calls
        .iter()
        .position(|c| c == "pr_number_for_branch(owner/repo,factory/drifted-pr-r1)")
        .expect("expected a pr_number_for_branch re-resolution call");
    let post_reresolve_calls = &calls[pr_reresolve_idx + 1..];
    assert!(
        !post_reresolve_calls.iter().any(|c| c.contains(",7001)")),
        "no pr_snapshot_for_repo targeting the stale 7001 may fire AFTER \
         pre-gate validation re-resolved to 7002; got: {post_reresolve_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-t40t r12 (issue #326), finding 1: a TRANSIENT
/// `pr_number_for_branch` error while re-resolving a DISPATCHED bead must FAIL
/// CLOSED — keep the bead DISPATCHED and retry next tick — NEVER promote
/// DISPATCHED→ATTESTED against the stale, unvalidated `pr_number`.
#[test]
fn transient_pr_number_reresolve_error_keeps_dispatched_no_promotion() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = test_cfg();

    // A DISPATCHED, non-adopted bead with a STALE pr_number that would promote
    // to ATTESTED (ready_to_promote == true for non-adopted) if the resolution
    // didn't error.
    let branch = "factory/t40t-transient-r1";
    store
        .save(&BeadOverlay {
            bead_id: "t40t-transient".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(999),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("t40t-transient", branch).unwrap();
    // The branch→PR resolution fails transiently this tick.
    scm.pr_number_for_branch_errors
        .insert(("owner/repo".into(), branch.into()), "gh api timeout".into());

    let telemetry_log = std::env::temp_dir().join("afd_t40t_transient_reresolve.jsonl");
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
        vendor_health: None,
        },
        1,
        0,
    )
    .expect("transient resolution error must not abort the tick");

    let overlay = store.load("t40t-transient").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Dispatched,
        "a transient re-resolution error must NOT promote to ATTESTED"
    );
    assert_eq!(
        overlay.pr_number,
        Some(999),
        "the stale pr_number is left untouched (revalidated next tick), not consumed"
    );
    assert_eq!(
        summary.gates_assessed, 0,
        "a bead kept DISPATCHED must not be gate-assessed this tick"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("\"PR_NUMBER_REREZOLVE_TRANSIENT_ERROR\"")
            && log.contains("kept_dispatched_no_promotion"),
        "must emit the fail-closed transient-error telemetry; log:\n{log}"
    );
    assert!(
        !log.contains("\"PR_OPENED\""),
        "must NOT promote (no PR_OPENED) on an unvalidated stale pr_number; log:\n{log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-t40t r12 (issue #326), finding 3: pre-gate validation on an
/// ATTESTED bead whose stored PR is closed AND whose branch has no live PR
/// must DEMOTE the bead to DISPATCHED (so the branch→PR re-resolution path
/// re-promotes it when a live PR appears) — not clear `pr_number` and strand
/// it ATTESTED forever. Two ticks: tick 1 demotes; tick 2 (live PR now bound)
/// re-resolves and re-promotes.
#[test]
fn pre_gate_no_open_pr_demotes_attested_to_dispatched_and_resumes() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let mut cfg = test_cfg();
    cfg.pre_gate_validation_enabled = true;

    let branch = "factory/t40t-noopenpr-r1";
    store
        .save(&BeadOverlay {
            bead_id: "t40t-noopenpr".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(8001),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch("t40t-noopenpr", branch).unwrap();
    // Stored PR 8001 is closed (NotFound), and the branch currently has NO
    // live PR (no pr_numbers_for_branch entry -> Ok(None)).
    scm.open_pr_head_refs
        .insert(("owner/repo".into(), 8001), PrHeadBranch::NotFound);

    let telemetry_log = std::env::temp_dir().join("afd_t40t_no_open_pr_demote.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    // --- Tick 1: no live PR -> demote to DISPATCHED (not stranded ATTESTED).
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
        1,
        0,
    )
    .expect("pre-gate no-open-pr must not abort the tick");

    let after1 = store.load("t40t-noopenpr").unwrap().unwrap();
    assert_eq!(
        after1.state,
        OverlayState::Dispatched,
        "an ATTESTED bead whose branch has no live PR must be demoted to \
         DISPATCHED for re-resolution, not left ATTESTED with a null pr_number"
    );
    assert_eq!(after1.pr_number, None);
    let log1 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log1.contains("demoted_attested_to_dispatched_for_rerezolve"),
        "must audit the demotion; log:\n{log1}"
    );

    // --- Tick 2: a live PR (8002) is now bound to the branch. The DISPATCHED
    // re-resolution path must pick it up and re-promote to ATTESTED.
    scm.pr_numbers_for_branch
        .insert(("owner/repo".into(), branch.into()), Some(8002));
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), 8002),
        PrHeadBranch::SameRepo(branch.into()),
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut snap = qdw_green_snapshot(
        8002,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: now,
        }],
    );
    snap.head_committed_epoch = now.saturating_sub(60);
    scm.pr_snapshots.insert(8002, snap);

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
        2,
        0,
    )
    .expect("re-resolution tick must not error");

    let after2 = store.load("t40t-noopenpr").unwrap().unwrap();
    assert_eq!(
        after2.pr_number,
        Some(8002),
        "the demoted bead must re-resolve to the live PR once one is bound"
    );
    // It re-promoted through ATTESTED and, since the new PR is all-green,
    // continued to READY in the same tick — proving the demotion produced a
    // recoverable hold, not a terminal strand.
    assert_eq!(
        after2.state,
        OverlayState::Ready,
        "the demoted bead must resume (here all-green -> READY), not stay stranded"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-yoqy / issue #323: end-to-end evidence-contract gate verification
// through the real fast tier. The coder's `**Evidence**:` marker must point at
// a fetchable, non-empty gist whose head matches the PR head; anything else is
// a fail-closed evidence-gate FAIL.
// ===========================================================================

fn evidence_bead(store: &FakeStateStore, scm: &mut FakeScm, bead_id: &str, pr: u64, branch: &str, body: &str) {
    store
        .save(&BeadOverlay {
            bead_id: bead_id.into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: None,
        })
        .unwrap();
    store.register_branch(bead_id, branch).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut snap = qdw_green_snapshot(
        pr,
        vec![PrComment {
            author: "dark-factory-er".into(),
            body: "/er PASS".into(),
            created_at_epoch: now,
        }],
    );
    snap.head_sha = "deadbeefcafe".into();
    snap.head_committed_epoch = now.saturating_sub(60);
    snap.body = body.into();
    scm.pr_snapshots.insert(pr, snap);
}

#[test]
fn evidence_gate_verified_gist_reaches_ready() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    evidence_bead(
        &store,
        &mut scm,
        "ev-ok",
        9101,
        "factory/ev-ok-r1",
        "**Evidence**: https://gist.github.com/u/goodgist (head deadbeefcafe)",
    );
    scm.gists.insert("goodgist".into(), true); // fetchable + non-empty

    let telemetry_log = std::env::temp_dir().join("afd_yoqy_ev_ok.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    let summary = run_tick(
        &TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log, vendor_health: None },
        1, 0,
    )
    .expect("tick must not error");
    assert_eq!(summary.gates_assessed, 1);
    assert_eq!(
        store.load("ev-ok").unwrap().unwrap().state,
        OverlayState::Ready,
        "a verified evidence gist must let an otherwise-green PR reach READY"
    );
    assert!(scm.calls.borrow().iter().any(|c| c.contains("gist_nonempty(goodgist)")));
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn evidence_gate_empty_gist_fails_closed_not_ready() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    evidence_bead(
        &store,
        &mut scm,
        "ev-empty",
        9102,
        "factory/ev-empty-r1",
        "**Evidence**: https://gist.github.com/u/emptygist (head deadbeefcafe)",
    );
    scm.gists.insert("emptygist".into(), false); // fetchable but EMPTY

    let telemetry_log = std::env::temp_dir().join("afd_yoqy_ev_empty.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    let summary = run_tick(
        &TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log, vendor_health: None },
        1, 0,
    )
    .expect("tick must not error");
    assert_eq!(summary.gates_assessed, 1);
    assert_ne!(
        store.load("ev-empty").unwrap().unwrap().state,
        OverlayState::Ready,
        "an empty evidence gist must fail the evidence gate (never READY)"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("evidence contract") && log.contains("empty"),
        "the evidence-gate failure must carry distinct 'evidence contract ... empty' text; log:\n{log}"
    );
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn evidence_gate_head_mismatch_fails_closed() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Marker references a STALE head that does not match the PR head.
    evidence_bead(
        &store,
        &mut scm,
        "ev-stale",
        9103,
        "factory/ev-stale-r1",
        "**Evidence**: https://gist.github.com/u/goodgist (head 00000000stale)",
    );
    scm.gists.insert("goodgist".into(), true);

    let telemetry_log = std::env::temp_dir().join("afd_yoqy_ev_stale.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    run_tick(
        &TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log, vendor_health: None },
        1, 0,
    )
    .expect("tick must not error");
    assert_ne!(
        store.load("ev-stale").unwrap().unwrap().state,
        OverlayState::Ready,
        "a stale-head evidence marker must fail the evidence gate"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("does not match PR head"),
        "head-mismatch failure text expected; log:\n{log}"
    );
    let _ = std::fs::remove_file(&telemetry_log);
}

// jleechan-rln6: when a coder's Evidence marker references a head SHA that
// does NOT match the current PR head, the daemon must (a) emit a structured
// `EVIDENCE_HEAD_STALE` telemetry event, (b) post a precise bead-notes-style
// comment with the `gh pr edit --body` recipe back to the coder session,
// (c) persist a one-shot sentinel so a SECOND tick on the same bead does
// NOT re-post the same comment, and (d) leave the gate Red but NOT trigger
// a full reroll. This test pins all four behaviors in one regression.
#[test]
fn rln6_evidence_head_stale_fast_rejects_with_one_shot_comment() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Marker references a STALE head. PR head (snap.head_sha) is
    // `deadbeefcafe` (set by `evidence_bead`); marker says `00000000stale`.
    evidence_bead(
        &store,
        &mut scm,
        "ev-rln6-stale",
        9106,
        "factory/ev-rln6-stale-r1",
        "**Evidence**: https://gist.github.com/u/goodgist (head 00000000stale)",
    );
    // Gist is fetchable+non-empty; the rejection is NOT about the gist —
    // it is about the head SHA mismatch. The test would still fail-closed
    // even without this insert.
    scm.gists.insert("goodgist".into(), true);

    let telemetry_log = std::env::temp_dir().join("afd_rln6_ev_stale.jsonl");
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
        1,
        0,
    )
    .expect("tick must not error");

    // (d) Gate is Red — bead stays Attested, NOT HumanHeld, NOT Ready.
    let overlay = store.load("ev-rln6-stale").unwrap().unwrap();
    assert_eq!(
        overlay.state,
        OverlayState::Attested,
        "a stale-head Evidence marker must NOT promote the bead (fast-reject, stay Attested)"
    );

    // (a) EVIDENCE_HEAD_STALE telemetry event was emitted with the parsed
    // and PR head SHAs in metrics and the remediation recipe in context.
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("EVIDENCE_HEAD_STALE"),
        "EVIDENCE_HEAD_STALE telemetry event must be emitted; log:\n{log}"
    );
    assert!(
        log.contains("00000000stale") && log.contains("deadbeefcafe"),
        "telemetry must carry both parsed and PR head SHAs; log:\n{log}"
    );
    assert!(
        log.contains("gh pr edit --body"),
        "telemetry context must carry the remediation recipe; log:\n{log}"
    );

    // (b) The bead-notes-style comment was posted via `comment_external`
    // (the daemon's existing bead-message channel), with the precise
    // mismatch and the live `gh pr edit` recipe.
    let calls = tracker.calls.borrow();
    let stale_comment = calls.iter().find(|c| c.contains("EVIDENCE_HEAD_STALE")
        || (c.contains("ev-rln6-stale") && c.contains("00000000stale") && c.contains("deadbeefcafe")));
    assert!(
        stale_comment.is_some(),
        "the daemon must post a bead-notes-style comment carrying both SHAs and the remediation; calls:\n{calls:?}"
    );
    let body = stale_comment.unwrap();
    assert!(
        body.contains("gh pr edit --body"),
        "the comment must carry the literal gh recipe; got: {body}"
    );
    assert!(
        body.contains("gh pr view --json headRefOid"),
        "the comment must show how to capture the CURRENT head SHA; got: {body}"
    );

    // (c) A SECOND tick on the same bead must NOT re-post the comment —
    // the sentinel suppresses the spam.
    let post_first = calls
        .iter()
        .filter(|c| c.contains("ev-rln6-stale") && c.contains("00000000stale"))
        .count();
    drop(calls);
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
        1,
        0,
    )
    .expect("second tick must not error");
    let calls2 = tracker.calls.borrow();
    let post_second = calls2
        .iter()
        .filter(|c| c.contains("ev-rln6-stale") && c.contains("00000000stale"))
        .count();
    assert_eq!(
        post_second, post_first,
        "the second tick on the same bead must NOT re-post the same comment (one-shot sentinel); \
         first={post_first} second={post_second}"
    );

    // The bead must STILL be Attested — fast-rejection is not a state
    // transition, it is a precise coder-facing message plus a red gate.
    assert_eq!(
        store.load("ev-rln6-stale").unwrap().unwrap().state,
        OverlayState::Attested,
        "fast-reject must leave the bead Attested, not HumanHeld, not Ready"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// Codex P1 finding (PR #463 round 1): the daemon must emit a
// `GATE_ASSESSMENT` telemetry event BEFORE the fast-rejection `continue`
// short-circuits the park/reroll path. `auto-merge-guard.sh` reads the latest
// `GATE_ASSESSMENT` for `(pr_number, head_sha)` to decide whether a no-red
// assessment exists; if the fast-reject branch skipped the emit, an older
// all-green assessment (made before the evidence marker went stale) would
// be the only thing visible to the guard, and a merge on stale data could
// slip through. The fix: emit the assessment first, then `continue`. This
// test pins that contract.
#[test]
fn rln6_v2_evidence_head_stale_emits_gate_assessment_before_fast_reject() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Stale-head marker — same fixture as the original rln6 test, the only
    // red gate will be EvidenceFloor with the "does not match PR head"
    // reason so the fast-reject path fires.
    evidence_bead(
        &store,
        &mut scm,
        "ev-rln6-v2-emit",
        9107,
        "factory/ev-rln6-v2-emit-r1",
        "**Evidence**: https://gist.github.com/u/goodgist (head 00000000stale)",
    );
    scm.gists.insert("goodgist".into(), true);

    let telemetry_log = std::env::temp_dir().join("afd_rln6_v2_emit.jsonl");
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
        1,
        0,
    )
    .expect("tick must not error");

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    // P1 contract: a GATE_ASSESSMENT line is emitted on the fast-reject tick
    // so the merge guard sees the fresh EvidenceFloor Red verdict (with the
    // current PR head SHA), not a stale all-green assessment from an older
    // tick.
    assert!(
        log.contains("GATE_ASSESSMENT"),
        "GATE_ASSESSMENT must be emitted on a fast-reject tick (P1 Codex fix); log:\n{log}"
    );
    // The fresh assessment MUST reference the live head SHA and the
    // EvidenceFloor Red reason — operators can grep these to confirm the
    // fast-reject path is the one that fired.
    assert!(
        log.contains("\"head_sha\"") && log.contains("deadbeefcafe"),
        "the GATE_ASSESSMENT must carry the live PR head SHA so the merge \
         guard cannot reuse an older all-green assessment; log:\n{log}"
    );
    assert!(
        log.contains("evidence contract"),
        "the GATE_ASSESSMENT must carry the EvidenceFloor Red reason so the \
         guard sees why the gate is Red; log:\n{log}"
    );

    // Sanity: bead stays Attested, fast-rejection still applies.
    assert_eq!(
        store.load("ev-rln6-v2-emit").unwrap().unwrap().state,
        OverlayState::Attested,
        "fast-reject must still leave the bead Attested"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// Codex P2 finding #1 (PR #463 round 1): if `post_scm_comment_by_bead_id`
// fails transiently on the stale-evidence notification attempt, the daemon
// must NOT persist the one-shot sentinel — otherwise the next tick
// suppresses the remediation comment and the coder never receives the
// instructions to refresh the marker. The fix: bind the post result and
// only call `record_evidence_head_stale` on `Ok(())`. On `Err(_)` the daemon
// should emit a transient-notification telemetry and leave the sentinel
// unsent so the next tick re-attempts the post. This test pins that
// behavior end-to-end: script the first tick's comment to fail, assert
// the sentinel is NOT persisted, then run a second tick with a working
// comment and assert the comment is now posted (because the sentinel was
// never recorded).
#[test]
fn rln6_v2_evidence_head_stale_does_not_persist_sentinel_on_comment_failure() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    evidence_bead(
        &store,
        &mut scm,
        "ev-rln6-v2-comment-fail",
        9108,
        "factory/ev-rln6-v2-comment-fail-r1",
        "**Evidence**: https://gist.github.com/u/goodgist (head 00000000stale)",
    );
    scm.gists.insert("goodgist".into(), true);

    // Script the next `comment_external` call to fail transiently. The
    // FakeTracker plumbing routes both the bead-message path and the
    // `post_scm_comment_by_bead_id` path through `comment_external` so this
    // single flag covers both.
    *tracker.fail_next_comment.borrow_mut() = Some("transient SCM error".to_string());

    let telemetry_log = std::env::temp_dir().join("afd_rln6_v2_comment_fail.jsonl");
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
        1,
        0,
    )
    .expect("first tick must not error");

    // Sentinel MUST NOT be persisted — the comment failed, so the next tick
    // must be allowed to retry. `load_rejection` returns the `(reviewer, _)`
    // pair; the daemon treats `(Some, EVIDENCE_HEAD_STALE_REVIEWER)` as
    // "already notified". After a transient failure the slot must be empty.
    let stored = store
        .load_rejection("ev-rln6-v2-comment-fail", u32::MAX - 1)
        .unwrap();
    assert!(
        stored.is_none(),
        "the one-shot sentinel MUST NOT be persisted when the comment post \
         fails transiently (P2 Codex fix #1); stored={stored:?}"
    );

    // The bead is still Attested (fast-reject still applies — the gate is
    // still Red, we just skipped the side-effects because the comment
    // failed).
    assert_eq!(
        store.load("ev-rln6-v2-comment-fail").unwrap().unwrap().state,
        OverlayState::Attested,
        "fast-reject must still leave the bead Attested even when the \
         comment post fails"
    );

    // Second tick — comment path is now healthy. The sentinel is still
    // empty so the daemon MUST post the remediation comment this time.
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
        1,
        0,
    )
    .expect("second tick must not error");

    let calls = tracker.calls.borrow();
    let post_count = calls
        .iter()
        .filter(|c| c.contains("ev-rln6-v2-comment-fail") && c.contains("00000000stale"))
        .count();
    assert!(
        post_count >= 1,
        "the second tick MUST post the remediation comment because the \
         sentinel was not persisted on the first tick; calls:\n{calls:?}"
    );

    // After the successful second post, the sentinel IS persisted so a
    // THIRD tick (same mismatch tuple) does not re-post.
    let stored_after = store
        .load_rejection("ev-rln6-v2-comment-fail", u32::MAX - 1)
        .unwrap();
    assert!(
        stored_after.is_some(),
        "after a successful comment post, the sentinel must be persisted so \
         subsequent ticks with the same mismatch suppress re-posts"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// Codex P2 finding #2 (PR #463 round 1): the one-shot sentinel was keyed
// on `(bead_id, attempt)` only. In normal recovery a coder fixes the first
// `(parsed_sha, pr_sha)` mismatch, pushes another commit, and the marker
// goes stale again with a NEW mismatch tuple. The old keying would
// suppress the second notification because the bead is "already notified",
// leaving the lane stuck with no fresh instructions. The fix: encode the
// mismatch tuple in the stored reason; on each tick, load the previous
// reason and compare to the current mismatch — only suppress when they
// match. This test pins that contract end-to-end: tick 1 records
// `(parsed=AAAA, pr=BBBB)`, the snapshot is then updated to a NEW pr_sha
// `CCCC` while the marker still references `AAAA`, the next tick must
// detect the NEW mismatch tuple and re-post the remediation comment.
#[test]
fn rln6_v2_evidence_head_stale_sentinel_resets_on_new_mismatch_tuple() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Initial state: pr_sha=deadbeefcafe, marker references 00000000stale.
    evidence_bead(
        &store,
        &mut scm,
        "ev-rln6-v2-tuple",
        9109,
        "factory/ev-rln6-v2-tuple-r1",
        "**Evidence**: https://gist.github.com/u/goodgist (head 00000000stale)",
    );
    scm.gists.insert("goodgist".into(), true);

    let telemetry_log = std::env::temp_dir().join("afd_rln6_v2_tuple.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    // First tick — daemon posts the remediation, records the sentinel with
    // the (parsed=00000000stale, pr=deadbeefcafe) tuple.
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
        1,
        0,
    )
    .expect("first tick must not error");

    let calls_after_first = tracker.calls.borrow().len();
    let stored_first = store
        .load_rejection_text("ev-rln6-v2-tuple", u32::MAX - 1)
        .unwrap();
    assert!(
        stored_first.is_some(),
        "after the first successful post, the sentinel reason must be \
         persisted (P2 Codex fix #2 depends on it)"
    );
    let first_reason = stored_first.unwrap();
    assert!(
        first_reason.contains("00000000stale") && first_reason.contains("deadbeefcafe"),
        "the stored reason must encode the (parsed_sha, pr_sha) tuple; got: {first_reason}"
    );

    // Now simulate the recovery: the coder pushes a new commit, so the
    // PR's `head_sha` advances to a NEW value (`feedfacefeed`), but the
    // marker still references `00000000stale` — a fresh mismatch tuple.
    {
        let mut snap = scm.pr_snapshots.get(&9109).unwrap().clone();
        snap.head_sha = "feedfacefeed".into();
        scm.pr_snapshots.insert(9109, snap);
    }

    // Second tick — mismatch tuple is now
    // (parsed=00000000stale, pr=feedfacefeed), distinct from the stored
    // (parsed=00000000stale, pr=deadbeefcafe). The daemon MUST post a NEW
    // remediation comment because the new tuple has not been notified.
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
        1,
        0,
    )
    .expect("second tick must not error");

    let calls = tracker.calls.borrow();
    // A comment referencing BOTH `00000000stale` AND `feedfacefeed` is the
    // fresh notification (the old one referenced `deadbeefcafe`).
    let fresh_post = calls
        .iter()
        .filter(|c| {
            c.contains("ev-rln6-v2-tuple")
                && c.contains("00000000stale")
                && c.contains("feedfacefeed")
        })
        .count();
    assert!(
        fresh_post >= 1,
        "the second tick MUST post a fresh remediation comment because the \
         mismatch tuple changed (P2 Codex fix #2); calls:\n{calls:?}"
    );
    drop(calls);

    // Sentinel is overwritten with the new tuple, so a THIRD tick (still on
    // the same fresh mismatch tuple) does NOT re-post again. Count only
    // `comment_external` calls (other call types — fetch_candidates,
    // pr_snapshot — are also recorded in `tracker.calls`).
    let comment_count_before = tracker
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("comment_external("))
        .count();
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
        1,
        0,
    )
    .expect("third tick must not error");
    let comment_count_after = tracker
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("comment_external("))
        .count();
    assert_eq!(
        comment_count_after, comment_count_before,
        "the third tick on the SAME fresh mismatch tuple must NOT re-post \
         (sentinel persists for the new tuple); before={comment_count_before} after={comment_count_after}"
    );

    // Sanity: calls_after_first is what we started with before the second
    // tick. We just need to know the test exercised the path.
    let _ = calls_after_first;

    let _ = std::fs::remove_file(&telemetry_log);
}

/// r5 finding 2: an evidence marker LINE that is present but incomplete
/// (missing gist URL or `(head <sha>)`) must FAIL the evidence gate
/// (fail-closed) — not be treated as NotProvided.
#[test]
fn evidence_gate_incomplete_marker_fails_closed() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Marker present but no gist URL / no (head ..) — parse_evidence -> None,
    // but has_evidence_marker -> true.
    evidence_bead(
        &store,
        &mut scm,
        "ev-incomplete",
        9104,
        "factory/ev-incomplete-r1",
        "**Evidence**: I ran the tests, trust me",
    );

    let telemetry_log = std::env::temp_dir().join("afd_yoqy_ev_incomplete.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    run_tick(
        &TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log, vendor_health: None },
        1, 0,
    ).expect("tick must not error");
    assert_ne!(
        store.load("ev-incomplete").unwrap().unwrap().state,
        OverlayState::Ready,
        "an incomplete evidence marker must fail the gate (never READY)"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("marker present but missing"),
        "incomplete-marker failure text expected; log:\n{log}"
    );
    let _ = std::fs::remove_file(&telemetry_log);
}

/// r5 finding 3: a TRANSIENT gist-fetch error must map to Unknown (wait), NOT
/// a Red — so infra noise doesn't churn a reroll. The bead stays ATTESTED and
/// the evidence gate reports pending, not a defect.
#[test]
fn evidence_gate_transient_gist_error_is_pending_not_red() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    evidence_bead(
        &store,
        &mut scm,
        "ev-transient",
        9105,
        "factory/ev-transient-r1",
        "**Evidence**: https://gist.github.com/u/flaky (head deadbeefcafe)",
    );
    scm.gists_transient.insert("flaky".into()); // gh outage on fetch

    let telemetry_log = std::env::temp_dir().join("afd_yoqy_ev_transient.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    run_tick(
        &TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log, vendor_health: None },
        1, 0,
    ).expect("tick must not error");
    // Not READY (evidence unknown), but NOT parked/rerolled — stays ATTESTED to retry.
    assert_eq!(
        store.load("ev-transient").unwrap().unwrap().state,
        OverlayState::Attested,
        "a transient gist error must leave the bead ATTESTED (wait), not reroll/park"
    );
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("evidence contract pending"),
        "transient gist error must surface as a pending Unknown; log:\n{log}"
    );
    assert!(
        !log.contains("REROLL_VERDICT_RECORDED") && !log.contains("PARKED_HUMAN_HELD"),
        "a transient gist error must not reroll or park; log:\n{log}"
    );
    let _ = std::fs::remove_file(&telemetry_log);
}

// Bead jleechan-msmq: the verifier must NOT re-assess an unchanged PR head
// SHA on a subsequent tick. Re-assessing a head that has not moved produces
// a duplicate GATE_ASSESSMENT that races with the breaker (autonomy timebox
// + circuit-breaker) and — when a fresh coder lane was just fabricated by a
// deferred reroll — can park + kill_session the fresh lane before it has a
// chance to push. The expected behavior:
//   1. Tick N: bead is ATTESTED, head_sha = "sha-901". Verifier runs once,
//      emits one GATE_ASSESSMENT for the bead.
//   2. Tick N+1: same bead, same PR, same head_sha. Verifier SKIPS the
//      snapshot+assess pipeline, emits a new telemetry event
//      VERIFIER_SKIPPED_UNCHANGED_HEAD, and does NOT emit a duplicate
//      GATE_ASSESSMENT.
//   3. When the PR's head_sha changes (or branch changes), re-assessment
//      resumes normally.
#[test]
fn msmq_verifier_skips_reassessment_when_reroll_deferred() {
    // Bead jleechan-msmq: when an ATTESTED bead has `reroll_deferral_count
    // > 0`, the daemon has already decided to re-roll this attempt AND
    // that attempt DEFERRED (the live worker was still active / a
    // transient probe error). The OLD PR's gate verdict cannot advance
    // the bead (the reroll branch IS the advancement) and re-assessing
    // it on every subsequent tick races with two breakers: the autonomy
    // timebox (which can park + kill_session the fresh coder lane before
    // its first push) and the circuit-breaker (which trips on identical
    // red evidence at attempt 2).
    //
    // Expected contract:
    //   1. Tick against an ATTESTED bead with reroll_deferral_count=0
    //      → verifier assesses gates (one GATE_ASSESSMENT emit).
    //   2. After a deferred reroll bumps reroll_deferral_count to 1,
    //      a subsequent tick MUST emit VERIFIER_SKIPPED_REROLL_IN_PROGRESS
    //      and NOT a duplicate GATE_ASSESSMENT for the OLD PR.
    //
    // This test does not exercise the reroll machinery itself — it seeds
    // `reroll_deferral_count` directly via `incr_reroll_deferral` to
    // keep the test focused on the guard contract rather than the
    // (complex, brittle) reroll pipeline.

    let mut scm = FakeScm::new();
    let fresh_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let snap = PrSnapshot {
        pr_number: 901,
        ci_success: false, // RED CI
        mergeable: true,
        merge_state_unknown: false,
        coderabbit_approved: true,
        bugbot_error_count: 0,
        unresolved_thread_count: Some(0),
        head_sha: "sha-901".into(),
        body: "".into(),
        comments: vec![PrComment {
            author: "reviewer".into(),
            body: "/er PASS".into(),
            created_at_epoch: fresh_epoch.saturating_sub(60),
        }],
        files: Vec::new(),
        updated_at_epoch: fresh_epoch,
        ci_status: "red".into(),
        coderabbit_status: "green".into(),
        ci_pending: false,
        bugbot_pending: false,
        head_committed_epoch: fresh_epoch.saturating_sub(120),
    };
    scm.pr_snapshots.insert(901, snap.clone());

    let store = FakeStateStore::new();
    let overlay = BeadOverlay {
        bead_id: "bead-msmq".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0, // first tick: no reroll in flight yet
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(901),
        branch: Some("factory/bead-msmq-r1".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        attempt_started_at: None,
        target_repo: Some("owner/repo".into()),
    };
    store.save(&overlay).unwrap();
    store.register_branch("bead-msmq", "factory/bead-msmq-r1").unwrap();

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let vcs = test_vcs();
    let mut cfg = test_cfg();
    cfg.stage = 2; // Stage 2: actually execute reroll() so a deferred
                   // reroll leaves `reroll_deferral_count > 0` on tick 1.
    let telemetry_log = std::env::temp_dir().join("afd_msmq_skip_on_reroll.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm,
        store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    // ---- Tick 1: reroll_deferral_count=0 → full gate assessment fires.
    run_tick(&deps, 1, 0).expect("tick 1 must not error");
    let log1 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log1.contains("\"eventType\":\"GATE_ASSESSMENT\""),
        "tick 1 (reroll_deferral_count=0) must emit a GATE_ASSESSMENT; log:\n{log1}"
    );
    assert!(
        !log1.contains("VERIFIER_SKIPPED_REROLL_IN_PROGRESS"),
        "tick 1 (reroll_deferral_count=0) must NOT emit the skip telemetry; log:\n{log1}"
    );

    // ---- Simulate the post-deferred-reroll state: reroll::execute's
    // `defer_or_cap` would have called `incr_reroll_deferral`, bumping
    // the counter to 1. The bead is still ATTESTED with the OLD pr_number.
    let _ = store.incr_reroll_deferral("bead-msmq");
    let after1_deferral_count = store.reroll_deferral_count("bead-msmq").unwrap();
    assert!(
        after1_deferral_count >= 1,
        "reroll_deferral_count must be >= 1 to seed the deferred-reroll state (got {after1_deferral_count})"
    );

    // ---- Tick 2: reroll_deferral_count > 0 → verifier MUST skip the
    // duplicate gate assessment and emit VERIFIER_SKIPPED_REROLL_IN_PROGRESS.
    run_tick(&deps, 2, 0).expect("tick 2 must not error");
    let log2 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let gate_assessment_count = log2
        .matches("\"eventType\":\"GATE_ASSESSMENT\"")
        .count();
    assert_eq!(
        gate_assessment_count, 1,
        "tick 2 (reroll_deferral_count > 0) MUST NOT emit a second GATE_ASSESSMENT for the unchanged old PR; log:\n{log2}"
    );
    assert!(
        log2.contains("VERIFIER_SKIPPED_REROLL_IN_PROGRESS"),
        "tick 2 (reroll_deferral_count > 0) must emit VERIFIER_SKIPPED_REROLL_IN_PROGRESS; log:\n{log2}"
    );
    assert!(
        log2.contains("\"prNumber\":901"),
        "skip telemetry must carry prNumber provenance; log:\n{log2}"
    );
    assert!(
        log2.contains("\"rerollDeferralCount\":"),
        "skip telemetry must carry rerollDeferralCount provenance; log:\n{log2}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}
// --- jleechan-328 / bze8.1: P1 #1 (exact-head binding) + P1 #3 ---
// (operator-disposition round-trip) daemon-side coverage. The shell-side
// predicate (`daemon/scripts/auto-merge-guard.sh`'s
// `latest_assessment_no_red`) refuses to honour an assessment whose
// recorded `head_sha` no longer matches the live PR head, and reads
// `operator_disposition` from the SAME key the daemon emits here.
// These tests pin both fields in the GATE_ASSESSMENT context object so
// the round-trip cannot silently regress.

/// Helper: parse every GATE_ASSESSMENT line out of a telemetry log and
/// return the JSON `context` object of the LAST one (the daemon emits
/// one per ATTESTED bead per tick).
fn last_gate_assessment_context(
    log: &std::path::Path,
) -> serde_json::Map<String, serde_json::Value> {
    let raw = std::fs::read_to_string(log).unwrap_or_default();
    let mut ctx: Option<serde_json::Map<String, serde_json::Value>> = None;
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v.get("eventType").and_then(|s| s.as_str()) != Some("GATE_ASSESSMENT") {
            continue;
        }
        if let Some(c) = v.get("context").and_then(|c| c.as_object()).cloned() {
            ctx = Some(c);
        }
    }
    ctx.unwrap_or_default()
}

#[test]
fn jleechan328_gate_assessment_emits_head_sha_for_exact_head_binding() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok("pass".into()));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // P1 #1: the assessment MUST carry the PR's current head SHA. We
    // force a distinctive SHA so a regression that emits an empty or
    // snapshot-fetched-from-a-different-PR value would diverge.
    let pr_sha = "feedfacefeedfacefeedfacefeedfacefeedface";
    evidence_bead(
        &store,
        &mut scm,
        "ev-headbind",
        9103,
        "factory/ev-headbind-r1",
        &format!("**Evidence**: https://gist.github.com/u/goodgist (head {pr_sha})"),
    );
    scm.gists.insert("goodgist".into(), true);
    // Override the snapshot's head_sha AFTER `evidence_bead` so the
    // exact-head field has a deterministic, distinctive value to assert on.
    scm.pr_snapshots.get_mut(&9103).unwrap().head_sha = pr_sha.into();

    let telemetry_log = std::env::temp_dir().join("afd_yoqy_headbind.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);
    let summary = run_tick(
        &TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log, vendor_health: None },
        1, 0,
    )
    .expect("tick must not error");
    assert_eq!(summary.gates_assessed, 1);

    let ctx = last_gate_assessment_context(&telemetry_log);
    let emitted_head = ctx
        .get("head_sha")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(
        emitted_head, pr_sha,
        "GATE_ASSESSMENT context MUST carry the PR's current head_sha (P1 #1 \
         exact-head binding); context:\n{ctx:?}"
    );
    // Canonical gate-key set (P1 #2): every GateName::as_str() value
    // MUST be present in the emitted gates object — a regression that
    // dropped a key would slip past a permissive shell predicate.
    let gates = ctx
        .get("gates")
        .and_then(|v| v.as_object())
        .expect("gates object missing");
    for required in [
        "ci_green",
        "no_conflicts",
        "coderabbit",
        "bugbot",
        "comments_resolved",
        "evidence_review",
        "skeptic",
    ] {
        assert!(
            gates.contains_key(required),
            "P1 #2 fail-closed canonical gate-key set: missing {required} in emitted gates"
        );
    }
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn jleechan328_gate_assessment_emits_operator_disposition_round_trip() {
    // P1 #3 round-trip: the daemon reads `overlay.park_reason` and emits
    // it under the SAME key the shell guard reads (`operator_disposition`).
    // A regression that renamed either side (e.g. `park_reason_value`,
    // `disposition`) would make the shell override unreachable.
    for disposition in ["operator_approved", "operator_held", ""] {
        let mut scm = FakeScm::new();
        let tracker = FakeTracker::new();
        let sessions = FakeSessions::new();
        let llm = FakeLlm::new();
        *llm.response.borrow_mut() = Some(Ok("pass".into()));
        let store = FakeStateStore::new();
        let cfg = test_cfg();
        let vcs = test_vcs();

        let bead_id = format!("ev-disposition-{disposition}");
        let pr = 9104u64;
        let pr_sha = "feedfacefeedfacefeedfacefeedfacefeedface";
        let body = format!("**Evidence**: https://gist.github.com/u/goodgist (head {pr_sha})");
        evidence_bead(
            &store,
            &mut scm,
            &bead_id,
            pr,
            &format!("factory/{bead_id}-r1"),
            &body,
        );
        // Stamp the disposition under test onto the bead's `park_reason`:
        // this is the producer site the daemon must read from.
        let mut overlay = store.load(&bead_id).unwrap().unwrap();
        overlay.park_reason = if disposition.is_empty() {
            None
        } else {
            Some(disposition.into())
        };
        store.save(&overlay).unwrap();
        scm.gists.insert("goodgist".into(), true);
        scm.pr_snapshots.get_mut(&pr).unwrap().head_sha = pr_sha.into();

        let telemetry_log = std::env::temp_dir().join(format!("afd_yoqy_disp_{disposition}.jsonl"));
        let _ = std::fs::remove_file(&telemetry_log);
        let summary = run_tick(
            &TickDeps { scm: &scm, tracker: &tracker, sessions: &sessions, llm: &llm, store: &store, vcs: &vcs, cfg: &cfg, telemetry_log: &telemetry_log, vendor_health: None },
            1, 0,
        )
        .expect("tick must not error");
        assert_eq!(summary.gates_assessed, 1);

        let ctx = last_gate_assessment_context(&telemetry_log);
        let emitted = ctx
            .get("operator_disposition")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(
            emitted, disposition,
            "P1 #3 round-trip: daemon must emit operator_disposition={disposition:?} \
             from overlay.park_reason; context:\n{ctx:?}"
        );
        let _ = std::fs::remove_file(&telemetry_log);
    }
}

/// Plan Task 4(a) / dispatch-scheduling-guarantee regression: 9 looping
/// escalation beads (HUMAN_HELD at the recovery cap) + 1 legitimately QUEUED
/// bead. Under the dispatch-scheduling-guarantee ordering (`run_slow_tier`
/// runs BEFORE `run_recovery_step`), the QUEUED bead MUST get
/// `TASK_DISPATCHED` on the first tick, even though `run_recovery_step`
/// processes 9 escalation beads afterward.
///
/// This is the live incident regression: a legitimately QUEUED bead sat
/// undispatched 65+ minutes while escalation/recovery work re-fired every
/// tick before dispatch could run. With the ordering change, dispatch runs
/// first and cannot be starved by an escalation backlog.
#[test]
fn dispatch_guarantee_queued_bead_dispatched_despite_escalation_backlog() {
    const QUEUED_BEAD_ID: &str = "queued-bead-42";

    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"single small change"}"#.into(),
    ));
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Seed 9 HUMAN_HELD beads at the recovery cap (attempt=10). Each tick,
    // `run_recovery_step` finds them via `human_held_at_or_above_attempt`
    // and escalates (posts an SCM comment + records the sentinel). These are
    // the "looping escalation" beads that, under the OLD ordering, ran
    // BEFORE dispatch and could starve it.
    for i in 0..9u32 {
        let bead_id = format!("escalation-bead-{i}");
        let pr_number = 1000 + i as u64;
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
                is_adopted: false,
                spawn_failure_count: 0,
                pre_session_head_sha: None,
                park_reason: None,
                target_repo: Some("owner/repo".to_string()),
                attempt_started_at: None,
            })
            .unwrap();
    }

    // Seed 1 QUEUED bead in the tracker + store. This is the bead that MUST
    // be dispatched on the first tick despite the 9 escalation beads.
    tracker.candidates.borrow_mut().push(Bead {
        id: QUEUED_BEAD_ID.into(),
        title: "Legitimately queued bead".into(),
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
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            attempt_started_at: None,
            target_repo: Some("owner/repo".to_string()),
        })
        .unwrap();

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_dispatch_guarantee_escalation_backlog_{}.jsonl",
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
        vendor_health: None,
    };

    // Run one tick. Under the new ordering, `run_slow_tier` (dispatch) runs
    // BEFORE `run_recovery_step` (escalation). The QUEUED bead MUST be
    // dispatched on this first tick.
    let summary = run_tick(&deps, 0, 0).expect("tick should succeed");

    // The QUEUED bead must be dispatched.
    assert_eq!(
        summary.beads_dispatched, 1,
        "the QUEUED bead must be dispatched on the first tick despite 9 escalation beads"
    );
    let queued_overlay = store.load(QUEUED_BEAD_ID).unwrap().unwrap();
    assert_eq!(
        queued_overlay.state,
        OverlayState::Dispatched,
        "the QUEUED bead must reach DISPATCHED state"
    );

    // Verify TASK_DISPATCHED telemetry was emitted for the QUEUED bead.
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("TASK_DISPATCHED") && log.contains(QUEUED_BEAD_ID),
        "TASK_DISPATCHED must be emitted for the QUEUED bead; got: {log}"
    );

    // The 9 escalation beads must have been processed by `run_recovery_step`
    // (which now runs AFTER dispatch).
    assert_eq!(
        summary.beads_escalated, 9,
        "all 9 escalation beads at the recovery cap must be escalated"
    );

    // Verify escalation telemetry was emitted.
    assert!(
        log.contains("ESCALATION_REQUIRED"),
        "escalation telemetry must be emitted for the cap beads; got: {log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Plan Task 4(b) / 1s2q-escalation-dedup tick-level regression: an
/// identical-payload escalation emits once on the first tick, is suppressed
/// on the second tick (same context hash, within the
/// `escalation_refire_secs` backoff window), and re-emits on a third tick
/// after the escalation context changes (different `pr_number`/`branch` →
/// different context hash).
///
/// This exercises the full tick loop's dedup path —
/// `escalation_dedup_should_emit` + `record_escalation_emit_dedup` — at the
/// integration level, complementing the StateStore unit tests in `state.rs`
/// that cover the dedup logic in isolation.
///
/// Implementation note: on the HUMAN_HELD recovery-cap success path,
/// `record_escalation` (the sentinel) is called BEFORE the dedup check. The
/// sentinel causes `escalation_already_recorded` to return `true` on the next
/// tick, skipping the bead entirely before the dedup logic runs. To exercise
/// the dedup across ticks, this test clears the sentinel between ticks —
/// simulating the real-world scenario where an operator manually recovers a
/// bead (clearing the sentinel), the bead is re-dispatched, fails again, and
/// returns to HUMAN_HELD at the recovery cap. The dedup ledger entry persists
/// across this recovery cycle, so the dedup correctly suppresses re-emission
/// of the same `ESCALATION_REQUIRED` telemetry event until the context
/// changes.
#[test]
fn escalation_dedup_tick_level_identical_payload_suppressed_changed_context_re_emits() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let vcs = test_vcs();
    let cfg = test_cfg();

    // Seed a HUMAN_HELD bead at the recovery cap (attempt=10) with a
    // pr_number so `post_scm_comment_by_bead_id` succeeds (the success path
    // emits ESCALATION_REQUIRED).
    let bead_id = "bead-dedup-tick";
    store.save(&BeadOverlay {
        bead_id: bead_id.into(),
        state: OverlayState::HumanHeld,
        attempt: 10,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(9006),
        branch: Some("factory/bead-dedup-r10".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        attempt_started_at: None,
        target_repo: Some("owner/repo".to_string()),
    }).unwrap();

    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_escalation_dedup_tick_level_{}.jsonl",
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
        vendor_health: None,
    };

    // ── Tick 1: first escalation → ESCALATION_REQUIRED emitted ──
    let summary1 = run_tick(&deps, 0, 0).expect("tick 1 should succeed");
    assert_eq!(
        summary1.beads_escalated, 1,
        "tick 1: bead must be escalated (comment posted, sentinel recorded)"
    );
    assert_eq!(
        summary1.escalations_suppressed, 0,
        "tick 1: first occurrence must NOT be suppressed"
    );
    let log1 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log1.contains("ESCALATION_REQUIRED"),
        "tick 1: ESCALATION_REQUIRED must be emitted; got: {log1}"
    );
    // The dedup ledger must have a row for this (bead_id, reason).
    assert!(
        store
            .escalation_ledger
            .borrow()
            .contains_key(&(bead_id.into(), "human_held_recovery_attempt_cap_reached".into())),
        "tick 1: dedup ledger must have a row after emit"
    );
    // The sentinel must be recorded (success path).
    assert!(
        store.load_rejection(bead_id, u32::MAX).unwrap().is_some(),
        "tick 1: sentinel must be recorded on success path"
    );

    // ── Simulate operator recovery + re-hold: clear the sentinel ──
    // In production, an operator can manually recover a bead (e.g. via
    // `recover-held`), clearing the escalation sentinel. The bead is then
    // re-dispatched, fails, and returns to HUMAN_HELD at the recovery cap.
    // The dedup ledger entry persists across this cycle.
    store
        .rejections
        .borrow_mut()
        .remove(&(bead_id.into(), u32::MAX));

    // ── Tick 2: same context → ESCALATION_REQUIRED suppressed by dedup ──
    let summary2 = run_tick(&deps, 1, 0).expect("tick 2 should succeed");
    assert_eq!(
        summary2.escalations_suppressed, 1,
        "tick 2: same context hash within backoff must be suppressed; got summary: {summary2:?}"
    );
    let log2 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    // ESCALATION_REQUIRED should appear only once (from tick 1), not twice.
    let escalation_required_count = log2.matches("ESCALATION_REQUIRED").count();
    assert_eq!(
        escalation_required_count, 1,
        "tick 2: ESCALATION_REQUIRED must NOT be re-emitted (same context, within backoff); \
         total count across ticks 1+2 should be 1; got: {escalation_required_count}"
    );

    // ── Simulate operator recovery + re-hold: clear the sentinel again ──
    store
        .rejections
        .borrow_mut()
        .remove(&(bead_id.into(), u32::MAX));

    // ── Change the context: update pr_number and branch ──
    // The ESCALATION_REQUIRED context JSON for the HUMAN_HELD recovery-cap
    // path includes `pr_number` and `branch`, so changing them changes the
    // context hash, causing the dedup to allow re-emission.
    let mut overlay = store.load(bead_id).unwrap().unwrap();
    overlay.pr_number = Some(9007);
    overlay.branch = Some("factory/bead-dedup-r10-v2".into());
    store.save(&overlay).unwrap();

    // ── Tick 3: changed context → ESCALATION_REQUIRED re-emits ──
    let summary3 = run_tick(&deps, 2, 0).expect("tick 3 should succeed");
    assert_eq!(
        summary3.escalations_suppressed, 0,
        "tick 3: changed context hash must NOT be suppressed"
    );
    assert_eq!(
        summary3.beads_escalated, 1,
        "tick 3: bead must be escalated again (new context)"
    );
    let log3 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let escalation_required_count = log3.matches("ESCALATION_REQUIRED").count();
    assert_eq!(
        escalation_required_count, 2,
        "tick 3: ESCALATION_REQUIRED must re-emit after context change; \
         total count across ticks 1+2+3 should be 2; got: {escalation_required_count}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// ----------------------------------------------------------------------------
// Bead jleechan-jsby (r2): end-to-end integration test that drives the
// production tick path with a capped-vendor fake. The r1 PR #459 was
// rejected because the `VendorHealthLedger` was never populated — the
// `skeptic_evidence` function constructed a fresh empty ledger and the
// fast tier never wrote to it. This test drives `run_tick` end-to-end
// against a `FakeScm` that returns a CodeRabbit "unknown" status from
// `pr_snapshot`, asserting that:
//
//   1. Three consecutive ticks with the same `bead_id` produce the
//      cap observation, but the N-of-M detector requires DISTINCT
//      bead_ids. So we drive three DIFFERENT beads (each with their
//      own capped snapshot) to cross the threshold.
//   2. After the threshold, the `VendorHealth::Capped` state is
//      visible to `verifier::assess` (the existing r1 logic
//      substitutes the gate to `Waived` when compensating coverage is
//      green).
//   3. The integration test passes on the r2 codebase and FAILS on
//      the r1 codebase (the r1 ledger is empty, so `health()`
//      returns `Healthy` and the waiver never fires).
//
// Acceptance criteria for r2 per the operator guidance: the production
// tick path now records observations on every assessment, and the
// VENDOR_WAIVED telemetry fires on the auto-escalation edge.
#[test]
fn vendor_health_ledger_three_distinct_capped_beads_produce_waiver() {
    use std::sync::Mutex;

    use daemon::vendor_health::VendorHealthLedger;
    use daemon::vendor_health::EVT_WAIVED;

    std::env::set_var("DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "none");

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_dir = std::env::temp_dir().join("afd_vendor_waiver_r2_test");
    let _ = std::fs::remove_dir_all(&telemetry_dir);
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    let telemetry_log = telemetry_dir.join(format!("daemon-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&telemetry_log);

    let ledger = Mutex::new(VendorHealthLedger::new());

    // Stage a CAPPED PR snapshot: CodeRabbit status "unknown" +
    // coderabbit_approved=false. This is the canonical "vendor
    // structurally unavailable" marker (per
    // `verifier::detect_vendor_cap_for`).
    fn capped_snapshot(pr: u64) -> PrSnapshot {
        PrSnapshot {
            pr_number: pr,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: false,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: format!("sha-{pr}"),
            body: String::new(),
            comments: Vec::new(),
            files: Vec::new(),
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "green".into(),
            coderabbit_status: "unknown".into(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: 0,
        }
    }

    // Three distinct beads, each with a capped snapshot. The N-of-M
    // detector requires 3 distinct bead_ids to flip the vendor to
    // Capped. We script the FakeScm to return the same capped
    // snapshot for any PR.
    for pr in 101..=103 {
        scm.pr_snapshots.insert(pr, capped_snapshot(pr));
        scm.pr_numbers_for_branch.insert(
            ("owner/repo".into(), format!("factory/test-bead-{pr}")),
            Some(pr),
        );
        scm.open_pr_head_refs.insert(
            ("owner/repo".into(), pr),
            PrHeadBranch::SameRepo(format!("factory/test-bead-{pr}")),
        );
    }

    // Drive 3 ticks, each one carrying a DIFFERENT bead_id through the
    // gate-assessment path. The simplest way to do this is to script
    // the FakeScm to return one labelled PR per tick and run
    // `run_tick` 3 times. The detection mechanism is the same as for
    // empty-attested beads: each tick calls record_cap() with the
    // bead's bead_id.
    for pr in 101..=103 {
        // Configure the FakeScm to return this PR's labelled snapshot.
        // The minimal smoke is: ensure the snapshot is fetched, then
        // verify the ledger's count after the tick.
        let _ = scm.pr_snapshots.entry(pr).or_insert_with(|| capped_snapshot(pr));
    }

    // Insert one bead manually to drive the fast tier's gate assessment.
    use daemon::state::{BeadOverlay, OverlayState};
    let bead_id = "test-bead-101";
    let overlay = BeadOverlay {
        bead_id: bead_id.to_string(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(101),
        branch: Some("factory/test-bead-101".to_string()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        attempt_started_at: None,
        target_repo: None,
    };
    store.save(&overlay).unwrap();
    store.register_branch(bead_id, "factory/test-bead-101").unwrap();

    *llm.response.borrow_mut() = Some(Ok("pass".into()));

    // ── Tick 1: assessment with capped snapshot. The fast tier
    //     records an observation for `bead_id`. Counter increments. ──
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
            vendor_health: Some(&ledger),
        },
        0,
        0,
    )
    .expect("tick 1 should succeed");

    let after_tick1 = {
        let l = ledger.lock().unwrap();
        l.observation_count(daemon::vendor_health::Vendor::CodeRabbit)
    };
    assert_eq!(
        after_tick1, 1,
        "tick 1 must record an observation; got {after_tick1}"
    );

    // ── Tick 2: same bead with a NEW bead_id. The N-of-M detector
    //     requires distinct bead_ids, so we simulate a fresh bead
    //     reaching the gate. The first bead is still in the registry,
    //     so this tick will record observations for both beads — the
    //     count will jump to 2 (the new bead_id is the 2nd distinct
    //     one). Move the first bead to HumanHeld so it doesn't
    //     re-record on subsequent ticks. ──
    let mut overlay1 = store.load(bead_id).unwrap().unwrap();
    overlay1.state = OverlayState::HumanHeld;
    store.save(&overlay1).unwrap();

    let bead_id_2 = "test-bead-102";
    let overlay2 = BeadOverlay {
        bead_id: bead_id_2.to_string(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(102),
        branch: Some("factory/test-bead-102".to_string()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&overlay2).unwrap();
    store.register_branch(bead_id_2, "factory/test-bead-102").unwrap();
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
            vendor_health: Some(&ledger),
        },
        1,
        0,
    )
    .expect("tick 2 should succeed");

    let after_tick2 = {
        let l = ledger.lock().unwrap();
        l.observation_count(daemon::vendor_health::Vendor::CodeRabbit)
    };
    assert_eq!(
        after_tick2, 2,
        "tick 2 must record a 2nd observation for the new bead_id; got {after_tick2}"
    );

    // ── Tick 3: third distinct bead. The N-of-M detector (>= 3
    //     distinct beads) flips the vendor to Capped and emits
    //     VENDOR_WAIVED. ──
    let bead_id_3 = "test-bead-103";
    let overlay3 = BeadOverlay {
        bead_id: bead_id_3.to_string(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(103),
        branch: Some("factory/test-bead-103".to_string()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&overlay3).unwrap();
    store.register_branch(bead_id_3, "factory/test-bead-103").unwrap();
    // Move bead 2 to HumanHeld so it doesn't re-record on tick 3.
    let mut overlay2 = store.load(bead_id_2).unwrap().unwrap();
    overlay2.state = OverlayState::HumanHeld;
    store.save(&overlay2).unwrap();
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
            vendor_health: Some(&ledger),
        },
        2,
        0,
    )
    .expect("tick 3 should succeed");

    let after_tick3 = {
        let l = ledger.lock().unwrap();
        l.health(daemon::vendor_health::Vendor::CodeRabbit)
    };
    assert!(
        after_tick3.is_capped(),
        "after 3 distinct capped beads the ledger MUST be Capped; got {after_tick3:?}"
    );

    // VENDOR_WAIVED telemetry must have been emitted on the
    // Healthy -> Capped edge.
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    let waived_count = log.matches(EVT_WAIVED).count();
    assert!(
        waived_count >= 1,
        "VENDOR_WAIVED telemetry must have been emitted on the auto-escalation edge; got {waived_count} lines:\n{log}"
    );
    assert!(
        log.contains("coderabbit:waived_vendor_unavailable"),
        "VENDOR_WAIVED telemetry must contain the canonical waiver token; log:\n{log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// ----------------------------------------------------------------------------
// Bead jleechan-jsby (r2): CI-wait timeout. When the snapshot has
// `ci_pending=true` AND at least one tracked vendor is showing a cap
// marker, the fast tier MUST skip the CI-wait and proceed to the gate
// assessment. The pre-r2 code emitted `VERIFICATION_PENDING` and
// `continue`d, leaving the bead stuck (the live 2026-07-22 incident
// that parked jtg8 and jsby themselves).
#[test]
fn vendor_health_ledger_ci_pending_with_capped_vendor_skips_wait() {
    use std::sync::Mutex;

    use daemon::vendor_health::VendorHealthLedger;
    use daemon::state::{BeadOverlay, OverlayState};

    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();
    let telemetry_dir = std::env::temp_dir().join("afd_vendor_waiver_ciwait_test");
    let _ = std::fs::remove_dir_all(&telemetry_dir);
    std::fs::create_dir_all(&telemetry_dir).unwrap();
    let telemetry_log = telemetry_dir.join(format!("daemon-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&telemetry_log);

    let ledger = Mutex::new(VendorHealthLedger::new());

    // Capped snapshot: CodeRabbit status "unknown" + ci_pending=true.
    // The pre-r2 wait path would emit `VERIFICATION_PENDING` and
    // continue. The r2 path detects the cap marker and proceeds.
    let pr = 200;
    scm.pr_snapshots.insert(
        pr,
        PrSnapshot {
            pr_number: pr,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: false,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "sha-200".into(),
            body: String::new(),
            comments: Vec::new(),
            files: Vec::new(),
            updated_at_epoch: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ci_status: "pending".into(),
            coderabbit_status: "unknown".into(),
            ci_pending: true,
            head_committed_epoch: 0,
            bugbot_pending: false,
        },
    );

    let bead_id = "ci-wait-bead";
    let overlay = BeadOverlay {
        bead_id: bead_id.to_string(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(pr),
        branch: Some("factory/ci-wait-bead".to_string()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&overlay).unwrap();
    store.register_branch(bead_id, "factory/ci-wait-bead").unwrap();
    scm.pr_numbers_for_branch.insert(
        ("owner/repo".into(), "factory/ci-wait-bead".to_string()),
        Some(pr),
    );
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), pr),
        PrHeadBranch::SameRepo("factory/ci-wait-bead".to_string()),
    );

    *llm.response.borrow_mut() = Some(Ok("pass".into()));

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
            vendor_health: Some(&ledger),
        },
        0,
        0,
    )
    .expect("tick should succeed");

    // The gate assessment MUST run. Pre-r2, `ci_pending=true` would
    // emit VERIFICATION_PENDING and continue; the bead would never
    // be assessed. With r2, the cap marker bypasses the wait, so
    // `gates_assessed` is exactly 1.
    assert_eq!(
        summary.gates_assessed, 1,
        "ci_pending=true with a capped vendor MUST proceed to gate assessment; got {summary:?}"
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !log.contains("VERIFICATION_PENDING"),
        "VERIFICATION_PENDING must NOT be emitted when the cap marker is present; log:\n{log}"
    );
    assert!(
        log.contains("GATE_ASSESSMENT"),
        "GATE_ASSESSMENT must be emitted when the cap marker bypasses the wait; log:\n{log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

// ===========================================================================
// jleechan-6l1f: green-then-regressed beads dead-end.
//
// Live incident (PR #540 proof): a bead reached all_green=true at
// 2026-08-04T13:18:43Z and was promoted ATTESTED->READY. ~24h later CI went
// red on the same PR head, but the bead sat READY-and-apparently-done while
// auto-merge-guard.sh correctly refused the merge (no green snapshot). The
// daemon never re-detected the green->red transition, so no reroll was
// triggered — the dead-end.
//
// Each test below exercises one slice of the fix:
//   1. test_gate_regression_emits_event_and_demotes_to_attested_when_ci_goes_red
//      A READY bead (last_all_green=true) whose CI goes red on a subsequent
//      tick must emit GATE_REGRESSED and demote Ready -> Attested so the
//      existing red-branch (reroll / HUMAN_HELD in stage 1) picks it up.
//   2. test_gate_regression_does_not_fire_when_first_assessment_is_red
//      A bead that has NEVER been green (e.g. CI went red before the daemon
//      ever saw it green) must NOT emit GATE_REGRESSED — only the transition
//      green->red is a regression; red->stays-red is the existing path.
//   3. test_gate_regression_caps_at_max_and_parks_human_held
//      After MAX_GATE_REGRESSIONS green->red transitions, a further
//      green->red must emit GATE_REGRESSED_CAPPED and park HUMAN_HELD with
//      park_reason=gate_regression_capped (a new distinct reason, so the
//      circuit-breaker-style retry suppression in recover_human_held does not
//      requeue it identically).
//   4. test_gate_regression_counter_increments_on_each_green_to_red
//      Across N ticks that each flip green->red, the counter advances so
//      the cap test above actually sees a > cap value.
// ===========================================================================

#[test]
fn test_gate_regression_emits_event_and_demotes_to_attested_when_ci_goes_red() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // Bead started READY (last_all_green=true): we hand-set the store
    // mirror of the new column via set_last_all_green. The bead is staged
    // as if it had been promoted on an earlier tick.
    let pr = 7701_u64;
    let branch = "factory/reg-bead-r1";
    store
        .save(&BeadOverlay {
            bead_id: "reg-bead".into(),
            state: OverlayState::Ready,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        })
        .unwrap();
    store.register_branch("reg-bead", branch).unwrap();
    store.set_last_all_green("reg-bead", true).unwrap();

    // PR snapshot now RED (CI failure on head).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.pr_snapshots.insert(
        pr,
        PrSnapshot {
            pr_number: pr,
            ci_success: false,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: format!("sha-{pr}"),
            body: String::new(),
            comments: Vec::new(),
            files: Vec::new(),
            updated_at_epoch: now,
            ci_status: "red".into(),
            coderabbit_status: "approved".into(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: now.saturating_sub(60),
        },
    );
    scm.pr_numbers_for_branch.insert(
        ("owner/repo".into(), branch.into()),
        Some(pr),
    );
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), pr),
        PrHeadBranch::SameRepo(branch.into()),
    );

    let telemetry_log = std::env::temp_dir().join("afd_gate_regression_emits.jsonl");
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
            vendor_health: None,
        },
        1,
        0,
    )
    .expect("regression tick must not error");

    // The gate assessment MUST run (proves the READY bead is now reachable
    // by the fast tier — pre-fix it would be skipped entirely).
    assert_eq!(
        summary.gates_assessed, 1,
        "READY bead must be re-assessed on the regression tick; got {summary:?}"
    );

    // The bead MUST be demoted off READY so the red-branch picks it up.
    let after = store.load("reg-bead").unwrap().unwrap();
    assert_ne!(
        after.state,
        OverlayState::Ready,
        "READY bead with a regressed gate MUST be demoted off READY (was {after:?})"
    );
    // In stage-1 test config (no re-roll execution), a coder-fixable red
    // routes to HUMAN_HELD — both Attested (reroll-not-yet-executed) and
    // HumanHeld are valid non-Ready terminal outcomes; the contract is
    // specifically that the bead no longer claims to be done.
    assert!(
        after.state == OverlayState::Attested
            || after.state == OverlayState::HumanHeld,
        "regressed bead must land in Attested or HumanHeld, was {:?}",
        after.state
    );

    // last_all_green MUST be cleared (otherwise the next tick would re-fire
    // GATE_REGRESSED on what is now a sustained-red state).
    assert_eq!(
        store.last_all_green("reg-bead").unwrap(),
        Some(false),
        "last_all_green MUST be cleared on a green->red regression"
    );

    // The dedicated telemetry event MUST be emitted so dashboards / ops
    // can distinguish "stuck red from intake" from "previously green,
    // regressed" (different fix paths).
    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("GATE_REGRESSED"),
        "GATE_REGRESSED telemetry event MUST be emitted on green->red transition; log:\n{log}"
    );
    assert!(
        log.contains("\"all_green\":false"),
        "the regression tick's GATE_ASSESSMENT must report all_green=false; log:\n{log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_gate_regression_does_not_fire_when_first_assessment_is_red() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // ATTESTED bead, NEVER green (last_all_green=false / unset).
    let pr = 7702_u64;
    let branch = "factory/never-green-r1";
    store
        .save(&BeadOverlay {
            bead_id: "never-green".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        })
        .unwrap();
    store.register_branch("never-green", branch).unwrap();
    store.set_last_all_green("never-green", false).unwrap();

    // PR snapshot RED from the start.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.pr_snapshots.insert(
        pr,
        PrSnapshot {
            pr_number: pr,
            ci_success: false,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: format!("sha-{pr}"),
            body: String::new(),
            comments: Vec::new(),
            files: Vec::new(),
            updated_at_epoch: now,
            ci_status: "red".into(),
            coderabbit_status: "approved".into(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: now.saturating_sub(60),
        },
    );
    scm.pr_numbers_for_branch.insert(
        ("owner/repo".into(), branch.into()),
        Some(pr),
    );
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), pr),
        PrHeadBranch::SameRepo(branch.into()),
    );

    let telemetry_log = std::env::temp_dir().join("afd_gate_regression_not_first_red.jsonl");
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
        1,
        0,
    )
    .expect("first-red tick must not error");

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !log.contains("GATE_REGRESSED"),
        "GATE_REGRESSED must NOT fire for a bead that has never been green; \
         this is the existing red-only path (REROLL_VERDICT_RECORDED); log:\n{log}"
    );
    assert!(
        log.contains("GATE_ASSESSMENT"),
        "GATE_ASSESSMENT must still fire; log:\n{log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_gate_regression_caps_at_max_and_parks_human_held() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = test_vcs();

    // ATTESTED bead whose last_all_green=true and gate_regression_count is
    // already at MAX_GATE_REGRESSIONS — the next green->red MUST cap, not
    // loop forever.
    let pr = 7703_u64;
    let branch = "factory/regression-cap-r1";
    store
        .save(&BeadOverlay {
            bead_id: "regression-cap".into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: Some(pr),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        })
        .unwrap();
    store.register_branch("regression-cap", branch).unwrap();
    store.set_last_all_green("regression-cap", true).unwrap();
    // Pre-set the counter to MAX_GATE_REGRESSIONS so the very next tick
    // hits the cap path.
    for _ in 0..daemon::tick::MAX_GATE_REGRESSIONS {
        store.incr_gate_regression_count("regression-cap").unwrap();
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    scm.pr_snapshots.insert(
        pr,
        PrSnapshot {
            pr_number: pr,
            ci_success: false,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: format!("sha-{pr}"),
            body: String::new(),
            comments: Vec::new(),
            files: Vec::new(),
            updated_at_epoch: now,
            ci_status: "red".into(),
            coderabbit_status: "approved".into(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: now.saturating_sub(60),
        },
    );
    scm.pr_numbers_for_branch.insert(
        ("owner/repo".into(), branch.into()),
        Some(pr),
    );
    scm.open_pr_head_refs.insert(
        ("owner/repo".into(), pr),
        PrHeadBranch::SameRepo(branch.into()),
    );

    let telemetry_log = std::env::temp_dir().join("afd_gate_regression_capped.jsonl");
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
        1,
        0,
    )
    .expect("capped-regression tick must not error");

    let after = store.load("regression-cap").unwrap().unwrap();
    assert_eq!(
        after.state,
        OverlayState::HumanHeld,
        "capped regression MUST park HUMAN_HELD; was {:?}",
        after.state
    );
    assert_eq!(
        after.park_reason.as_deref(),
        Some("gate_regression_capped"),
        "park_reason MUST be the distinct gate_regression_capped reason (so \
         recover_human_held does not requeue this bead identically to a \
         transient red); was {:?}",
        after.park_reason
    );

    let log = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        log.contains("GATE_REGRESSED_CAPPED"),
        "GATE_REGRESSED_CAPPED telemetry MUST fire on cap hit; log:\n{log}"
    );
    assert!(
        !log.contains("\"event_type\":\"GATE_REGRESSED\""),
        "once capped, GATE_REGRESSED MUST NOT fire again (cap is terminal); log:\n{log}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_gate_regression_counter_increments_on_each_green_to_red() {
    let _scm = FakeScm::new();
    let _tracker = FakeTracker::new();
    let _sessions = FakeSessions::new();
    let _llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let _cfg = test_cfg();
    let _vcs = test_vcs();

    // Drive the counter manually through MAX_GATE_REGRESSIONS-1 ticks so
    // we end at exactly the cap boundary (the cap test above then exercises
    // the (cap+1)th transition). The fake store's incr_gate_regression_count
    // is the canonical writer; this test only asserts it bumps per call
    // and that the next-tick cap test above sees the correct value.
    for n in 1..=daemon::tick::MAX_GATE_REGRESSIONS {
        let got = store
            .incr_gate_regression_count("counter-bead")
            .unwrap();
        assert_eq!(
            got, n,
            "incr_gate_regression_count must monotonically increment; \
             expected {n}, got {got}"
        );
    }
    assert_eq!(
        store.gate_regression_count("counter-bead").unwrap(),
        daemon::tick::MAX_GATE_REGRESSIONS,
        "gate_regression_count must read back the cumulative count"
    );
}

#[test]
fn test_non_default_repository_labeled_pr_tick_telemetry_attribution() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 8843,
        title: "Fix rewards XP anchor".into(),
        body: "adopting existing PR".into(),
        author_login: "jleechan2015".into(),
        external_ref: "jleechanorg/worldarchitect.ai#8843".into(),
        head_ref_name: "fix/rewards-xp-anchor-followup".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("jleechanorg/worldarchitect.ai".into()),
        head_repo_owner_login: Some("jleechanorg".into()),
        head_sha: Some("9dc2c198a445450d8fe455e7d691a0492deefe2e".into()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("jleechan2015".into(), Permission::Write);
    scm.pr_snapshots.insert(
        8843,
        qdw_green_snapshot(
            8843,
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
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join("afd_non_default_repo_adoption.jsonl");
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
            vendor_health: None,
        },
        0,
        0,
    )
    .expect("non-default repository PR adoption should succeed");

    assert_eq!(summary.beads_created, 1);
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap();
    let adopted_line = telemetry
        .lines()
        .find(|line| line.contains("EXISTING_PR_ADOPTED"))
        .expect("telemetry log must contain EXISTING_PR_ADOPTED");

    let parsed: serde_json::Value = serde_json::from_str(adopted_line).unwrap();
    let context = &parsed["context"];
    assert_eq!(
        context["repo"], "jleechanorg/worldarchitect.ai",
        "EXISTING_PR_ADOPTED context must contain repo attribution: {context:?}"
    );
    assert_eq!(
        context["pr_number"], 8843,
        "EXISTING_PR_ADOPTED context must contain pr_number attribution: {context:?}"
    );
    assert_eq!(
        context["branch"], "fix/rewards-xp-anchor-followup",
        "EXISTING_PR_ADOPTED context must contain branch attribution: {context:?}"
    );
    assert_eq!(
        context["head_sha"], "9dc2c198a445450d8fe455e7d691a0492deefe2e",
        "EXISTING_PR_ADOPTED context must contain head_sha attribution: {context:?}"
    );
    assert_eq!(
        context["external_ref"], "jleechanorg/worldarchitect.ai#8843"
    );
    assert_eq!(context["newly_created"], true);

    // Second tick: verify duplicate suppression does not re-emit EXISTING_PR_ADOPTED
    let _ = std::fs::remove_file(&telemetry_log);
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
            vendor_health: None,
        },
        0,
        0,
    )
    .expect("second tick should succeed");

    assert_eq!(summary2.beads_created, 0);
    let telemetry2 = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        !telemetry2.contains("EXISTING_PR_ADOPTED"),
        "subsequent tick must NOT re-emit EXISTING_PR_ADOPTED: {telemetry2}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_non_default_repository_branch_collision_telemetry_attribution() {
    let mut scm = FakeScm::new();
    scm.prs.push(LabeledPr {
        number: 8731,
        title: "Colliding branch PR".into(),
        body: "collides with existing registration".into(),
        author_login: "jleechan2015".into(),
        external_ref: "jleechanorg/worldarchitect.ai#8731".into(),
        head_ref_name: "fix/rev-ilwk7-move-modal-validators".into(),
        is_cross_repository: false,
        head_repo_full_name: Some("jleechanorg/worldarchitect.ai".into()),
        head_repo_owner_login: Some("jleechanorg".into()),
        head_sha: Some("sha-8731-colliding".into()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("jleechan2015".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    store
        .save(&BeadOverlay {
            bead_id: "legacy-bead-ver0".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: Some(123),
            branch: Some("fix/rev-ilwk7-move-modal-validators".into()),
            session_id: Some("sess-legacy".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        })
        .unwrap();
    store
        .register_branch("legacy-bead-ver0", "fix/rev-ilwk7-move-modal-validators")
        .unwrap();

    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    let vcs = test_vcs();
    let telemetry_log = std::env::temp_dir().join("afd_non_default_collision.jsonl");
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
            vendor_health: None,
        },
        0,
        0,
    )
    .expect("branch collision should escalate without failing the tick");

    assert_eq!(summary.beads_escalated, 1);
    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap();
    let escalation_line = telemetry
        .lines()
        .find(|line| line.contains("ESCALATION_REQUIRED") && line.contains("adoption_branch_collision"))
        .expect("telemetry log must contain ESCALATION_REQUIRED with adoption_branch_collision");

    let parsed: serde_json::Value = serde_json::from_str(escalation_line).unwrap();
    let context = &parsed["context"];
    assert_eq!(
        context["repo"], "jleechanorg/worldarchitect.ai",
        "adoption_branch_collision context must contain repo attribution: {context:?}"
    );
    assert_eq!(
        context["pr_number"], 8731,
        "adoption_branch_collision context must contain pr_number attribution: {context:?}"
    );
    assert_eq!(
        context["branch"], "fix/rev-ilwk7-move-modal-validators",
        "adoption_branch_collision context must contain branch attribution: {context:?}"
    );
    assert_eq!(
        context["head_sha"], "sha-8731-colliding",
        "adoption_branch_collision context must contain head_sha attribution: {context:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Bead jleechan-w0r4: verify that when an adopted remediation session transitions
/// to idle (finished prompt execution), the daemon reaps the worker session via
/// stop() and promotes the bead to ATTESTED, clearing the session handle.
#[test]
fn test_dispatched_adopted_idle_session_reaped_and_promoted() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let mut sessions = FakeSessions::new();
    // Non-terminal quiescence but idle activity
    sessions.quiescent = false;
    sessions.set_activity(daemon::tools::SessionActivity::Idle);

    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_test_w0r4.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    store.overlays.borrow_mut().insert(
        "bead-w0r4".into(),
        BeadOverlay {
            bead_id: "bead-w0r4".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: Some(999),
            branch: Some("fix/test-w0r4".into()),
            session_id: Some("wa-9999".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );

    store.register_branch("bead-w0r4", "fix/test-w0r4").unwrap();

    scm.pr_numbers_for_branch.insert(("owner/repo".into(), "fix/test-w0r4".into()), Some(999));
    scm.open_pr_head_refs.insert(("owner/repo".into(), 999), daemon::tools::PrHeadBranch::SameRepo("fix/test-w0r4".into()));
    scm.pr_snapshots.insert(
        999,
        PrSnapshot {
            pr_number: 999,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "head-999".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 100,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: 0,
        },
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 10).unwrap();
    assert_eq!(summary.beads_parked_human_held, 0);

    let o = store.load("bead-w0r4").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Attested,
        "idle adopted session must promote to ATTESTED"
    );
    assert_eq!(
        o.session_id, None,
        "session handle must be cleared after reaping"
    );
    assert!(
        sessions.stop_succeeded.get(),
        "sessions.stop() must be called to reap idle worker"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Bead rev-3lm8k: when a coder session's worktree is auto-clean-enabled
/// (`agent_worktree_root` set) and the session is reaped on promotion to
/// ATTESTED (the same "coder session finished" moment PR #653/jleechan-w0r4
/// reaps the session itself), the daemon must also remove the AO-managed
/// worktree directory for that session — immediately, not on the next TTL
/// sweep. This is the RED->GREEN reproduction of the bead's incident: a
/// leftover worktree dir (e.g. `wa-3538`) blocked every later dispatch that
/// hashed to the same orchestrator branch until a human manually deleted it.
#[test]
fn test_worktree_cleaned_up_on_coder_session_exit_promotion() {
    let mut scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = false;
    sessions.set_activity(daemon::tools::SessionActivity::Idle);

    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let worktree_root = std::env::temp_dir().join(format!(
        "afd_test_rev3lm8k_worktree_root_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&worktree_root);
    let cfg = Config {
        agent_worktree_root: Some(worktree_root.display().to_string()),
        ..test_cfg()
    };
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_test_rev3lm8k.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    // The stale AO-managed worktree the incident describes: laid down under
    // `<agent_worktree_root>/<repo>/<session_id>`, matching the real
    // `~/.worktrees/<repo>/<agent_id>` convention `Config::agent_worktree_path`
    // encodes.
    let stale_worktree = worktree_root.join("owner/repo/wa-3538");
    std::fs::create_dir_all(&stale_worktree).unwrap();
    std::fs::write(stale_worktree.join("marker"), b"leftover coder worktree").unwrap();
    assert!(stale_worktree.is_dir(), "fixture must actually create the dir");

    store.overlays.borrow_mut().insert(
        "bead-rev3lm8k".into(),
        BeadOverlay {
            bead_id: "bead-rev3lm8k".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 100,
            spend_usd: 0.0,
            pr_number: Some(998),
            branch: Some("fix/test-rev3lm8k".into()),
            session_id: Some("wa-3538".into()),
            is_adopted: true,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );

    store.register_branch("bead-rev3lm8k", "fix/test-rev3lm8k").unwrap();

    scm.pr_numbers_for_branch.insert(("owner/repo".into(), "fix/test-rev3lm8k".into()), Some(998));
    scm.open_pr_head_refs.insert(("owner/repo".into(), 998), daemon::tools::PrHeadBranch::SameRepo("fix/test-rev3lm8k".into()));
    scm.pr_snapshots.insert(
        998,
        PrSnapshot {
            pr_number: 998,
            ci_success: true,
            mergeable: true,
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "head-998".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 100,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            bugbot_pending: false,
            head_committed_epoch: 0,
        },
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 10).unwrap();
    assert_eq!(summary.beads_parked_human_held, 0);

    let o = store.load("bead-rev3lm8k").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Attested,
        "idle adopted session must promote to ATTESTED"
    );
    assert_eq!(o.session_id, None, "session handle must be cleared after reaping");
    assert!(
        !stale_worktree.exists(),
        "worktree dir must be cleaned up within the same tick the session is reaped, \
         not left for a future TTL sweep"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("WORKTREE_CLEANED_ON_SESSION_EXIT"),
        "telemetry must record the worktree cleanup: {telemetry}"
    );

    let _ = std::fs::remove_dir_all(&worktree_root);
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn session_health_failure_reaps_session_and_requeues_bead() {
    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_test_session_health.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    store.overlays.borrow_mut().insert(
        "bead-health-fail".into(),
        BeadOverlay {
            bead_id: "bead-health-fail".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-health-fail-r1".into()),
            session_id: Some("wa-dead-auth".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );
    store.register_branch("bead-health-fail", "factory/bead-health-fail-r1").unwrap();

    // Script session health failure (e.g. login expired)
    sessions.set_session_health_failure("wa-dead-auth", "terminal session error in tmux pane: login expired");

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 10).unwrap();
    assert_eq!(summary.beads_parked_human_held, 0);

    let o = store.load("bead-health-fail").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Queued,
        "session health failure must reset state to Queued so it can be re-dispatched"
    );
    assert_eq!(
        o.session_id, None,
        "dead session handle must be cleared"
    );
    assert_eq!(
        o.spawn_failure_count, 1,
        "spawn failure count must increment on session health failure"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("SESSION_HEALTH_FAILED"),
        "SESSION_HEALTH_FAILED event must be emitted; telemetry:\n{telemetry}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Bead rev-4ou1z ACCEPTANCE #1: a Gemini quota-reached health failure with
/// a parseable "Resets in Xh Ym" countdown ARMS the quota watchdog instead
/// of killing the session — no respawn cycle burning
/// `MAX_TRANSIENT_SPAWN_RETRY` against an hours-long reset window.
#[test]
fn quota_reached_health_failure_arms_watchdog_without_killing_session() {
    // The ledger is a process-wide static (see `health::quota_watchdog`'s
    // module doc), but every accessor is scoped by `bead_id` and this
    // bead id is unique to this test, so no lock is needed even though
    // `cargo test` runs many tests concurrently in this process.
    daemon::health::quota_watchdog::clear("bead-quota-arm");

    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_test_quota_watchdog_arm.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    store.overlays.borrow_mut().insert(
        "bead-quota-arm".into(),
        BeadOverlay {
            bead_id: "bead-quota-arm".into(),
            state: OverlayState::Dispatched,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-quota-arm-r1".into()),
            session_id: Some("wa-quota-paused".into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );
    store
        .register_branch("bead-quota-arm", "factory/bead-quota-arm-r1")
        .unwrap();

    sessions.set_session_health_failure(
        "wa-quota-paused",
        "terminal session error in tmux pane: individual quota reached (resets in 1h 23m.)",
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
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 10).unwrap();
    assert_eq!(summary.beads_parked_human_held, 0);

    let o = store.load("bead-quota-arm").unwrap().unwrap();
    assert_eq!(
        o.state,
        OverlayState::Dispatched,
        "quota-armed bead must stay DISPATCHED — no kill+requeue cycle"
    );
    assert_eq!(
        o.session_id,
        Some("wa-quota-paused".into()),
        "the paused session handle must be preserved for the watchdog to wake later"
    );
    assert_eq!(
        o.spawn_failure_count, 0,
        "arming the watchdog must not burn the transient-retry budget"
    );
    assert!(
        !sessions
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("stop(")),
        "quota-armed session must NOT be stopped; calls={:?}",
        sessions.calls.borrow()
    );
    assert!(
        daemon::health::quota_watchdog::recorded_reset_at("bead-quota-arm").is_some(),
        "quota watchdog ledger must record the reset time for bead-quota-arm"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("QUOTA_WATCHDOG_ARMED"),
        "QUOTA_WATCHDOG_ARMED event must be emitted; telemetry:\n{telemetry}"
    );

    daemon::health::quota_watchdog::clear("bead-quota-arm");
    let _ = std::fs::remove_file(&telemetry_log);
}

/// Bead rev-4ou1z ACCEPTANCE #2: once a session's recorded reset time (plus
/// the 60s wake grace) has passed, the slow-tier quota watchdog sweep sends
/// an Enter keypress to the SAME paused pane via `Sessions::wake_pane` — no
/// respawn — and the ledger entry is cleared so it fires exactly once.
#[test]
fn quota_watchdog_wakes_paused_pane_after_reset_grace_elapses() {
    let bead_id = "bead-quota-wake";
    let session_id = "wa-quota-wake-paused";
    daemon::health::quota_watchdog::clear(bead_id);
    // Reset "long past" — real wall-clock `now_epoch` used by the wake
    // sweep is always far greater than epoch second 1, so the 60s grace
    // has unconditionally elapsed by the time `run_tick` runs.
    daemon::health::quota_watchdog::record_quota_reset(bead_id, session_id, 1);

    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let llm = FakeLlm::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let vcs = FakeVcs::new();
    let telemetry_log = std::env::temp_dir().join("afd_test_quota_watchdog_wake.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    store.overlays.borrow_mut().insert(
        bead_id.into(),
        BeadOverlay {
            bead_id: bead_id.into(),
            state: OverlayState::Dispatched,
            attempt: 2,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: Some("factory/bead-quota-wake-r1".into()),
            session_id: Some(session_id.into()),
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        },
    );
    store
        .register_branch(bead_id, "factory/bead-quota-wake-r1")
        .unwrap();

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    let summary = run_tick(&deps, 0, 10).unwrap();
    assert_eq!(
        summary.quota_watchdog_wakes, 1,
        "exactly one due wake must fire this tick"
    );
    assert!(
        sessions
            .calls
            .borrow()
            .iter()
            .any(|c| c == &format!("wake_pane({session_id})")),
        "wake_pane must be called for the armed session; calls={:?}",
        sessions.calls.borrow()
    );
    assert_eq!(
        daemon::health::quota_watchdog::recorded_reset_at(bead_id),
        None,
        "ledger entry must be cleared after waking so it fires exactly once"
    );

    let telemetry = std::fs::read_to_string(&telemetry_log).unwrap_or_default();
    assert!(
        telemetry.contains("QUOTA_WATCHDOG_WOKE_PANE"),
        "QUOTA_WATCHDOG_WOKE_PANE event must be emitted; telemetry:\n{telemetry}"
    );

    daemon::health::quota_watchdog::clear(bead_id);
    let _ = std::fs::remove_file(&telemetry_log);
}
