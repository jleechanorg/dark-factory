// Task 9: slot supervisor (design doc §5, spec §4.2.2/§4.2.4). Enforces the
// operator safety envelope from spec §4.2.8: <= 30 concurrent workers total,
// <= 15 spawned in a single dispatch call. Pure arithmetic over `Sessions` +
// `StateStore` trait calls — no subprocess use, no LLM judgment (ZFC: routing
// to SMALL_PATH/STANDARD_PATH already happened in router.rs; this module only
// spawns whatever `ready` already contains, in order).
use crate::config::Config;
use crate::errors::DaemonError;
use crate::router::RoutingVerdict;
use crate::state::{BeadOverlay, OverlayState, StateStore};
use crate::tools::{Bead, Sessions, SpawnSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchSuccess {
    pub bead_id: String,
    pub attempt: u32,
    pub branch: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchFailure {
    pub bead_id: String,
    pub attempt: u32,
    pub branch: Option<String>,
    pub phase: &'static str,
    pub error: String,
    pub transient: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchReport {
    pub successes: Vec<DispatchSuccess>,
    pub failures: Vec<DispatchFailure>,
}

impl DispatchReport {
    pub fn success_count(&self) -> usize {
        self.successes.len()
    }
}

fn failure(
    bead: &Bead,
    attempt: u32,
    branch: Option<String>,
    phase: &'static str,
    err: DaemonError,
) -> DispatchFailure {
    DispatchFailure {
        bead_id: bead.id.clone(),
        attempt,
        branch,
        phase,
        transient: err.is_transient(),
        error: err.to_string(),
    }
}

/// Dispatch as many `ready` beads as the safety envelope allows.
///
/// Free slots = `min(max_workers - active_count, max_batch)` (spec §4.2.8).
/// Spawns strictly in `ready` order, up to that many. Each spawn is made
/// failure-atomic (spec §4.2.2/§4.2.4): the DISPATCHING intent + branch
/// registration are made durable BEFORE the worker process exists, and any
/// failure after a successful spawn is rolled back so no live session is
/// ever left untracked on disk:
///   1. Loads (or defaults to a fresh QUEUED) the bead's overlay.
///   2. Computes the attempt branch `factory/<bead_id>-r<attempt>`.
///   3. Registers the branch in the store's branch registry.
///   4. Persists the overlay with `state = Dispatching` and `branch` set —
///      the durable "about to spawn" record. Nothing has been spawned yet,
///      so a transient failure here needs no rollback and can be reported
///      without stopping later beads.
///   5. Calls `sessions.spawn(&SpawnSpec { .. })`.
///   6. Saves the overlay with `state = Dispatched` to confirm the spawn.
///      If THIS save fails, the spawn already succeeded and would otherwise
///      be exactly the "spawn succeeds, store error leaves an untracked live
///      session" bug this closes: roll back by calling
///      `sessions.stop(&session_id)` on the just-spawned worker. If that stop
///      succeeds, requeue the bead durably, then report the original
///      transient save failure and continue. If stop or requeue persistence
///      fails, stop the batch because a live untracked worker or stranded
///      DISPATCHING row may remain. A `sessions.spawn` error is also fatal
///      because the real spawn path may have created a process before failing
///      to return a session id.
///
/// Returns a per-bead report. Never spawns past the cap; if zero slots are
/// free, returns an empty report without calling `sessions.spawn` (verified by
/// the fake's call log in tests — spec §4.2.8's caps are absolute).
pub fn dispatch_ready(
    sessions: &dyn Sessions,
    store: &dyn StateStore,
    cfg: &Config,
    ready: &[(Bead, RoutingVerdict)],
) -> Result<DispatchReport, DaemonError> {
    let active = sessions.active_count()?;
    let free_slots = cfg.max_workers.saturating_sub(active);
    let batch = free_slots.min(cfg.max_batch);

    let mut report = DispatchReport::default();
    for (bead, verdict) in ready {
        if report.success_count() >= batch {
            break;
        }

        let mut overlay = match store.load(&bead.id) {
            Ok(Some(overlay)) => overlay,
            Ok(None) => BeadOverlay {
                bead_id: bead.id.clone(),
                state: OverlayState::Queued,
                attempt: 1,
                reroll_count: 0,
                autonomy_secs: 0,
                spend_usd: 0.0,
                pr_number: None,
                branch: None,
                session_id: None,
            },
            Err(err) if err.is_transient() => {
                report
                    .failures
                    .push(failure(bead, 1, None, "load_overlay", err));
                continue;
            }
            Err(err) => return Err(err),
        };

        let branch = format!("factory/{}-r{}", bead.id, overlay.attempt);

        // Register the branch + persist the DISPATCHING intent BEFORE
        // spawning a worker. Neither creates a live process, so a failure
        // here needs no rollback.
        if let Err(err) = store.register_branch(&bead.id, &branch) {
            if err.is_transient() {
                report.failures.push(failure(
                    bead,
                    overlay.attempt,
                    Some(branch),
                    "register_branch",
                    err,
                ));
                continue;
            }
            return Err(err);
        }

        overlay.state = OverlayState::Dispatching;
        overlay.branch = Some(branch.clone());
        if let Err(err) = store.save(&overlay) {
            if err.is_transient() {
                report.failures.push(failure(
                    bead,
                    overlay.attempt,
                    Some(branch),
                    "save_dispatching",
                    err,
                ));
                continue;
            }
            return Err(err);
        }

        let prompt = match verdict {
            RoutingVerdict::ResearchPath => {
                format!(
                    "Route to RESEARCH_PATH: Run /factory with pipelines/slim/minimal_research.dot to research: {}",
                    bead.title
                )
            }
            RoutingVerdict::GenericPath => {
                format!(
                    "Route to GENERIC_PATH: Run /factory with pipelines/slim/spec_gen.dot to handle: {}",
                    bead.title
                )
            }
            _ => bead.title.clone(),
        };

        let spec = SpawnSpec {
            bead_id: bead.id.clone(),
            branch: branch.clone(),
            prompt,
        };
        let session_id = sessions.spawn(&spec)?;

        overlay.state = OverlayState::Dispatched;
        overlay.session_id = Some(session_id.0.clone());
        if let Err(save_err) = store.save(&overlay) {
            // The worker process now exists but the daemon failed to
            // durably record it as DISPATCHED. Kill the just-spawned worker
            // so no live session survives without a matching on-disk record
            // (spec §4.2.2/§4.2.4 failure-atomicity). If `stop` ITSELF fails
            // we now have an untracked live session we can't even kill —
            // that's a more urgent operator-facing failure than the original
            // save error, so it takes priority and is returned instead.
            sessions.stop(&session_id)?;
            if save_err.is_transient() {
                overlay.state = OverlayState::Queued;
                overlay.session_id = None;
                store.save(&overlay)?;
                report.failures.push(failure(
                    bead,
                    overlay.attempt,
                    Some(branch),
                    "save_dispatched",
                    save_err,
                ));
                continue;
            }
            return Err(save_err);
        }

        report.successes.push(DispatchSuccess {
            bead_id: bead.id.clone(),
            attempt: overlay.attempt,
            branch,
            session_id: session_id.0,
        });
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::OverlayState;
    use crate::tools::SessionId;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Local unit-test fake mirroring `tests/common/mod.rs`'s `FakeSessions`
    /// (same call-log shape) without the `daemon::` crate-qualified imports
    /// that fakes file needs when included from `tests/*.rs` as a separate
    /// integration-test crate. Kept in sync by hand; both fakes log
    /// `spawn(<bead_id>)` so assertions read identically either place.
    struct FakeSessions {
        active_count: usize,
        calls: RefCell<Vec<String>>,
        fail_spawn_for: RefCell<Vec<String>>,
        fail_stop_for: RefCell<Vec<String>>,
    }

    impl FakeSessions {
        fn new(active_count: usize) -> Self {
            Self {
                active_count,
                calls: RefCell::new(Vec::new()),
                fail_spawn_for: RefCell::new(Vec::new()),
                fail_stop_for: RefCell::new(Vec::new()),
            }
        }

        fn fail_spawn_for(&self, bead_id: &str) {
            self.fail_spawn_for.borrow_mut().push(bead_id.to_string());
        }

        fn fail_stop_for(&self, session_id: &str) {
            self.fail_stop_for.borrow_mut().push(session_id.to_string());
        }
    }

    impl Sessions for FakeSessions {
        fn active_count(&self) -> Result<usize, DaemonError> {
            self.calls.borrow_mut().push("active_count".into());
            Ok(self.active_count)
        }

        fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("spawn({})", spec.bead_id));
            if self.fail_spawn_for.borrow().contains(&spec.bead_id) {
                return Err(DaemonError::Tool {
                    tool: "ao".into(),
                    rc: 1,
                    stderr: format!("scripted spawn failure for {}", spec.bead_id),
                });
            }
            Ok(SessionId("fake-session-1".into()))
        }

        fn attach(&self, branch: &str, bead_id: &str) -> Result<SessionId, DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("attach({branch},{bead_id})"));
            Ok(SessionId("fake-session-1".into()))
        }

        fn stop(&self, id: &SessionId) -> Result<(), DaemonError> {
            self.calls.borrow_mut().push(format!("stop({})", id.0));
            if self.fail_stop_for.borrow().contains(&id.0) {
                return Err(DaemonError::Tool {
                    tool: "ao".into(),
                    rc: 1,
                    stderr: format!("scripted stop failure for {}", id.0),
                });
            }
            Ok(())
        }

        fn is_quiescent(&self, id: &SessionId) -> Result<bool, DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("is_quiescent({})", id.0));
            Ok(true)
        }
    }

    /// Local unit-test fake mirroring `tests/common/mod.rs`'s `FakeStateStore`,
    /// plus a `fail_save_for_state` hook so rollback-on-save-failure tests can
    /// script the SECOND save (the DISPATCHED confirmation) to fail while the
    /// first save (the DISPATCHING intent) still succeeds.
    #[derive(Default)]
    struct FakeStateStore {
        overlays: RefCell<HashMap<String, BeadOverlay>>,
        branches: RefCell<Vec<String>>,
        branch_beads: RefCell<HashMap<String, String>>,
        rejections: RefCell<HashMap<(String, u32), (String, String)>>,
        fail_save_for_state: RefCell<Vec<(String, OverlayState)>>,
    }

    impl FakeStateStore {
        fn new() -> Self {
            Self::default()
        }

        fn failing_on(state: OverlayState) -> Self {
            let store = Self::default();
            store
                .fail_save_for_state
                .borrow_mut()
                .push(("*".into(), state));
            store
        }

        fn fail_save_for(&self, bead_id: &str, state: OverlayState) {
            self.fail_save_for_state
                .borrow_mut()
                .push((bead_id.to_string(), state));
        }
    }

    impl StateStore for FakeStateStore {
        fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
            Ok(self.overlays.borrow().get(bead_id).cloned())
        }

        fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
            if self
                .fail_save_for_state
                .borrow()
                .iter()
                .any(|(bead_id, state)| {
                    (bead_id == "*" || bead_id == &overlay.bead_id) && *state == overlay.state
                })
            {
                return Err(DaemonError::Tool {
                    tool: "sqlite".into(),
                    rc: -1,
                    stderr: format!("scripted save failure for {}", overlay.state.as_str()),
                });
            }
            self.overlays
                .borrow_mut()
                .insert(overlay.bead_id.clone(), overlay.clone());
            Ok(())
        }

        fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError> {
            self.branches.borrow_mut().push(branch.to_string());
            self.branch_beads
                .borrow_mut()
                .insert(branch.to_string(), bead_id.to_string());
            Ok(())
        }

        fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError> {
            Ok(self.branch_beads.borrow().get(branch).cloned())
        }

        fn owned_branches(&self) -> Result<Vec<String>, DaemonError> {
            Ok(self.branches.borrow().clone())
        }

        fn increment_active_autonomy(
            &self,
            elapsed_secs: u64,
        ) -> Result<Vec<BeadOverlay>, DaemonError> {
            let updated = self.list_active_overlays()?;
            if elapsed_secs > 0 {
                for overlay in &updated {
                    self.bump_autonomy_secs(&overlay.bead_id, elapsed_secs)?;
                }
            }
            self.list_active_overlays()
        }

        fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError> {
            let mut out = Vec::new();
            for overlay in self.overlays.borrow().values() {
                if overlay.state == OverlayState::Dispatched
                    || overlay.state == OverlayState::Attested
                {
                    out.push(overlay.clone());
                }
            }
            Ok(out)
        }

        fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), DaemonError> {
            if delta_secs == 0 {
                return Ok(());
            }
            if let Some(overlay) = self.overlays.borrow_mut().get_mut(bead_id) {
                overlay.autonomy_secs += delta_secs;
            }
            Ok(())
        }

        fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, DaemonError> {
            let mut recovered = Vec::new();
            for overlay in self.overlays.borrow_mut().values_mut() {
                if overlay.state == OverlayState::HumanHeld && overlay.attempt < max_attempt {
                    overlay.state = OverlayState::Queued;
                    overlay.attempt += 1;
                    overlay.autonomy_secs = 0;
                    recovered.push(overlay.clone());
                }
            }
            Ok(recovered)
        }

        fn save_rejection(
            &self,
            bead_id: &str,
            attempt: u32,
            reviewer: &str,
            feedback_hash: &str,
            _feedback_text: &str,
        ) -> Result<(), DaemonError> {
            self.rejections.borrow_mut().insert(
                (bead_id.to_string(), attempt),
                (reviewer.to_string(), feedback_hash.to_string()),
            );
            Ok(())
        }

        fn load_rejection(
            &self,
            bead_id: &str,
            attempt: u32,
        ) -> Result<Option<(String, String)>, DaemonError> {
            Ok(self
                .rejections
                .borrow()
                .get(&(bead_id.to_string(), attempt))
                .cloned())
        }
    }

    fn cfg() -> Config {
        Config {
            target_repo: "owner/repo".into(),
            base_branch: "main".into(),
            stage: 1,
            max_workers: 30,
            max_batch: 15,
            fast_tick_secs: 60,
            slow_tick_secs: 600,
            autonomy_timebox_secs: 10_800,
            budget_warn_usd: 0.0,
            spec_dir: ".factory/specs/".into(),
        }
    }

    fn beads(n: usize) -> Vec<(Bead, RoutingVerdict)> {
        (0..n)
            .map(|i| {
                (
                    Bead {
                        id: format!("bead-{i}"),
                        title: format!("title {i}"),
                        description: String::new(),
                        file_tree_summary: String::new(),
                        external_ref: None,
                    },
                    RoutingVerdict::StandardPath,
                )
            })
            .collect()
    }

    #[test]
    fn forty_ready_zero_active_spawns_exactly_max_batch() {
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::new();
        let cfg = cfg();
        let ready = beads(40);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(
            report.success_count(),
            15,
            "must cap at max_batch even with 30 free slots"
        );
        let spawn_calls = sessions
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("spawn("))
            .count();
        assert_eq!(spawn_calls, 15);
    }

    #[test]
    fn twenty_eight_active_of_thirty_spawns_exactly_two() {
        let sessions = FakeSessions::new(28);
        let store = FakeStateStore::new();
        let cfg = cfg();
        let ready = beads(40);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(
            report.success_count(),
            2,
            "only 2 free slots remain under the 30-worker cap"
        );
        let spawn_calls = sessions
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("spawn("))
            .count();
        assert_eq!(spawn_calls, 2);
    }

    #[test]
    fn thirty_active_spawns_nothing_and_never_calls_spawn() {
        let sessions = FakeSessions::new(30);
        let store = FakeStateStore::new();
        let cfg = cfg();
        let ready = beads(40);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(report.success_count(), 0);
        let spawn_calls = sessions
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("spawn("))
            .count();
        assert_eq!(
            spawn_calls, 0,
            "at the cap, dispatch must not call Sessions::spawn at all"
        );
    }

    #[test]
    fn spawn_registers_branch_and_flips_queued_to_dispatched() {
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::new();
        // Pre-seed the overlay as QUEUED (as intake would leave it) with attempt=1.
        store
            .save(&BeadOverlay {
                bead_id: "bead-0".into(),
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
        let cfg = cfg();
        let ready = beads(1);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();
        assert_eq!(report.success_count(), 1);

        assert_eq!(store.branches.borrow().as_slice(), ["factory/bead-0-r1"]);

        let overlay = store.load("bead-0").unwrap().unwrap();
        assert_eq!(overlay.state, OverlayState::Dispatched);
        assert_eq!(overlay.branch.as_deref(), Some("factory/bead-0-r1"));
    }

    #[test]
    fn dispatch_order_follows_ready_slice_order() {
        let sessions = FakeSessions::new(29);
        let store = FakeStateStore::new();
        let cfg = cfg();
        let ready = beads(5);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();
        assert_eq!(
            report.success_count(),
            1,
            "only 1 free slot under the 30-worker cap"
        );

        let spawn_calls: Vec<String> = sessions
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("spawn("))
            .cloned()
            .collect();
        assert_eq!(spawn_calls, ["spawn(bead-0)"]);
    }

    #[test]
    fn branch_registered_and_dispatching_intent_saved_before_spawn() {
        // Failure-atomicity contract: `register_branch` + the DISPATCHING
        // save must both be durable BEFORE `Sessions::spawn` is ever called,
        // so a crash between them and the spawn leaves an accurate on-disk
        // record rather than a phantom worker with nothing tracking it.
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::new();
        let cfg = cfg();
        let ready = beads(1);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();
        assert_eq!(report.success_count(), 1);

        // Final state is DISPATCHED (spawn + confirmation both succeeded),
        // but the branch registry write happened unconditionally up front.
        assert_eq!(store.branches.borrow().as_slice(), ["factory/bead-0-r1"]);
        let overlay = store.load("bead-0").unwrap().unwrap();
        assert_eq!(overlay.state, OverlayState::Dispatched);
    }

    #[test]
    fn save_failure_after_spawn_rolls_back_via_stop_and_propagates_error() {
        // Reproduces the exact bug this hardening closes: spawn succeeds,
        // then the DISPATCHED confirmation save fails. Before the fix this
        // left an untracked live session; after the fix, `Sessions::stop`
        // is called on the just-spawned session and the original save error
        // propagates to the caller instead of being swallowed.
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::failing_on(OverlayState::Dispatched);
        let cfg = cfg();
        let ready = beads(1);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();
        assert_eq!(report.success_count(), 0);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].bead_id, "bead-0");
        assert_eq!(report.failures[0].phase, "save_dispatched");

        let calls = sessions.calls.borrow();
        assert!(
            calls.iter().any(|c| c == "spawn(bead-0)"),
            "spawn must still have been called: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "stop(fake-session-1)"),
            "the just-spawned session must be stopped on save failure: {calls:?}"
        );

        // Rollback kills the process and durably requeues the bead. Leaving
        // the earlier DISPATCHING intent on disk would strand this bead
        // because the successful tick would not run top-level reconciliation.
        let overlay = store.load("bead-0").unwrap().unwrap();
        assert_eq!(overlay.state, OverlayState::Queued);
        assert_eq!(overlay.branch.as_deref(), Some("factory/bead-0-r1"));
        assert_eq!(store.branches.borrow().as_slice(), ["factory/bead-0-r1"]);
    }

    #[test]
    fn dispatching_save_failure_for_first_bead_does_not_prevent_later_dispatch() {
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::new();
        store.fail_save_for("bead-0", OverlayState::Dispatching);
        let cfg = cfg();
        let ready = beads(2);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(report.success_count(), 1);
        assert_eq!(report.successes[0].bead_id, "bead-1");
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].bead_id, "bead-0");
        assert_eq!(report.failures[0].phase, "save_dispatching");

        assert!(
            store.load("bead-0").unwrap().is_none(),
            "DISPATCHING save failed before spawn, so no overlay mutation is durable"
        );
        let bead_1 = store.load("bead-1").unwrap().unwrap();
        assert_eq!(bead_1.state, OverlayState::Dispatched);

        let calls = sessions.calls.borrow();
        assert!(
            !calls.iter().any(|c| c == "spawn(bead-0)"),
            "pre-spawn save failure must not spawn the failed bead"
        );
        assert!(calls.iter().any(|c| c == "spawn(bead-1)"));
    }

    #[test]
    fn pre_spawn_failures_do_not_consume_worker_capacity() {
        let sessions = FakeSessions::new(29);
        let store = FakeStateStore::new();
        store.fail_save_for("bead-0", OverlayState::Dispatching);
        let cfg = cfg();
        let ready = beads(2);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(
            report.success_count(),
            1,
            "one free worker slot should still dispatch a later bead when the first failure happened before spawn"
        );
        assert_eq!(report.successes[0].bead_id, "bead-1");
        assert_eq!(report.failures[0].bead_id, "bead-0");
    }

    #[test]
    fn spawn_failure_after_dispatching_intent_is_fatal() {
        let sessions = FakeSessions::new(0);
        sessions.fail_spawn_for("bead-0");
        let store = FakeStateStore::new();
        let cfg = cfg();
        let ready = beads(2);

        let err = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap_err();
        assert!(
            matches!(err, DaemonError::Tool { .. }),
            "spawn failure must stop the batch because a worker may exist without a session id: {err:?}"
        );

        let bead_0 = store.load("bead-0").unwrap().unwrap();
        assert_eq!(bead_0.state, OverlayState::Dispatching);
        assert!(
            store.load("bead-1").unwrap().is_none(),
            "later beads must not dispatch after an ambiguous spawn failure"
        );
        let calls = sessions.calls.borrow();
        assert!(calls.iter().any(|c| c == "spawn(bead-0)"));
        assert!(!calls.iter().any(|c| c == "spawn(bead-1)"));
    }

    #[test]
    fn save_failure_after_spawn_stops_session_and_continues_later_dispatch() {
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::new();
        store.fail_save_for("bead-0", OverlayState::Dispatched);
        let cfg = cfg();
        let ready = beads(2);

        let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(report.success_count(), 1);
        assert_eq!(report.successes[0].bead_id, "bead-1");
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].bead_id, "bead-0");
        assert_eq!(report.failures[0].phase, "save_dispatched");

        let calls = sessions.calls.borrow();
        assert!(
            calls.iter().any(|c| c == "stop(fake-session-1)"),
            "the just-spawned session must be stopped on save failure: {calls:?}"
        );

        let bead_0 = store.load("bead-0").unwrap().unwrap();
        assert_eq!(bead_0.state, OverlayState::Queued);
        let bead_1 = store.load("bead-1").unwrap().unwrap();
        assert_eq!(bead_1.state, OverlayState::Dispatched);
    }

    #[test]
    fn requeue_save_failure_after_rollback_is_fatal() {
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::new();
        store.fail_save_for("bead-0", OverlayState::Dispatched);
        store.fail_save_for("bead-0", OverlayState::Queued);
        let cfg = cfg();
        let ready = beads(2);

        let err = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap_err();
        assert!(
            matches!(err, DaemonError::Tool { .. }),
            "failed rollback requeue must be fatal so top-level reconciliation can recover: {err:?}"
        );

        let calls = sessions.calls.borrow();
        assert!(calls.iter().any(|c| c == "spawn(bead-0)"));
        assert!(calls.iter().any(|c| c == "stop(fake-session-1)"));
        assert!(
            !calls.iter().any(|c| c == "spawn(bead-1)"),
            "later beads must not dispatch after rollback requeue persistence fails"
        );

        let bead_0 = store.load("bead-0").unwrap().unwrap();
        assert_eq!(bead_0.state, OverlayState::Dispatching);
    }

    #[test]
    fn stop_failure_after_spawn_save_failure_is_fatal() {
        let sessions = FakeSessions::new(0);
        sessions.fail_stop_for("fake-session-1");
        let store = FakeStateStore::new();
        store.fail_save_for("bead-0", OverlayState::Dispatched);
        let cfg = cfg();
        let ready = beads(2);

        let err = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap_err();
        assert!(
            matches!(err, DaemonError::Tool { .. }),
            "stop failure must remain fatal because a live untracked worker may remain: {err:?}"
        );

        let calls = sessions.calls.borrow();
        assert!(calls.iter().any(|c| c == "spawn(bead-0)"));
        assert!(calls.iter().any(|c| c == "stop(fake-session-1)"));
        assert!(
            !calls.iter().any(|c| c == "spawn(bead-1)"),
            "later beads must not dispatch after failed rollback stop"
        );
    }
}
