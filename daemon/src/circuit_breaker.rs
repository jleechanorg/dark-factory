use crate::telemetry::{self, TelemetryEvent};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_BASE_BACKOFF_SECS: u64 = 60;
pub const DEFAULT_MAX_BACKOFF_SECS: u64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BreakerState {
    Closed,
    Open,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedBreakerState {
    pub state: BreakerState,
    pub deadline_epoch: u64,
    pub consecutive_trips: u32,
    pub suppressed_call_count: u64,
    pub last_reason: String,
    pub last_opened_at_epoch: u64,
}

#[derive(Debug, Clone)]
pub struct RateLimitDetails {
    pub is_secondary: bool,
    pub retry_after: Option<Duration>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Allowed,
    Denied {
        deadline_epoch: u64,
        retry_after_secs: u64,
        suppressed_calls: u64,
        reason: String,
    },
}

pub struct GhCircuitBreaker {
    state: BreakerState,
    deadline_epoch: u64,
    consecutive_trips: u32,
    suppressed_call_count: u64,
    last_reason: String,
    last_opened_at_epoch: u64,
    storage_path: Option<PathBuf>,
    telemetry_log_path: Option<PathBuf>,
    base_backoff_secs: u64,
    max_backoff_secs: u64,
}

impl Default for GhCircuitBreaker {
    fn default() -> Self {
        let base_secs = std::env::var("DARK_FACTORY_GH_RATE_LIMIT_BASE_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_BASE_BACKOFF_SECS);
        let max_secs = std::env::var("DARK_FACTORY_GH_RATE_LIMIT_MAX_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_BACKOFF_SECS);

        let storage_path = if let Some(path) = std::env::var_os("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH") {
            Some(PathBuf::from(path))
        } else {
            Some(crate::intake::runtime_state_dir().join("gh_circuit_breaker.json"))
        };

        let mut cb = Self {
            state: BreakerState::Closed,
            deadline_epoch: 0,
            consecutive_trips: 0,
            suppressed_call_count: 0,
            last_reason: String::new(),
            last_opened_at_epoch: 0,
            storage_path,
            telemetry_log_path: None,
            base_backoff_secs: base_secs,
            max_backoff_secs: max_secs,
        };
        cb.load();
        cb
    }
}

impl GhCircuitBreaker {
    pub fn new(storage_path: Option<PathBuf>, telemetry_log_path: Option<PathBuf>) -> Self {
        let mut cb = Self {
            storage_path,
            telemetry_log_path,
            ..Default::default()
        };
        cb.load();
        cb
    }

    pub fn set_paths(&mut self, storage_path: Option<PathBuf>, telemetry_log_path: Option<PathBuf>) {
        self.storage_path = storage_path;
        self.telemetry_log_path = telemetry_log_path;
        self.load();
    }

    fn now_epoch_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn now_iso8601() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let s = now % 60;
        let m = (now / 60) % 60;
        let h = (now / 3600) % 24;
        let days = now / 86400;

        let mut year = 1970;
        let mut rem_days = days;
        loop {
            let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
            let days_in_year = if leap { 366 } else { 365 };
            if rem_days < days_in_year {
                break;
            }
            rem_days -= days_in_year;
            year += 1;
        }

        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_months = [
            31,
            if leap { 29 } else { 28 },
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
        for &dim in &days_in_months {
            if rem_days < dim {
                break;
            }
            rem_days -= dim;
            month += 1;
        }
        let day = rem_days + 1;

        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year, month, day, h, m, s
        )
    }

    pub fn is_open(&mut self) -> bool {
        match self.check_admission_at(Self::now_epoch_secs()) {
            Admission::Denied { .. } => true,
            Admission::Allowed => false,
        }
    }

    pub fn check_admission(&mut self) -> Admission {
        self.check_admission_at(Self::now_epoch_secs())
    }

    pub fn check_admission_at(&mut self, now_epoch: u64) -> Admission {
        if self.state == BreakerState::Open {
            if now_epoch < self.deadline_epoch {
                self.suppressed_call_count += 1;
                self.persist();
                let retry_after_secs = self.deadline_epoch.saturating_sub(now_epoch);
                return Admission::Denied {
                    deadline_epoch: self.deadline_epoch,
                    retry_after_secs,
                    suppressed_calls: self.suppressed_call_count,
                    reason: self.last_reason.clone(),
                };
            } else {
                // Cooldown has expired -> Transition to Closed
                let duration_secs = now_epoch.saturating_sub(self.last_opened_at_epoch);
                let suppressed = self.suppressed_call_count;
                self.state = BreakerState::Closed;
                self.suppressed_call_count = 0;
                self.emit_transition_event(
                    "GH_RATE_LIMIT_CIRCUIT_BREAKER_CLOSE",
                    serde_json::json!({
                        "suppressedCalls": suppressed,
                        "durationSecs": duration_secs,
                        "consecutiveTrips": self.consecutive_trips,
                    }),
                    serde_json::json!({
                        "reason": "cooldown_expired",
                        "previousDeadlineEpoch": self.deadline_epoch,
                    }),
                );
                self.persist();
                return Admission::Allowed;
            }
        }
        Admission::Allowed
    }

    pub fn record_rate_limit(&mut self, details: &RateLimitDetails) {
        self.record_rate_limit_at(details, Self::now_epoch_secs());
    }

    pub fn record_rate_limit_at(&mut self, details: &RateLimitDetails, now_epoch: u64) {
        let cooldown_secs = if let Some(dur) = details.retry_after {
            dur.as_secs().clamp(5, 3600)
        } else {
            let exponent = self.consecutive_trips.min(10);
            let factor = 1u64.checked_shl(exponent).unwrap_or(1024);
            let computed = self.base_backoff_secs.saturating_mul(factor);
            computed.min(self.max_backoff_secs)
        };

        if details.retry_after.is_none() {
            self.consecutive_trips = self.consecutive_trips.saturating_add(1);
        }

        let new_deadline_epoch = now_epoch.saturating_add(cooldown_secs);

        if self.state == BreakerState::Open {
            if new_deadline_epoch > self.deadline_epoch {
                self.deadline_epoch = new_deadline_epoch;
            }
            self.last_reason = details.reason.clone();
            self.emit_transition_event(
                "GH_RATE_LIMIT_CIRCUIT_BREAKER_EXTEND",
                serde_json::json!({
                    "cooldownSecs": cooldown_secs,
                    "suppressedCalls": self.suppressed_call_count,
                    "consecutiveTrips": self.consecutive_trips,
                    "deadlineEpoch": self.deadline_epoch,
                }),
                serde_json::json!({
                    "reason": details.reason,
                    "isSecondary": details.is_secondary,
                    "hasRetryAfter": details.retry_after.is_some(),
                }),
            );
        } else {
            self.state = BreakerState::Open;
            self.deadline_epoch = new_deadline_epoch;
            self.last_opened_at_epoch = now_epoch;
            self.suppressed_call_count = 0;
            self.last_reason = details.reason.clone();
            self.emit_transition_event(
                "GH_RATE_LIMIT_CIRCUIT_BREAKER_OPEN",
                serde_json::json!({
                    "cooldownSecs": cooldown_secs,
                    "suppressedCalls": 0,
                    "consecutiveTrips": self.consecutive_trips,
                    "deadlineEpoch": self.deadline_epoch,
                }),
                serde_json::json!({
                    "reason": details.reason,
                    "isSecondary": details.is_secondary,
                    "hasRetryAfter": details.retry_after.is_some(),
                }),
            );
        }
        self.persist();
    }

    pub fn record_success(&mut self) {
        if self.state == BreakerState::Closed && self.consecutive_trips > 0 {
            self.consecutive_trips = 0;
            self.persist();
        }
    }

    pub fn clear(&mut self) {
        self.state = BreakerState::Closed;
        self.deadline_epoch = 0;
        self.consecutive_trips = 0;
        self.suppressed_call_count = 0;
        self.last_reason.clear();
        self.last_opened_at_epoch = 0;
        self.persist();
    }

    pub fn mark_open_manual(&mut self, duration: Duration, reason: &str) {
        let details = RateLimitDetails {
            is_secondary: false,
            retry_after: Some(duration),
            reason: reason.to_string(),
        };
        self.record_rate_limit(&details);
    }

    fn emit_transition_event(
        &self,
        event_type: &str,
        metrics: serde_json::Value,
        context: serde_json::Value,
    ) {
        let log_path = self
            .telemetry_log_path
            .clone()
            .or_else(|| {
                std::env::var_os("DARK_FACTORY_TELEMETRY_LOG")
                    .map(PathBuf::from)
            })
            .or_else(|| {
                std::env::var_os("HOME").map(|home| {
                    Path::new(&home)
                        .join("Library/Logs/dark-factory")
                        .join("daemon.jsonl")
                })
            });

        if let Some(path) = log_path {
            let ev = TelemetryEvent {
                timestamp: Self::now_iso8601(),
                bead_id: "gh_circuit_breaker".into(),
                attempt_id: 0,
                lifecycle_state: "CIRCUIT_BREAKER".into(),
                event_type: event_type.to_string(),
                metrics,
                context,
            };
            let _ = telemetry::emit(&path, &ev);
        }
    }

    fn load(&mut self) {
        let Some(path) = &self.storage_path else {
            return;
        };
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(data) = serde_json::from_str::<PersistedBreakerState>(&raw) {
                let now = Self::now_epoch_secs();
                if data.state == BreakerState::Open && now < data.deadline_epoch {
                    self.state = BreakerState::Open;
                    self.deadline_epoch = data.deadline_epoch;
                    self.consecutive_trips = data.consecutive_trips;
                    self.suppressed_call_count = data.suppressed_call_count;
                    self.last_reason = data.last_reason;
                    self.last_opened_at_epoch = data.last_opened_at_epoch;
                } else {
                    self.state = BreakerState::Closed;
                    self.consecutive_trips = data.consecutive_trips;
                }
            }
        }
    }

    fn persist(&self) {
        let Some(path) = &self.storage_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let data = PersistedBreakerState {
            state: self.state,
            deadline_epoch: self.deadline_epoch,
            consecutive_trips: self.consecutive_trips,
            suppressed_call_count: self.suppressed_call_count,
            last_reason: self.last_reason.clone(),
            last_opened_at_epoch: self.last_opened_at_epoch,
        };
        if let Ok(json) = serde_json::to_string(&data) {
            let temp_path = path.with_extension(format!("tmp.{}", std::process::id()));
            if std::fs::write(&temp_path, json).is_ok() {
                let _ = std::fs::rename(&temp_path, path);
            }
        }
    }
}

pub fn parse_rate_limit(stderr: &str, stdout: &str, rc: i32) -> Option<RateLimitDetails> {
    let combined = format!("{stderr}\n{stdout}");
    let lower = combined.to_ascii_lowercase();

    let is_rl = lower.contains("api rate limit exceeded")
        || lower.contains("rate limit hit")
        || lower.contains("rate limit exceeded")
        || lower.contains("rate limit circuit breaker")
        || lower.contains("secondary rate limit")
        || lower.contains("abuse detection mechanism")
        || lower.contains("please wait a few minutes before you try again")
        || (lower.contains("403") && (lower.contains("rate limit") || lower.contains("secondary") || lower.contains("rate_limit")))
        || (rc == 403 && (lower.contains("rate limit") || lower.contains("secondary") || lower.contains("abuse") || lower.contains("retry-after") || lower.contains("please wait")))
        || lower.contains("too many requests")
        || (rc == 429)
        || lower.contains("rate_limited");

    if !is_rl {
        return None;
    }

    let is_secondary = lower.contains("secondary")
        || lower.contains("abuse detection mechanism")
        || lower.contains("please wait a few minutes");

    let retry_after = extract_retry_after(&lower);

    let reason = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else if !stdout.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        format!("HTTP {rc} rate limit")
    };

    Some(RateLimitDetails {
        is_secondary,
        retry_after,
        reason,
    })
}

