use daemon::gh_circuit_breaker::{
    detect_rate_limit, is_gh_rate_limit_text, parse_retry_after, GhCircuitBreaker,
};
use std::fs;
use std::time::Duration;

#[test]
fn test_primary_rate_limit_detection() {
    let stderr = "gh: API rate limit exceeded for installation ID 123456";
    let detection = detect_rate_limit("gh", 1, stderr, "").expect("should detect primary rate limit");
    assert!(!detection.is_secondary);
    assert!(is_gh_rate_limit_text(stderr));
}

#[test]
fn test_secondary_rate_limit_detection() {
    let stderr = "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again. https://docs.github.com/rest/overview/resources-in-the-rest-api#secondary-rate-limits";
    let detection = detect_rate_limit("gh", 1, stderr, "").expect("should detect secondary rate limit");
    assert!(detection.is_secondary);
    assert!(is_gh_rate_limit_text(stderr));
}

#[test]
fn test_retry_after_parsing() {
    let header_stderr = "HTTP 403: rate limit exceeded\nRetry-After: 120\n";
    let dur = parse_retry_after(header_stderr).expect("should parse Retry-After header");
    assert_eq!(dur, Duration::from_secs(120));

    let json_stderr = r#"{"message": "rate limited", "retry_after": 45}"#;
    let dur_json = parse_retry_after(json_stderr).expect("should parse retry_after from json");
    assert_eq!(dur_json, Duration::from_secs(45));

    let prose_stderr = "You have exceeded a rate limit. Please retry after 30 seconds.";
    let dur_prose = parse_retry_after(prose_stderr).expect("should parse prose retry after");
    assert_eq!(dur_prose, Duration::from_secs(30));
}

