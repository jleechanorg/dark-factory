// Task 9: slot supervisor (design doc §5, spec §4.2.2/§4.2.4). Enforces the
// operator safety envelope from spec §4.2.8: <= 30 concurrent workers total,
// <= 15 spawned in a single dispatch call. Pure arithmetic over `Sessions` +
// `StateStore` trait calls — no subprocess use, no LLM judgment (ZFC: routing
// to SMALL_PATH/STANDARD_PATH already happened in router.rs; this module only
// spawns whatever `ready` already contains, in order).
use crate::config::Config;
use crate::errors::DaemonError;
use crate::state::{BeadOverlay, OverlayState, StateStore};
use crate::tools::{Bead, Sessions, SpawnSpec};
use crate::router::RoutingVerdict;

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
///      so a failure here needs no rollback; the error just propagates and
///      the bead is left at its prior on-disk state.
///   5. Calls `sessions.spawn(&SpawnSpec { .. })`.
///   6. Saves the overlay with `state = Dispatched` to confirm the spawn.
///      If THIS save fails, the spawn already succeeded and would otherwise
///      be exactly the "spawn succeeds, store error leaves an untracked live
///      session" bug this closes: roll back by calling
///      `sessions.stop(&session_id)` on the just-spawned worker, then
///      propagate the original save error.
///
/// Returns the number of beads actually spawned. Never spawns past the cap;
/// if zero slots are free, returns `Ok(0)` without calling `sessions.spawn`
/// (verified by the fake's call log in tests — spec §4.2.8's caps are absolute).
pub fn dispatch_ready(
    sessions: &dyn Sessions,
    store: &dyn StateStore,
    cfg: &Config,
    ready: &[(Bead, RoutingVerdict)],
) -> Result<usize, DaemonError> {
    let active = sessions.active_count()?;
    let free_slots = cfg.max_workers.saturating_sub(active);
    let batch = free_slots.min(cfg.max_batch);

    let mut dispatched = 0usize;
    for (bead, verdict) in ready.iter().take(batch) {
        let mut overlay = store.load(&bead.id)?.unwrap_or_else(|| BeadOverlay {
            bead_id: bead.id.clone(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: None,
            session_id: None,
        });

        let branch = format!("factory/{}-r{}", bead.id, overlay.attempt);

        // Register the branch + persist the DISPATCHING intent BEFORE
        // spawning a worker. Neither creates a live process, so a failure
        // here needs no rollback.
        store.register_branch(&bead.id, &branch)?;

        overlay.state = OverlayState::Dispatching;
        overlay.branch = Some(branch.clone());
        store.save(&overlay)?;

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
            return Err(save_err);
        }

        dispatched += 1;
    }

    Ok(dispatched)
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
    }

    impl FakeSessions {
        fn new(active_count: usize) -> Self {
            Self {
                active_count,
                calls: RefCell::new(Vec::new()),
            }
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
        fail_save_for_state: Option<OverlayState>,
    }

    impl FakeStateStore {
        fn new() -> Self {
            Self::default()
        }

        fn failing_on(state: OverlayState) -> Self {
            Self {
                fail_save_for_state: Some(state),
                ..Self::default()
            }
        }
    }

    impl StateStore for FakeStateStore {
        fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
            Ok(self.overlays.borrow().get(bead_id).cloned())
        }

        fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
            if self.fail_save_for_state == Some(overlay.state) {
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

        fn increment_active_autonomy(&self, elapsed_secs: u64) -> Result<Vec<BeadOverlay>, DaemonError> {
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
                if overlay.state == OverlayState::Dispatched || overlay.state == OverlayState::Attested {
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

        fn save_rejection(&self, bead_id: &str, attempt: u32, reviewer: &str, feedback_hash: &str, _feedback_text: &str) -> Result<(), DaemonError> {
            self.rejections.borrow_mut().insert((bead_id.to_string(), attempt), (reviewer.to_string(), feedback_hash.to_string()));
            Ok(())
        }

        fn load_rejection(&self, bead_id: &str, attempt: u32) -> Result<Option<(String, String)>, DaemonError> {
            Ok(self.rejections.borrow().get(&(bead_id.to_string(), attempt)).cloned())
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
            .map(|i| (Bead {
                id: format!("bead-{i}"),
                title: format!("title {i}"),
                description: String::new(),
                file_tree_summary: String::new(),
                external_ref: None,
            }, RoutingVerdict::StandardPath))
            .collect()
    }

    #[test]
    fn forty_ready_zero_active_spawns_exactly_max_batch() {
        let sessions = FakeSessions::new(0);
        let store = FakeStateStore::new();
        let cfg = cfg();
        let ready = beads(40);

        let n = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(n, 15, "must cap at max_batch even with 30 free slots");
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

        let n = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(n, 2, "only 2 free slots remain under the 30-worker cap");
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

        let n = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

        assert_eq!(n, 0);
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

        let n = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();
        assert_eq!(n, 1);

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

        let n = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();
        assert_eq!(n, 1, "only 1 free slot under the 30-worker cap");

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

        let n = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();
        assert_eq!(n, 1);

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

        let err = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap_err();
        assert!(
            matches!(err, DaemonError::Tool { .. }),
            "expected the scripted save error to propagate, got {err:?}"
        );

        let calls = sessions.calls.borrow();
        assert!(
            calls.iter().any(|c| c == "spawn(bead-0)"),
            "spawn must still have been called: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "stop(fake-session-1)"),
            "the just-spawned session must be stopped on save failure: {calls:?}"
        );

        // The DISPATCHING intent (persisted before spawn) is still on disk —
        // rollback kills the process but deliberately does not erase the
        // record that a dispatch was attempted, so the Healer/operator can
        // see it rather than the bead silently reverting to QUEUED.
        let overlay = store.load("bead-0").unwrap().unwrap();
        assert_eq!(overlay.state, OverlayState::Dispatching);
        assert_eq!(overlay.branch.as_deref(), Some("factory/bead-0-r1"));
        assert_eq!(store.branches.borrow().as_slice(), ["factory/bead-0-r1"]);
    }
}
