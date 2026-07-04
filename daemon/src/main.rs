// Tasks 5+ (tools.rs, intake.rs, router.rs, dispatch.rs, verifier.rs) wire
// Config/DaemonError/telemetry::emit/state::StateStore into the poll loop. Until then
// these modules are exercised only by their own unit tests, so allow dead_code at the
// crate level rather than deleting spec-mandated fields/variants ahead of the tasks
// that consume them.
//
// Modules live in `lib.rs` (see that file) so `daemon/tests/*` integration tests
// can `use daemon::tools::{...}`; future tasks wire `daemon::{...}` modules into
// the poll loop here as they're consumed.
#![allow(dead_code)]

fn main() {
    println!("auto-factory daemon (stage 1)");
}

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_compiles() {
        let ok = true;
        assert!(ok);
    }
}
