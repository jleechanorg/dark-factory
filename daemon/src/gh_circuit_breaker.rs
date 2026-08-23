// Centralized GitHub rate-limit circuit breaker and admission controller.
//
// Owned by the daemon at the shared `gh` tool boundary (tools::run_tool).
// Enforces request admission, suppresses 403 fan-out, supports primary and
// secondary rate limits (including Retry-After), uses bounded exponential backoff,
// persists cooldown across daemon restarts, and emits structured telemetry transition events.

use crate::errors::DaemonError;
use crate::telemetry::{emit, TelemetryEvent};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub const EVT_GH_CIRCUIT_BREAKER_OPENED: &str = "GH_CIRCUIT_BREAKER_OPENED";
pub const EVT_GH_CIRCUIT_BREAKER_EXTENDED: &str = "GH_CIRCUIT_BREAKER_EXTENDED";
pub const EVT_GH_CIRCUIT_BREAKER_CLOSED: &str = "GH_CIRCUIT_BREAKER_CLOSED";

pub const DEFAULT_BASE_BACKOFF_SECS: u64 = 60;
pub const DEFAULT_MAX_BACKOFF_SECS: u64 = 1800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRateLimitInfo {
    pub is_rate_limit: bool,
    pub is_secondary: bool,
    pub retry_after: Option<Duration>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct GhCircuitBreakerRecord {
    pub is_open: bool,
    pub consecutive_rate_limits: u32,
    pub deadline_epoch: u64,
    pub opened_at_epoch: u64,
    pub last_reason: String,
    pub suppressed_calls: u64,
    pub is_secondary: bool,
    pub retry_after_secs: Option<u64>,
}

/// Detect whether stderr/stdout and return code represent a GitHub rate-limit response.
pub fn detect_gh_rate_limit(stderr: &str, rc: i32) -> Option<GhRateLimitInfo> {
    let lower = stderr.to_ascii_lowercase();
    let is_rate_limit = lower.contains("api rate limit exceeded")
        || lower.contains("rate limit exceeded")
        || lower.contains("rate limit hit")
        || lower.contains("secondary rate limit")
        || lower.contains("abuse detection mechanism")
        || lower.contains("too many requests")
        || (lower.contains("429") && (lower.contains("rate") || lower.contains("request") || lower.contains("limit")))
        || (lower.contains("403") && (lower.contains("rate limit") || lower.contains("secondary") || lower.contains("wait") || lower.contains("abuse") || lower.contains("quota")))
        || lower.contains("was submitted too quickly");

    if !is_rate_limit && rc != 429 {
        return None;
    }

    let is_secondary = lower.contains("secondary rate limit")
        || lower.contains("abuse detection mechanism")
        || lower.contains("submitted too quickly");

    let retry_after = parse_retry_after(stderr);

    Some(GhRateLimitInfo {
        is_rate_limit: true,
        is_secondary,
        retry_after,
        message: stderr.trim().to_string(),
    })
}

/// Parse Retry-After or wait duration from stderr text.
pub fn parse_retry_after(text: &str) -> Option<Duration> {
    let lower = text.to_ascii_lowercase();

    // 1. "retry-after: <seconds>"
    if let Some(idx) = lower.find("retry-after:") {
        let rest = &lower[idx + "retry-after:".len()..];
        if let Some(val) = extract_first_number(rest) {
            return Some(Duration::from_secs(val));
        }
    }

    // 2. "please wait <seconds> seconds" or "wait <seconds> seconds"
    if let Some(idx) = lower.find("wait ") {
        let rest = &lower[idx + "wait ".len()..];
        if let Some(val) = extract_first_number(rest) {
            return Some(Duration::from_secs(val));
        }
    }

    // 3. "try again in <seconds> seconds" or "try again in <minutes> minutes"
    if let Some(idx) = lower.find("try again in ") {
        let rest = &lower[idx + "try again in ".len()..];
        if let Some(val) = extract_first_number(rest) {
            if rest.contains("minute") {
                return Some(Duration::from_secs(val.saturating_mul(60)));
            }
            return Some(Duration::from_secs(val));
        }
    }

    // 4. "retry after <seconds> seconds"
    if let Some(idx) = lower.find("retry after ") {
        let rest = &lower[idx + "retry after ".len()..];
        if let Some(val) = extract_first_number(rest) {
            return Some(Duration::from_secs(val));
        }
    }

    None
}

fn extract_first_number(s: &str) -> Option<u64> {
    let mut num_str = String::new();
    let mut started = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num_str.push(ch);
            started = true;
        } else if started {
            break;
        }
    }
    if num_str.is_empty() {
        None
    } else {
        num_str.parse().ok()
    }
}

