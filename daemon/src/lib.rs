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
pub mod target_worktree;
pub mod worktree_reaper;


pub mod gates_compute;
pub mod vendor_health;
