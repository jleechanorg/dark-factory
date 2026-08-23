use crate::errors::DaemonError;
use crate::telemetry::{self, TelemetryEvent};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MIN_COOLDOWN_SECS: u64 = 5;
pub const BASE_COOLDOWN_SECS: u64 = 60;
pub const MAX_COOLDOWN_SECS: u64 = 1800; // 30 minutes
pub const BREAKER_STATE_FILE_NAME: &str = "gh_circuit_breaker.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitDetection {
    pub is_secondary: bool,
    pub retry_after: Option<Duration>,
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GhCircuitBreakerRecord {
    pub is_open: bool,
    pub deadline_epoch: u64,
    pub consecutive_rate_limits: u32,
    pub last_reason: String,
    pub suppressed_calls_during_open: u64,
    pub total_suppressed_calls: u64,
    pub last_transition_epoch: u64,
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub fn epoch_to_iso8601(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Checks whether stderr or stdout contains a rate limit phrase (primary, secondary, or breaker open).
pub fn is_gh_rate_limit_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("api rate limit exceeded")
        || lower.contains("rate limit hit")
        || lower.contains("rate limit exceeded")
        || lower.contains("secondary rate limit")
        || lower.contains("please wait a few minutes before you try again")
        || lower.contains("abuse detection mechanism")
        || lower.contains("too many requests")
        || (lower.contains("403") && lower.contains("rate limit"))
        || (lower.contains("429") && lower.contains("rate limit"))
        || lower.contains("gh rate limit circuit breaker is open")
        || lower.contains("rate limit circuit breaker active")
}

/// Parse Retry-After duration from headers, JSON response, or prose text in error messages.
pub fn parse_retry_after(text: &str) -> Option<Duration> {
    let lower = text.to_ascii_lowercase();

    // 1. Check HTTP header format: Retry-After: <seconds>
    if let Some(pos) = lower.find("retry-after:") {
        let after = &lower[pos + "retry-after:".len()..];
        let num_str: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(secs) = num_str.parse::<u64>() {
            if secs > 0 {
                return Some(Duration::from_secs(secs));
            }
        }
    }

    // 2. Check JSON: "retry_after": <seconds> or "retry_after_seconds": <seconds>
    if let Some(pos) = lower.find("\"retry_after\"") {
        let after = &lower[pos + "\"retry_after\"".len()..];
        if let Some(colon_pos) = after.find(':') {
            let val_part = &after[colon_pos + 1..];
            let num_str: String = val_part
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(secs) = num_str.parse::<u64>() {
                if secs > 0 {
                    return Some(Duration::from_secs(secs));
                }
            }
        }
    }

    // 3. Check prose: "retry after X seconds" or "retry after X minutes" or "retry after Xs"
    if let Some(pos) = lower.find("retry after") {
        let after = &lower[pos + "retry after".len()..];
        let trimmed = after.trim_start();
        let num_str: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse::<u64>() {
            let unit_part = trimmed[num_str.len()..].trim_start();
            if unit_part.starts_with("m") || unit_part.starts_with("min") {
                return Some(Duration::from_secs(num * 60));
            }
            return Some(Duration::from_secs(num));
        }
    }

    // 4. Check x-ratelimit-reset: <epoch>
    if let Some(pos) = lower.find("x-ratelimit-reset:") {
        let after = &lower[pos + "x-ratelimit-reset:".len()..];
        let num_str: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(reset_epoch) = num_str.parse::<u64>() {
            let now = now_epoch();
            if reset_epoch > now {
                return Some(Duration::from_secs(reset_epoch - now));
            }
        }
    }

    None
}

/// Detect primary vs secondary rate limits and parse retry information.
pub fn detect_rate_limit(
    tool: &str,
    _rc: i32,
    stderr: &str,
    stdout: &str,
) -> Option<RateLimitDetection> {
    if tool != "gh" {
        return None;
    }

    let combined = format!("{stderr} {stdout}");
    if !is_gh_rate_limit_text(&combined) {
        return None;
    }

    let lower = combined.to_ascii_lowercase();
    let is_secondary = lower.contains("secondary rate limit")
        || lower.contains("please wait a few minutes before you try again")
        || lower.contains("abuse detection mechanism");

    let retry_after = parse_retry_after(&combined);

    let reason = if is_secondary {
        "secondary_rate_limit".to_string()
    } else if lower.contains("api rate limit exceeded") {
        "primary_api_rate_limit_exceeded".to_string()
    } else if lower.contains("graphql: api rate limit exceeded") {
        "graphql_rate_limit_exceeded".to_string()
    } else {
        "rate_limit_exceeded".to_string()
    };

    Some(RateLimitDetection {
        is_secondary,
        retry_after,
        reason,
    })
}

pub fn default_state_file_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH") {
        return PathBuf::from(path);
    }
    crate::intake::runtime_state_dir().join(BREAKER_STATE_FILE_NAME)
}

