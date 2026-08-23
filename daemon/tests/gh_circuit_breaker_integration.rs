mod common;

use daemon::adapters::CliScm;
use daemon::config::{Config, RepoConfig};
use daemon::intake::{self, AdoptionProbeCache};
use daemon::tools::Scm;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static TEST_LOCK: Mutex<()> = Mutex::new(());

struct TestEnvGuard {
    fake_bin_dir: PathBuf,
    counter_file: PathBuf,
    telemetry_log: PathBuf,
    #[allow(dead_code)]
    state_dir: PathBuf,
    orig_path: Option<std::ffi::OsString>,
    orig_state_dir: Option<std::ffi::OsString>,
    orig_telemetry: Option<std::ffi::OsString>,
    orig_cb_path: Option<std::ffi::OsString>,
}

impl TestEnvGuard {
    fn setup(prefix: &str) -> Self {
        let unique = format!(
            "{}_{}_{}",
            prefix,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let temp_dir = std::env::temp_dir().join(format!("cb_test_{unique}"));
        let fake_bin_dir = temp_dir.join("bin");
        let state_dir = temp_dir.join("state");
        fs::create_dir_all(&fake_bin_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();

        let counter_file = temp_dir.join("gh_invocations.txt");
        let telemetry_log = temp_dir.join("daemon.jsonl");
        let cb_path = state_dir.join("gh_circuit_breaker.json");

        // Write fake gh script
        let gh_script_path = fake_bin_dir.join("gh");
        let script = format!(
            r#"#!/bin/sh
COUNTER="{}"
echo "$@" >> "$COUNTER"

MODE="${{FAKE_GH_MODE:-rate_limit_first}}"

if [ "$MODE" = "always_403" ]; then
    echo "gh: API rate limit exceeded for installation ID 12345 (HTTP 403)" >&2
    exit 1
elif [ "$MODE" = "secondary_403" ]; then
    echo "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again." >&2
    exit 1
elif [ "$MODE" = "retry_after_90" ]; then
    echo "HTTP 403: rate limit exceeded. Retry-After: 90" >&2
    exit 1
elif [ "$MODE" = "rate_limit_first" ]; then
    LINE_COUNT=$(wc -l < "$COUNTER" 2>/dev/null || echo 0)
    if [ "$LINE_COUNT" -le 1 ]; then
        echo "gh: API rate limit exceeded (HTTP 403)" >&2
        exit 1
    fi
fi

# Default success response
args="$*"
case "$args" in
    *"pr list"*)
        echo '[]'
        exit 0
        ;;
    *"issue list"*)
        echo '[]'
        exit 0
        ;;
    *"pr view"*)
        echo '{{"mergeable":"MERGEABLE","reviews":[],"headRefOid":"0000000000000000000000000000000000000000","body":"","comments":[],"files":[],"updatedAt":"2026-01-01T00:00:00Z"}}'
        exit 0
        ;;
    *"pr checks"*)
        echo '[{{"state":"SUCCESS","bucket":"pass","name":"build"}}]'
        exit 0
        ;;
    *"api graphql"*)
        echo '{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[]}}}}}}}}}}'
        exit 0
        ;;
    *)
        echo '[]'
        exit 0
        ;;
