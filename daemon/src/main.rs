// Tasks 4+ (state.rs, tools.rs, intake.rs, router.rs, dispatch.rs, verifier.rs) wire
// Config/DaemonError/telemetry::emit into the poll loop. Until then these modules are
// exercised only by their own unit tests, so allow dead_code at the crate level rather
// than deleting spec-mandated fields/variants ahead of the tasks that consume them.
#![allow(dead_code)]

mod config;
mod errors;
mod telemetry;

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
