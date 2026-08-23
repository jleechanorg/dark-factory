use daemon::adapters::CliScm;
use daemon::config::Config;
use daemon::errors::DaemonError;
use daemon::gh_circuit_breaker::{
    gh_circuit_breaker_suppressed_count, is_gh_circuit_breaker_open, reset_global_circuit_breaker,
    CircuitState, GhCircuitBreaker, DEFAULT_BASE_COOLDOWN_SECS,
};
use daemon::intake::{self, AdoptionProbeCache};
use daemon::tools::{run_tool, Bead, Tracker};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

static TEST_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    vars: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&str, &str)]) -> Self {
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((k.to_string(), std::env::var(k).ok()));
            std::env::set_var(k, v);
        }
        Self { vars: saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn write_mock_gh(bin_dir: &Path, invocation_log: &Path) {
    let gh_path = bin_dir.join("gh");
    let script = format!(
        r#"#!/bin/sh
# Log this invocation with its arguments
echo "$@" >> "{}"

if [ "$MOCK_GH_MODE" = "403_rate_limit" ]; then
    echo "HTTP 403: API rate limit exceeded for installation ID 12345" >&2
    exit 1
elif [ "$MOCK_GH_MODE" = "403_secondary_retry_after" ]; then
    echo "HTTP 403: You have exceeded a secondary rate limit. Please wait. Retry-After: 90" >&2
    exit 1
elif [ "$MOCK_GH_MODE" = "success" ]; then
    case "$*" in
        *"pr list"*)
            echo '[]'
            exit 0
            ;;
        *"issue list"*)
            echo '[]'
            exit 0
            ;;
        *)
            echo '{{"status":"ok"}}'
            exit 0
            ;;
    esac
else
    echo "[]"
    exit 0
fi
"#,
        invocation_log.display()
    );
    std::fs::write(&gh_path, script).expect("failed to write mock gh");
    let mut perms = std::fs::metadata(&gh_path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&gh_path, perms).expect("set perms");
}

struct StubTracker;
impl Tracker for StubTracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
        Ok(Vec::new())
    }
    fn fetch_all_external_refs(&self) -> Result<HashSet<String>, DaemonError> {
        Ok(HashSet::new())
    }
    fn create_bead(&self, _title: &str, _body: &str, _external_ref: &str) -> Result<String, DaemonError> {
        Ok(String::new())
    }
    fn comment_external(&self, _external_ref: &str, _body: &str) -> Result<(), DaemonError> {
        Ok(())
    }
}

