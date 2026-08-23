// Integration test suite for the centralized GitHub rate-limit circuit breaker
// Acceptance criteria:
// 1. Detect both primary and secondary GitHub rate-limit responses, including Retry-After when available.
// 2. Open one shared cooldown on the first such response; all later gh calls from intake, verification, comments, and rerolls must short-circuit without spawning subprocesses until it expires.
// 3. Use bounded exponential backoff if GitHub provides no retry time; persist the deadline across daemon restart.
// 4. Emit one structured transition event per open/extend/close, with suppressed-call count, and never fan out one 403 into per-repository 403 probes.
// 5. Preserve already-queued work and resume it after the deadline.
// 6. Replace duplicated rate-limit branching in intake/adapters/gates with this component.

use daemon::gh_circuit_breaker::{
    detect_gh_rate_limit, GhCircuitBreaker, EVT_GH_CIRCUIT_BREAKER_CLOSED,
    EVT_GH_CIRCUIT_BREAKER_EXTENDED, EVT_GH_CIRCUIT_BREAKER_OPENED,
};
use std::fs;
use std::sync::Mutex;
use std::time::Duration;

static TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_detect_gh_rate_limit_primary_and_secondary_patterns() {
    // 1. Primary rate limit
    let primary_err = "gh: API rate limit exceeded for installation ID 12345";
    let info = detect_gh_rate_limit(primary_err, 1).expect("should detect primary rate limit");
    assert!(info.is_rate_limit);
    assert!(!info.is_secondary);
    assert_eq!(info.retry_after, None);

    // 2. Secondary rate limit with Retry-After header
    let secondary_err = "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.\nRetry-After: 120";
    let info = detect_gh_rate_limit(secondary_err, 1).expect("should detect secondary rate limit");
    assert!(info.is_rate_limit);
    assert!(info.is_secondary);
    assert_eq!(info.retry_after, Some(Duration::from_secs(120)));

    // 3. Secondary rate limit with inline wait seconds
    let inline_err = "HTTP 403: You have exceeded a secondary rate limit. please wait 45 seconds before trying again.";
    let info = detect_gh_rate_limit(inline_err, 1).expect("should detect secondary rate limit with inline wait");
    assert!(info.is_rate_limit);
    assert!(info.is_secondary);
    assert_eq!(info.retry_after, Some(Duration::from_secs(45)));

    // 4. Non-rate-limit error should return None
    let other_err = "gh: Could not resolve to a User with the login of 'unknown_user'.";
    assert!(detect_gh_rate_limit(other_err, 1).is_none());
}