#[test]
fn test_first_403_opens_breaker_and_suppresses_subsequent_calls() {
    let test_dir = std::env::temp_dir().join(format!("cb_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&test_dir).unwrap();
    let state_file = test_dir.join("gh_circuit_breaker.json");
    let tel_log = test_dir.join("daemon.jsonl");

    let breaker = GhCircuitBreaker::new_with_paths(state_file.clone(), Some(tel_log.clone()));
    assert!(!breaker.is_open());

    // 1st 403 error occurs
    let err_stderr = "HTTP 403: API rate limit exceeded for user";
    breaker.on_error("gh", 1, err_stderr);
    assert!(breaker.is_open());

    // Subsequent N calls to before_call must short circuit and increment suppressed count
    for i in 1..=5 {
        let res = breaker.before_call("gh");
        assert!(res.is_err(), "call {} must be short-circuited", i);
        assert_eq!(breaker.suppressed_calls_during_open(), i);
    }

    assert_eq!(breaker.total_suppressed_calls(), 5);
}

#[test]
fn test_circuit_breaker_persists_across_restart() {
    let test_dir = std::env::temp_dir().join(format!("cb_persist_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&test_dir).unwrap();
    let state_file = test_dir.join("gh_circuit_breaker.json");
    let tel_log = test_dir.join("daemon.jsonl");

    {
        let breaker = GhCircuitBreaker::new_with_paths(state_file.clone(), Some(tel_log.clone()));
        breaker.on_error("gh", 1, "HTTP 403: rate limit exceeded\nRetry-After: 300\n");
        assert!(breaker.is_open());
        assert_eq!(breaker.consecutive_rate_limits(), 1);
        let _ = breaker.before_call("gh");
        let _ = breaker.before_call("gh");
        assert_eq!(breaker.suppressed_calls_during_open(), 2);
    }

    // Simulate daemon restart: create a new GhCircuitBreaker instance pointing to the same state file
    {
        let reloaded_breaker = GhCircuitBreaker::new_with_paths(state_file.clone(), Some(tel_log.clone()));
        assert!(reloaded_breaker.is_open(), "breaker must remain open across restart");
        assert_eq!(reloaded_breaker.consecutive_rate_limits(), 1);
        assert_eq!(reloaded_breaker.suppressed_calls_during_open(), 2);
        assert!(reloaded_breaker.deadline_epoch() > 0);
    }
}

#[test]
fn test_telemetry_transition_events_emitted() {
    let test_dir = std::env::temp_dir().join(format!("cb_tel_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&test_dir).unwrap();
    let state_file = test_dir.join("gh_circuit_breaker.json");
    let tel_log = test_dir.join("daemon.jsonl");

    let breaker = GhCircuitBreaker::new_with_paths(state_file.clone(), Some(tel_log.clone()));
    
    // Trigger Open
    breaker.on_error("gh", 1, "HTTP 403: API rate limit exceeded");
    
    // Suppress 3 calls
    let _ = breaker.before_call("gh");
    let _ = breaker.before_call("gh");
    let _ = breaker.before_call("gh");

    // Close breaker
    breaker.force_close();

    let content = fs::read_to_string(&tel_log).expect("telemetry log should exist");
    let lines: Vec<&str> = content.lines().collect();
    assert!(lines.len() >= 2, "must emit at least OPEN and CLOSED events");

    let open_ev: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(open_ev["eventType"], "GH_RATE_LIMIT_BREAKER_OPEN");
    assert_eq!(open_ev["lifecycleState"], "RATE_LIMIT");
    assert!(open_ev["metrics"]["cooldownSecs"].as_u64().unwrap() >= 60);

    let close_ev: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(close_ev["eventType"], "GH_RATE_LIMIT_BREAKER_CLOSED");
    assert_eq!(close_ev["metrics"]["suppressedCalls"], 3);
}

#[test]
fn test_bounded_exponential_backoff() {
    let test_dir = std::env::temp_dir().join(format!("cb_backoff_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&test_dir).unwrap();
    let state_file = test_dir.join("gh_circuit_breaker.json");

    let breaker = GhCircuitBreaker::new_with_paths(state_file, None);

    // 1st hit -> 60s cooldown
    let dur1 = breaker.compute_next_cooldown(None, 1);
    assert_eq!(dur1, Duration::from_secs(60));

    // 2nd hit -> 120s
    let dur2 = breaker.compute_next_cooldown(None, 2);
    assert_eq!(dur2, Duration::from_secs(120));

    // 3rd hit -> 240s
    let dur3 = breaker.compute_next_cooldown(None, 3);
    assert_eq!(dur3, Duration::from_secs(240));

    // 6th hit -> 1800s (bounded)
    let dur6 = breaker.compute_next_cooldown(None, 6);
    assert_eq!(dur6, Duration::from_secs(1800));

    // 10th hit -> still bounded at 1800s
    let dur10 = breaker.compute_next_cooldown(None, 10);
    assert_eq!(dur10, Duration::from_secs(1800));
}

#[test]
fn test_first_403_prevents_n_subsequent_subprocess_spawns() {
    use std::os::unix::fs::PermissionsExt;

    // Reset circuit breaker
    daemon::gh_circuit_breaker::global().reset();

    let test_dir = std::env::temp_dir().join(format!("fake_gh_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let bin_dir = test_dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let count_file = test_dir.join("gh_spawn_count.txt");
    fs::write(&count_file, "0").unwrap();

    let gh_script = format!(
        r#"#!/bin/sh
cnt=$(cat "{}")
cnt=$((cnt + 1))
echo "$cnt" > "{}"
echo "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again." >&2
exit 1
"#,
        count_file.display(),
        count_file.display()
    );
    let gh_bin = bin_dir.join("gh");
    fs::write(&gh_bin, gh_script).unwrap();
    let mut perms = fs::metadata(&gh_bin).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&gh_bin, perms).unwrap();

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.display(), orig_path);

    unsafe {
        std::env::set_var("PATH", &new_path);
    }

    // First call executes the script, which returns 403 secondary rate limit
    let res1 = daemon::tools::run_tool("gh", &["api", "user"], 5);
    assert!(res1.is_err());
    assert!(daemon::gh_circuit_breaker::global().is_open());

    let count_after_1: u32 = fs::read_to_string(&count_file).unwrap().trim().parse().unwrap();
    assert_eq!(count_after_1, 1, "first call must have spawned gh once");

    // Next 10 calls to run_tool("gh", ...)
    for _ in 0..10 {
        let res = daemon::tools::run_tool("gh", &["api", "user"], 5);
        assert!(res.is_err());
        assert!(res.unwrap_err().is_gh_rate_limit());
    }

    // Verify spawn count in count_file is STILL exactly 1!
    let count_after_10: u32 = fs::read_to_string(&count_file).unwrap().trim().parse().unwrap();
    assert_eq!(count_after_10, 1, "0 subsequent subprocesses should have been spawned after circuit breaker opened");

    // Clean up
    unsafe {
        std::env::set_var("PATH", orig_path);
    }
    daemon::gh_circuit_breaker::global().reset();
}
