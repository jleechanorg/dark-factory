// Capacity contract test (bead dark-factory-59wt):
//
// Factory capacity is standardized to `max_workers=40`, `max_batch=15` across
// all canonical tracked configs (config/daemon.toml, daemon/config.toml,
// daemon/contracts/daemon.toml.example). This test loads each canonical
// config file and asserts the values; stale 30/15 or live 80/25 values are
// rejected. Related sibling configs that intentionally exercise the
// safety envelope with tiny values (worktree reaper unit tests use 1/1)
// are not in scope.
//
// Run with: cargo test --test capacity_contract -- --nocapture
//
// RED proof: with current canonical files the assertions fail — the test
// demonstrates that production config carries values other than 40/15.
// GREEN proof: after standardizing the canonical files the assertions pass
// and the test exercises the contract end-to-end.

use daemon::config::load;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at daemon/; the repo root is its parent.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR must have a parent (repo root)")
        .to_path_buf()
}

#[test]
fn config_daemon_toml_enforces_40_15_capacity_contract() {
    let p = repo_root().join("config/daemon.toml");
    let cfg = load(&p).unwrap_or_else(|e| {
        panic!(
            "config/daemon.toml must parse for the canonical capacity contract test: {e}"
        )
    });
    assert_eq!(
        cfg.max_workers, 40,
        "config/daemon.toml: max_workers must be 40 (capacity contract, bead dark-factory-59wt)"
    );
    assert_eq!(
        cfg.max_batch, 15,
        "config/daemon.toml: max_batch must be 15 (capacity contract, bead dark-factory-59wt)"
    );
}

#[test]
fn daemon_config_toml_enforces_40_15_capacity_contract() {
    let p = repo_root().join("daemon/config.toml");
    let cfg = load(&p).unwrap_or_else(|e| {
        panic!(
            "daemon/config.toml must parse for the canonical capacity contract test: {e}"
        )
    });
    assert_eq!(
        cfg.max_workers, 40,
        "daemon/config.toml: max_workers must be 40 (capacity contract, bead dark-factory-59wt)"
    );
    assert_eq!(
        cfg.max_batch, 15,
        "daemon/config.toml: max_batch must be 15 (capacity contract, bead dark-factory-59wt)"
    );
}

#[test]
fn daemon_toml_example_enforces_40_15_capacity_contract() {
    let p = repo_root().join("daemon/contracts/daemon.toml.example");
    let cfg = load(&p).unwrap_or_else(|e| {
        panic!(
            "daemon/contracts/daemon.toml.example must parse for the canonical capacity contract test: {e}"
        )
    });
    assert_eq!(
        cfg.max_workers, 40,
        "daemon/contracts/daemon.toml.example: max_workers must be 40 (capacity contract, bead dark-factory-59wt)"
    );
    assert_eq!(
        cfg.max_batch, 15,
        "daemon/contracts/daemon.toml.example: max_batch must be 15 (capacity contract, bead dark-factory-59wt)"
    );
}