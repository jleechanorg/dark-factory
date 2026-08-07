// Library target so `daemon/tests/*` integration tests (and their shared
// `tests/common/mod.rs` fakes) can `use daemon::tools::{...}` against the five
// tool-boundary traits. `main.rs` stays the binary entry point and pulls its
// modules from this lib rather than redeclaring them, so there is exactly one
// copy of each module's source.
#![allow(dead_code)]

pub mod config;
pub mod constraints;
pub mod reroll;
pub mod dispatch;
pub mod errors;
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


pub mod gates_compute;
pub mod vendor_health;

/// Per-bead retry backoff gate (bead jleechan-w4q3, G12
/// retry-backoff-bleed-into-global-suppression). Isolated to a single bead's
/// queue slot via `BeadOverlay::next_attempt_at`, NOT the global tick loop's
/// `consecutive_failures` counter. Returns the bounded exponential backoff
/// in seconds for the given count of consecutive transient spawn failures
/// on this bead (5, 10, 20, 40, 60, 60, ...), capped at 60 s so a single
/// hot bead cannot monopolize the factory. The global tick backoff
/// (configured by `MAX_TICK_BACKOFF_SECS`) is a separate, bounded value
/// reserved for genuinely tick-level transient errors (e.g. `gh api`
/// rate-limit on intake). Per-bead spawn failures were previously
/// conflated with the global counter; the bleed let one bad bead block
/// the whole queue. See `/factory-evolve` G12 for context.
pub const MAX_PER_BEAD_BACKOFF_SECS: u64 = 60;

pub fn per_bead_backoff_secs(spawn_failure_count: u32) -> u64 {
    if spawn_failure_count == 0 {
        return 0;
    }
    let base = 5_u64;
    let exponent = spawn_failure_count.saturating_sub(1).min(63);
    base.saturating_mul(2_u64.saturating_pow(exponent))
        .min(MAX_PER_BEAD_BACKOFF_SECS)
}