fn test_cfg() -> Config {
    Config {
        target_repo: "jleechanorg/repo1".into(),
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
        repos: HashMap::new(),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

/// Acceptance #1: Prove a first 403 prevents N probes / calls across all daemon consumers.
/// When the first gh call fails with 403 rate limit, subsequent calls to run_tool("gh", ...)
/// MUST short-circuit without executing the subprocess, and increment the suppressed count.
#[test]
fn test_first_403_prevents_n_subprocesses_across_consumers() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let temp_dir = std::env::temp_dir().join(format!(
        "cb_subproc_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let invocation_log = temp_dir.join("gh_invocations.log");
    write_mock_gh(&temp_dir, &invocation_log);

    let state_dir = temp_dir.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", temp_dir.display(), old_path);

    let _env_guard = EnvGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_STATE_DIR", &state_dir.display().to_string()),
        ("MOCK_GH_MODE", "403_rate_limit"),
    ]);

    reset_global_circuit_breaker();

    assert!(!is_gh_circuit_breaker_open());

    // First call to gh: invokes mock gh subprocess, receives 403 rate limit, trips circuit breaker
    let first_result = run_tool("gh", &["pr", "list", "--repo", "owner/repo1"], 10);
    assert!(first_result.is_err());
    let err = first_result.unwrap_err();
    assert!(err.is_gh_rate_limit());

    // Verify mock gh was invoked exactly once
    let invocations_after_first = std::fs::read_to_string(&invocation_log).unwrap_or_default();
    let count_after_first = invocations_after_first.lines().count();
    assert_eq!(count_after_first, 1, "mock gh should have been called exactly once");

    assert!(is_gh_circuit_breaker_open(), "circuit breaker must now be open");

    // Now execute N subsequent gh calls across different consumers/repositories/commands:
    let n = 5;
    for i in 1..=n {
        let res = run_tool("gh", &["api", &format!("repos/owner/repo{i}/pulls")], 10);
        assert!(res.is_err(), "call #{i} must be rejected");
        let err = res.unwrap_err();
        assert!(
            err.is_gh_rate_limit(),
            "short-circuited error must be recognized as rate limit"
        );
        match &err {
            DaemonError::Tool { stderr, .. } => {
                assert!(
                    stderr.contains("circuit breaker open"),
                    "stderr must indicate circuit breaker is open: {stderr}"
                );
            }
            other => panic!("expected DaemonError::Tool, got: {other:?}"),
        }
    }

    // Verify mock gh was STILL invoked ONLY ONCE (no additional subprocesses spawned!)
    let invocations_after_n = std::fs::read_to_string(&invocation_log).unwrap_or_default();
    let count_after_n = invocations_after_n.lines().count();
    assert_eq!(
        count_after_n, 1,
        "mock gh MUST NOT have been called for any of the N subsequent requests"
    );

    assert_eq!(
        gh_circuit_breaker_suppressed_count(),
        n as u64,
        "suppressed count must match number of short-circuited calls"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    reset_global_circuit_breaker();
}

/// Acceptance #2: Cooldown state persists across daemon restarts.
#[test]
fn test_circuit_breaker_persists_across_daemon_restart() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let temp_dir = std::env::temp_dir().join(format!(
        "cb_restart_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let cb_path = temp_dir.join("gh_circuit_breaker.json");
    let tel_path = temp_dir.join("daemon.jsonl");

    let now_epoch = 1_700_000_000;

    // Instance 1: Trip circuit breaker with 403
    {
        let mut cb1 = GhCircuitBreaker::new(cb_path.clone(), Some(tel_path.clone()));
        cb1.record_result(1, "HTTP 403: API rate limit exceeded", "", now_epoch);
        assert!(cb1.is_open(now_epoch));
        assert_eq!(cb1.record().deadline_epoch, now_epoch + DEFAULT_BASE_COOLDOWN_SECS);

        // 2 calls suppressed
        assert!(cb1.check_admission(now_epoch + 5).is_err());
        assert!(cb1.check_admission(now_epoch + 10).is_err());
        assert_eq!(cb1.record().suppressed_calls, 2);
    }

    // Instance 2 (simulating daemon restart): reload from cb_path
    {
        let mut cb2 = GhCircuitBreaker::load_or_default_at(cb_path.clone(), Some(tel_path.clone()));
        assert!(cb2.is_open(now_epoch + 20), "must still be open on restart");
        assert_eq!(
            cb2.record().deadline_epoch,
            now_epoch + DEFAULT_BASE_COOLDOWN_SECS
        );
        assert_eq!(cb2.record().consecutive_triggers, 1);
        assert_eq!(cb2.record().suppressed_calls, 2);

        // Next call is also suppressed
        assert!(cb2.check_admission(now_epoch + 25).is_err());
        assert_eq!(cb2.record().suppressed_calls, 3);
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
}

/// Acceptance #3 & #4: Bounded exponential backoff, Retry-After header, and clean recovery on success.
#[test]
fn test_circuit_breaker_backoff_retry_after_and_recovery() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let temp_dir = std::env::temp_dir().join(format!(
        "cb_backoff_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let cb_path = temp_dir.join("gh_circuit_breaker.json");
    let tel_path = temp_dir.join("daemon.jsonl");

    let mut cb = GhCircuitBreaker::new(cb_path.clone(), Some(tel_path.clone()));

    let mut t = 1_700_000_000;

    // Trigger 1 (no retry-after): base cooldown 60s
    cb.record_result(1, "rate limit exceeded", "", t);
    assert_eq!(cb.record().deadline_epoch, t + 60);
    assert_eq!(cb.record().consecutive_triggers, 1);

    // Advance to deadline
    t += 60;
    assert!(cb.check_admission(t).is_ok(), "probe allowed at deadline");

    // Trigger 2 (no retry-after): exponential backoff 2^1 * 60 = 120s
    cb.record_result(1, "rate limit exceeded", "", t);
    assert_eq!(cb.record().deadline_epoch, t + 120);
    assert_eq!(cb.record().consecutive_triggers, 2);

    // Advance to deadline
    t += 120;
    assert!(cb.check_admission(t).is_ok());

    // Trigger 3 with Retry-After: 300s -> uses 300s
    cb.record_result(
        1,
        "You have exceeded a secondary rate limit. Retry-After: 300",
        "",
        t,
    );
    assert_eq!(cb.record().deadline_epoch, t + 300);

    // Advance past deadline
    t += 300;
    assert!(cb.check_admission(t).is_ok());

    // Probe succeeds!
    cb.record_result(0, "", "[]", t);

    // Breaker is now CLOSED and consecutive_triggers is reset
    assert_eq!(cb.record().state, CircuitState::Closed);
    assert_eq!(cb.record().consecutive_triggers, 0);
    assert_eq!(cb.record().suppressed_calls, 0);

    let _ = std::fs::remove_dir_all(&temp_dir);
}

/// Acceptance #5 & Root Cause verification:
/// Multi-repository intake sweep must NOT fan out 403s.
/// When the first repo hits a 403 rate limit, subsequent repos short-circuit with 0 subprocesses.
#[test]
fn test_intake_multi_repo_sweep_suppresses_403_fanout() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let temp_dir = std::env::temp_dir().join(format!(
        "cb_intake_fanout_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let invocation_log = temp_dir.join("gh_invocations.log");
    write_mock_gh(&temp_dir, &invocation_log);

    let state_dir = temp_dir.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", temp_dir.display(), old_path);

    let _env_guard = EnvGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_STATE_DIR", &state_dir.display().to_string()),
        ("MOCK_GH_MODE", "403_rate_limit"),
    ]);

    reset_global_circuit_breaker();

    let scm = CliScm::new("jleechanorg/repo1".to_string());
    let tracker = StubTracker;

    let mut cfg = test_cfg();
    cfg.repos.insert(
        "jleechanorg/repo2".to_string(),
        daemon::config::RepoConfig {
            ao_project: "repo2".to_string(),
            push_remote: "origin".to_string(),
            local_checkout: None,
        },
    );
    cfg.repos.insert(
        "jleechanorg/repo3".to_string(),
        daemon::config::RepoConfig {
            ao_project: "repo3".to_string(),
            push_remote: "origin".to_string(),
            local_checkout: None,
        },
    );

    let mut cache = AdoptionProbeCache::new();
    let tel_log = temp_dir.join("daemon.jsonl");

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &tel_log,
    )
    .unwrap();

    assert!(outcome.rate_limited, "outcome must report rate_limited=true");
    assert_eq!(outcome.metrics.rate_limited_skips, 3, "all 3 repos recorded as rate limited");

    // The key invariant: subprocess gh was invoked ONLY for repo1 (1 invocation), NOT 3 times!
    let invocations = std::fs::read_to_string(&invocation_log).unwrap_or_default();
    let total_invocations = invocations.lines().count();
    assert_eq!(
        total_invocations, 1,
        "gh subprocess MUST have been spawned only once for the first repo; remaining repos were short-circuited by the circuit breaker"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
    reset_global_circuit_breaker();
}
