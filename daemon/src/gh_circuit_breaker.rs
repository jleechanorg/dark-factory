use crate::errors::DaemonError;
use crate::telemetry::{self, TelemetryEvent};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_BASE_COOLDOWN_SECS: u64 = 60;
pub const MAX_COOLDOWN_SECS: u64 = 1800; // 30 minutes max backoff

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CircuitState {
    Closed,
    Open,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct GhCircuitBreakerRecord {
    pub state: CircuitState,
    pub deadline_epoch: u64,
    pub opened_at_epoch: u64,
    pub consecutive_triggers: u32,
    pub suppressed_calls: u64,
    pub last_reason: String,
}

impl Default for GhCircuitBreakerRecord {
    fn default() -> Self {
        Self {
            state: CircuitState::Closed,
            deadline_epoch: 0,
            opened_at_epoch: 0,
            consecutive_triggers: 0,
            suppressed_calls: 0,
            last_reason: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitDetection {
    pub is_rate_limit: bool,
    pub retry_after_secs: Option<u64>,
    pub reason: String,
}

/// Detect primary and secondary GitHub rate-limit responses from exit code, stderr, and stdout.
pub fn detect_gh_rate_limit(rc: i32, stderr: &str, stdout: &str) -> Option<RateLimitDetection> {
    let stderr_lower = stderr.to_ascii_lowercase();
    let stdout_lower = stdout.to_ascii_lowercase();

    let combined = format!("{stderr_lower}\n{stdout_lower}");

    let is_primary_rate_limit = combined.contains("api rate limit exceeded")
        || combined.contains("rate limit hit")
        || combined.contains("rate limit exceeded")
        || combined.contains("graphql: api rate limit exceeded")
        || combined.contains("\"rate_limited\"")
        || combined.contains("rate_limit");

    let is_secondary_rate_limit = combined.contains("secondary rate limit")
        || combined.contains("you have exceeded a secondary rate limit")
        || combined.contains("please wait a few minutes before trying again")
        || combined.contains("please wait a few minutes before you try again")
        || combined.contains("was submitted too quickly")
        || combined.contains("abuse detection mechanism");

    let is_403_rate_limit = (rc == 403 || combined.contains("403") || combined.contains("http 403") || combined.contains("forbidden"))
        && (is_primary_rate_limit
            || is_secondary_rate_limit
            || combined.contains("rate limit")
            || combined.contains("abuse")
            || combined.contains("try again later")
            || combined.contains("wait a few minutes")
            || combined.contains("temporarily blocked")
            || combined.contains("exceeded"));

    if !is_primary_rate_limit && !is_secondary_rate_limit && !is_403_rate_limit {
        return None;
    }

    let retry_after_secs = parse_retry_after(&combined);

    let reason = if is_secondary_rate_limit {
        "secondary_rate_limit"
    } else if is_primary_rate_limit {
        "primary_rate_limit"
    } else {
        "http_403_rate_limit"
    };

    Some(RateLimitDetection {
        is_rate_limit: true,
        retry_after_secs,
        reason: reason.to_string(),
    })
}

/// Parse retry-after from text if present (e.g. `retry-after: 120`, `"retry_after": 60`, `retry after 30 seconds`).
pub fn parse_retry_after(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();

    // Check for "retry-after" or "retry_after" or "retry after"
    let markers = ["retry-after", "retry_after", "retry after"];
    for marker in markers {
        if let Some(idx) = lower.find(marker) {
            let after = &lower[idx + marker.len()..];
            // Skip non-digit characters up to a reasonable distance
            let trimmed = after.trim_start_matches(|c: char| c == ':' || c == '=' || c == '"' || c == ' ' || c == '\'' || c == '\t');
            let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(secs) = digits.parse::<u64>() {
                    if secs > 0 {
                        return Some(secs);
                    }
                }
            }
        }
    }
    None
}

pub struct GhCircuitBreaker {
    record: GhCircuitBreakerRecord,
    persistence_path: PathBuf,
    telemetry_path: Option<PathBuf>,
}

impl GhCircuitBreaker {
    pub fn new(persistence_path: PathBuf, telemetry_path: Option<PathBuf>) -> Self {
        Self {
            record: GhCircuitBreakerRecord::default(),
            persistence_path,
            telemetry_path,
        }
    }

    pub fn load_or_default_at(persistence_path: PathBuf, telemetry_path: Option<PathBuf>) -> Self {
        let record = if persistence_path.exists() {
            match std::fs::read_to_string(&persistence_path) {
                Ok(content) => serde_json::from_str::<GhCircuitBreakerRecord>(&content).unwrap_or_default(),
                Err(_) => GhCircuitBreakerRecord::default(),
            }
        } else {
            GhCircuitBreakerRecord::default()
        };

        Self {
            record,
            persistence_path,
            telemetry_path,
        }
    }

    pub fn record(&self) -> &GhCircuitBreakerRecord {
        &self.record
    }

    pub fn is_open(&self, now_epoch: u64) -> bool {
        self.record.state == CircuitState::Open && now_epoch < self.record.deadline_epoch
    }

    pub fn check_admission(&mut self, now_epoch: u64) -> Result<(), DaemonError> {
        if self.record.state == CircuitState::Open {
            if now_epoch < self.record.deadline_epoch {
                self.record.suppressed_calls = self.record.suppressed_calls.saturating_add(1);
                let _ = self.persist();
                let remaining = self.record.deadline_epoch.saturating_sub(now_epoch);
                return Err(DaemonError::Tool {
                    tool: "gh".into(),
                    rc: -1,
                    stderr: format!(
                        "gh rate limit circuit breaker open: cooldown active until {} ({}s remaining, suppressed calls: {})",
                        epoch_to_rfc3339(self.record.deadline_epoch),
                        remaining,
                        self.record.suppressed_calls
                    ),
                });
            } else {
                // Deadline has elapsed! Allow probe through.
            }
        }
        Ok(())
    }

    pub fn record_result(&mut self, rc: i32, stderr: &str, stdout: &str, now_epoch: u64) {
        if let Some(detection) = detect_gh_rate_limit(rc, stderr, stdout) {
            let cooldown_secs = match detection.retry_after_secs {
                Some(secs) => secs.clamp(1, MAX_COOLDOWN_SECS),
                None => {
                    self.record.consecutive_triggers = self.record.consecutive_triggers.saturating_add(1);
                    let exp = self.record.consecutive_triggers.saturating_sub(1).min(10);
                    DEFAULT_BASE_COOLDOWN_SECS
                        .saturating_mul(2_u64.pow(exp))
                        .min(MAX_COOLDOWN_SECS)
                }
            };

            let new_deadline = now_epoch.saturating_add(cooldown_secs);

            if self.record.state == CircuitState::Open && now_epoch < self.record.deadline_epoch {
                // Extend cooldown
                self.record.deadline_epoch = self.record.deadline_epoch.max(new_deadline);
                self.record.last_reason = detection.reason.clone();
                let _ = self.persist();
                self.emit_transition("GITHUB_CIRCUIT_BREAKER_EXTEND", cooldown_secs, now_epoch);
            } else {
                // Transition to Open
                self.record.state = CircuitState::Open;
                self.record.opened_at_epoch = now_epoch;
                self.record.deadline_epoch = new_deadline;
                self.record.suppressed_calls = 0;
                self.record.last_reason = detection.reason.clone();
                let _ = self.persist();
                self.emit_transition("GITHUB_CIRCUIT_BREAKER_OPEN", cooldown_secs, now_epoch);
            }
        } else if rc == 0 {
            // Success
            if self.record.state == CircuitState::Open {
                let total_suppressed = self.record.suppressed_calls;
                let total_duration = now_epoch.saturating_sub(self.record.opened_at_epoch);
                self.record.state = CircuitState::Closed;
                self.record.consecutive_triggers = 0;
                self.record.suppressed_calls = 0;
                self.record.deadline_epoch = 0;
                let _ = self.persist();
                self.emit_close_transition(total_suppressed, total_duration, now_epoch);
            } else if self.record.consecutive_triggers > 0 {
                self.record.consecutive_triggers = 0;
                let _ = self.persist();
            }
        }
    }

    pub fn persist(&self) -> Result<(), DaemonError> {
        if let Some(parent) = self.persistence_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DaemonError::Config(format!("create circuit breaker state dir: {e}"))
            })?;
        }

        let json = serde_json::to_string_pretty(&self.record)
            .map_err(|e| DaemonError::Parse(format!("serialize circuit breaker record: {e}")))?;

        let tmp = self.persistence_path.with_extension(format!(
            "json.tmp.{}.{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
        ));

        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| DaemonError::Config(format!("write circuit breaker state tmp: {e}")))?;

        if let Err(e) = std::fs::rename(&tmp, &self.persistence_path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(DaemonError::Config(format!(
                "rename circuit breaker state tmp->final: {e}"
            )));
        }
        Ok(())
    }

    fn emit_transition(&self, event_type: &str, cooldown_secs: u64, now_epoch: u64) {
        if let Some(log_path) = &self.telemetry_path {
            let ev = TelemetryEvent {
                timestamp: epoch_to_rfc3339(now_epoch),
                bead_id: "gh_circuit_breaker".to_string(),
                attempt_id: self.record.consecutive_triggers,
                lifecycle_state: "CIRCUIT_BREAKER".to_string(),
                event_type: event_type.to_string(),
                metrics: serde_json::json!({
                    "cooldownSecs": cooldown_secs,
                    "suppressedCalls": self.record.suppressed_calls,
                    "consecutiveTriggers": self.record.consecutive_triggers,
                    "deadlineEpoch": self.record.deadline_epoch,
                }),
                context: serde_json::json!({
                    "state": format!("{:?}", self.record.state),
                    "reason": self.record.last_reason,
                    "deadline": epoch_to_rfc3339(self.record.deadline_epoch),
                }),
            };
            let _ = telemetry::emit(log_path, &ev);
        }
    }

    fn emit_close_transition(&self, total_suppressed: u64, total_duration: u64, now_epoch: u64) {
        if let Some(log_path) = &self.telemetry_path {
            let ev = TelemetryEvent {
                timestamp: epoch_to_rfc3339(now_epoch),
                bead_id: "gh_circuit_breaker".to_string(),
                attempt_id: 0,
                lifecycle_state: "CIRCUIT_BREAKER".to_string(),
                event_type: "GITHUB_CIRCUIT_BREAKER_CLOSE".to_string(),
                metrics: serde_json::json!({
                    "totalSuppressedCalls": total_suppressed,
                    "durationSecs": total_duration,
                }),
                context: serde_json::json!({
                    "state": "CLOSED",
                    "recoveredAt": epoch_to_rfc3339(now_epoch),
                }),
            };
            let _ = telemetry::emit(log_path, &ev);
        }
    }
}

