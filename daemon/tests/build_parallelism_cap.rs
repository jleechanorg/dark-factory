// RED (jleechanorg/dark-factory#220): cargo build/test on this crate must
// cap parallelism via .cargo/config.toml, or a single `cargo test --release`
// spikes host load past nproc on shared hosts (load 48 on a 32-thread box,
// confirmed twice 2026-07-10). Asserts the cap is present and sane rather
// than asserting a specific numeric value, so this doesn't churn if the
// safe number is tuned later.

use std::fs;
use std::path::Path;

#[test]
fn cargo_config_caps_build_jobs() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let config_path = Path::new(manifest_dir).join(".cargo").join("config.toml");

    assert!(
        config_path.exists(),
        "expected {} to exist so `cargo build`/`cargo test` on the daemon \
         crate cannot silently use unbounded -j on shared hosts (see \
         jleechanorg/dark-factory#220)",
        config_path.display()
    );

    let contents = fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", config_path.display()));

    let parsed: toml::Value = contents
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", config_path.display()));

    let jobs = parsed
        .get("build")
        .and_then(|b| b.get("jobs"))
        .and_then(|j| j.as_integer())
        .unwrap_or_else(|| {
            panic!(
                "{} must set [build] jobs = <N> to cap cargo parallelism \
                 (jleechanorg/dark-factory#220)",
                config_path.display()
            )
        });

    let logical_cpus = std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(32);

    assert!(
        jobs >= 1 && jobs < logical_cpus,
        "[build] jobs = {jobs} in {} must be a positive number strictly \
         less than the host's logical CPU count ({logical_cpus}) — the \
         whole point is to leave headroom for other processes on shared \
         hosts (jleechanorg/dark-factory#220)",
        config_path.display()
    );
}