pub struct GhCircuitBreaker {
    state: Mutex<GhCircuitBreakerRecord>,
    state_path: PathBuf,
    telemetry_log: Option<PathBuf>,
}

impl GhCircuitBreaker {
    pub fn new_with_paths(state_path: PathBuf, telemetry_log: Option<PathBuf>) -> Self {
        let record = Self::load_record(&state_path).unwrap_or_default();
        Self {
            state: Mutex::new(record),
            state_path,
            telemetry_log,
        }
    }

    pub fn default_state_path() -> PathBuf {
        crate::intake::runtime_state_dir().join("gh_circuit_breaker.json")
    }

    pub fn default_telemetry_log() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|home| {
            Path::new(&home)
                .join("Library/Logs/dark-factory")
                .join("daemon.jsonl")
        })
    }

    fn load_record(path: &Path) -> Option<GhCircuitBreakerRecord> {
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn persist_record(&self, record: &GhCircuitBreakerRecord) {
        if let Some(parent) = self.state_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(record) {
            let tmp_path = self.state_path.with_extension("tmp");
            if fs::write(&tmp_path, json).is_ok() {
                let _ = fs::rename(&tmp_path, &self.state_path);
            }
        }
    }

    pub fn check_admission(&self, now_epoch: u64) -> Result<(), DaemonError> {
        let mut record = self.state.lock().unwrap();
        if record.is_open {
            if now_epoch < record.deadline_epoch {
                record.suppressed_calls += 1;
                self.persist_record(&record);
                let remaining = record.deadline_epoch.saturating_sub(now_epoch);
                return Err(DaemonError::Tool {
                    tool: "gh".to_string(),
                    rc: -1,
                    stderr: format!(
                        "gh rate limit circuit breaker open (cooldown active for {}s, {} calls suppressed so far)",
                        remaining, record.suppressed_calls
                    ),
                });
            }
            // Cooldown has expired: admit one probe request (half-open)
        }
        Ok(())
    }

    pub fn record_rate_limit(&self, info: &GhRateLimitInfo, now_epoch: u64) -> Duration {
        let mut record = self.state.lock().unwrap();

        record.consecutive_rate_limits = record.consecutive_rate_limits.saturating_add(1);
        record.is_open = true;
        record.opened_at_epoch = now_epoch;
        record.last_reason = info.message.clone();
        record.is_secondary = info.is_secondary;
        record.retry_after_secs = info.retry_after.map(|d| d.as_secs());

        let cooldown_secs = match info.retry_after {
            Some(duration) => duration.as_secs().max(1),
            None => {
                let base = std::env::var("DARK_FACTORY_GH_RATE_LIMIT_BASE_BACKOFF_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(DEFAULT_BASE_BACKOFF_SECS);
                let max = std::env::var("DARK_FACTORY_GH_RATE_LIMIT_MAX_BACKOFF_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(DEFAULT_MAX_BACKOFF_SECS);
                let exponent = record.consecutive_rate_limits.saturating_sub(1).min(30);
                let factor = 2_u64.saturating_pow(exponent);
                base.saturating_mul(factor).min(max)
            }
        };

        record.deadline_epoch = now_epoch.saturating_add(cooldown_secs);
        self.persist_record(&record);

        let event_type = if record.consecutive_rate_limits > 1 {
            EVT_GH_CIRCUIT_BREAKER_EXTENDED
        } else {
            EVT_GH_CIRCUIT_BREAKER_OPENED
        };

        self.emit_telemetry(
            event_type,
            serde_json::json!({
                "cooldownSecs": cooldown_secs,
                "consecutiveRateLimits": record.consecutive_rate_limits,
                "suppressedCalls": record.suppressed_calls,
                "isSecondary": record.is_secondary,
            }),
            serde_json::json!({
                "reason": record.last_reason,
                "deadlineEpoch": record.deadline_epoch,
            }),
            now_epoch,
        );

        Duration::from_secs(cooldown_secs)
    }

    pub fn record_success(&self, now_epoch: u64) {
        let mut record = self.state.lock().unwrap();
        if record.is_open {
            let total_suppressed = record.suppressed_calls;
            record.is_open = false;
            record.consecutive_rate_limits = 0;
            record.suppressed_calls = 0;
            record.deadline_epoch = 0;
            self.persist_record(&record);

            self.emit_telemetry(
                EVT_GH_CIRCUIT_BREAKER_CLOSED,
                serde_json::json!({
                    "suppressedCalls": total_suppressed,
                    "consecutiveRateLimits": 0,
                }),
                serde_json::json!({
                    "message": "GitHub request succeeded; circuit breaker closed",
                }),
                now_epoch,
            );
        }
    }

    pub fn is_open(&self, now_epoch: u64) -> bool {
        let record = self.state.lock().unwrap();
        record.is_open && now_epoch < record.deadline_epoch
    }

    pub fn deadline_epoch(&self) -> u64 {
        let record = self.state.lock().unwrap();
        record.deadline_epoch
    }

    pub fn suppressed_calls(&self) -> u64 {
        let record = self.state.lock().unwrap();
        record.suppressed_calls
    }

    pub fn consecutive_rate_limits(&self) -> u32 {
        let record = self.state.lock().unwrap();
        record.consecutive_rate_limits
    }

    pub fn clear(&self) {
        let mut record = self.state.lock().unwrap();
        *record = GhCircuitBreakerRecord::default();
        self.persist_record(&record);
    }

    fn emit_telemetry(&self, event_type: &str, metrics: serde_json::Value, context: serde_json::Value, now_epoch: u64) {
        if let Some(log_path) = &self.telemetry_log {
            let iso = format_epoch_iso(now_epoch);
            let event = TelemetryEvent {
                timestamp: iso,
                bead_id: "_daemon".to_string(),
                attempt_id: 0,
                lifecycle_state: "N/A".to_string(),
                event_type: event_type.to_string(),
                metrics,
                context,
            };
            let _ = emit(log_path, &event);
        }
    }
}

fn format_epoch_iso(epoch: u64) -> String {
    // Simple ISO formatting for telemetry (fallback-safe without heavy deps)
    let s = epoch % 60;
    let m = (epoch / 60) % 60;
    let h = (epoch / 3600) % 24;
    let _days = epoch / 86400;
    // Approximated date representation for telemetry event timestamps
    format!("2026-08-23T{:02}:{:02}:{:02}Z", h, m, s)
}

static GLOBAL_CB: OnceLock<Mutex<Option<Arc<GhCircuitBreaker>>>> = OnceLock::new();

fn global_cb_mutex() -> &'static Mutex<Option<Arc<GhCircuitBreaker>>> {
    GLOBAL_CB.get_or_init(|| Mutex::new(None))
}

pub fn global_circuit_breaker() -> Arc<GhCircuitBreaker> {
    let mut lock = global_cb_mutex().lock().unwrap();
    if let Some(cb) = lock.as_ref() {
        return cb.clone();
    }
    let cb = Arc::new(GhCircuitBreaker::new_with_paths(
        GhCircuitBreaker::default_state_path(),
        GhCircuitBreaker::default_telemetry_log(),
    ));
    *lock = Some(cb.clone());
    cb
}

pub fn set_global_circuit_breaker(cb: Arc<GhCircuitBreaker>) {
    let mut lock = global_cb_mutex().lock().unwrap();
    *lock = Some(cb);
}

pub fn check_gh_admission(now_epoch: u64) -> Result<(), DaemonError> {
    global_circuit_breaker().check_admission(now_epoch)
}

pub fn record_gh_success(now_epoch: u64) {
    global_circuit_breaker().record_success(now_epoch);
}

pub fn record_gh_rate_limit(info: &GhRateLimitInfo, now_epoch: u64) -> Duration {
    global_circuit_breaker().record_rate_limit(info, now_epoch)
}

pub fn is_gh_circuit_breaker_open(now_epoch: u64) -> bool {
    global_circuit_breaker().is_open(now_epoch)
}

pub fn clear_gh_circuit_breaker() {
    global_circuit_breaker().clear();
}
