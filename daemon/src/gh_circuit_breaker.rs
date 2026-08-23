use crate::errors::DaemonError;
use crate::telemetry::{self, TelemetryEvent};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_BASE_COOLDOWN_SECS: u64 = 60;
pub const MAX_COOLDOWN_SECS: u64 = 3600;
pub const MIN_COOLDOWN_SECS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CircuitBreakerSnapshot {
    pub state: CircuitState,
    pub until_epoch_secs: Option<u64>,
    pub consecutive_rate_limits: u32,
    pub suppressed_calls: u64,
    pub last_reason: Option<String>,
    pub last_retry_after_secs: Option<u64>,
    pub opened_at_epoch_secs: Option<u64>,
}

impl Default for CircuitBreakerSnapshot {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            until_epoch_secs: None,
            consecutive_rate_limits: 0,
            suppressed_calls: 0,
            last_reason: None,
            last_retry_after_secs: None,
            opened_at_epoch_secs: None,
        }
    }
}

pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn format_epoch_iso8601(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = crate::state::civil_from_days_pub(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

pub fn is_github_rate_limit(rc: i32, stdout: &str, stderr: &str) -> bool {
    let lower_err = stderr.to_ascii_lowercase();
    let lower_out = stdout.to_ascii_lowercase();

    lower_err.contains("api rate limit exceeded")
        || lower_err.contains("rate limit exceeded")
        || lower_err.contains("rate limit hit")
        || lower_err.contains("secondary rate limit")
        || lower_err.contains("you have exceeded a secondary rate limit")
        || lower_err.contains("too many requests")
        || lower_err.contains("rate_limited")
        || lower_err.contains("circuit breaker active")
        || lower_err.contains("circuit breaker probing")
        || lower_err.contains("please wait a few minutes before you try again")
        || lower_err.contains("was submitted too quickly")
        || (lower_err.contains("403") && lower_err.contains("rate limit"))
        || (lower_err.contains("429") && lower_err.contains("rate limit"))
        || lower_out.contains("\"type\":\"rate_limited\"")
        || lower_out.contains("\"type\": \"rate_limited\"")
        || lower_out.contains("secondary rate limit")
        || lower_out.contains("api rate limit exceeded")
        || ((rc == 403 || rc == 429)
            && (lower_err.contains("rate")
                || lower_err.contains("limit")
                || lower_err.contains("secondary")
                || lower_err.contains("exceeded")
                || lower_err.contains("try again")))
}

pub fn extract_retry_after(text: &str) -> Option<Duration> {
    let lower = text.to_ascii_lowercase();
    for pattern in ["retry-after:", "retry-after", "retry after"] {
        if let Some(idx) = lower.find(pattern) {
            let rest = &text[idx + pattern.len()..];
            let digits: String = rest
                .chars()
                .skip_while(|c| c.is_whitespace() || *c == ':' || *c == '=')
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(secs) = digits.parse::<u64>() {
                if secs > 0 {
                    return Some(Duration::from_secs(secs));
                }
            }
        }
    }
    None
}

pub fn calculate_cooldown(
    retry_after: Option<Duration>,
    consecutive_rate_limits: u32,
    base_cooldown_secs: u64,
) -> Duration {
    if let Some(ra) = retry_after {
        let secs = ra.as_secs().clamp(MIN_COOLDOWN_SECS, MAX_COOLDOWN_SECS);
        Duration::from_secs(secs)
    } else {
        let shift = consecutive_rate_limits.saturating_sub(1).min(10);
        let multiplier = 1u64.checked_shl(shift).unwrap_or(1024);
        let secs = base_cooldown_secs
            .saturating_mul(multiplier)
            .clamp(base_cooldown_secs, MAX_COOLDOWN_SECS);
        Duration::from_secs(secs)
    }
}

pub fn circuit_breaker_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH") {
        return PathBuf::from(path);
    }
    crate::intake::runtime_state_dir().join("gh_circuit_breaker.json")
}