esac
"#,
            counter_file.display()
        );

        fs::write(&gh_script_path, script).unwrap();
        let mut perms = fs::metadata(&gh_script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&gh_script_path, perms).unwrap();

        let orig_path = std::env::var_os("PATH");
        let mut new_path = std::ffi::OsString::from(fake_bin_dir.as_os_str());
        if let Some(ref p) = orig_path {
            new_path.push(":");
            new_path.push(p);
        }

        let orig_state_dir = std::env::var_os("DARK_FACTORY_STATE_DIR");
        let orig_telemetry = std::env::var_os("DARK_FACTORY_TELEMETRY_LOG");
        let orig_cb_path = std::env::var_os("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH");

        unsafe {
            std::env::set_var("PATH", &new_path);
            std::env::set_var("DARK_FACTORY_STATE_DIR", &state_dir);
            std::env::set_var("DARK_FACTORY_TELEMETRY_LOG", &telemetry_log);
            std::env::set_var("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH", &cb_path);
        }

        // Reset in-memory circuit breaker
        daemon::gh_circuit_breaker::reset();

        Self {
            fake_bin_dir,
            counter_file,
            telemetry_log,
            state_dir,
            orig_path,
            orig_state_dir,
            orig_telemetry,
            orig_cb_path,
        }
    }

    fn invocation_count(&self) -> usize {
        if !self.counter_file.exists() {
            return 0;
        }
        let content = fs::read_to_string(&self.counter_file).unwrap_or_default();
        content.lines().filter(|l| !l.trim().is_empty()).count()
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        daemon::gh_circuit_breaker::reset();
        unsafe {
            if let Some(ref p) = self.orig_path {
                std::env::set_var("PATH", p);
            } else {
                std::env::remove_var("PATH");
            }
            if let Some(ref s) = self.orig_state_dir {
                std::env::set_var("DARK_FACTORY_STATE_DIR", s);
            } else {
                std::env::remove_var("DARK_FACTORY_STATE_DIR");
            }
            if let Some(ref t) = self.orig_telemetry {
                std::env::set_var("DARK_FACTORY_TELEMETRY_LOG", t);
            } else {
                std::env::remove_var("DARK_FACTORY_TELEMETRY_LOG");
            }
            if let Some(ref c) = self.orig_cb_path {
                std::env::set_var("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH", c);
            } else {
                std::env::remove_var("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH");
            }
            std::env::remove_var("FAKE_GH_MODE");
        }
        if let Some(parent) = self.fake_bin_dir.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

fn test_cfg_multi_repo() -> Config {
    let mut repos = std::collections::HashMap::new();
    repos.insert(
        "owner/repo1".to_string(),
        RepoConfig {
            ao_project: "proj1".to_string(),
            push_remote: "origin".to_string(),
            local_checkout: None,
        },
    );
    repos.insert(
        "owner/repo2".to_string(),
        RepoConfig {
            ao_project: "proj2".to_string(),
            push_remote: "origin".to_string(),
            local_checkout: None,
        },
    );
    repos.insert(
        "owner/repo3".to_string(),
        RepoConfig {
            ao_project: "proj3".to_string(),
            push_remote: "origin".to_string(),
            local_checkout: None,
        },
    );
    Config {
        target_repo: "owner/repo1".into(),
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

/// RED Acceptance Test 1:
/// A first 403 prevents subsequent fake gh invocations across two subsystems
/// (intake sweep across 3 repos + verification snapshot across 3 PRs),
/// short-circuiting all subsequent calls with 0 subprocesses spawned.
#[test]
fn test_first_403_prevents_subsequent_fake_gh_invocations_across_two_subsystems() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvGuard::setup("red_prevent_fanout");
    unsafe {
        std::env::set_var("FAKE_GH_MODE", "always_403");
    }

    let cfg = test_cfg_multi_repo();
    let scm = CliScm::new("owner/repo1".to_string());
    let tracker = common::FakeTracker::new();
    let mut cache = AdoptionProbeCache::default();
    let now_epoch = 1_700_000_000;

    // Subsystem 1: Intake sweep across 3 configured repositories
    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        now_epoch,
        &env.telemetry_log,
    );
    assert!(outcome.is_ok());
    let res = outcome.unwrap();
    assert!(res.rate_limited, "intake outcome should be flagged as rate limited");

    // Subsystem 2: Verification snapshot calls across PRs
    let snap1 = scm.pr_snapshot_for_repo("owner/repo1", 101);
    let snap2 = scm.pr_snapshot_for_repo("owner/repo2", 102);
    let snap3 = scm.pr_snapshot_for_repo("owner/repo3", 103);

    assert!(snap1.is_err());
    assert!(snap2.is_err());
    assert!(snap3.is_err());

    // Critical assertion: Only EXACTLY ONE fake gh subprocess was spawned!
    // The first call hit 403, and all later calls (repo 2, repo 3, pr 101, 102, 103) were short-circuited.
    let count = env.invocation_count();
    assert_eq!(
        count, 1,
        "Expected exactly 1 gh invocation across all subsystems, got {count}"
    );

    // Verify circuit breaker status reports suppressed calls
    let status = daemon::gh_circuit_breaker::status();
    assert!(status.consecutive_rate_limits >= 1);
    assert!(status.suppressed_calls >= 4, "Expected at least 4 suppressed calls, got {}", status.suppressed_calls);
}

/// RED Acceptance Test 2:
/// The circuit breaker deadline persists across reconstructed daemon state / restarts.
#[test]
fn test_circuit_breaker_persists_through_reconstructed_daemon_state() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvGuard::setup("red_persist");
    unsafe {
        std::env::set_var("FAKE_GH_MODE", "always_403");
    }

    let scm = CliScm::new("owner/repo1".to_string());

    // First call opens the circuit breaker
    let _ = scm.labeled_prs("factory", &mut 0);
    assert_eq!(env.invocation_count(), 1);

    // Simulate daemon restart: reset in-memory state and reload from disk
    daemon::gh_circuit_breaker::reset_in_memory_only();
    daemon::gh_circuit_breaker::load_from_disk();

    // After restart, circuit breaker is STILL active
    let scm2 = CliScm::new("owner/repo2".to_string());
    let err = scm2.pr_snapshot(202).unwrap_err();
    assert!(err.is_gh_rate_limit());

    // No new subprocess was spawned after restart
    assert_eq!(
        env.invocation_count(),
        1,
        "No new subprocess should have been spawned after restart while circuit breaker is open"
    );
}