pub fn default_circuit_breaker_path() -> PathBuf {
    crate::intake::runtime_state_dir().join("gh_circuit_breaker.json")
}

pub fn default_telemetry_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        Path::new(&home)
            .join("Library/Logs/dark-factory")
            .join("daemon.jsonl")
    })
}

static GLOBAL_CIRCUIT_BREAKER: Mutex<Option<GhCircuitBreaker>> = Mutex::new(None);

pub fn global_circuit_breaker() -> &'static Mutex<Option<GhCircuitBreaker>> {
    &GLOBAL_CIRCUIT_BREAKER
}

pub fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn check_gh_admission() -> Result<(), DaemonError> {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    let cb = lock.get_or_insert_with(|| {
        GhCircuitBreaker::load_or_default_at(default_circuit_breaker_path(), default_telemetry_path())
    });
    cb.check_admission(current_epoch_secs())
}

pub fn record_gh_result(rc: i32, stderr: &str, stdout: &str) {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    let cb = lock.get_or_insert_with(|| {
        GhCircuitBreaker::load_or_default_at(default_circuit_breaker_path(), default_telemetry_path())
    });
    cb.record_result(rc, stderr, stdout, current_epoch_secs());
}

pub fn is_gh_circuit_breaker_open() -> bool {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    let cb = lock.get_or_insert_with(|| {
        GhCircuitBreaker::load_or_default_at(default_circuit_breaker_path(), default_telemetry_path())
    });
    cb.is_open(current_epoch_secs())
}

