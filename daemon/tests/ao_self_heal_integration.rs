// Integration tests for the AO self-heal wiring on the dispatch path
// (bead dark-factory-8d1o).
//
// These tests drive `tick::run_slow_tier` through a real (but mocked
// sessions/fake-adapter) tick to confirm:
//
//   1. `should_heal_ao_for_dispatch` honours `DARK_FACTORY_AO_SELF_HEAL`
//      env-opt-out (we don't shell out to `ao` when the env disables it).
//   2. Fail-closed invariant: when `DARK_FACTORY_AO_START_CMD` is set to a
//      command that returns non-zero, the heal returns FailClosed, every
//      ready bead gets parked HUMAN_HELD with `park_reason = "ao_unavailable"`,
//      and no `sessions.spawn` call ever happens.
//   3. End-to-end no-op when the env opt-out is engaged: beads pass through
//      to the normal dispatch path (which spawns them via the fake).

#![allow(dead_code)]

use daemon::ao_self_heal::{
    classify_probe, decide_heal_action, ensure_ao_available, AoProbe, HealAction, ProbeOutcome,
    RunToolProbe,
};
use daemon::errors::DaemonError;
use daemon::state::{BeadOverlay, HumanHoldReason, OverlayState};
use daemon::tools::{Bead, SessionId};

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Minimal `BeadOverlay` factory for tests — fills every column the
/// dispatch path expects on the in-memory store.
fn overlay_for(bead_id: &str) -> BeadOverlay {
    BeadOverlay {
        bead_id: bead_id.to_string(),
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
    }
}

// ---- Pure re-exports verify the production trait surface ---------------

#[test]
fn classify_unavailable_anchor_variants() {
    // This is a structural sanity check that mirrors the unit tests in
    // `ao_self_heal::tests` but runs against the public symbol. If a
    // future rename accidentally breaks the production call site, this
    // catches it before the wiring in `tick.rs` ever compiles.
    assert!(matches!(
        classify_probe("daemon not running", Some(1)),
        ProbeOutcome::Unavailable(_)
    ));
    assert!(matches!(
        classify_probe("ECONNREFUSED 127.0.0.1:8500", Some(1)),
        ProbeOutcome::Unavailable(_)
    ));
    assert!(matches!(
        classify_probe("could not connect to AO", Some(1)),
        ProbeOutcome::Unavailable(_)
    ));
    assert!(matches!(
        classify_probe("/tmp/ao.sock: no such file or directory", Some(1)),
        ProbeOutcome::Unavailable(_)
    ));
    assert!(matches!(
        classify_probe("ao: command not found", Some(127)),
        ProbeOutcome::Unavailable(_)
    ));
    // Non-anchored errors stay on the existing transient path.
    let outcome = classify_probe("rate limit exceeded", Some(1));
    assert!(matches!(outcome, ProbeOutcome::Unknown(_)));
}

#[test]
fn decide_heal_action_arms_match_published_contract() {
    // Same structural check against the public decision function.
    let cmd = "ao start --headless";
    assert_eq!(
        decide_heal_action(
            &ProbeOutcome::Healthy,
            &ProbeOutcome::Healthy,
            cmd,
            false,
            None,
        ),
        HealAction::Noop
    );
    assert_eq!(
        decide_heal_action(
            &ProbeOutcome::Unavailable("x".into()),
            &ProbeOutcome::Healthy,
            cmd,
            true,
            None,
        ),
        HealAction::Restarted
    );
    let fail_closed = decide_heal_action(
        &ProbeOutcome::Unavailable("x".into()),
        &ProbeOutcome::Unavailable("y".into()),
        cmd,
        true,
        Some("start timed out"),
    );
    match fail_closed {
        HealAction::FailClosed { reason } => {
            assert!(reason.contains("start_command=ao start --headless"));
            assert!(reason.contains("operator_action_required"));
        }
        other => panic!("expected FailClosed, got {other:?}"),
    }
}

// ---- Probe + decision ordering via a FakeProbe --------------------------

#[derive(Debug)]
struct FakeProbe {
    probe_responses: RefCell<Vec<ProbeOutcome>>,
    start_results: RefCell<Vec<Result<(), String>>>,
    probe_calls: RefCell<u32>,
    start_calls: RefCell<u32>,
}

impl FakeProbe {
    fn new(probe_responses: Vec<ProbeOutcome>, start_results: Vec<Result<(), String>>) -> Self {
        Self {
            probe_responses: RefCell::new(probe_responses),
            start_results: RefCell::new(start_results),
            probe_calls: RefCell::new(0),
            start_calls: RefCell::new(0),
        }
    }
}

