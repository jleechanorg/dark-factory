// Integration tests for the centralized GitHub rate-limit circuit breaker and tool boundary.
#[path = "common/mod.rs"]
mod common;

use common::{FakeScm, FakeTracker};
use daemon::config::{Config, RepoConfig};
use daemon::errors::DaemonError;
use daemon::gh_circuit_breaker::{
    clear_gh_circuit_breaker, is_gh_rate_limited, set_circuit_breaker_persist_path,
    set_circuit_breaker_telemetry_path, trip_gh_rate_limit_with_duration,
};
use daemon::intake;
use daemon::tools::run_tool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_cfg_multi_repo() -> Config {
    let mut repos = HashMap::new();
    repos.insert(
        "owner/repo-one".to_string(),
        RepoConfig {
            ao_project: "proj1".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    repos.insert(
        "owner/repo-two".to_string(),
        RepoConfig {
            ao_project: "proj2".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    repos.insert(
        "owner/repo-three".to_string(),
        RepoConfig {
            ao_project: "proj3".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );

    Config {
        target_repo: "owner/repo-one".into(),
        ao_project: None,
        base_branch: "main".into(),
        stage: 1,
        max_workers: 30,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 600,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
        reroll_head_stability_window_secs: 1,
        reroll_death_confirm_secs: 0,
        held_recheck_cooldown_secs: 900,
        repos,
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "afd_cb_integ_{}_{}_{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[test]
fn test_tool_boundary_first_403_prevents_n_subsequent_calls_without_subprocess_spawn() {
    let _guard = TEST_LOCK.lock().unwrap();
    let dir = test_dir("boundary_suppression");
    let cb_path = dir.join("cb.json");
    let tel_path = dir.join("tel.jsonl");

    clear_gh_circuit_breaker();
    set_circuit_breaker_persist_path(Some(cb_path.clone()));
    set_circuit_breaker_telemetry_path(Some(tel_path.clone()));

    assert!(!is_gh_rate_limited(), "circuit breaker initially closed");

    // Manually trip the circuit breaker with 60s cooldown (simulating a first 403 response)
    trip_gh_rate_limit_with_duration(Duration::from_secs(60));
    assert!(is_gh_rate_limited(), "circuit breaker is now open");

    // Attempt 10 gh calls through the shared run_tool boundary
    for i in 1..=10 {
        let err = run_tool("gh", &["pr", "list", "--repo", "owner/repo"], 10)
            .expect_err("gh call must be short-circuited when breaker is open");

        assert!(
            err.is_gh_rate_limit(),
            "short-circuited error must satisfy is_gh_rate_limit()"
        );

        match err {
            DaemonError::Tool { tool, rc, stderr } => {
                assert_eq!(tool, "gh");
                assert_eq!(rc, 403);
                assert!(
                    stderr.contains("rate limit circuit breaker open"),
                    "stderr must explain circuit breaker state"
                );
                assert!(
                    stderr.contains(&format!("suppressed call #{}", i)),
                    "stderr must report suppressed call index"
                );
            }
            other => panic!("expected DaemonError::Tool, got {other:?}"),
        }
    }

    // Verify telemetry event for open transition was written
    let tel_body = std::fs::read_to_string(&tel_path).unwrap();
    assert!(tel_body.contains("GH_CIRCUIT_BREAKER_OPENED"));

    // Cleanup
    clear_gh_circuit_breaker();
    set_circuit_breaker_persist_path(None);
    set_circuit_breaker_telemetry_path(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_intake_sweep_marks_rate_limited_without_fanning_out_probes() {
    let _guard = TEST_LOCK.lock().unwrap();
    let dir = test_dir("intake_fanout");
    let cb_path = dir.join("cb.json");
    let tel_path = dir.join("tel.jsonl");

    clear_gh_circuit_breaker();
    set_circuit_breaker_persist_path(Some(cb_path.clone()));
    set_circuit_breaker_telemetry_path(Some(tel_path.clone()));

    let scm = FakeScm::new();
    let tracker = FakeTracker::new();
    let cfg = test_cfg_multi_repo();

    // Script scm to return a rate limit on the first repo
    *scm.rate_limit_next_labeled_prs.borrow_mut() = true;

    let mut cache = intake::AdoptionProbeCache::new();
    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &tel_path,
    )
    .unwrap();

    assert!(outcome.rate_limited, "sweep must be marked rate_limited");
    assert!(outcome.adopted.is_empty(), "no PRs should be adopted during rate limit");

    // Cleanup
    clear_gh_circuit_breaker();
    set_circuit_breaker_persist_path(None);
    set_circuit_breaker_telemetry_path(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_circuit_breaker_resumes_after_cooldown_expiry() {
    let _guard = TEST_LOCK.lock().unwrap();
    let dir = test_dir("resume");
    let cb_path = dir.join("cb.json");
    let tel_path = dir.join("tel.jsonl");

    clear_gh_circuit_breaker();
    set_circuit_breaker_persist_path(Some(cb_path.clone()));
    set_circuit_breaker_telemetry_path(Some(tel_path.clone()));

    // Trip with a short 1-second cooldown
    trip_gh_rate_limit_with_duration(Duration::from_secs(1));
    assert!(is_gh_rate_limited());

    // 1 call suppressed during the 1s window
    let err = run_tool("gh", &["version"], 10).unwrap_err();
    assert!(err.is_gh_rate_limit());

    // Sleep until deadline expires
    std::thread::sleep(Duration::from_millis(1100));

    assert!(!is_gh_rate_limited(), "circuit breaker must close after deadline expires");

    // Telemetry must record both OPEN and CLOSE
    let tel_body = std::fs::read_to_string(&tel_path).unwrap();
    assert!(tel_body.contains("GH_CIRCUIT_BREAKER_OPENED"));
    assert!(tel_body.contains("GH_CIRCUIT_BREAKER_CLOSED"));

    // Cleanup
    clear_gh_circuit_breaker();
    set_circuit_breaker_persist_path(None);
    set_circuit_breaker_telemetry_path(None);
    let _ = std::fs::remove_dir_all(&dir);
}
