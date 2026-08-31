// RED test for dark-factory #780 — production 40-worker / 15-batch capacity
// contract (jleechanorg/worldarchitect.ai production deployment).
//
// Scope: prove the production TOML ships the exact capacity envelope the
// worldarchitect.ai auto-factory actually runs under, and that the dispatch
// safety math (spec §4.2.8 / ao-spawn-safety) honours it.
//
// Acceptance criteria (verbatim from the bead):
//   1. Canonical production TOMLs are loaded from disk, NOT assigned as
//      constants in this test. The values asserted against are derived from
//      `Config::max_workers` / `Config::max_batch` on the parsed config — so
//      the contract travels with the TOML, not with this test file.
//   2. With `cfg.max_workers - 1` workers active, the dispatch safety math
//      permits exactly one free slot (one new spawn per tick).
//   3. With `cfg.max_workers` workers active, zero free slots (no spawn).
//   4. A single tick dispatches at most `cfg.max_batch` beads regardless of
//      how many ready beads the router hands the dispatcher.
//
// RED proof: this test MUST FAIL against the current production values
// (`config/daemon.toml` ships `max_workers = 80` / `max_batch = 25` and
// `daemon/contracts/daemon.toml.example` ships `max_workers = 30`). The GREEN
// PR (#790 — out of scope for this PR) will land the corrected values.
//
// This file is a standalone RED PR. It MUST NOT edit scheduling or config
// GREEN code, and MUST NOT cherry-pick the GREEN PR.

mod common;

use common::{FakeSessions, FakeStateStore};
use daemon::config::{Config, RepoConfig};
use daemon::dispatch::{dispatch_ready, DriveBranchDecision};
use daemon::router::RoutingVerdict;
use daemon::tools::Bead;
use std::path::{Path, PathBuf};

/// The live production config for `jleechanorg/worldarchitect.ai` — the
/// canonical production TOML the daemon is started with on jeff-ubuntu. The
/// contracts example below mirrors this; both are authoritative.
///
/// Resolved relative to the daemon package's `CARGO_MANIFEST_DIR`
/// (`.../dark-factory/daemon`), so the test finds the files whether it is
/// invoked from the repo root, the daemon dir, or a CI checkout. The
/// production file lives at `<repo>/config/daemon.toml` (one level up from
/// the package); the canonical schema lives at
/// `<repo>/daemon/contracts/daemon.toml.example` (next to the package).
fn production_toml_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("config")
        .join("daemon.toml")
}

fn contracts_example_toml_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("contracts")
        .join("daemon.toml.example")
}

/// Build a ready-list of `n` placeholder beads sharing the same fake repo
/// identity. The router-side routing verdict is irrelevant to the capacity
/// math — `StandardPath` exercises the normal coder-dispatch path.
fn ready_beads(n: usize) -> Vec<(Bead, RoutingVerdict, DriveBranchDecision)> {
    (0..n)
        .map(|i| {
            (
                Bead {
                    id: format!("bead-{i}"),
                    title: format!("title {i}"),
                    description: String::new(),
                    notes: String::new(),
                    file_tree_summary: String::new(),
                    external_ref: None,
                },
                RoutingVerdict::StandardPath,
                DriveBranchDecision::Generated,
            )
        })
        .collect()
}

/// Force the production config into a state the dispatch path accepts in the
/// test harness. The production file targets `jleechanorg/worldarchitect.ai`
/// with no `[repos.*]` entry for that identity (the bare `cfg.target_repo`
/// fallback resolves it), but the dispatch safety math also runs the
/// `worker_checkout_is_configured` gate which requires an absolute existing
/// `local_checkout` for non-fixture repos. We override the `[repos]` table
/// with a fixture entry pointing at the test's CWD so the dispatch loop
/// reaches the spawn path and exercises the capacity envelope.
///
/// `test_repo` and `test_ao_project` are kept distinct from the production
/// identities on purpose — we only care about the dispatch SAFETY MATH
/// (capacity envelope), not the production repo wiring. The dispatch loop
/// also asserts the spawned session's remote URL matches the bead's
/// resolved repo; `script_worktree_remote_for` (below) scripts the fake
/// `Sessions::worktree_remote_url` to match.
fn with_dispatchable_repos(cfg: Config) -> (Config, &'static str) {
    let mut cfg = cfg;
    let cwd = std::env::current_dir().expect("test cwd");
    let test_repo = "owner/red-test-780";
    let test_ao_project = "red-test-780";
    cfg.target_repo = test_repo.to_string();
    cfg.repos.insert(
        test_repo.to_string(),
        RepoConfig {
            ao_project: test_ao_project.to_string(),
            push_remote: "origin".to_string(),
            local_checkout: Some(cwd),
        },
    );
    (cfg, test_ao_project)
}