pub fn default_telemetry_log_path() -> Option<PathBuf> {
    if let Some(log) = std::env::var_os("DARK_FACTORY_LOG_PATH") {
        return Some(PathBuf::from(log));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Some(
            Path::new(&home)
                .join("Library/Logs/dark-factory")
                .join("daemon.jsonl"),
        );
    }
    None
}

pub struct GhCircuitBreaker {
    state_file: PathBuf,
    telemetry_log: Option<PathBuf>,
    inner: Mutex<GhCircuitBreakerRecord>,
}

static GLOBAL_BREAKER: OnceLock<GhCircuitBreaker> = OnceLock::new();

pub fn global() -> &'static GhCircuitBreaker {
    GLOBAL_BREAKER.get_or_init(|| {
        GhCircuitBreaker::new_with_paths(
            default_state_file_path(),
            default_telemetry_log_path(),
        )
    })
}

impl GhCircuitBreaker {
    pub fn new_with_paths(state_file: PathBuf, telemetry_log: Option<PathBuf>) -> Self {
        let initial_record = Self::load_record_from_file(&state_file).unwrap_or_default();
        let now = now_epoch();

        // Check if an existing open deadline has already expired
        let is_open = initial_record.is_open && now < initial_record.deadline_epoch;

        let record = GhCircuitBreakerRecord {
            is_open,
            ..initial_record
        };

        Self {
            state_file,
            telemetry_log,
            inner: Mutex::new(record),
        }
    }

