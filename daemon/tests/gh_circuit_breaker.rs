use daemon::adapters::CliScm;
use daemon::config::Config;
use daemon::errors::DaemonError;
use daemon::gh_circuit_breaker::{
    self, is_gh_rate_limit_error, parse_retry_after, CircuitBreakerState, GhCircuitBreaker,
};
use daemon::intake::{self, AdoptionProbeCache};
use daemon::tools::{run_tool, Scm, Tracker};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static TEST_MUTEX: Mutex<()> = Mutex::new(());

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((*k, std::env::var(k).ok()));
            unsafe { std::env::set_var(k, v) };
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

struct DummyTracker;
impl Tracker for DummyTracker {
    fn fetch_candidates(&self) -> Result<Vec<daemon::tools::Bead>, DaemonError> {
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
        Ok("bead-test".to_string())
    }
    fn comment_external(&self, _external_ref: &str, _body: &str) -> Result<(), DaemonError> {
        Ok(())
    }
}

fn test_cfg() -> Config {
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
        repos: std::collections::HashMap::new(),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

fn create_fake_gh_script(dir: &Path, log_file: &Path, behavior_file: &Path) -> PathBuf {
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let gh_path = bin_dir.join("gh");

    let script = format!(
        r#"#!/bin/bash
echo "$@" >> "{log_file}"
BEHAVIOR=$(cat "{behavior_file}" 2>/dev/null || echo "ok")
if [ "$BEHAVIOR" = "403_primary" ]; then
    echo "gh: API rate limit exceeded for installation ID 12345" >&2
    exit 1
elif [ "$BEHAVIOR" = "403_secondary" ]; then
    echo "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again." >&2
    exit 1
elif [ "$BEHAVIOR" = "403_retry_after" ]; then
    echo "HTTP 403: Rate limit exceeded. Retry-After: 45" >&2
    exit 1
elif [ "$BEHAVIOR" = "429" ]; then
    echo "HTTP 429: Too Many Requests" >&2
    exit 1
elif [ "$BEHAVIOR" = "empty_prs" ]; then
    echo "[]"
    exit 0
else
    echo '{{"permission":"write","mergeable":"MERGEABLE","reviews":[],"headRefOid":"sha123","body":"test","comments":[],"files":[],"updatedAt":"2026-08-23T00:00:00Z"}}'
    exit 0
fi
"#,
        log_file = log_file.display(),
        behavior_file = behavior_file.display()
    );

    let mut f = File::create(&gh_path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    let mut perms = f.metadata().unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&gh_path, perms).unwrap();

    bin_dir
}

#[test]
fn test_rate_limit_detection_and_retry_after_parsing() {
    assert!(is_gh_rate_limit_error(
        1,
        "gh: API rate limit exceeded for user ID 123",
        ""
    ));
    assert!(is_gh_rate_limit_error(
        1,
        "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
        ""
    ));
    assert!(is_gh_rate_limit_error(
        1,
        "HTTP 403: You have triggered an abuse detection mechanism.",
        ""
    ));
    assert!(is_gh_rate_limit_error(
        1,
        "was submitted too quickly",
        ""
    ));
    assert!(is_gh_rate_limit_error(429, "Too Many Requests", ""));
    assert!(is_gh_rate_limit_error(
        1,
        "github rate limit circuit breaker open until 2026-08-23T12:00:00Z (suppressed 3 calls)",
        ""
    ));
    assert!(!is_gh_rate_limit_error(
        1,
        "git: remote repository not found",
        ""
    ));

    // Retry-After parsing
    assert_eq!(
        parse_retry_after("HTTP 403: rate limit. Retry-After: 60", "", 1000),
        Some(60)
    );
    assert_eq!(
        parse_retry_after("retry-after: 120", "", 1000),
        Some(120)
    );
    assert_eq!(
        parse_retry_after("Please wait 5 minutes before trying again", "", 1000),
        Some(300)
    );
    assert_eq!(
        parse_retry_after("Please wait 30 seconds before trying again", "", 1000),
        Some(30)
    );
    assert_eq!(
        parse_retry_after("x-ratelimit-reset: 1060", "", 1000),
        Some(60)
    );
}

#[test]
fn test_first_403_prevents_subsequent_gh_invocations_across_two_subsystems() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let temp_dir = std::env::temp_dir().join(format!("df_cb_test_subsystems_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let log_file = temp_dir.join("gh_invocations.log");
    let behavior_file = temp_dir.join("gh_behavior");
    fs::write(&behavior_file, "403_secondary").unwrap();

    let bin_dir = create_fake_gh_script(&temp_dir, &log_file, &behavior_file);
    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.display(), original_path);
    let state_dir = temp_dir.join("state");
    fs::create_dir_all(&state_dir).unwrap();
    let tel_log = temp_dir.join("daemon.jsonl");

    let _env = EnvGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_STATE_DIR", state_dir.to_str().unwrap()),
    ]);

    gh_circuit_breaker::reset_global();

    // Subsystem 1: PR Intake sweep over 3 repositories
    let mut cfg = test_cfg();
    cfg.target_repo = "owner/repo1".to_string();
    cfg.repos.insert(
        "owner/repo2".to_string(),
        daemon::config::RepoConfig {
            ao_project: "proj2".to_string(),
            push_remote: "origin".to_string(),
            local_checkout: None,
        },
    );
    cfg.repos.insert(
        "owner/repo3".to_string(),
        daemon::config::RepoConfig {
            ao_project: "proj3".to_string(),
            push_remote: "origin".to_string(),
            local_checkout: None,
        },
    );

    let scm = CliScm::new(cfg.target_repo.clone());
    let tracker = DummyTracker;
    let mut cache = AdoptionProbeCache::new();

    let intake_outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1000,
        &tel_log,
    );
    assert!(intake_outcome.is_ok());
    let outcome = intake_outcome.unwrap();
    assert!(outcome.rate_limited);

    // Subsystem 2: Verification / collaborator permission / pr_snapshot
    let perm_res = scm.collaborator_permission("alice");
    assert!(perm_res.is_err());
    assert!(perm_res.unwrap_err().is_gh_rate_limit());

    let pr_res = scm.pr_snapshot(101);
    assert!(pr_res.is_err());
    assert!(pr_res.unwrap_err().is_gh_rate_limit());

    // Subsystem 3: Direct run_tool
    let direct_res = run_tool("gh", &["api", "user"], 30);
    assert!(direct_res.is_err());
    assert!(direct_res.unwrap_err().is_gh_rate_limit());

    // Check invocation count of the fake `gh` executable
    let log_content = fs::read_to_string(&log_file).unwrap_or_default();
    let lines: Vec<&str> = log_content.lines().collect();

    // Crucial acceptance check: exactly ONE fake gh invocation took place!
    // All other calls across intake (repo 2, repo 3) and subsystem 2/3 were short-circuited!
    assert_eq!(
        lines.len(),
        1,
        "Expected exactly 1 fake gh process spawn, but found {}: {:?}",
        lines.len(),
        lines
    );

    // Verify circuit breaker suppressed calls count
    assert!(gh_circuit_breaker::suppressed_calls() >= 4);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_circuit_breaker_persists_across_reconstructed_daemon_state() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let temp_dir = std::env::temp_dir().join(format!("df_cb_test_persist_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let log_file = temp_dir.join("gh_invocations.log");
    let behavior_file = temp_dir.join("gh_behavior");
    fs::write(&behavior_file, "403_primary").unwrap();

    let bin_dir = create_fake_gh_script(&temp_dir, &log_file, &behavior_file);
    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.display(), original_path);
    let state_dir = temp_dir.join("state");
    fs::create_dir_all(&state_dir).unwrap();

    let _env = EnvGuard::set(&[
        ("PATH", &new_path),
        ("DARK_FACTORY_STATE_DIR", state_dir.to_str().unwrap()),
    ]);

    gh_circuit_breaker::reset_global();

    // Trigger initial 403
    let res = run_tool("gh", &["api", "rate_limit"], 30);
    assert!(res.is_err());
    assert!(res.unwrap_err().is_gh_rate_limit());

    // Reconstruct daemon state by creating a new GhCircuitBreaker from the persistent state file
    let cb_path = state_dir.join("gh_circuit_breaker.json");
    assert!(cb_path.exists(), "State file must exist on disk");

    let reconstructed_cb = GhCircuitBreaker::load_or_default_at(&cb_path);
    assert_eq!(reconstructed_cb.state(), CircuitBreakerState::Open);
    assert!(reconstructed_cb.deadline_epoch() > 0);

    // Set the global breaker to the reconstructed instance
    gh_circuit_breaker::set_global_circuit_breaker(reconstructed_cb);

    // Make more calls - they must still short-circuit without spawning gh
    let res2 = run_tool("gh", &["api", "user"], 30);
    assert!(res2.is_err());
    assert!(res2.unwrap_err().is_gh_rate_limit());

    let log_content = fs::read_to_string(&log_file).unwrap_or_default();
    let lines: Vec<&str> = log_content.lines().collect();
    assert_eq!(lines.len(), 1, "Persisted circuit breaker must short-circuit without spawning gh");

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_expiry_allows_one_request_and_second_403_extends_cooldown() {
    let _lock = TEST_MUTEX.lock().unwrap();
    let temp_dir = std::env::temp_dir().join(format!("df_cb_test_expiry_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let cb_path = temp_dir.join("gh_circuit_breaker.json");
    let tel_log = temp_dir.join("daemon.jsonl");

    let cb = GhCircuitBreaker::new(cb_path.clone(), tel_log.clone());

    let t0 = 1_000_000u64;

    // 1. Initial state is closed
    assert_eq!(cb.state(), CircuitBreakerState::Closed);
    assert!(cb.check_admission(t0).is_ok());

    // 2. First 403 occurs with no Retry-After -> opens with 60s cooldown
    cb.record_result(1, "", "HTTP 403: You have exceeded a secondary rate limit.", t0).unwrap();
    assert_eq!(cb.state(), CircuitBreakerState::Open);
    assert_eq!(cb.deadline_epoch(), t0 + 60);
    assert_eq!(cb.consecutive_rate_limits(), 1);

    // 3. During cooldown -> short-circuit
    assert!(cb.check_admission(t0 + 30).is_err());
    assert_eq!(cb.suppressed_calls(), 1);
    assert!(cb.check_admission(t0 + 50).is_err());
    assert_eq!(cb.suppressed_calls(), 2);

    // 4. Time advances past deadline (t0 + 61) -> exactly ONE request allowed
    assert!(cb.check_admission(t0 + 61).is_ok());

    // 5. That trial request fails with a second 403 -> cooldown is EXTENDED with exponential backoff (120s)
    cb.record_result(1, "", "HTTP 403: You have exceeded a secondary rate limit.", t0 + 61).unwrap();
    assert_eq!(cb.state(), CircuitBreakerState::Open);
    assert_eq!(cb.deadline_epoch(), t0 + 61 + 120);
    assert_eq!(cb.consecutive_rate_limits(), 2);

    // 6. Calls during extended cooldown are short-circuited
    assert!(cb.check_admission(t0 + 100).is_err());
    assert_eq!(cb.suppressed_calls(), 3);

    // 7. Time advances past second deadline (t0 + 182) -> trial request allowed
    assert!(cb.check_admission(t0 + 182).is_ok());

    // 8. Trial request succeeds -> circuit breaker CLOSES
    cb.record_result(0, "{}", "", t0 + 182).unwrap();
    assert_eq!(cb.state(), CircuitBreakerState::Closed);
    assert_eq!(cb.consecutive_rate_limits(), 0);

    // Verify structured telemetry events
    let tel_content = fs::read_to_string(&tel_log).unwrap_or_default();
    let events: Vec<serde_json::Value> = tel_content
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert!(events.iter().any(|e| e["eventType"] == "GH_CIRCUIT_BREAKER_OPENED"));
    assert!(events.iter().any(|e| e["eventType"] == "GH_CIRCUIT_BREAKER_EXTENDED"));
    assert!(events.iter().any(|e| e["eventType"] == "GH_CIRCUIT_BREAKER_CLOSED"));

    let _ = fs::remove_dir_all(&temp_dir);
}