/// Script the fake `Sessions::worktree_remote_url` to return a URL whose
/// `github.com/<owner>/<repo>` path matches the test repo the dispatch
/// loop is driving — otherwise the spawn-time remote-assertion gate
/// (jleechan-bqdv Stage C / jleechan-9sh5) parks the bead
/// `worktree_remote_mismatch` and zero dispatches succeed. Default
/// `FakeSessions::worktree_remote_url` only knows `worldarchitect` and
/// `owner/repo`; this helper lets the test inject the contract's
/// `owner/red-test-780` repo URL.
fn script_worktree_remote_for(sessions: &FakeSessions) {
    sessions.set_worktree_remote("https://github.com/owner/red-test-780.git");
}

fn load_config(path: &Path) -> Config {
    daemon::config::load(path)
        .unwrap_or_else(|e| panic!("failed to load canonical production TOML {}: {e}", path.display()))
}

/// `tests/common/mod.rs`'s `FakeSessions::new()` takes no arguments — set
/// the scripted `active_count` field directly so this test can dial in the
/// exact capacity envelope the production TOML ships.
fn sessions_with_active(active: usize) -> FakeSessions {
    let mut s = FakeSessions::new();
    s.active_count = active;
    s
}

#[test]
fn production_daemon_toml_matches_40_worker_15_batch_contract() {
    // RED assertion #1: the live production TOML must carry the 40/15
    // capacity envelope. Currently fails — production is 80/25.
    let path = production_toml_path();
    let cfg = load_config(&path);
    assert_eq!(
        cfg.max_workers, 40,
        "production {path} must enforce max_workers=40 (40-worker capacity contract, jleechanorg/worldarchitect.ai); \
         got max_workers={got}. See dark-factory #780.",
        path = path.display(),
        got = cfg.max_workers,
    );
    assert_eq!(
        cfg.max_batch, 15,
        "production {path} must enforce max_batch=15 (15-batch capacity contract); \
         got max_batch={got}. See dark-factory #780.",
        path = path.display(),
        got = cfg.max_batch,
    );
    // The production config's target_repo is the one this contract binds.
    assert_eq!(
        cfg.target_repo, "jleechanorg/worldarchitect.ai",
        "production {path} target_repo must remain jleechanorg/worldarchitect.ai (the contract this PR pins)",
        path = path.display(),
    );
}

#[test]
fn contracts_example_daemon_toml_matches_40_worker_15_batch_contract() {
    // RED assertion #2: the canonical example TOML committed to the repo
    // must mirror the 40/15 contract — anyone using the example as a
    // starting point must land inside the envelope. Currently fails — the
    // example still ships 30/15.
    let path = contracts_example_toml_path();
    let cfg = load_config(&path);
    assert_eq!(
        cfg.max_workers, 40,
        "{path} (the schema example) must enforce max_workers=40; \
         got max_workers={got}. See dark-factory #780.",
        path = path.display(),
        got = cfg.max_workers,
    );
    assert_eq!(
        cfg.max_batch, 15,
        "{path} (the schema example) must enforce max_batch=15; \
         got max_batch={got}. See dark-factory #780.",
        path = path.display(),
        got = cfg.max_batch,
    );
}