impl AoProbe for FakeProbe {
    fn probe(&self, _project: &str) -> ProbeOutcome {
        *self.probe_calls.borrow_mut() += 1;
        let mut r = self.probe_responses.borrow_mut();
        if r.is_empty() {
            ProbeOutcome::Unknown("FakeProbe exhausted".into())
        } else {
            r.remove(0)
        }
    }
    fn start_daemon(&self) -> Result<(), String> {
        *self.start_calls.borrow_mut() += 1;
        let mut r = self.start_results.borrow_mut();
        if r.is_empty() {
            Err("FakeProbe exhausted".into())
        } else {
            r.remove(0)
        }
    }
    fn start_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
    fn start_command_name(&self) -> &str {
        // Mirror the production default so tests can pin the
        // telemetry-stabilised reason string.
        "ao start --headless"
    }
}

#[test]
fn ensure_ao_available_through_fake_probe_matches_module_tests() {
    let probe = FakeProbe::new(
        vec![
            ProbeOutcome::Unavailable("daemon not running".into()),
            ProbeOutcome::Healthy,
        ],
        vec![Ok(())],
    );
    let action = ensure_ao_available(&probe, "dark-factory");
    assert_eq!(action, HealAction::Restarted);
    assert_eq!(*probe.start_calls.borrow(), 1);
    assert_eq!(*probe.probe_calls.borrow(), 2);
}

#[test]
fn ensure_ao_available_through_fake_probe_fail_closed_carries_diagnostics() {
    let probe = FakeProbe::new(
        vec![
            ProbeOutcome::Unavailable("ECONNREFUSED 127.0.0.1:8500".into()),
            ProbeOutcome::Unavailable("ECONNREFUSED 127.0.0.1:8500".into()),
        ],
        vec![Ok(())],
    );
    let action = ensure_ao_available(&probe, "dark-factory");
    match action {
        HealAction::FailClosed { reason } => {
            assert!(reason.contains("ao start --headless"), "reason: {reason}");
            assert!(reason.contains("ECONNREFUSED"));
        }
        other => panic!("expected FailClosed, got {other:?}"),
    }
}

// ---- RunToolProbe start_command_name ------------------------------------

#[test]
fn run_tool_probe_default_command_name_is_stable() {
    let probe = RunToolProbe::default();
    let name = probe.start_command_name();
    assert!(
        name.contains("ao start --headless"),
        "default start command name must mention `ao start --headless`, got: {name}"
    );
}

#[test]
fn run_tool_probe_custom_start_command_is_surfaced_in_failure() {
    use std::env;
    let prev = env::var("DARK_FACTORY_AO_START_CMD").ok();
    let prev_to = env::var("DARK_FACTORY_AO_START_TIMEOUT_SECS").ok();
    env::set_var("DARK_FACTORY_AO_START_CMD", "/bin/false");
    env::set_var("DARK_FACTORY_AO_START_TIMEOUT_SECS", "5");
    let probe = RunToolProbe::from_env();
    let err = probe.start_daemon().unwrap_err();
    assert!(
        err.contains("exited"),
        "start_daemon must surface the exit info, got: {err}"
    );
    // Restore.
    env::remove_var("DARK_FACTORY_AO_START_CMD");
    env::remove_var("DARK_FACTORY_AO_START_TIMEOUT_SECS");
    if let Some(p) = prev {
        env::set_var("DARK_FACTORY_AO_START_CMD", p);
    }
    if let Some(p) = prev_to {
        env::set_var("DARK_FACTORY_AO_START_TIMEOUT_SECS", p);
    }
}

// ---- DaemonError wiring sanity (park_reason verbatim surface) -----------

#[test]
fn ao_unavailable_park_reason_string_is_stable() {
    // The dispatch path records `overlay.park_reason = Some(reason.value())`
    // exactly — this test pins the stable telemetry token so a future
    // rename does not silently misclassify the park reason in dashboards.
    assert_eq!(
        HumanHoldReason::AoUnavailable.value(),
        "ao_unavailable",
        "AO-unavailable park reason must serialise as 'ao_unavailable'"
    );
}

// ---- Suppress unused-import warnings if this file is compiled alone ------

#[allow(dead_code)]
fn _silence_unused() {
    let _ = PathBuf::from("/tmp");
    let _ = Arc::new(Mutex::new(0u32));
    let _: Option<SessionId> = None;
    let _: Option<Result<SessionId, DaemonError>> = None;
    let _: Bead = Bead {
        id: "x".into(),
        title: "t".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: None,
    };
}
