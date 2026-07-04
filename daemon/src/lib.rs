// Library target so `daemon/tests/*` integration tests (and their shared
// `tests/common/mod.rs` fakes) can `use daemon::tools::{...}` against the five
// tool-boundary traits. `main.rs` stays the binary entry point and pulls its
// modules from this lib rather than redeclaring them, so there is exactly one
// copy of each module's source.
#![allow(dead_code)]

pub mod config;
pub mod dispatch;
pub mod errors;
pub mod intake;
pub mod router;
pub mod state;
pub mod telemetry;
pub mod tools;
pub mod verifier;
