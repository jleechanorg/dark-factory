mod common;

use common::{FakeSessions, FakeStateStore};
use daemon::config;
use daemon::dispatch::{dispatch_ready, DriveBranchDecision};
use daemon::router::RoutingVerdict;
use daemon::tools::Bead;
use std::path::PathBuf;

#[test]
fn production_capacity_config_integration() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let live_config = manifest_dir.join("../config/daemon.toml");
    let example_config = manifest_dir.join("contracts/daemon.toml.example");
    let selected = if live_config.is_file() {
        live_config
    } else {
        example_config
    };

    let production = config::load(&selected).unwrap_or_else(|error| {
        panic!(
            "production-selected config {} failed to load: {error}",
            selected.display()
        )
    });
    assert_eq!(production.max_workers, 40);
    assert_eq!(production.max_batch, 15);

    let ready: Vec<_> = (0..20)
        .map(|index| {
            (
                Bead {
                    id: format!("production-capacity-{index}"),
                    title: format!("capacity test {index}"),
                    ..Bead::default()
                },
                RoutingVerdict::StandardPath,
                DriveBranchDecision::Generated,
            )
        })
        .collect();
    let sessions = FakeSessions::new();
    let store = FakeStateStore::new();
    let report = dispatch_ready(&sessions, &store, &production, &ready).unwrap();

    assert_eq!(
        report.success_count(),
        production.max_batch,
        "production max_batch must bound one dispatch tick"
    );
}