    fn load_record_from_file(path: &Path) -> Option<GhCircuitBreakerRecord> {
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_record(&self, record: &GhCircuitBreakerRecord) {
        if let Some(parent) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(record) {
            let _ = std::fs::write(&self.state_file, json);
        }
    }

    fn emit_event(&self, event_type: &str, metrics: serde_json::Value, context: serde_json::Value) {
        let Some(log_path) = &self.telemetry_log else {
            return;
        };
        let event = TelemetryEvent {
            timestamp: epoch_to_iso8601(now_epoch()),
            bead_id: "gh_circuit_breaker".to_string(),
            attempt_id: 1,
            lifecycle_state: "RATE_LIMIT".to_string(),
            event_type: event_type.to_string(),
            metrics,
            context,
        };
        let _ = telemetry::emit(log_path, &event);
    }

    pub fn compute_next_cooldown(
        &self,
        retry_after: Option<Duration>,
        consecutive_hits: u32,
    ) -> Duration {
        if let Some(retry) = retry_after {
            let secs = retry.as_secs().clamp(MIN_COOLDOWN_SECS, MAX_COOLDOWN_SECS);
            return Duration::from_secs(secs);
        }

        let exponent = (consecutive_hits.saturating_sub(1)).min(5);
        let cooldown_secs = (BASE_COOLDOWN_SECS * (1 << exponent)).min(MAX_COOLDOWN_SECS);
        Duration::from_secs(cooldown_secs)
    }

    pub fn is_open(&self) -> bool {
        let mut lock = self.inner.lock().unwrap();
        let now = now_epoch();
        if lock.is_open && now >= lock.deadline_epoch {
            // Expired -> Transition from Open to Closed
            let suppressed = lock.suppressed_calls_during_open;
            let duration_open = now.saturating_sub(lock.last_transition_epoch);
            let prev_deadline = epoch_to_iso8601(lock.deadline_epoch);
            let last_reason = lock.last_reason.clone();

            lock.is_open = false;
            lock.suppressed_calls_during_open = 0;
            lock.last_transition_epoch = now;
            self.save_record(&lock);

            self.emit_event(
                "GH_RATE_LIMIT_BREAKER_CLOSED",
                serde_json::json!({
                    "suppressedCalls": suppressed,
                    "openDurationSecs": duration_open,
                }),
                serde_json::json!({
                    "previousDeadline": prev_deadline,
                    "lastReason": last_reason,
                }),
            );
            return false;
        }
        lock.is_open
    }

    pub fn before_call(&self, tool: &str) -> Result<(), DaemonError> {
        if tool != "gh" {
            return Ok(());
        }

        let mut lock = self.inner.lock().unwrap();
        if lock.is_open {
            let now = now_epoch();
            if now >= lock.deadline_epoch {
                // Cooldown expired: allow one probe request and transition to closed
                let suppressed = lock.suppressed_calls_during_open;
                let duration_open = now.saturating_sub(lock.last_transition_epoch);
                let prev_deadline = epoch_to_iso8601(lock.deadline_epoch);
                let last_reason = lock.last_reason.clone();

                lock.is_open = false;
                lock.suppressed_calls_during_open = 0;
                lock.last_transition_epoch = now;
                self.save_record(&lock);

                self.emit_event(
                    "GH_RATE_LIMIT_BREAKER_CLOSED",
                    serde_json::json!({
                        "suppressedCalls": suppressed,
                        "openDurationSecs": duration_open,
                    }),
                    serde_json::json!({
                        "previousDeadline": prev_deadline,
                        "lastReason": last_reason,
                    }),
                );
                return Ok(());
            }

            // Breaker is active: record suppressed call and short-circuit
            lock.suppressed_calls_during_open += 1;
            lock.total_suppressed_calls += 1;
            let deadline = lock.deadline_epoch;
            let suppressed = lock.suppressed_calls_during_open;
            let remaining = deadline.saturating_sub(now);
            self.save_record(&lock);

            return Err(DaemonError::Tool {
                tool: "gh".to_string(),
                rc: 403,
                stderr: format!(
                    "gh rate limit circuit breaker is open (cooldown active until {}, {}s remaining, {} calls suppressed)",
                    epoch_to_iso8601(deadline),
                    remaining,
                    suppressed
                ),
            });
        }

        Ok(())
    }

    pub fn on_error(&self, tool: &str, rc: i32, stderr: &str) {
        if tool != "gh" {
            return;
        }

        if let Some(detection) = detect_rate_limit(tool, rc, stderr, "") {
            let mut lock = self.inner.lock().unwrap();
            let now = now_epoch();
            lock.consecutive_rate_limits = lock.consecutive_rate_limits.saturating_add(1);
            let cooldown = self.compute_next_cooldown(detection.retry_after, lock.consecutive_rate_limits);
            let cooldown_secs = cooldown.as_secs();
            let new_deadline = now + cooldown_secs;
            if lock.is_open {
                lock.deadline_epoch = std::cmp::max(lock.deadline_epoch, new_deadline);
            } else {
                lock.deadline_epoch = new_deadline;
            }
            lock.last_reason = detection.reason.clone();

            if !lock.is_open {
                lock.is_open = true;
                lock.suppressed_calls_during_open = 0;
                lock.last_transition_epoch = now;
                self.save_record(&lock);

                self.emit_event(
                    "GH_RATE_LIMIT_BREAKER_OPEN",
                    serde_json::json!({
                        "cooldownSecs": cooldown_secs,
                        "consecutiveHits": lock.consecutive_rate_limits,
                        "suppressedCalls": 0,
                    }),
                    serde_json::json!({
                        "reason": detection.reason,
                        "deadline": epoch_to_iso8601(lock.deadline_epoch),
                        "isSecondary": detection.is_secondary,
                        "retryAfterSeconds": detection.retry_after.map(|d| d.as_secs()),
                    }),
                );
            } else {
                let current_suppressed = lock.suppressed_calls_during_open;
                self.save_record(&lock);

                self.emit_event(
                    "GH_RATE_LIMIT_BREAKER_EXTENDED",
                    serde_json::json!({
                        "cooldownSecs": cooldown_secs,
                        "consecutiveHits": lock.consecutive_rate_limits,
                        "suppressedCalls": current_suppressed,
                    }),
                    serde_json::json!({
                        "reason": detection.reason,
                        "deadline": epoch_to_iso8601(lock.deadline_epoch),
                        "isSecondary": detection.is_secondary,
                        "retryAfterSeconds": detection.retry_after.map(|d| d.as_secs()),
                    }),
                );
            }
        }
    }

    pub fn on_success(&self, tool: &str) {
        if tool != "gh" {
            return;
        }
        let mut lock = self.inner.lock().unwrap();
        if !lock.is_open && lock.consecutive_rate_limits > 0 {
            lock.consecutive_rate_limits = 0;
            self.save_record(&lock);
        }
    }

    pub fn force_open(&self, duration: Duration, reason: &str) {
        let mut lock = self.inner.lock().unwrap();
        let now = now_epoch();
        let cooldown_secs = duration.as_secs().clamp(1, MAX_COOLDOWN_SECS);
        lock.consecutive_rate_limits = lock.consecutive_rate_limits.saturating_add(1);
        let new_deadline = now + cooldown_secs;
        let was_open = lock.is_open;
        if was_open {
            lock.deadline_epoch = std::cmp::max(lock.deadline_epoch, new_deadline);
        } else {
            lock.deadline_epoch = new_deadline;
        }
        lock.last_reason = reason.to_string();
        lock.is_open = true;

        if !was_open {
            lock.suppressed_calls_during_open = 0;
            lock.last_transition_epoch = now;
            self.save_record(&lock);

            self.emit_event(
                "GH_RATE_LIMIT_BREAKER_OPEN",
                serde_json::json!({
                    "cooldownSecs": cooldown_secs,
                    "consecutiveHits": lock.consecutive_rate_limits,
                    "suppressedCalls": 0,
                }),
                serde_json::json!({
                    "reason": reason,
                    "deadline": epoch_to_iso8601(lock.deadline_epoch),
                    "isSecondary": false,
                    "retryAfterSeconds": Some(cooldown_secs),
                }),
            );
        } else {
            let current_suppressed = lock.suppressed_calls_during_open;
            self.save_record(&lock);

            self.emit_event(
                "GH_RATE_LIMIT_BREAKER_EXTENDED",
                serde_json::json!({
                    "cooldownSecs": cooldown_secs,
                    "consecutiveHits": lock.consecutive_rate_limits,
                    "suppressedCalls": current_suppressed,
                }),
                serde_json::json!({
                    "reason": reason,
                    "deadline": epoch_to_iso8601(lock.deadline_epoch),
                    "isSecondary": false,
                    "retryAfterSeconds": Some(cooldown_secs),
                }),
            );
        }
    }

    pub fn force_close(&self) {
        let mut lock = self.inner.lock().unwrap();
        if lock.is_open {
            let now = now_epoch();
            let suppressed = lock.suppressed_calls_during_open;
            let duration_open = now.saturating_sub(lock.last_transition_epoch);
            let prev_deadline = epoch_to_iso8601(lock.deadline_epoch);
            let last_reason = lock.last_reason.clone();

            lock.is_open = false;
            lock.suppressed_calls_during_open = 0;
            lock.last_transition_epoch = now;
            self.save_record(&lock);

            self.emit_event(
                "GH_RATE_LIMIT_BREAKER_CLOSED",
                serde_json::json!({
                    "suppressedCalls": suppressed,
                    "openDurationSecs": duration_open,
                }),
                serde_json::json!({
                    "previousDeadline": prev_deadline,
                    "lastReason": last_reason,
                }),
            );
        }
    }

    pub fn reset(&self) {
        let mut lock = self.inner.lock().unwrap();
        *lock = GhCircuitBreakerRecord::default();
        let _ = std::fs::remove_file(&self.state_file);
    }

    pub fn suppressed_calls_during_open(&self) -> u64 {
        self.inner.lock().unwrap().suppressed_calls_during_open
    }

    pub fn total_suppressed_calls(&self) -> u64 {
        self.inner.lock().unwrap().total_suppressed_calls
    }

    pub fn consecutive_rate_limits(&self) -> u32 {
        self.inner.lock().unwrap().consecutive_rate_limits
    }

    pub fn deadline_epoch(&self) -> u64 {
        self.inner.lock().unwrap().deadline_epoch
    }
}