struct Inner {
    snapshot: CircuitBreakerSnapshot,
    loaded_from_disk: bool,
}

impl Inner {
    fn new() -> Self {
        Self {
            snapshot: CircuitBreakerSnapshot::default(),
            loaded_from_disk: false,
        }
    }

    fn ensure_loaded(&mut self) {
        if self.loaded_from_disk {
            return;
        }
        self.load_disk();
        self.loaded_from_disk = true;
    }

    fn load_disk(&mut self) {
        let path = circuit_breaker_path();
        if !path.exists() {
            return;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(snap) = serde_json::from_str::<CircuitBreakerSnapshot>(&content) {
                let now = now_epoch_secs();
                if snap.state == CircuitState::Open {
                    if let Some(until) = snap.until_epoch_secs {
                        if now < until {
                            self.snapshot = snap;
                            return;
                        }
                    }
                }
                self.snapshot = snap;
                // If deadline passed while offline, state resets to Closed or probe
                if let Some(until) = self.snapshot.until_epoch_secs {
                    if now >= until && self.snapshot.state == CircuitState::Open {
                        self.snapshot.state = CircuitState::Closed;
                    }
                }
            }
        }
    }

    fn save_disk(&self) {
        let path = circuit_breaker_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.snapshot) {
            let tmp = path.with_extension("tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }
}

static INSTANCE: Mutex<Option<Inner>> = Mutex::new(None);

fn with_inner<F, R>(f: F) -> R
where
    F: FnOnce(&mut Inner) -> R,
{
    let mut guard = INSTANCE.lock().unwrap();
    if guard.is_none() {
        let mut inner = Inner::new();
        inner.ensure_loaded();
        *guard = Some(inner);
    }
    f(guard.as_mut().unwrap())
}

pub fn is_rate_limited() -> bool {
    with_inner(|inner| {
        let now = now_epoch_secs();
        if inner.snapshot.state == CircuitState::Open {
            if let Some(until) = inner.snapshot.until_epoch_secs {
                return now < until;
            }
        }
        false
    })
}

pub fn check_admission() -> Result<(), DaemonError> {
    with_inner(|inner| {
        let now = now_epoch_secs();
        match inner.snapshot.state {
            CircuitState::Closed => Ok(()),
            CircuitState::Open => {
                if let Some(until) = inner.snapshot.until_epoch_secs {
                    if now < until {
                        inner.snapshot.suppressed_calls += 1;
                        inner.save_disk();
                        let until_iso = format_epoch_iso8601(until);
                        return Err(DaemonError::Tool {
                            tool: "gh".to_string(),
                            rc: 403,
                            stderr: format!(
                                "GitHub rate-limit circuit breaker active until {until_iso} ({} call(s) suppressed)",
                                inner.snapshot.suppressed_calls
                            ),
                        });
                    } else {
                        // Cooldown expired! Transition to HalfOpen to admit exactly one probe
                        inner.snapshot.state = CircuitState::HalfOpen;
                        inner.save_disk();
                        Ok(())
                    }
                } else {
                    inner.snapshot.state = CircuitState::Closed;
                    inner.save_disk();
                    Ok(())
                }
            }
            CircuitState::HalfOpen => {
                // A probe is currently in flight. Suppress concurrent calls until the probe resolves.
                inner.snapshot.suppressed_calls += 1;
                inner.save_disk();
                Err(DaemonError::Tool {
                    tool: "gh".to_string(),
                    rc: 403,
                    stderr: format!(
                        "GitHub rate-limit circuit breaker probing (half-open, {} call(s) suppressed)",
                        inner.snapshot.suppressed_calls
                    ),
                })
            }
        }
    })
}

pub fn record_rate_limit(_rc: i32, stderr: &str, custom_telemetry: Option<&Path>) -> u64 {
    with_inner(|inner| {
        let now = now_epoch_secs();
        let retry_after = extract_retry_after(stderr);
        let retry_after_secs = retry_after.map(|d| d.as_secs());

        let (event_type, state_str) = match inner.snapshot.state {
            CircuitState::Closed => {
                inner.snapshot.consecutive_rate_limits = 1;
                inner.snapshot.suppressed_calls = 0;
                inner.snapshot.opened_at_epoch_secs = Some(now);
                ("GH_CIRCUIT_BREAKER_OPENED", "OPEN")
            }
            CircuitState::Open | CircuitState::HalfOpen => {
                inner.snapshot.consecutive_rate_limits += 1;
                ("GH_CIRCUIT_BREAKER_EXTENDED", "EXTENDED")
            }
        };

        inner.snapshot.state = CircuitState::Open;
        inner.snapshot.last_reason = Some(stderr.chars().take(300).collect());
        inner.snapshot.last_retry_after_secs = retry_after_secs;

        let cooldown = calculate_cooldown(
            retry_after,
            inner.snapshot.consecutive_rate_limits,
            DEFAULT_BASE_COOLDOWN_SECS,
        );
        let cooldown_secs = cooldown.as_secs();
        let until = now + cooldown_secs;
        inner.snapshot.until_epoch_secs = Some(until);
        inner.save_disk();

        emit_transition_event(
            event_type,
            state_str,
            cooldown_secs,
            Some(until),
            inner.snapshot.last_reason.as_deref(),
            retry_after_secs,
            inner.snapshot.consecutive_rate_limits,
            inner.snapshot.suppressed_calls,
            custom_telemetry,
        );

        until
    })
}

pub fn record_success(custom_telemetry: Option<&Path>) {
    with_inner(|inner| {
        if inner.snapshot.state == CircuitState::HalfOpen || inner.snapshot.state == CircuitState::Open {
            let final_suppressed = inner.snapshot.suppressed_calls;
            inner.snapshot.state = CircuitState::Closed;
            inner.snapshot.consecutive_rate_limits = 0;
            inner.snapshot.suppressed_calls = 0;
            inner.snapshot.until_epoch_secs = None;
            inner.save_disk();

            emit_transition_event(
                "GH_CIRCUIT_BREAKER_CLOSED",
                "CLOSED",
                0,
                None,
                None,
                None,
                0,
                final_suppressed,
                custom_telemetry,
            );
        } else if inner.snapshot.consecutive_rate_limits > 0 {
            inner.snapshot.consecutive_rate_limits = 0;
            inner.save_disk();
        }
    });
}

pub fn force_cooldown(duration: Duration, reason: &str) {
    with_inner(|inner| {
        let now = now_epoch_secs();
        inner.snapshot.state = CircuitState::Open;
        inner.snapshot.consecutive_rate_limits = 1;
        inner.snapshot.suppressed_calls = 0;
        inner.snapshot.opened_at_epoch_secs = Some(now);
        inner.snapshot.last_reason = Some(reason.to_string());
        inner.snapshot.last_retry_after_secs = Some(duration.as_secs());
        inner.snapshot.until_epoch_secs = Some(now + duration.as_secs());
        inner.save_disk();

        emit_transition_event(
            "GH_CIRCUIT_BREAKER_OPENED",
            "OPEN",
            duration.as_secs(),
            inner.snapshot.until_epoch_secs,
            Some(reason),
            Some(duration.as_secs()),
            1,
            0,
            None,
        );
    });
}

pub fn force_expiry() {
    with_inner(|inner| {
        if inner.snapshot.state == CircuitState::Open {
            inner.snapshot.until_epoch_secs = Some(now_epoch_secs().saturating_sub(1));
            inner.save_disk();
        }
    });
}

pub fn status() -> CircuitBreakerSnapshot {
    with_inner(|inner| inner.snapshot.clone())
}

pub fn reset() {
    let mut guard = INSTANCE.lock().unwrap();
    *guard = Some(Inner {
        snapshot: CircuitBreakerSnapshot::default(),
        loaded_from_disk: true,
    });
    let path = circuit_breaker_path();
    let _ = fs::remove_file(path);
}

pub fn reset_in_memory_only() {
    let mut guard = INSTANCE.lock().unwrap();
    *guard = None;
}

pub fn load_from_disk() {
    with_inner(|inner| {
        inner.load_disk();
    });
}

fn emit_transition_event(
    event_type: &str,
    state_str: &str,
    cooldown_secs: u64,
    until_epoch: Option<u64>,
    reason: Option<&str>,
    retry_after: Option<u64>,
    backoff_level: u32,
    suppressed_calls: u64,
    custom_telemetry_log: Option<&Path>,
) {
    let log_path = match custom_telemetry_log {
        Some(p) => p.to_path_buf(),
        None => telemetry::default_telemetry_log(),
    };
    let until_iso = until_epoch.map(format_epoch_iso8601).unwrap_or_default();
    let event = TelemetryEvent {
        timestamp: crate::state::now_iso8601(),
        bead_id: "gh_circuit_breaker".to_string(),
        attempt_id: 1,
        lifecycle_state: "RATE_LIMIT".to_string(),
        event_type: event_type.to_string(),
        metrics: serde_json::json!({
            "consecutive_rate_limits": backoff_level,
            "suppressed_calls": suppressed_calls,
            "cooldown_secs": cooldown_secs,
        }),
        context: serde_json::json!({
            "state": state_str,
            "until": until_iso,
            "cooldown_secs": cooldown_secs,
            "reason": reason.unwrap_or(""),
            "retry_after_secs": retry_after,
            "backoff_level": backoff_level,
            "suppressed_calls": suppressed_calls,
        }),
    };
    let _ = telemetry::emit(&log_path, &event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static UNIT_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct ScopedCbTest {
        temp_dir: PathBuf,
        telemetry_log: PathBuf,
        cb_path: PathBuf,
        orig_cb_path: Option<std::ffi::OsString>,
        orig_telemetry: Option<std::ffi::OsString>,
    }

    impl ScopedCbTest {
        fn new(name: &str) -> Self {
            let temp_dir = std::env::temp_dir().join(format!(
                "cb_unit_{}_{}_{}",
                name,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::create_dir_all(&temp_dir).unwrap();
            let telemetry_log = temp_dir.join("telemetry.jsonl");
            let cb_path = temp_dir.join("gh_circuit_breaker.json");

            let orig_cb_path = std::env::var_os("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH");
            let orig_telemetry = std::env::var_os("DARK_FACTORY_TELEMETRY_LOG");

            unsafe {
                std::env::set_var("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH", &cb_path);
                std::env::set_var("DARK_FACTORY_TELEMETRY_LOG", &telemetry_log);
            }

            reset();

            Self {
                temp_dir,
                telemetry_log,
                cb_path,
                orig_cb_path,
                orig_telemetry,
            }
        }
    }

    impl Drop for ScopedCbTest {
        fn drop(&mut self) {
            reset();
            unsafe {
                if let Some(ref c) = self.orig_cb_path {
                    std::env::set_var("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH", c);
                } else {
                    std::env::remove_var("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH");
                }
                if let Some(ref t) = self.orig_telemetry {
                    std::env::set_var("DARK_FACTORY_TELEMETRY_LOG", t);
                } else {
                    std::env::remove_var("DARK_FACTORY_TELEMETRY_LOG");
                }
            }
            let _ = fs::remove_dir_all(&self.temp_dir);
        }
    }

    #[test]
    fn test_extract_retry_after_formats() {
        assert_eq!(
            extract_retry_after("HTTP 403: Retry-After: 60"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            extract_retry_after("retry-after: 120"),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            extract_retry_after("Please retry after 45 seconds"),
            Some(Duration::from_secs(45))
        );
        assert_eq!(extract_retry_after("no retry after header here"), None);
    }

    #[test]
    fn test_is_github_rate_limit_varieties() {
        assert!(is_github_rate_limit(
            1,
            "",
            "gh: API rate limit exceeded for installation ID 12345 (HTTP 403)"
        ));
        assert!(is_github_rate_limit(
            1,
            "",
            "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again."
        ));
        assert!(is_github_rate_limit(1, "", "HTTP 429: Too Many Requests"));
        assert!(is_github_rate_limit(
            0,
            "{\"errors\":[{\"type\":\"RATE_LIMITED\",\"message\":\"API rate limit exceeded\"}]}",
            ""
        ));
        assert!(!is_github_rate_limit(
            1,
            "",
            "fatal: repository 'owner/repo' not found"
        ));
        assert!(!is_github_rate_limit(1, "", "gh: Could not resolve to a PullRequest with the number of 999."));
    }

    #[test]
    fn test_calculate_cooldown_exponential() {
        assert_eq!(
            calculate_cooldown(None, 1, 60),
            Duration::from_secs(60)
        );
        assert_eq!(
            calculate_cooldown(None, 2, 60),
            Duration::from_secs(120)
        );
        assert_eq!(
            calculate_cooldown(None, 3, 60),
            Duration::from_secs(240)
        );
        assert_eq!(
            calculate_cooldown(None, 4, 60),
            Duration::from_secs(480)
        );
        // With explicit Retry-After
        assert_eq!(
            calculate_cooldown(Some(Duration::from_secs(90)), 3, 60),
            Duration::from_secs(90)
        );
    }

    #[test]
    fn test_admission_and_state_transitions() {
        let _lock = UNIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = ScopedCbTest::new("transitions");

        // Initial state: Closed, admission Ok
        assert_eq!(status().state, CircuitState::Closed);
        assert!(check_admission().is_ok());

        // Record rate limit: transitions to Open
        let until = record_rate_limit(1, "HTTP 403: secondary rate limit", Some(&env.telemetry_log));
        assert!(until > now_epoch_secs());
        assert_eq!(status().state, CircuitState::Open);
        assert_eq!(status().consecutive_rate_limits, 1);
        assert!(is_rate_limited());

        // Subsequent check_admission: short-circuits with error
        let err1 = check_admission().unwrap_err();
        assert!(err1.is_gh_rate_limit());
        let err2 = check_admission().unwrap_err();
        assert!(err2.is_gh_rate_limit());

        let cur_status = status();
        assert_eq!(cur_status.suppressed_calls, 2);

        // Force expiry -> HalfOpen probe
        force_expiry();
        assert!(check_admission().is_ok(), "Probe request should be admitted");
        assert_eq!(status().state, CircuitState::HalfOpen);

        // Concurrent call during half-open is suppressed
        let err3 = check_admission().unwrap_err();
        assert!(err3.is_gh_rate_limit());

        // Successful probe -> Closed
        record_success(Some(&env.telemetry_log));
        assert_eq!(status().state, CircuitState::Closed);
        assert_eq!(status().consecutive_rate_limits, 0);
        assert!(!is_rate_limited());

        // Verify telemetry output
        let tel_content = fs::read_to_string(&env.telemetry_log).unwrap_or_default();
        assert!(tel_content.contains("GH_CIRCUIT_BREAKER_OPENED"));
        assert!(tel_content.contains("GH_CIRCUIT_BREAKER_CLOSED"));
    }

    #[test]
    fn test_persistence_roundtrip() {
        let _lock = UNIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let env = ScopedCbTest::new("persistence");

        record_rate_limit(1, "HTTP 403: rate limit exceeded", Some(&env.telemetry_log));
        assert!(env.cb_path.exists());

        // Reset in memory to simulate fresh process startup
        reset_in_memory_only();

        // On first access, it auto-loads from disk
        assert_eq!(status().state, CircuitState::Open);
        assert_eq!(status().consecutive_rate_limits, 1);
    }
}

