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
    assert!(
        live_config.is_file(),
        "tracked production config is required at {}",
        live_config.display()
    );
    let production = load_capacity_contract(&live_config);
    assert_dispatch_batch_boundary(&production);
}

#[test]
fn example_capacity_config_integration() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let example_config = manifest_dir.join("contracts/daemon.toml.example");
    load_capacity_contract(&example_config);
}

fn load_capacity_contract(config_path: &std::path::Path) -> config::Config {
    let production = config::load(config_path).unwrap_or_else(|error| {
        panic!(
            "capacity config {} failed to load: {error}",
            config_path.display()
        )
    });
    assert_eq!(production.max_workers, 40);
    assert_eq!(production.max_batch, 15);
    production
}

fn assert_dispatch_batch_boundary(production: &config::Config) {
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
    let report = dispatch_ready(&sessions, &store, production, &ready).unwrap();

    assert_eq!(
        report.success_count(),
        production.max_batch,
        "production max_batch must bound one dispatch tick"
    );
}
