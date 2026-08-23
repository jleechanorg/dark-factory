// Centralized GitHub rate-limit circuit breaker (Task: fix(factory): centralize GitHub rate-limit circuit breaker)
// Detects primary and secondary GitHub rate-limit responses, opens shared cooldown,
// short-circuits subsequent gh invocations at the tool boundary without spawning subprocesses,
// uses bounded exponential backoff with Retry-After support, persists across restarts,
// and emits structured transition events.

use crate::errors::DaemonError;
use crate::telemetry::{self, TelemetryEvent};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_BASE_COOLDOWN_SECS: u64 = 60;
pub const DEFAULT_MAX_COOLDOWN_SECS: u64 = 1800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CircuitBreakerState {
    Closed,
    Open,
    Extended,
}

impl CircuitBreakerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CircuitBreakerState::Closed => "CLOSED",
            CircuitBreakerState::Open => "OPEN",
            CircuitBreakerState::Extended => "EXTENDED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RateLimitDetails {
    pub is_secondary: bool,
    pub retry_after_secs: Option<u64>,
    pub matched_reason: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedCircuitBreaker {
    pub state: CircuitBreakerState,
    pub deadline_epoch_secs: u64,
    pub backoff_level: u32,
    pub suppressed_calls: u64,
    pub reason: String,
    pub is_secondary: bool,
    pub updated_at_epoch_secs: u64,
}

pub fn parse_gh_rate_limit(stderr: &str, rc: i32) -> Option<RateLimitDetails> {
    let lower = stderr.to_ascii_lowercase();

    let is_primary = lower.contains("api rate limit exceeded")
        || lower.contains("rate limit exceeded")
        || lower.contains("rate limit hit");

    let is_secondary = lower.contains("secondary rate limit")
        || lower.contains("abuse detection mechanism")
        || lower.contains("please wait a few minutes before you try again")
        || lower.contains("please retry your request again later");

    let is_http_429 = rc == 429 || lower.contains("429") || lower.contains("too many requests");

    let is_circuit_breaker_open = lower.contains("rate limit circuit breaker open");

    let is_http_403_rate_limit = (rc == 403 || lower.contains("403"))
        && (lower.contains("rate limit")
            || lower.contains("secondary")
            || lower.contains("abuse")
            || lower.contains("retry"));

    if !is_primary && !is_secondary && !is_http_429 && !is_http_403_rate_limit && !is_circuit_breaker_open {
        return None;
    }

    let retry_after_secs = parse_retry_after(&lower);
    let matched_reason = if is_circuit_breaker_open {
        "circuit_breaker_active".to_string()
    } else if is_secondary {
        "secondary_rate_limit".to_string()
    } else if is_primary {
        "primary_rate_limit".to_string()
    } else if is_http_429 {
        "http_429_too_many_requests".to_string()
    } else {
        "http_403_rate_limit".to_string()
    };

    Some(RateLimitDetails {
        is_secondary,
        retry_after_secs,
        matched_reason,
    })
}

