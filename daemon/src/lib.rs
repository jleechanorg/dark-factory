// Library target so `daemon/tests/*` integration tests (and their shared
// `tests/common/mod.rs` fakes) can `use daemon::tools::{...}` against the five
// tool-boundary traits. `main.rs` stays the binary entry point and pulls its
// modules from this lib rather than redeclaring them, so there is exactly one
// copy of each module's source.
#![allow(dead_code)]

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

/// Process-wide lock for unit tests that mutate inherited environment state.
///
/// `libtest` runs tests in parallel, while PATH/HOME and related variables are
/// process-global. Keep the lock in the crate root so every test module shares
/// the same serialization point.
#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static Mutex<()> {
    static TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_ENV_LOCK.get_or_init(|| Mutex::new(()))
}

pub mod config;
pub mod constraints;
pub mod reroll;
pub mod dispatch;
pub mod errors;
pub mod health;
pub mod intake;
pub mod router;
pub mod state;
pub mod telemetry;
pub mod tick;
pub mod tools;
pub mod verifier;
pub mod adapters;
pub mod er_runner;
pub mod vacuous;
pub mod vacuous_red_green;
pub mod target_worktree;
pub mod worktree_reaper;


pub mod gates_compute;
pub mod vendor_health;
pub mod session_health_markers;
pub mod vendor_aliases;
pub mod gh_circuit_breaker;