fn extract_retry_after(s: &str) -> Option<Duration> {
    // 1. "retry-after" or "retry after"
    if let Some(pos) = s.find("retry-after").or_else(|| s.find("retry after")) {
        let after_slice = &s[pos..];
        if let Some(digits_str) = find_first_number(after_slice) {
            if let Ok(secs) = digits_str.parse::<u64>() {
                if secs > 0 {
                    return Some(Duration::from_secs(secs));
                }
            }
        }
    }
    // 2. "please wait X minutes" or "please wait X seconds"
    if let Some(pos) = s.find("please wait") {
        let after_slice = &s[pos + "please wait".len()..];
        if let Some((digits_str, unit)) = find_number_and_unit(after_slice) {
            if let Ok(val) = digits_str.parse::<u64>() {
                if unit.starts_with("min") || unit == "m" {
                    return Some(Duration::from_secs(val * 60));
                } else if unit.starts_with("sec") || unit == "s" {
                    return Some(Duration::from_secs(val));
                }
            }
        }
    }
    // 3. "reset at X" or "ratelimit-reset"
    if let Some(pos) = s.find("reset at").or_else(|| s.find("ratelimit-reset")) {
        let after_slice = &s[pos..];
        if let Some(digits_str) = find_first_number(after_slice) {
            if let Ok(epoch) = digits_str.parse::<u64>() {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if epoch > now {
                    return Some(Duration::from_secs(epoch - now));
                }
            }
        }
    }
    // 4. "resets in X" or "reset in X"
    if let Some(pos) = s.find("resets in").or_else(|| s.find("reset in")) {
        let after_slice = &s[pos..];
        if let Some((digits_str, unit)) = find_number_and_unit(after_slice) {
            if let Ok(val) = digits_str.parse::<u64>() {
                if unit.starts_with("min") || unit == "m" {
                    return Some(Duration::from_secs(val * 60));
                } else if unit.starts_with("sec") || unit == "s" {
                    return Some(Duration::from_secs(val));
                }
            }
        }
    }
    None
}

