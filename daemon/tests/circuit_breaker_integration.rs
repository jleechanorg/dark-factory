use daemon::circuit_breaker::{
    clear_gh_circuit_breaker, is_gh_circuit_breaker_open, set_gh_circuit_breaker_paths,
};
use daemon::config::Config;
use daemon::intake::{self, AdoptionProbeCache};
use daemon::tools::run_tool;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static TEST_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    vars: Vec<(String, Option<String>)>,
}

impl EnvVarGuard {
    fn set(kvs: &[(&str, &str)]) -> Self {
        let mut vars = Vec::new();
        for &(k, v) in kvs {
            vars.push((k.to_string(), std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        Self { vars }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (k, prev) in &self.vars {
            match prev {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn create_counting_fake_gh(dir: &Path, count_file: &Path, mode: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let gh_path = dir.join("gh");
    let script = format!(
        r#"#!/usr/bin/env bash
COUNT_FILE="{count_file}"
if [ -f "$COUNT_FILE" ]; then
    COUNT=$(cat "$COUNT_FILE")
    COUNT=$((COUNT + 1))
    echo "$COUNT" > "$COUNT_FILE"
else
    echo "1" > "$COUNT_FILE"
fi

MODE="{mode}"
if [ "$MODE" = "403_secondary" ]; then
    echo "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again." >&2
    exit 1
elif [ "$MODE" = "403_retry_after" ]; then
    echo "HTTP 403: API rate limit exceeded. Retry-After: 60" >&2
    exit 1
elif [ "$MODE" = "success_then_fail" ]; then
    COUNT=$(cat "$COUNT_FILE")
    if [ "$COUNT" -le 1 ]; then
        echo "[]"
        exit 0
    else
        echo "HTTP 403: API rate limit exceeded" >&2
        exit 1
    fi
else
    echo "[]"
    exit 0
fi
"#,
        count_file = count_file.display(),
        mode = mode
    );

    fs::write(&gh_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&gh_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&gh_path, perms).unwrap();
    }
    gh_path
}

fn test_cfg(repos: Vec<&str>) -> Config {
    let mut repo_map = HashMap::new();
    for r in &repos {
        repo_map.insert(
            r.to_string(),
            daemon::config::RepoConfig {
                ao_project: "test".to_string(),
                push_remote: "origin".to_string(),
                local_checkout: None,
            },
        );
    }
    Config {
        target_repo: repos.first().cloned().unwrap_or("owner/repo").to_string(),
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
        repos: repo_map,
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

#[test]
#[cfg(unix)]
fn test_circuit_breaker_first_403_suppresses_subsequent_gh_calls() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let test_dir = std::env::temp_dir().join(format!(
        "afd_cb_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&test_dir).unwrap();

    let count_file = test_dir.join("gh_call_count.txt");
    let state_file = test_dir.join("cb_state.json");
    let tel_log = test_dir.join("telemetry.jsonl");

    let fake_bin_dir = test_dir.join("bin");
    let _fake_gh = create_counting_fake_gh(&fake_bin_dir, &count_file, "403_secondary");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_GH_RATE_LIMIT_BASE_SECS", "60"),
    ]);

    set_gh_circuit_breaker_paths(Some(state_file.clone()), Some(tel_log.clone()));
    clear_gh_circuit_breaker();

    assert!(!is_gh_circuit_breaker_open());

    // 1. First gh call triggers 403
    let res1 = run_tool("gh", &["pr", "list", "--repo", "owner/repo1"], 30);
    assert!(res1.is_err(), "First call should fail with 403");
    let err1 = res1.unwrap_err();
    assert!(err1.is_gh_rate_limit(), "Error should be identified as gh rate limit: {:?}", err1);

    // Assert the fake gh was called once
    let count1: u32 = fs::read_to_string(&count_file).unwrap().trim().parse().unwrap();
    assert_eq!(count1, 1, "Fake gh should have been invoked exactly once");

    // Circuit breaker is now OPEN
    assert!(is_gh_circuit_breaker_open(), "Circuit breaker must be open after 403");

    // 2. Perform 5 subsequent gh calls across different repos and commands
    for i in 2..=6 {
        let res = run_tool("gh", &["pr", "list", "--repo", &format!("owner/repo{i}")], 30);
        assert!(res.is_err(), "Subsequent call {i} should be short-circuited");
        let err = res.unwrap_err();
        assert!(
            err.is_gh_rate_limit(),
            "Short-circuited error {i} must be identified as gh rate limit: {:?}",
            err
        );
    }

    // 3. Proves ZERO subsequent subprocesses were spawned: count file remains 1!
    let count2: u32 = fs::read_to_string(&count_file).unwrap().trim().parse().unwrap();
    assert_eq!(count2, 1, "Fake gh count must REMAIN 1 — all 5 subsequent calls were short-circuited");

    // 4. Check persistence file on disk
    assert!(state_file.is_file(), "State file must exist on disk");
    let state_content = fs::read_to_string(&state_file).unwrap();
    assert!(state_content.contains("OPEN"), "Persisted state must be OPEN");

    // 5. Check telemetry event
    assert!(tel_log.is_file(), "Telemetry log must exist");
    let tel_content = fs::read_to_string(&tel_log).unwrap();
    assert!(
        tel_content.contains("GH_RATE_LIMIT_CIRCUIT_BREAKER_OPEN"),
        "Telemetry must record OPEN transition event: {tel_content}"
    );

    clear_gh_circuit_breaker();
    let _ = fs::remove_dir_all(&test_dir);
}

#[test]
#[cfg(unix)]
fn test_circuit_breaker_multi_repo_intake_sweep_suppresses_fanout() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let test_dir = std::env::temp_dir().join(format!(
        "afd_cb_intake_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&test_dir).unwrap();

    let count_file = test_dir.join("gh_call_count.txt");
    let state_file = test_dir.join("cb_state.json");
    let tel_log = test_dir.join("telemetry.jsonl");

    let fake_bin_dir = test_dir.join("bin");
    let _fake_gh = create_counting_fake_gh(&fake_bin_dir, &count_file, "403_secondary");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);
    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_GH_RATE_LIMIT_BASE_SECS", "60"),
    ]);

    set_gh_circuit_breaker_paths(Some(state_file.clone()), Some(tel_log.clone()));
    clear_gh_circuit_breaker();

    let scm = daemon::adapters::CliScm::new("owner/repo1".to_string());
    let tracker = daemon::adapters::CliTracker;
    let cfg = test_cfg(vec!["owner/repo1", "owner/repo2", "owner/repo3", "owner/repo4"]);
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &tel_log,
    )
    .unwrap();

    assert!(outcome.rate_limited, "Intake outcome must be marked rate_limited");

    // Total gh invocations across the entire 4-repo sweep MUST be exactly 1!
    let count: u32 = fs::read_to_string(&count_file).unwrap().trim().parse().unwrap();
    assert_eq!(
        count, 1,
        "Multi-repo intake sweep must invoke gh subprocess EXACTLY ONCE; remaining 3 repos were short-circuited"
    );

    clear_gh_circuit_breaker();
    let _ = fs::remove_dir_all(&test_dir);
}