#[test]
fn test_circuit_breaker_first_403_prevents_n_subsequent_fake_gh_invocations_across_two_subsystems() {
    let _guard = TEST_LOCK.lock().unwrap();
    let temp_dir = std::env::temp_dir().join(format!("df_cb_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let state_file = temp_dir.join("gh_circuit_breaker.json");
    let tel_log = temp_dir.join("telemetry.jsonl");

    let cb = GhCircuitBreaker::new_with_paths(state_file.clone(), Some(tel_log.clone()));
    let now = 1_700_000_000;

    // Initially closed, admission granted
    assert!(cb.check_admission(now).is_ok());

    // Subsystem 1 (Intake) receives a 403
    let rate_limit_msg = "gh: API rate limit exceeded for installation ID 999";
    let info = detect_gh_rate_limit(rate_limit_msg, 1).unwrap();
    let cooldown = cb.record_rate_limit(&info, now);
    assert_eq!(cooldown, Duration::from_secs(60)); // default base backoff

    // Verify telemetry event emitted for OPENED
    let tel_content = fs::read_to_string(&tel_log).unwrap();
    assert!(tel_content.contains(EVT_GH_CIRCUIT_BREAKER_OPENED));

    // Subsystem 2 (Verifier / Gates) attempts N requests during the cooldown window
    let n = 10;
    for i in 1..=n {
        let err = cb.check_admission(now + i).unwrap_err();
        assert!(err.is_gh_rate_limit(), "short-circuited error must be recognized as gh rate limit");
    }

    assert_eq!(cb.suppressed_calls(), n as u64);

    // Persists through a reconstructed daemon state
    let cb_reconstructed = GhCircuitBreaker::new_with_paths(state_file, Some(tel_log));
    assert!(cb_reconstructed.is_open(now + 10));
    let err_recon = cb_reconstructed.check_admission(now + 15).unwrap_err();
    assert!(err_recon.is_gh_rate_limit());
    assert_eq!(cb_reconstructed.suppressed_calls(), (n + 1) as u64);

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_circuit_breaker_expiry_allows_one_request_and_second_403_extends_shared_cooldown() {
    let _guard = TEST_LOCK.lock().unwrap();
    let temp_dir = std::env::temp_dir().join(format!("df_cb_expiry_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let state_file = temp_dir.join("gh_circuit_breaker.json");
    let tel_log = temp_dir.join("telemetry.jsonl");

    let cb = GhCircuitBreaker::new_with_paths(state_file.clone(), Some(tel_log.clone()));
    let start = 1_700_000_000;

    // 1. First 403 opens cooldown for 60s (until start + 60)
    let info = detect_gh_rate_limit("gh: API rate limit exceeded", 1).unwrap();
    cb.record_rate_limit(&info, start);
    assert_eq!(cb.deadline_epoch(), start + 60);

    // Suppress 3 calls during cooldown
    assert!(cb.check_admission(start + 10).is_err());
    assert!(cb.check_admission(start + 20).is_err());
    assert!(cb.check_admission(start + 30).is_err());
    assert_eq!(cb.suppressed_calls(), 3);

    // 2. Advance time past deadline (start + 61). Expiry allows exactly one probe request.
    let probe_time = start + 61;
    assert!(cb.check_admission(probe_time).is_ok(), "probe request must be admitted upon expiry");

    // 3. Probe request fails with a second 403 -> Cooldown is EXTENDED with exponential backoff (120s)
    let info2 = detect_gh_rate_limit("HTTP 403: secondary rate limit", 1).unwrap();
    let extended_cooldown = cb.record_rate_limit(&info2, probe_time);
    assert_eq!(extended_cooldown, Duration::from_secs(120));
    assert_eq!(cb.deadline_epoch(), probe_time + 120);

    // Verify EXTENDED telemetry event
    let tel_content = fs::read_to_string(&tel_log).unwrap();
    assert!(tel_content.contains(EVT_GH_CIRCUIT_BREAKER_EXTENDED));

    // Calls during extended cooldown are suppressed
    assert!(cb.check_admission(probe_time + 50).is_err());
    assert_eq!(cb.suppressed_calls(), 4);

    // 4. Advance time past extended deadline (probe_time + 121)
    let probe_time_2 = probe_time + 121;
    assert!(cb.check_admission(probe_time_2).is_ok(), "second probe request admitted");

    // 5. Probe request succeeds -> Circuit breaker is CLOSED, emitting CLOSED event with total suppressed count
    cb.record_success(probe_time_2);
    assert!(!cb.is_open(probe_time_2));
    assert_eq!(cb.suppressed_calls(), 0);
    assert_eq!(cb.consecutive_rate_limits(), 0);

    let tel_content_final = fs::read_to_string(&tel_log).unwrap();
    assert!(tel_content_final.contains(EVT_GH_CIRCUIT_BREAKER_CLOSED));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
#[cfg(unix)]
fn test_run_tool_gh_end_to_end_circuit_breaker_short_circuit() {
    use std::os::unix::fs::PermissionsExt;
    use daemon::gh_circuit_breaker::{set_global_circuit_breaker, clear_gh_circuit_breaker};
    use daemon::tools::run_tool;
    use std::sync::Arc;

    let _guard = TEST_LOCK.lock().unwrap();
    clear_gh_circuit_breaker();

    let temp_dir = std::env::temp_dir().join(format!("df_e2e_cb_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).unwrap();

    let state_file = temp_dir.join("gh_circuit_breaker.json");
    let tel_log = temp_dir.join("telemetry.jsonl");
    let counter_file = temp_dir.join("gh_invocation_count.txt");
    fs::write(&counter_file, "0").unwrap();

    let fake_bin_dir = temp_dir.join("bin");
    fs::create_dir_all(&fake_bin_dir).unwrap();
    let fake_gh_path = fake_bin_dir.join("gh");

    let script = format!(
        r#"#!/bin/sh
count=$(cat "{counter}")
count=$((count + 1))
echo "$count" > "{counter}"

if [ "$count" -eq 1 ]; then
    echo "gh: API rate limit exceeded for installation ID 42" >&2
    exit 1
fi

echo "[]"
exit 0
"#,
        counter = counter_file.display()
    );

    fs::write(&fake_gh_path, script).unwrap();
    let mut perms = fs::metadata(&fake_gh_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&fake_gh_path, perms).unwrap();

    // Set custom global circuit breaker
    let cb = Arc::new(GhCircuitBreaker::new_with_paths(state_file, Some(tel_log)));
    set_global_circuit_breaker(cb.clone());

    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{}", fake_bin_dir.display(), old_path));

    // Call 1: First invocation hits fake gh and returns 403
    let res1 = run_tool("gh", &["pr", "list"], 5);
    assert!(res1.is_err(), "first call must return error");
    let err1 = res1.unwrap_err();
    assert!(err1.is_gh_rate_limit());

    let count1: usize = fs::read_to_string(&counter_file).unwrap().trim().parse().unwrap();
    assert_eq!(count1, 1, "fake gh binary must have been executed exactly once");

    // Call 2..6: Next 5 calls across different subsystems / arguments must short-circuit without executing fake gh
    for _ in 0..5 {
        let res = run_tool("gh", &["pr", "view", "123"], 5);
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.is_gh_rate_limit());
    }

    let count2: usize = fs::read_to_string(&counter_file).unwrap().trim().parse().unwrap();
    assert_eq!(count2, 1, "fake gh invocation count must STILL be 1 (0 subprocesses spawned!)");
    assert_eq!(cb.suppressed_calls(), 5);

    // Reset circuit breaker and verify next invocation succeeds
    clear_gh_circuit_breaker();
    let res_after_clear = run_tool("gh", &["pr", "list"], 5);
    assert!(res_after_clear.is_ok());

    let count3: usize = fs::read_to_string(&counter_file).unwrap().trim().parse().unwrap();
    assert_eq!(count3, 2, "fake gh invocation count must now be 2");

    std::env::set_var("PATH", old_path);
    let _ = fs::remove_dir_all(&temp_dir);
}