fn find_first_number(s: &str) -> Option<String> {
    let mut digits = String::new();
    let mut in_number = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            in_number = true;
            digits.push(c);
        } else if in_number {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

fn find_number_and_unit(s: &str) -> Option<(String, String)> {
    let mut digits = String::new();
    let mut unit = String::new();
    let mut state = 0; // 0: before digits, 1: reading digits, 2: reading unit

    for c in s.chars() {
        if state == 0 {
            if c.is_ascii_digit() {
                state = 1;
                digits.push(c);
            }
        } else if state == 1 {
            if c.is_ascii_digit() {
                digits.push(c);
            } else if c.is_ascii_alphabetic() {
                state = 2;
                unit.push(c);
            } else if !c.is_whitespace() && c != ':' {
                break;
            }
        } else if state == 2 {
            if c.is_ascii_alphabetic() {
                unit.push(c);
            } else {
                break;
            }
        }
    }

    if !digits.is_empty() {
        Some((digits, unit))
    } else {
        None
    }
}

static GLOBAL_CIRCUIT_BREAKER: Mutex<Option<GhCircuitBreaker>> = Mutex::new(None);

pub fn with_gh_circuit_breaker<F, R>(f: F) -> R
where
    F: FnOnce(&mut GhCircuitBreaker) -> R,
{
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap();
    if lock.is_none() {
        *lock = Some(GhCircuitBreaker::default());
    }
    f(lock.as_mut().unwrap())
}

pub fn check_gh_admission() -> Admission {
    with_gh_circuit_breaker(|cb| cb.check_admission())
}

pub fn check_and_record_gh_rate_limit(stderr: &str, stdout: &str, rc: i32) -> Option<RateLimitDetails> {
    if let Some(details) = parse_rate_limit(stderr, stdout, rc) {
        with_gh_circuit_breaker(|cb| cb.record_rate_limit(&details));
        Some(details)
    } else {
        None
    }
}

pub fn record_gh_success() {
    with_gh_circuit_breaker(|cb| cb.record_success());
}

pub fn is_gh_circuit_breaker_open() -> bool {
    with_gh_circuit_breaker(|cb| cb.is_open())
}

pub fn mark_gh_circuit_breaker_open(duration: Duration, reason: &str) {
    with_gh_circuit_breaker(|cb| cb.mark_open_manual(duration, reason));
}

pub fn clear_gh_circuit_breaker() {
    with_gh_circuit_breaker(|cb| cb.clear());
}

pub fn set_gh_circuit_breaker_paths(storage_path: Option<PathBuf>, telemetry_log: Option<PathBuf>) {
    with_gh_circuit_breaker(|cb| cb.set_paths(storage_path, telemetry_log));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rate_limit_primary() {
        let details = parse_rate_limit(
            "HTTP 403: API rate limit exceeded for user",
            "",
            403,
        )
        .expect("should detect primary rate limit");
        assert!(!details.is_secondary);
        assert_eq!(details.retry_after, None);
    }

    #[test]
    fn test_parse_rate_limit_secondary() {
        let details = parse_rate_limit(
            "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
            "",
            403,
        )
        .expect("should detect secondary rate limit");
        assert!(details.is_secondary);
    }

    #[test]
    fn test_parse_rate_limit_abuse() {
        let details = parse_rate_limit(
            "HTTP 403: You have triggered an abuse detection mechanism. Please wait 5 minutes.",
            "",
            403,
        )
        .expect("should detect abuse rate limit");
        assert!(details.is_secondary);
        assert_eq!(details.retry_after, Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_parse_rate_limit_retry_after_header() {
        let details = parse_rate_limit(
            "HTTP 403: rate limit hit\nretry-after: 120",
            "",
            403,
        )
        .expect("should parse retry-after");
        assert_eq!(details.retry_after, Some(Duration::from_secs(120)));
    }

    #[test]
    fn test_parse_rate_limit_retry_after_phrase() {
        let details = parse_rate_limit(
            "API rate limit exceeded. Retry after 45 seconds",
            "",
            1,
        )
        .expect("should parse retry after seconds phrase");
        assert_eq!(details.retry_after, Some(Duration::from_secs(45)));
    }

    #[test]
    fn test_parse_rate_limit_429_too_many_requests() {
        let details = parse_rate_limit(
            "HTTP 429: Too Many Requests",
            "",
            429,
        )
        .expect("should detect 429");
        assert_eq!(details.retry_after, None);
    }

    #[test]
    fn test_parse_rate_limit_unrelated_error_returns_none() {
        assert!(parse_rate_limit("could not resolve host", "", 1).is_none());
        assert!(parse_rate_limit("file not found", "", 1).is_none());
    }

    #[test]
    fn test_exponential_backoff_bounding() {
        let mut cb = GhCircuitBreaker {
            base_backoff_secs: 60,
            max_backoff_secs: 480,
            storage_path: None,
            telemetry_log_path: None,
            ..Default::default()
        };

        let now = 1_000_000;
        let details = RateLimitDetails {
            is_secondary: false,
            retry_after: None,
            reason: "rate limit".into(),
        };

        // Trip 1: 60s
        cb.record_rate_limit_at(&details, now);
        assert_eq!(cb.deadline_epoch, now + 60);
        assert_eq!(cb.consecutive_trips, 1);

        // Trip 2: 120s
        cb.record_rate_limit_at(&details, now);
        assert_eq!(cb.deadline_epoch, now + 120);
        assert_eq!(cb.consecutive_trips, 2);

        // Trip 3: 240s
        cb.record_rate_limit_at(&details, now);
        assert_eq!(cb.deadline_epoch, now + 240);
        assert_eq!(cb.consecutive_trips, 3);

        // Trip 4: 480s (hits max_backoff_secs)
        cb.record_rate_limit_at(&details, now);
        assert_eq!(cb.deadline_epoch, now + 480);
        assert_eq!(cb.consecutive_trips, 4);

        // Trip 5: bounded at max 480s
        cb.record_rate_limit_at(&details, now);
        assert_eq!(cb.deadline_epoch, now + 480);
    }

    #[test]
    fn test_admission_short_circuits_and_expires() {
        let mut cb = GhCircuitBreaker {
            base_backoff_secs: 60,
            max_backoff_secs: 900,
            storage_path: None,
            telemetry_log_path: None,
            ..Default::default()
        };

        let now = 1_000_000;
        let details = RateLimitDetails {
            is_secondary: true,
            retry_after: Some(Duration::from_secs(60)),
            reason: "secondary rate limit".into(),
        };

        cb.record_rate_limit_at(&details, now);
        assert_eq!(cb.state, BreakerState::Open);

        // Call before deadline -> Denied
        match cb.check_admission_at(now + 10) {
            Admission::Denied { deadline_epoch, retry_after_secs, suppressed_calls, .. } => {
                assert_eq!(deadline_epoch, now + 60);
                assert_eq!(retry_after_secs, 50);
                assert_eq!(suppressed_calls, 1);
            }
            Admission::Allowed => panic!("Should be denied"),
        }

        // Another call before deadline -> Denied, suppressed count = 2
        match cb.check_admission_at(now + 20) {
            Admission::Denied { suppressed_calls, .. } => {
                assert_eq!(suppressed_calls, 2);
            }
            Admission::Allowed => panic!("Should be denied"),
        }

        // Call at/after deadline -> Allowed, transitions to Closed
        match cb.check_admission_at(now + 60) {
            Admission::Allowed => {
                assert_eq!(cb.state, BreakerState::Closed);
                assert_eq!(cb.suppressed_call_count, 0);
            }
            Admission::Denied { .. } => panic!("Should be allowed once expired"),
        }
    }
}