fn parse_retry_after(lower: &str) -> Option<u64> {
    // 1. "retry-after: 120"
    for (idx, _) in lower.match_indices("retry-after") {
        let after = &lower[idx + "retry-after".len()..];
        let after_trimmed = after.trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace());
        let num_str: String = after_trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u64>() {
            if num > 0 {
                return Some(num.clamp(1, 86400));
            }
        }
    }

    // 2. "retry after 60 seconds" or "retry after 60s"
    for (idx, _) in lower.match_indices("retry after") {
        let after = &lower[idx + "retry after".len()..];
        let after_trimmed = after.trim_start();
        let num_str: String = after_trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u64>() {
            if num > 0 {
                return Some(num.clamp(1, 86400));
            }
        }
    }

    // 3. "wait 2 minutes" or "wait 30 seconds" or "wait 30s"
    for (idx, _) in lower.match_indices("wait") {
        let after = &lower[idx + "wait".len()..];
        let after_trimmed = after.trim_start();
        let num_str: String = after_trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u64>() {
            let remainder = after_trimmed[num_str.len()..].trim_start();
            if remainder.starts_with("minute") || remainder.starts_with("min") || remainder == "m" {
                return Some((num * 60).clamp(1, 86400));
            } else if num > 0 {
                return Some(num.clamp(1, 86400));
            }
        }
    }

    // 4. "in 30s" or "in 5 minutes" or "in 15 seconds"
    for (idx, _) in lower.match_indices("in ") {
        // Ensure word boundary before "in " (start of string or preceded by whitespace/punctuation)
        if idx > 0 {
            let prev_char = lower.as_bytes()[idx - 1] as char;
            if !prev_char.is_whitespace() && prev_char != '(' && prev_char != '[' && prev_char != ':' {
                continue;
            }
        }
        let after = &lower[idx + "in ".len()..];
        let after_trimmed = after.trim_start();
        let num_str: String = after_trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u64>() {
            let remainder = after_trimmed[num_str.len()..].trim_start();
            if remainder.starts_with("minute") || remainder.starts_with("min") || remainder == "m" {
                return Some((num * 60).clamp(1, 86400));
            } else if remainder.starts_with("second") || remainder.starts_with("sec") || remainder.starts_with('s') || remainder.is_empty() {
                return Some(num.clamp(1, 86400));
            }
        }
    }

    None
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn base_cooldown_secs() -> u64 {
    if let Ok(val) = std::env::var("DARK_FACTORY_GH_RATE_LIMIT_BASE_SECS") {
        if let Ok(num) = val.parse::<u64>() {
            return num.max(1);
        }
    }
    DEFAULT_BASE_COOLDOWN_SECS
}

fn max_cooldown_secs() -> u64 {
    if let Ok(val) = std::env::var("DARK_FACTORY_GH_RATE_LIMIT_MAX_SECS") {
        if let Ok(num) = val.parse::<u64>() {
            return num.max(1);
        }
    }
    DEFAULT_MAX_COOLDOWN_SECS
}

#[derive(Debug)]
pub struct GhCircuitBreaker {
    pub state: CircuitBreakerState,
    pub deadline: Option<Instant>,
    pub deadline_epoch_secs: u64,
    pub backoff_level: u32,
    pub suppressed_calls: u64,
    pub reason: String,
    pub is_secondary: bool,
    pub custom_persist_path: Option<PathBuf>,
    pub custom_telemetry_path: Option<PathBuf>,
    pub loaded_from_disk: bool,
}

impl Default for GhCircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

impl GhCircuitBreaker {
    pub fn new() -> Self {
        Self {
            state: CircuitBreakerState::Closed,
            deadline: None,
            deadline_epoch_secs: 0,
            backoff_level: 0,
            suppressed_calls: 0,
            reason: String::new(),
            is_secondary: false,
            custom_persist_path: None,
            custom_telemetry_path: None,
            loaded_from_disk: false,
        }
    }

    pub fn persist_path(&self) -> PathBuf {
        if let Some(p) = &self.custom_persist_path {
            return p.clone();
        }
        if let Some(p) = std::env::var_os("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH") {
            return PathBuf::from(p);
        }
        crate::intake::runtime_state_dir().join("gh_circuit_breaker.json")
    }

    pub fn telemetry_log_path(&self) -> PathBuf {
        if let Some(p) = &self.custom_telemetry_path {
            return p.clone();
        }
        if let Some(p) = std::env::var_os("DARK_FACTORY_TELEMETRY_LOG") {
            return PathBuf::from(p);
        }
        if let Some(home) = std::env::var_os("HOME") {
            Path::new(&home)
                .join("Library/Logs/dark-factory")
                .join("daemon.jsonl")
        } else {
            PathBuf::from("daemon.jsonl")
        }
    }

