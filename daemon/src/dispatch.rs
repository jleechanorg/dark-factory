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

/// Dispatch as many `ready` beads as the safety envelope allows.
///
/// Free slots = `min(max_workers - active_count, max_batch)` (spec §4.2.8).
/// Spawns strictly in `ready` order, up to that many. Each spawn:
///   1. Loads (or defaults to a fresh QUEUED) the bead's overlay.
///   2. Computes the attempt branch `factory/<bead_id>-r<attempt>`.
///   3. Calls `sessions.spawn(&SpawnSpec { .. })`.
///   4. Registers the branch in the store's branch registry.
///   5. Saves the overlay with `state = Dispatched` and `branch` set.
///
/// Returns the number of beads actually spawned. Never spawns past the cap;
/// if zero slots are free, returns `Ok(0)` without calling `sessions.spawn`
/// (verified by the fake's call log in tests — spec §4.2.8's caps are absolute).
pub fn dispatch_ready(
    sessions: &dyn Sessions,
    store: &dyn StateStore,
    cfg: &Config,
    ready: &[Bead],
) -> Result<usize, DaemonError> {
    let active = sessions.active_count()?;
    let free_slots = cfg.max_workers.saturating_sub(active);
    let batch = free_slots.min(cfg.max_batch);

    let mut dispatched = 0usize;
    for bead in ready.iter().take(batch) {
        let mut overlay = store.load(&bead.id)?.unwrap_or_else(|| BeadOverlay {
            bead_id: bead.id.clone(),
            state: OverlayState::Queued,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 0,
            spend_usd: 0.0,
            pr_number: None,
            branch: None,
        });

        let branch = format!("factory/{}-r{}", bead.id, overlay.attempt);

        let spec = SpawnSpec {
            bead_id: bead.id.clone(),
            branch: branch.clone(),
            prompt: bead.title.clone(),
        };
        sessions.spawn(&spec)?;

        store.register_branch(&bead.id, &branch)?;

        overlay.state = OverlayState::Dispatched;
        overlay.branch = Some(branch);
        store.save(&overlay)?;

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

    /// Local unit-test fake mirroring `tests/common/mod.rs`'s `FakeStateStore`.
    #[derive(Default)]
    struct FakeStateStore {
        overlays: RefCell<HashMap<String, BeadOverlay>>,
        branches: RefCell<Vec<String>>,
    }

    impl FakeStateStore {
        fn new() -> Self {
            Self::default()
        }
    }

    impl StateStore for FakeStateStore {
        fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
            Ok(self.overlays.borrow().get(bead_id).cloned())
        }

        fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
            self.overlays
                .borrow_mut()
                .insert(overlay.bead_id.clone(), overlay.clone());
            Ok(())
        }

        fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError> {
            self.branches.borrow_mut().push(branch.to_string());
            let _ = bead_id;
            Ok(())
        }

        fn owned_branches(&self) -> Result<Vec<String>, DaemonError> {
            Ok(self.branches.borrow().clone())
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

    fn beads(n: usize) -> Vec<Bead> {
        (0..n)
            .map(|i| Bead {
                id: format!("bead-{i}"),
                title: format!("title {i}"),
                external_ref: None,
            })
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
}