pub fn gh_circuit_breaker_suppressed_count() -> u64 {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    let cb = lock.get_or_insert_with(|| {
        GhCircuitBreaker::load_or_default_at(default_circuit_breaker_path(), default_telemetry_path())
    });
    cb.record().suppressed_calls
}

pub fn reset_global_circuit_breaker() {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    let path = default_circuit_breaker_path();
    let _ = std::fs::remove_file(&path);
    *lock = Some(GhCircuitBreaker::new(path, default_telemetry_path()));
}

pub fn set_global_circuit_breaker(cb: GhCircuitBreaker) {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap_or_else(|e| e.into_inner());
    *lock = Some(cb);
}

/// Dependency-free ISO-8601 UTC timestamp
pub fn epoch_to_rfc3339(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_primary_rate_limit() {
        let detection = detect_gh_rate_limit(1, "gh: API rate limit exceeded for installation ID 12345", "")
            .expect("should detect primary rate limit");
        assert!(detection.is_rate_limit);
        assert_eq!(detection.reason, "primary_rate_limit");
        assert!(detection.retry_after_secs.is_none());
    }

    #[test]
    fn detects_secondary_rate_limit_with_retry_after() {
        let detection = detect_gh_rate_limit(
            1,
            "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before trying again. Retry-After: 120",
            "",
        )
        .expect("should detect secondary rate limit");
        assert!(detection.is_rate_limit);
        assert_eq!(detection.reason, "secondary_rate_limit");
        assert_eq!(detection.retry_after_secs, Some(120));
    }

    #[test]
    fn parses_various_retry_after_formats() {
        assert_eq!(parse_retry_after("Retry-After: 60"), Some(60));
        assert_eq!(parse_retry_after("retry-after: 300"), Some(300));
        assert_eq!(parse_retry_after(r#"{"message": "rate limit", "retry_after": 45}"#), Some(45));
        assert_eq!(parse_retry_after("Please retry after 90 seconds"), Some(90));
        assert_eq!(parse_retry_after("no retry time given"), None);
    }

    #[test]
    fn non_rate_limit_error_is_not_detected() {
        assert!(detect_gh_rate_limit(1, "Could not resolve host: github.com", "").is_none());
        assert!(detect_gh_rate_limit(1, "HTTP 404: Not Found", "").is_none());
    }

    #[test]
    fn circuit_breaker_lifecycle_and_backoff() {
        let temp_dir = std::env::temp_dir().join(format!("cb_test_{}", current_epoch_secs()));
        let cb_path = temp_dir.join("gh_circuit_breaker.json");
        let tel_path = temp_dir.join("daemon.jsonl");

        let mut cb = GhCircuitBreaker::new(cb_path.clone(), Some(tel_path.clone()));

        let start_epoch = 1_700_000_000;
        assert!(cb.check_admission(start_epoch).is_ok());

        // First 403 without retry-after: backoff 60s
        cb.record_result(1, "gh: API rate limit exceeded", "", start_epoch);
        assert!(cb.is_open(start_epoch));
        assert_eq!(cb.record.deadline_epoch, start_epoch + 60);
        assert_eq!(cb.record.consecutive_triggers, 1);

        // Immediate subsequent call is short-circuited
        let admission_err = cb.check_admission(start_epoch + 10).unwrap_err();
        assert!(admission_err.is_gh_rate_limit());
        assert_eq!(cb.record.suppressed_calls, 1);

        // Third call still during cooldown
        assert!(cb.check_admission(start_epoch + 30).is_err());
        assert_eq!(cb.record.suppressed_calls, 2);

        // Advance clock past deadline -> admission allowed (probe)
        assert!(cb.check_admission(start_epoch + 61).is_ok());

        // Probe fails with secondary rate limit + Retry-After: 180s
        cb.record_result(
            1,
            "You have exceeded a secondary rate limit. Retry-After: 180",
            "",
            start_epoch + 61,
        );
        assert!(cb.is_open(start_epoch + 61));
        assert_eq!(cb.record.deadline_epoch, start_epoch + 61 + 180);

        // Advance clock past second deadline -> admission allowed
        assert!(cb.check_admission(start_epoch + 242).is_ok());

        // Probe succeeds -> breaker transitions to CLOSED
        cb.record_result(0, "", "[]", start_epoch + 242);
        assert!(!cb.is_open(start_epoch + 242));
        assert_eq!(cb.record.state, CircuitState::Closed);
        assert_eq!(cb.record.consecutive_triggers, 0);

        // Verify telemetry log exists and has OPEN, EXTEND/OPEN, CLOSE events
        let tel_content = std::fs::read_to_string(&tel_path).unwrap();
        assert!(tel_content.contains("GITHUB_CIRCUIT_BREAKER_OPEN"));
        assert!(tel_content.contains("GITHUB_CIRCUIT_BREAKER_CLOSE"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn circuit_breaker_persists_across_restart() {
        let temp_dir = std::env::temp_dir().join(format!("cb_persist_test_{}", current_epoch_secs()));
        let cb_path = temp_dir.join("gh_circuit_breaker.json");
        let tel_path = temp_dir.join("daemon.jsonl");

        let start_epoch = 1_700_000_000;
        {
            let mut cb = GhCircuitBreaker::new(cb_path.clone(), Some(tel_path.clone()));
            cb.record_result(1, "API rate limit exceeded", "", start_epoch);
            assert!(cb.check_admission(start_epoch + 10).is_err());
            assert_eq!(cb.record.suppressed_calls, 1);
        }

        // Simulate daemon restart: load from disk
        {
            let mut cb_reloaded = GhCircuitBreaker::load_or_default_at(cb_path.clone(), Some(tel_path.clone()));
            assert!(cb_reloaded.is_open(start_epoch + 20));
            assert_eq!(cb_reloaded.record.deadline_epoch, start_epoch + 60);
            assert_eq!(cb_reloaded.record.consecutive_triggers, 1);
            assert_eq!(cb_reloaded.record.suppressed_calls, 1);

            // Call again -> suppressed_calls increases to 2
            assert!(cb_reloaded.check_admission(start_epoch + 30).is_err());
            assert_eq!(cb_reloaded.record.suppressed_calls, 2);
        }

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