    pub fn ensure_loaded(&mut self) {
        if self.loaded_from_disk {
            return;
        }
        self.loaded_from_disk = true;
        let path = self.persist_path();
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(persisted) = serde_json::from_str::<PersistedCircuitBreaker>(&raw) {
                let now = now_epoch_secs();
                if persisted.deadline_epoch_secs > now {
                    let remaining_secs = persisted.deadline_epoch_secs - now;
                    self.state = persisted.state;
                    self.deadline = Some(Instant::now() + Duration::from_secs(remaining_secs));
                    self.deadline_epoch_secs = persisted.deadline_epoch_secs;
                    self.backoff_level = persisted.backoff_level;
                    self.suppressed_calls = persisted.suppressed_calls;
                    self.reason = persisted.reason;
                    self.is_secondary = persisted.is_secondary;
                    return;
                }
            }
        }
    }

    pub fn persist(&self) {
        let path = self.persist_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let record = PersistedCircuitBreaker {
            state: self.state,
            deadline_epoch_secs: self.deadline_epoch_secs,
            backoff_level: self.backoff_level,
            suppressed_calls: self.suppressed_calls,
            reason: self.reason.clone(),
            is_secondary: self.is_secondary,
            updated_at_epoch_secs: now_epoch_secs(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&record) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn emit_transition_event(
        &self,
        event_type: &str,
        transition: &str,
        cooldown_secs: u64,
        suppressed_calls: u64,
        backoff_level: u32,
    ) {
        let log_path = self.telemetry_log_path();
        let ev = TelemetryEvent {
            timestamp: format_now_rfc3339(),
            bead_id: "system".into(),
            attempt_id: 0,
            lifecycle_state: self.state.as_str().to_string(),
            event_type: event_type.to_string(),
            metrics: serde_json::json!({
                "cooldownSecs": cooldown_secs,
                "backoffLevel": backoff_level,
                "deadlineEpoch": self.deadline_epoch_secs,
                "suppressedCalls": suppressed_calls,
            }),
            context: serde_json::json!({
                "transition": transition,
                "reason": self.reason,
                "isSecondary": self.is_secondary,
            }),
        };
        let _ = telemetry::emit(&log_path, &ev);
    }

    pub fn check_admission(&mut self) -> Result<(), DaemonError> {
        self.ensure_loaded();
        match self.state {
            CircuitBreakerState::Closed => Ok(()),
            CircuitBreakerState::Open | CircuitBreakerState::Extended => {
                let now_epoch = now_epoch_secs();
                let is_expired = if let Some(d) = self.deadline {
                    Instant::now() >= d || now_epoch >= self.deadline_epoch_secs
                } else {
                    now_epoch >= self.deadline_epoch_secs
                };

                if is_expired {
                    let total_suppressed = self.suppressed_calls;
                    let final_backoff = self.backoff_level;
                    self.state = CircuitBreakerState::Closed;
                    self.deadline = None;
                    self.deadline_epoch_secs = 0;
                    self.suppressed_calls = 0;
                    self.backoff_level = 0;
                    self.reason = String::new();
                    self.is_secondary = false;

                    self.emit_transition_event(
                        "GH_CIRCUIT_BREAKER_CLOSED",
                        "close",
                        0,
                        total_suppressed,
                        final_backoff,
                    );
                    self.persist();
                    Ok(())
                } else {
                    self.suppressed_calls += 1;
                    self.persist();
                    let remaining = self.deadline_epoch_secs.saturating_sub(now_epoch);
                    Err(DaemonError::Tool {
                        tool: "gh".to_string(),
                        rc: 403,
                        stderr: format!(
                            "gh: rate limit circuit breaker open until epoch {} ({}s remaining, suppressed call #{})",
                            self.deadline_epoch_secs, remaining, self.suppressed_calls
                        ),
                    })
                }
            }
        }
    }

    pub fn record_rate_limit(&mut self, stderr: &str, rc: i32) {
        self.ensure_loaded();
        if let Some(details) = parse_gh_rate_limit(stderr, rc) {
            let now_epoch = now_epoch_secs();
            let base_secs = base_cooldown_secs();
            let max_secs = max_cooldown_secs();

            let cooldown_secs = if let Some(retry_after) = details.retry_after_secs {
                retry_after.clamp(1, max_secs)
            } else {
                let mult = 1u64 << self.backoff_level.min(10);
                (base_secs.saturating_mul(mult)).clamp(base_secs, max_secs)
            };

            self.backoff_level = self.backoff_level.saturating_add(1);
            self.deadline_epoch_secs = now_epoch + cooldown_secs;
            self.deadline = Some(Instant::now() + Duration::from_secs(cooldown_secs));
            self.reason = details.matched_reason;
            self.is_secondary = details.is_secondary;

            let transition = match self.state {
                CircuitBreakerState::Closed => {
                    self.state = CircuitBreakerState::Open;
                    self.suppressed_calls = 0;
                    "open"
                }
                CircuitBreakerState::Open | CircuitBreakerState::Extended => {
                    self.state = CircuitBreakerState::Extended;
                    "extend"
                }
            };

            let event_type = if transition == "open" {
                "GH_CIRCUIT_BREAKER_OPENED"
            } else {
                "GH_CIRCUIT_BREAKER_EXTENDED"
            };

            self.emit_transition_event(
                event_type,
                transition,
                cooldown_secs,
                self.suppressed_calls,
                self.backoff_level,
            );
            self.persist();
        }
    }

    pub fn trip_with_duration(&mut self, duration: Duration, reason: &str) {
        self.ensure_loaded();
        let now_epoch = now_epoch_secs();
        let cooldown_secs = duration.as_secs().max(1);

        self.backoff_level = self.backoff_level.saturating_add(1);
        self.deadline_epoch_secs = now_epoch + cooldown_secs;
        self.deadline = Some(Instant::now() + duration);
        self.reason = reason.to_string();
        self.is_secondary = false;

        let transition = match self.state {
            CircuitBreakerState::Closed => {
                self.state = CircuitBreakerState::Open;
                self.suppressed_calls = 0;
                "open"
            }
            CircuitBreakerState::Open | CircuitBreakerState::Extended => {
                self.state = CircuitBreakerState::Extended;
                "extend"
            }
        };

        let event_type = if transition == "open" {
            "GH_CIRCUIT_BREAKER_OPENED"
        } else {
            "GH_CIRCUIT_BREAKER_EXTENDED"
        };

        self.emit_transition_event(
            event_type,
            transition,
            cooldown_secs,
            self.suppressed_calls,
            self.backoff_level,
        );
        self.persist();
    }

    pub fn record_success(&mut self) {
        self.ensure_loaded();
        if self.state == CircuitBreakerState::Closed && self.backoff_level > 0 {
            self.backoff_level = 0;
            self.persist();
        }
    }

    pub fn clear(&mut self) {
        self.state = CircuitBreakerState::Closed;
        self.deadline = None;
        self.deadline_epoch_secs = 0;
        self.backoff_level = 0;
        self.suppressed_calls = 0;
        self.reason = String::new();
        self.is_secondary = false;
        self.persist();
    }

    pub fn is_open(&mut self) -> bool {
        self.ensure_loaded();
        match self.state {
            CircuitBreakerState::Closed => false,
            CircuitBreakerState::Open | CircuitBreakerState::Extended => {
                let now_epoch = now_epoch_secs();
                if now_epoch >= self.deadline_epoch_secs {
                    let _ = self.check_admission();
                    false
                } else {
                    true
                }
            }
        }
    }
}

fn format_now_rfc3339() -> String {
    let now = SystemTime::now();
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs();
    // Format timestamp representation
    let days = secs / 86400;
    let rem_secs = secs % 86400;
    let hours = rem_secs / 3600;
    let mins = (rem_secs % 3600) / 60;
    let s = rem_secs % 60;

    // Approximate date from days since Unix epoch
    let mut year = 1970;
    let mut days_left = days;
    loop {
        let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if is_leap { 366 } else { 365 };
        if days_left < days_in_year {
            break;
        }
        days_left -= days_in_year;
        year += 1;
    }
    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let month_days = [
        31,
        if is_leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for &md in &month_days {
        if days_left < md {
            break;
        }
        days_left -= md;
        month += 1;
    }
    let day = days_left + 1;

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{mins:02}:{s:02}Z")
}

static GLOBAL_CIRCUIT_BREAKER: Mutex<Option<GhCircuitBreaker>> = Mutex::new(None);

pub fn with_global_circuit_breaker<R>(f: impl FnOnce(&mut GhCircuitBreaker) -> R) -> R {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap();
    if lock.is_none() {
        *lock = Some(GhCircuitBreaker::new());
    }
    f(lock.as_mut().unwrap())
}

pub fn check_gh_admission() -> Result<(), DaemonError> {
    with_global_circuit_breaker(|cb| cb.check_admission())
}

pub fn record_gh_success() {
    with_global_circuit_breaker(|cb| cb.record_success())
}

pub fn record_gh_failure(stderr: &str, rc: i32) {
    with_global_circuit_breaker(|cb| cb.record_rate_limit(stderr, rc))
}

pub fn is_gh_rate_limited() -> bool {
    with_global_circuit_breaker(|cb| cb.is_open())
}

pub fn trip_gh_rate_limit_with_duration(duration: Duration) {
    with_global_circuit_breaker(|cb| cb.trip_with_duration(duration, "manual_trip"))
}

pub fn clear_gh_circuit_breaker() {
    with_global_circuit_breaker(|cb| cb.clear())
}

pub fn set_circuit_breaker_persist_path(path: Option<PathBuf>) {
    with_global_circuit_breaker(|cb| {
        cb.custom_persist_path = path;
        cb.loaded_from_disk = false;
    })
}

pub fn set_circuit_breaker_telemetry_path(path: Option<PathBuf>) {
    with_global_circuit_breaker(|cb| cb.custom_telemetry_path = path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_primary_rate_limit_detection() {
        let stderr1 = "gh: API rate limit exceeded for installation ID 12345";
        let res1 = parse_gh_rate_limit(stderr1, 1).expect("must detect primary rate limit");
        assert!(!res1.is_secondary);
        assert_eq!(res1.matched_reason, "primary_rate_limit");

        let stderr2 = "HTTP 403: Rate limit exceeded (https://api.github.com/graphql)";
        let res2 = parse_gh_rate_limit(stderr2, 1).expect("must detect primary rate limit 403");
        assert_eq!(res2.matched_reason, "primary_rate_limit");

        let stderr3 = "error: rate limit hit for query";
        let res3 = parse_gh_rate_limit(stderr3, 1).expect("must detect rate limit hit");
        assert_eq!(res3.matched_reason, "primary_rate_limit");
    }

    #[test]
    fn parse_secondary_rate_limit_detection() {
        let stderr1 = "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.";
        let res1 = parse_gh_rate_limit(stderr1, 1).expect("must detect secondary rate limit");
        assert!(res1.is_secondary);
        assert_eq!(res1.matched_reason, "secondary_rate_limit");

        let stderr2 = "You have triggered an abuse detection mechanism and have been temporarily blocked from content creation. Please retry your request again later.";
        let res2 = parse_gh_rate_limit(stderr2, 1).expect("must detect abuse detection rate limit");
        assert!(res2.is_secondary);
        assert_eq!(res2.matched_reason, "secondary_rate_limit");
    }

    #[test]
    fn parse_http_429_detection() {
        let stderr = "HTTP 429 Too Many Requests";
        let res = parse_gh_rate_limit(stderr, 429).expect("must detect 429");
        assert_eq!(res.matched_reason, "http_429_too_many_requests");
    }

    #[test]
    fn parse_unrelated_error_returns_none() {
        let stderr = "error: repository not found";
        assert!(parse_gh_rate_limit(stderr, 1).is_none());

        let stderr2 = "fatal: not a git repository";
        assert!(parse_gh_rate_limit(stderr2, 128).is_none());
    }

    #[test]
    fn parse_retry_after_formats() {
        assert_eq!(parse_retry_after("retry-after: 120"), Some(120));
        assert_eq!(parse_retry_after("retry-after: 45"), Some(45));
        assert_eq!(parse_retry_after("please retry after 30 seconds"), Some(30));
        assert_eq!(parse_retry_after("please wait 5 minutes before retrying"), Some(300));
        assert_eq!(parse_retry_after("try again in 15s"), Some(15));
        assert_eq!(parse_retry_after("try again in 2 minutes"), Some(120));
        assert_eq!(parse_retry_after("no retry info here"), None);
    }

    #[test]
    fn circuit_breaker_opens_on_rate_limit_and_short_circuits() {
        let dir = std::env::temp_dir().join(format!("afd_cb_test_{}_{}", std::process::id(), now_epoch_secs()));
        let _ = std::fs::create_dir_all(&dir);
        let persist_path = dir.join("cb.json");
        let telemetry_path = dir.join("telemetry.jsonl");

        let mut cb = GhCircuitBreaker::new();
        cb.custom_persist_path = Some(persist_path.clone());
        cb.custom_telemetry_path = Some(telemetry_path.clone());

        assert_eq!(cb.check_admission().is_ok(), true, "initially closed and admission allowed");

        // Record a secondary rate limit with 60s cooldown
        let stderr = "HTTP 403: You have exceeded a secondary rate limit. Please retry after 60 seconds.";
        cb.record_rate_limit(stderr, 1);

        assert_eq!(cb.state, CircuitBreakerState::Open);
        assert!(cb.is_open());

        // Subsequent N calls MUST short-circuit without spawning subprocesses
        for i in 1..=5 {
            let res = cb.check_admission();
            assert!(res.is_err(), "call {} must short-circuit", i);
            let err = res.unwrap_err();
            assert!(err.is_gh_rate_limit(), "error must be recognized as rate limit");
            match err {
                DaemonError::Tool { rc, stderr, tool } => {
                    assert_eq!(tool, "gh");
                    assert_eq!(rc, 403);
                    assert!(stderr.contains("rate limit circuit breaker open"));
                    assert!(stderr.contains(&format!("suppressed call #{}", i)));
                }
                other => panic!("expected Tool error, got {other:?}"),
            }
        }
        assert_eq!(cb.suppressed_calls, 5);

        // Verify telemetry was written
        let tel_body = std::fs::read_to_string(&telemetry_path).unwrap();
        assert!(tel_body.contains("GH_CIRCUIT_BREAKER_OPENED"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn circuit_breaker_bounded_exponential_backoff_without_retry_after() {
        let dir = std::env::temp_dir().join(format!("afd_cb_backoff_test_{}_{}", std::process::id(), now_epoch_secs()));
        let _ = std::fs::create_dir_all(&dir);
        let persist_path = dir.join("cb.json");
        let telemetry_path = dir.join("telemetry.jsonl");

        let mut cb = GhCircuitBreaker::new();
        cb.custom_persist_path = Some(persist_path.clone());
        cb.custom_telemetry_path = Some(telemetry_path.clone());

        let stderr = "gh: API rate limit exceeded";

        // First hit: backoff level 0 -> 60s
        let t0 = now_epoch_secs();
        cb.record_rate_limit(stderr, 1);
        assert_eq!(cb.state, CircuitBreakerState::Open);
        assert_eq!(cb.backoff_level, 1);
        assert!(cb.deadline_epoch_secs >= t0 + 60 && cb.deadline_epoch_secs <= t0 + 62);

        // Second hit: backoff level 1 -> 120s
        cb.record_rate_limit(stderr, 1);
        assert_eq!(cb.state, CircuitBreakerState::Extended);
        assert_eq!(cb.backoff_level, 2);
        assert!(cb.deadline_epoch_secs >= t0 + 120 && cb.deadline_epoch_secs <= t0 + 122);

        // Third hit: backoff level 2 -> 240s
        cb.record_rate_limit(stderr, 1);
        assert_eq!(cb.state, CircuitBreakerState::Extended);
        assert_eq!(cb.backoff_level, 3);
        assert!(cb.deadline_epoch_secs >= t0 + 240 && cb.deadline_epoch_secs <= t0 + 242);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn circuit_breaker_persists_across_restart() {
        let dir = std::env::temp_dir().join(format!("afd_cb_persist_test_{}_{}", std::process::id(), now_epoch_secs()));
        let _ = std::fs::create_dir_all(&dir);
        let persist_path = dir.join("cb.json");
        let telemetry_path = dir.join("telemetry.jsonl");

        {
            let mut cb = GhCircuitBreaker::new();
            cb.custom_persist_path = Some(persist_path.clone());
            cb.custom_telemetry_path = Some(telemetry_path.clone());

            let stderr = "HTTP 403: You have exceeded a secondary rate limit. Please retry after 100 seconds.";
            cb.record_rate_limit(stderr, 1);
            let _ = cb.check_admission(); // suppressed call 1
            let _ = cb.check_admission(); // suppressed call 2
            assert_eq!(cb.suppressed_calls, 2);
        }

        // Simulate new process / restart by creating a new GhCircuitBreaker
        {
            let mut restarted = GhCircuitBreaker::new();
            restarted.custom_persist_path = Some(persist_path.clone());
            restarted.custom_telemetry_path = Some(telemetry_path.clone());
            restarted.ensure_loaded();

            assert_eq!(restarted.state, CircuitBreakerState::Open);
            assert_eq!(restarted.suppressed_calls, 2);
            assert!(restarted.deadline_epoch_secs > now_epoch_secs());

            // Admission check still short-circuits
            let res = restarted.check_admission();
            assert!(res.is_err());
            assert_eq!(restarted.suppressed_calls, 3);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn circuit_breaker_closes_and_emits_telemetry_when_deadline_expires() {
        let dir = std::env::temp_dir().join(format!("afd_cb_close_test_{}_{}", std::process::id(), now_epoch_secs()));
        let _ = std::fs::create_dir_all(&dir);
        let persist_path = dir.join("cb.json");
        let telemetry_path = dir.join("telemetry.jsonl");

        let mut cb = GhCircuitBreaker::new();
        cb.custom_persist_path = Some(persist_path.clone());
        cb.custom_telemetry_path = Some(telemetry_path.clone());

        // Trip with 1 second cooldown
        cb.trip_with_duration(Duration::from_secs(1), "test_quick_trip");
        assert!(cb.is_open());
        let _ = cb.check_admission(); // suppressed call 1
        assert_eq!(cb.suppressed_calls, 1);

        // Wait for deadline to expire
        std::thread::sleep(Duration::from_millis(1100));

        // Next admission check should transition to Closed and succeed
        let res = cb.check_admission();
        assert!(res.is_ok(), "admission must be granted after deadline expiration");
        assert_eq!(cb.state, CircuitBreakerState::Closed);
        assert_eq!(cb.suppressed_calls, 0);
        assert_eq!(cb.backoff_level, 0);

        // Verify telemetry contains CLOSED event
        let tel_body = std::fs::read_to_string(&telemetry_path).unwrap();
        assert!(tel_body.contains("GH_CIRCUIT_BREAKER_OPENED"));
        assert!(tel_body.contains("GH_CIRCUIT_BREAKER_CLOSED"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