#[test]
fn production_max_workers_minus_one_active_permits_exactly_one_slot() {
    // "39 active permits one slot" — read as: with `max_workers - 1` workers
    // already active, the dispatch safety envelope has room for exactly one
    // more spawn (one free slot under the saturating-subtract math in
    // `dispatch_ready_with_vcs`).
    //
    // We derive BOTH the cap and the active-count from the production TOML,
    // never assign constants, so this test tracks whatever capacity envelope
    // the production file ships — that's the whole point of the contract.
    let (cfg, _test_ao_project) = with_dispatchable_repos(load_config(&production_toml_path()));
    let max_workers = cfg.max_workers;
    assert!(
        max_workers >= 1,
        "production max_workers must be >= 1 to exercise the saturating-subtract math; got {max_workers}",
    );
    let sessions = sessions_with_active(max_workers - 1);
    script_worktree_remote_for(&sessions);
    let store = FakeStateStore::new();
    // Hand the dispatcher many more ready beads than the batch cap so the
    // only thing that can stop it is the free-slot count.
    let ready = ready_beads(max_workers * 2);

    let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

    assert_eq!(
        report.success_count(),
        1,
        "with {workers} of {cap} workers already active, exactly one slot must be free \
         and exactly one bead must dispatch per tick (one free slot under the \
         saturating-subtract math)",
        workers = max_workers - 1,
        cap = max_workers,
    );
    // Failures must be empty (all ready beads past the one-slot cap stay
    // un-spawned; the dispatcher silently truncates rather than erroring).
    assert!(
        report.failures.is_empty(),
        "the dispatch capacity gate must not surface bead failures — it must silently \
         leave the over-cap beads for the next tick; failures = {:?}",
        report.failures,
    );
}

#[test]
fn production_max_workers_active_permits_zero_slots() {
    // "40 permits zero" — read as: with `max_workers` workers already
    // active, the saturating-subtract math leaves zero free slots and the
    // dispatcher must spawn nothing.
    let (cfg, _test_ao_project) = with_dispatchable_repos(load_config(&production_toml_path()));
    let max_workers = cfg.max_workers;
    assert!(
        max_workers >= 1,
        "production max_workers must be >= 1 to exercise the saturating-subtract math; got {max_workers}",
    );
    let sessions = sessions_with_active(max_workers);
    script_worktree_remote_for(&sessions);
    let store = FakeStateStore::new();
    let ready = ready_beads(max_workers * 2);

    let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

    assert_eq!(
        report.success_count(),
        0,
        "with {cap} workers already active (production capacity ceiling), zero slots \
         must be free and zero beads must dispatch — saturating-subtract must clamp at 0",
        cap = max_workers,
    );
    assert!(
        report.failures.is_empty(),
        "the dispatch capacity gate must not surface bead failures when zero slots \
         are free; failures = {:?}",
        report.failures,
    );
}

#[test]
fn production_one_tick_dispatches_at_most_max_batch_beads() {
    // "one tick dispatches at most 15" — read as: when the production
    // capacity envelope has more free slots than `max_batch`, the dispatcher
    // still caps per-tick spawns at exactly `max_batch`. This pins the
    // `min(free_slots, max_batch)` ceiling the dispatch safety math
    // enforces, derived from the production TOML.
    let (cfg, _test_ao_project) = with_dispatchable_repos(load_config(&production_toml_path()));
    let max_batch = cfg.max_batch;
    assert!(
        max_batch >= 1,
        "production max_batch must be >= 1 to exercise the per-tick batch cap; got {max_batch}",
    );
    let sessions = sessions_with_active(0);
    script_worktree_remote_for(&sessions);
    let store = FakeStateStore::new();
    // Hand the dispatcher far more ready beads than the cap can absorb, so
    // the only thing that limits the per-tick count is the batch cap.
    let ready = ready_beads(max_batch * 4);

    let report = dispatch_ready(&sessions, &store, &cfg, &ready).unwrap();

    assert!(
        report.success_count() <= max_batch,
        "one tick must dispatch at most {} beads (production max_batch); spawned {}",
        max_batch,
        report.success_count(),
    );
    // When the batch cap is the binding constraint (active_count = 0 so the
    // free-slot count is not the limiter), the dispatcher must hit the cap
    // exactly — anything less means a readiness bug, not a capacity bug.
    assert_eq!(
        report.success_count(),
        max_batch,
        "with zero workers active and many ready beads, the per-tick batch cap must \
         bind at exactly {}; spawned {} (fewer = a readiness regression, not a \
         capacity test failure)",
        max_batch,
        report.success_count(),
    );
}