/// GREEN Acceptance Test 3:
/// Expiry allows exactly one probe request, and a second 403 extends the shared cooldown.
#[test]
fn test_expiry_allows_exactly_one_probe_and_second_403_extends() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvGuard::setup("red_expiry_extend");
    unsafe {
        std::env::set_var("FAKE_GH_MODE", "always_403");
    }

    let scm = CliScm::new("owner/repo1".to_string());

    // Step 1: First 403 opens the circuit breaker
    let _ = scm.labeled_prs("factory", &mut 0);
    assert_eq!(env.invocation_count(), 1);
    let status1 = daemon::gh_circuit_breaker::status();
    assert_eq!(status1.consecutive_rate_limits, 1);

    // Step 2: Force expiry (set deadline in past)
    daemon::gh_circuit_breaker::force_expiry();

    // Step 3: Exactly one probe is allowed through; since FAKE_GH_MODE is always_403, it returns 403
    let res = scm.pr_snapshot(303);
    assert!(res.is_err());
    assert_eq!(env.invocation_count(), 2, "Probe request should have executed");

    // Step 4: Circuit breaker is now EXTENDED with level 2 (longer cooldown)
    let status2 = daemon::gh_circuit_breaker::status();
    assert_eq!(
        status2.consecutive_rate_limits, 2,
        "Consecutive rate limits should increase to 2"
    );

    // Subsequent calls are suppressed again
    let res2 = scm.pr_snapshot(304);
    assert!(res2.is_err());
    assert_eq!(
        env.invocation_count(),
        2,
        "Subsequent call should be short-circuited"
    );
}

/// GREEN Acceptance Test 4:
/// Successful probe after expiry closes the circuit breaker and resumes normal operations.
#[test]
fn test_successful_probe_after_expiry_closes_circuit_breaker() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let env = TestEnvGuard::setup("red_probe_success");
    unsafe {
        std::env::set_var("FAKE_GH_MODE", "rate_limit_first");
    }

    let scm = CliScm::new("owner/repo1".to_string());

    // Call 1 hits 403 (due to rate_limit_first)
    let _ = scm.labeled_prs("factory", &mut 0);
    assert_eq!(env.invocation_count(), 1);

    // Call 2 is suppressed
    let _ = scm.labeled_prs("factory", &mut 0);
    assert_eq!(env.invocation_count(), 1);

    // Force expiry
    daemon::gh_circuit_breaker::force_expiry();

    // Probe call: line count in counter > 1, so fake gh returns 0 (success)
    let res = scm.labeled_prs("factory", &mut 0);
    assert!(res.is_ok(), "Probe request should succeed: {:?}", res);
    assert_eq!(env.invocation_count(), 2, "Probe request was executed");

    // Circuit breaker is now CLOSED!
    let status = daemon::gh_circuit_breaker::status();
    assert_eq!(status.consecutive_rate_limits, 0);

    // Next call succeeds immediately
    let res2 = scm.labeled_prs("factory", &mut 0);
    assert!(res2.is_ok());
    assert_eq!(env.invocation_count(), 3);
}

/// GREEN Acceptance Test 5:
/// Retry-After header in 403 response is parsed and sets cooldown.
#[test]
fn test_retry_after_header_is_parsed_and_respected() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = TestEnvGuard::setup("red_retry_after");
    unsafe {
        std::env::set_var("FAKE_GH_MODE", "retry_after_90");
    }

    let scm = CliScm::new("owner/repo1".to_string());
    let _ = scm.labeled_prs("factory", &mut 0);

    let status = daemon::gh_circuit_breaker::status();
    assert_eq!(status.last_retry_after_secs, Some(90));
}
