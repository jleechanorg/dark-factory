use daemon::adapters::CliScm;
use daemon::config::Config;
use daemon::errors::DaemonError;
use daemon::gh_circuit_breaker;
use daemon::intake;
use daemon::tools::{run_tool, Bead, Scm, Tracker};
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static TEST_LOCK: Mutex<()> = Mutex::new(());

struct TestEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    temp_dir: PathBuf,
    orig_path: String,
    state_file: PathBuf,
    telemetry_file: PathBuf,
}

impl TestEnvGuard {
    fn new(prefix: &str) -> Self {
        let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_dir = std::env::temp_dir().join(format!("cb_int_test_{prefix}_{}_{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(temp_dir.join("bin")).unwrap();

        let state_file = temp_dir.join("gh_circuit_breaker.json");
        let telemetry_file = temp_dir.join("daemon.jsonl");

        gh_circuit_breaker::set_state_file_path(Some(state_file.clone()));
        gh_circuit_breaker::set_telemetry_log_path(Some(telemetry_file.clone()));
        gh_circuit_breaker::reset();

        let orig_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", temp_dir.join("bin").display(), orig_path);
        unsafe { std::env::set_var("PATH", &new_path) };

        Self {
            _lock: lock,
            temp_dir,
            orig_path,
            state_file,
            telemetry_file,
        }
    }

    fn write_fake_gh(&self, script: &str) {
        let gh_path = self.temp_dir.join("bin").join("gh");
        std::fs::write(&gh_path, script).unwrap();
        std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn invocation_log(&self) -> PathBuf {
        self.temp_dir.join("gh_invocations.log")
    }

    fn read_invocations(&self) -> Vec<String> {
        let log = self.invocation_log();
        if let Ok(content) = std::fs::read_to_string(&log) {
            content.lines().map(|s| s.to_string()).collect()
        } else {
            Vec::new()
        }
    }

    fn read_telemetry_events(&self) -> Vec<serde_json::Value> {
        if let Ok(content) = std::fs::read_to_string(&self.telemetry_file) {
            content
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        } else {
            Vec::new()
        }
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        unsafe { std::env::set_var("PATH", &self.orig_path) };
        gh_circuit_breaker::reset();
        gh_circuit_breaker::set_state_file_path(None);
        gh_circuit_breaker::set_telemetry_log_path(None);
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

fn test_cfg() -> Config {
    Config {
        target_repo: "jleechanorg/repo-a".into(),
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
        repos: std::collections::HashMap::new(),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

#[test]
fn test_first_403_suppresses_subsequent_gh_calls_across_subsystems() {
    let env = TestEnvGuard::new("subsystems");
    let log_path = env.invocation_log();

    let script = format!(
        r#"#!/usr/bin/env bash
echo "$*" >> "{}"
echo "HTTP 403: API rate limit exceeded for installation ID 99999" >&2
exit 1
"#,
        log_path.display()
    );
    env.write_fake_gh(&script);

    // Subsystem 1: intake / SCM labeled_prs
    let scm = CliScm::new("jleechanorg/repo-a".to_string());
    let mut gh_calls = 0;
    let res1 = scm.labeled_prs("factory", &mut gh_calls);
    assert!(res1.is_err());
    let err1 = res1.unwrap_err();
    assert!(err1.is_gh_rate_limit(), "err1 should be identified as gh rate limit");
    assert!(gh_circuit_breaker::is_rate_limited(), "circuit breaker must be active");

    // Subsystem 2: verifier / SCM pr_snapshot
    let res2 = scm.pr_snapshot(123);
    assert!(res2.is_err());
    let err2 = res2.unwrap_err();
    assert!(err2.is_gh_rate_limit(), "err2 should be identified as gh rate limit");

    // Subsystem 3: raw run_tool for comments
    let res3 = run_tool("gh", &["issue", "comment", "123", "--body", "hello"], 30);
    assert!(res3.is_err());
    let err3 = res3.unwrap_err();
    assert!(err3.is_gh_rate_limit(), "err3 should be identified as gh rate limit");

    // Subsystem 4: raw run_tool for api
    let res4 = run_tool("gh", &["api", "repos/jleechanorg/repo-a/pulls/123"], 30);
    assert!(res4.is_err());

    // CRITICAL: only the FIRST call spawned a real subprocess; all 3 subsequent calls were short-circuited!
    let invocations = env.read_invocations();
    assert_eq!(invocations.len(), 1, "only 1 subprocess should have been spawned, but got: {:?}", invocations);

    // Verify suppressed calls counter (REST fallback in labeled_prs + 3 REST calls in pr_snapshot + comment + api)
    assert_eq!(gh_circuit_breaker::suppressed_call_count(), 5);
    assert_eq!(gh_circuit_breaker::consecutive_trips(), 1);

    // Verify persistence to disk
    assert!(env.state_file.exists());
    let raw_state = std::fs::read_to_string(&env.state_file).unwrap();
    assert!(raw_state.contains("\"consecutive_trips\": 1"));
    assert!(raw_state.contains("\"suppressed_calls\": 5"));

    // Verify telemetry event
    let events = env.read_telemetry_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["eventType"], "GH_CIRCUIT_BREAKER_OPENED");
    assert_eq!(events[0]["metrics"]["cooldown_secs"], 60);
    assert_eq!(events[0]["metrics"]["consecutive_trips"], 1);
}

#[test]
fn test_persistence_across_reconstructed_daemon_state() {
    let env = TestEnvGuard::new("reconstruct");
    let log_path = env.invocation_log();

    let script = format!(
        r#"#!/usr/bin/env bash
echo "$*" >> "{}"
echo "HTTP 403: API rate limit exceeded" >&2
exit 1
"#,
        log_path.display()
    );
    env.write_fake_gh(&script);

    // 1st call trips breaker
    let _ = run_tool("gh", &["pr", "list"], 30);
    assert!(gh_circuit_breaker::is_rate_limited());
    assert_eq!(env.read_invocations().len(), 1);

    // Simulate daemon restart: create a new circuit breaker instance from the state file
    let mut cb_reconstructed = gh_circuit_breaker::GhCircuitBreaker::new();
    cb_reconstructed.state_file_path = Some(env.state_file.clone());
    cb_reconstructed.load_from_disk();

    assert!(cb_reconstructed.deadline.is_some(), "reconstructed deadline must be present");
    assert_eq!(cb_reconstructed.consecutive_trips, 1);

    // Reload the global circuit breaker from disk
    gh_circuit_breaker::reload();
    assert!(gh_circuit_breaker::is_rate_limited());

    // Another call is suppressed without spawning subprocess
    let res = run_tool("gh", &["pr", "list"], 30);
    assert!(res.is_err());
    assert_eq!(env.read_invocations().len(), 1, "no new subprocess should spawn after restart during cooldown");
    assert_eq!(gh_circuit_breaker::suppressed_call_count(), 1);
}

#[test]
fn test_expiry_allows_probe_and_second_403_extends_cooldown() {
    let env = TestEnvGuard::new("expiry_extend");
    let log_path = env.invocation_log();

    let script = format!(
        r#"#!/usr/bin/env bash
echo "$*" >> "{}"
echo "HTTP 403: API rate limit exceeded" >&2
exit 1
"#,
        log_path.display()
    );
    env.write_fake_gh(&script);

    // 1st trip: cooldown = 60s
    let _ = run_tool("gh", &["pr", "list"], 30);
    assert_eq!(gh_circuit_breaker::consecutive_trips(), 1);
    assert_eq!(env.read_invocations().len(), 1);

    // Suppress one call
    let _ = run_tool("gh", &["pr", "view", "1"], 30);
    assert_eq!(env.read_invocations().len(), 1);
    assert_eq!(gh_circuit_breaker::suppressed_call_count(), 1);

    // Fast-forward time: artificially set deadline to 1 second in the past
    gh_circuit_breaker::trip(Duration::from_secs(0), "test_fast_forward");
    std::thread::sleep(Duration::from_millis(50));
    assert!(!gh_circuit_breaker::is_rate_limited(), "should no longer be rate limited after expiry");

    // Exactly one new request is admitted as probe
    let res_probe = run_tool("gh", &["pr", "list"], 30);
    assert!(res_probe.is_err());
    assert_eq!(env.read_invocations().len(), 2, "probe request must have spawned a subprocess");

    // Since probe failed with 403, cooldown is extended with exponential backoff!
    assert!(gh_circuit_breaker::is_rate_limited(), "breaker must be re-tripped on failed probe");
    assert!(gh_circuit_breaker::consecutive_trips() >= 2);

    let events = env.read_telemetry_events();
    let extend_events: Vec<_> = events.iter().filter(|e| e["eventType"] == "GH_CIRCUIT_BREAKER_EXTENDED").collect();
    assert!(!extend_events.is_empty(), "must emit GH_CIRCUIT_BREAKER_EXTENDED event");
}

#[test]
fn test_expiry_and_successful_probe_closes_circuit_breaker() {
    let env = TestEnvGuard::new("expiry_close");
    let log_path = env.invocation_log();

    let script = format!(
        r#"#!/usr/bin/env bash
echo "$*" >> "{}"
if [ "${{GH_TEST_FAIL:-0}}" = "1" ]; then
  echo "HTTP 403: API rate limit exceeded" >&2
  exit 1
fi
echo '{{"status":"ok"}}'
exit 0
"#,
        log_path.display()
    );
    env.write_fake_gh(&script);

    // 1. Trip the breaker
    unsafe { std::env::set_var("GH_TEST_FAIL", "1") };
    let _ = run_tool("gh", &["pr", "list"], 30);
    assert!(gh_circuit_breaker::is_rate_limited());
    assert_eq!(env.read_invocations().len(), 1);

    // 2. Suppress a call
    let _ = run_tool("gh", &["pr", "view"], 30);
    assert_eq!(gh_circuit_breaker::suppressed_call_count(), 1);

    // 3. Make fake gh succeed and advance deadline past now
    unsafe { std::env::set_var("GH_TEST_FAIL", "0") };
    gh_circuit_breaker::trip(Duration::from_secs(0), "test_fast_forward");
    std::thread::sleep(Duration::from_millis(50));

    // 4. Run admitted probe request
    let res_success = run_tool("gh", &["pr", "list"], 30);
    assert!(res_success.is_ok(), "probe request should succeed");
    assert_eq!(env.read_invocations().len(), 2);

    // 5. Breaker is now CLOSED and trips/suppressed reset
    assert!(!gh_circuit_breaker::is_rate_limited());
    assert_eq!(gh_circuit_breaker::consecutive_trips(), 0);
    assert_eq!(gh_circuit_breaker::suppressed_call_count(), 0);

    let events = env.read_telemetry_events();
    let close_events: Vec<_> = events.iter().filter(|e| e["eventType"] == "GH_CIRCUIT_BREAKER_CLOSED").collect();
    assert_eq!(close_events.len(), 1, "must emit GH_CIRCUIT_BREAKER_CLOSED event");
    assert_eq!(close_events[0]["metrics"]["suppressed_calls"], 1);
}

#[test]
fn test_retry_after_header_parsing_sets_exact_cooldown() {
    let env = TestEnvGuard::new("retry_after");
    let log_path = env.invocation_log();

    let script = format!(
        r#"#!/usr/bin/env bash
echo "$*" >> "{}"
echo -e "HTTP 403 Forbidden\nRetry-After: 360\nAPI rate limit exceeded" >&2
exit 1
"#,
        log_path.display()
    );
    env.write_fake_gh(&script);

    let res = run_tool("gh", &["pr", "list"], 30);
    assert!(res.is_err());
    assert!(gh_circuit_breaker::is_rate_limited());

    let deadline = gh_circuit_breaker::current_deadline().unwrap();
    let remaining = deadline.duration_since(SystemTime::now()).unwrap().as_secs();
    // Should be approximately 360 seconds (allow 5s slack)
    assert!(
        (350..=365).contains(&remaining),
        "remaining seconds should be ~360, got {}",
        remaining
    );

    let events = env.read_telemetry_events();
    assert_eq!(events[0]["eventType"], "GH_CIRCUIT_BREAKER_OPENED");
    assert_eq!(events[0]["metrics"]["cooldown_secs"], 360);
    assert_eq!(events[0]["context"]["retry_after_secs"], 360);
}

#[test]
fn test_secondary_rate_limit_please_wait_minutes() {
    let env = TestEnvGuard::new("secondary_wait");
    let log_path = env.invocation_log();

    let script = format!(
        r#"#!/usr/bin/env bash
echo "$*" >> "{}"
echo "HTTP 403: You have exceeded a secondary rate limit. Please wait 10 minutes before you try again." >&2
exit 1
"#,
        log_path.display()
    );
    env.write_fake_gh(&script);

    let res = run_tool("gh", &["pr", "list"], 30);
    assert!(res.is_err());
    assert!(gh_circuit_breaker::is_rate_limited());

    let deadline = gh_circuit_breaker::current_deadline().unwrap();
    let remaining = deadline.duration_since(SystemTime::now()).unwrap().as_secs();
    // 10 minutes = 600s
    assert!(
        (590..=605).contains(&remaining),
        "remaining seconds should be ~600, got {}",
        remaining
    );

    let events = env.read_telemetry_events();
    assert_eq!(events[0]["eventType"], "GH_CIRCUIT_BREAKER_OPENED");
    assert_eq!(events[0]["context"]["is_secondary"], true);
    assert_eq!(events[0]["context"]["reason"], "secondary_rate_limit");
}

#[derive(Default)]
struct DummyTracker;
impl Tracker for DummyTracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
        Ok(Vec::new())
    }
    fn fetch_all_external_refs(&self) -> Result<HashSet<String>, DaemonError> {
        Ok(HashSet::new())
    }
    fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _external_ref: &str,
    ) -> Result<String, DaemonError> {
        Ok("dummy-1".into())
    }
    fn comment_external(&self, _external_ref: &str, _body: &str) -> Result<(), DaemonError> {
        Ok(())
    }
}

#[test]
fn test_intake_stops_sweep_on_first_403_suppressing_fanout() {
    let env = TestEnvGuard::new("intake_fanout");
    let log_path = env.invocation_log();

    let script = format!(
        r#"#!/usr/bin/env bash
echo "$*" >> "{}"
echo "HTTP 403: API rate limit exceeded" >&2
exit 1
"#,
        log_path.display()
    );
    env.write_fake_gh(&script);

    let scm = CliScm::new("jleechanorg/repo1".to_string());
    let tracker = DummyTracker;
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/repo1,jleechanorg/repo2,jleechanorg/repo3,jleechanorg/repo4,jleechanorg/repo5".to_string();

    let mut cache = intake::AdoptionProbeCache::new();
    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &env.telemetry_file,
    )
    .unwrap();

    assert!(outcome.rate_limited, "outcome must be marked rate_limited");
    assert_eq!(outcome.metrics.rate_limited_skips, 1);

    // CRITICAL: Intake probed repo 1, received 403, and STOPPED immediately!
    // It did NOT probe repo2, repo3, repo4, repo5!
    let invocations = env.read_invocations();
    assert_eq!(invocations.len(), 1, "intake must not fan out 403 probes to remaining repositories; invocations: {:?}", invocations);
}
