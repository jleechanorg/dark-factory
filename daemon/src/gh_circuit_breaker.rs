//! Centralized GitHub request-admission and rate-limit circuit-breaker component.
//!
//! Owns rate-limit detection (primary + secondary), Retry-After parsing,
//! bounded exponential backoff, and short-circuit admission control at the shared `gh`
//! tool boundary. Persists state across daemon restarts and emits structured telemetry
//! transitions on OPEN, EXTEND, and CLOSE without fanning out 403s across repositories.

use crate::errors::DaemonError;
use crate::intake::runtime_state_dir;
use crate::telemetry::{self, TelemetryEvent};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const BASE_COOLDOWN_SECS: u64 = 60;
pub const MAX_COOLDOWN_SECS: u64 = 1800; // 30 minutes
pub const STATE_FILE_NAME: &str = "gh_circuit_breaker.json";

pub const EVT_OPENED: &str = "GH_CIRCUIT_BREAKER_OPENED";
pub const EVT_EXTENDED: &str = "GH_CIRCUIT_BREAKER_EXTENDED";
pub const EVT_CLOSED: &str = "GH_CIRCUIT_BREAKER_CLOSED";

static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

struct StateFileLock {
    path: PathBuf,
    file: File,
}

impl StateFileLock {
    fn acquire(state_path: &Path) -> Result<Self, DaemonError> {
        let lock_path = state_path.with_extension("json.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| DaemonError::Config(format!("open gh_circuit_breaker lock: {error}")))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            const LOCK_EX: i32 = 2;
            let result = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
            if result != 0 {
                return Err(DaemonError::Config(format!(
                    "acquire gh_circuit_breaker lock: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        Ok(Self { path: lock_path, file })
    }
}

impl Drop for StateFileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            const LOCK_UN: i32 = 8;
            let _ = unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CircuitBreakerState {
    Closed,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GhCircuitBreakerRecord {
    pub state: CircuitBreakerState,
    pub deadline_epoch: u64,
    pub backoff_step: u32,
    pub consecutive_rate_limits: u32,
    pub suppressed_calls: u64,
    pub last_reason: String,
    pub opened_at_epoch: u64,
    #[serde(default)]
    pub retry_after_secs: Option<u64>,
}

impl Default for GhCircuitBreakerRecord {
    fn default() -> Self {
        Self {
            state: CircuitBreakerState::Closed,
            deadline_epoch: 0,
            backoff_step: 0,
            consecutive_rate_limits: 0,
            suppressed_calls: 0,
            last_reason: String::new(),
            opened_at_epoch: 0,
            retry_after_secs: None,
        }
    }
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn epoch_to_iso8601(epoch: u64) -> String {
    let days = (epoch / 86400) as i64;
    let rem = epoch % 86400;
    let h = rem / 3600;
    let min = (rem % 3600) / 60;
    let s = rem % 60;

    // Convert days since UNIX_EPOCH (1970-01-01) to Y-M-D
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// Detects both primary and secondary GitHub rate-limit errors from subprocess exit code and stderr/stdout.
pub fn is_gh_rate_limit_error(rc: i32, stderr: &str, stdout: &str) -> bool {
    if rc == 429 {
        return true;
    }
    let lower_err = stderr.to_ascii_lowercase();
    let lower_out = stdout.to_ascii_lowercase();
    let check = |s: &str| {
        s.contains("api rate limit exceeded")
            || s.contains("rate limit exceeded")
            || s.contains("rate limit hit")
            || s.contains("secondary rate limit")
            || s.contains("abuse detection mechanism")
            || s.contains("was submitted too quickly")
            || s.contains("please wait a few minutes")
            || s.contains("circuit breaker open")
            || s.contains("rate limit circuit breaker")
            || (s.contains("403") && (s.contains("rate limit") || s.contains("secondary") || s.contains("abuse") || s.contains("wait")))
            || (s.contains("429") && s.contains("too many requests"))
            || s.contains("too many requests")
    };
    check(&lower_err) || check(&lower_out)
}

/// Extracts `Retry-After` seconds or rate limit reset timestamp from stderr / stdout if available.
pub fn parse_retry_after(stderr: &str, stdout: &str, now_epoch: u64) -> Option<u64> {
    let combined = format!("{stderr}\n{stdout}");
    let lower = combined.to_ascii_lowercase();

    // 1. "retry-after: <digits>"
    if let Some(idx) = lower.find("retry-after:") {
        let rest = &combined[idx + 12..].trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(secs) = digits.parse::<u64>() {
            if secs > 0 {
                return Some(secs.min(MAX_COOLDOWN_SECS));
            }
        }
    }

    // 2. "retry after <digits>"
    if let Some(idx) = lower.find("retry after ") {
        let rest = &combined[idx + 12..].trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(secs) = digits.parse::<u64>() {
            if secs > 0 {
                return Some(secs.min(MAX_COOLDOWN_SECS));
            }
        }
    }

    // 3. "please wait <digits> minutes"
    if let Some(idx) = lower.find("please wait ") {
        let rest = &lower[idx + 12..].trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = digits.parse::<u64>() {
            let after_digits = rest[digits.len()..].trim_start();
            if after_digits.starts_with("minute") {
                return Some((num * 60).min(MAX_COOLDOWN_SECS));
            } else if after_digits.starts_with("second") || after_digits.starts_with('s') {
                return Some(num.min(MAX_COOLDOWN_SECS));
            }
        }
    }

    // 4. "wait <digits> seconds"
    if let Some(idx) = lower.find("wait ") {
        let rest = &lower[idx + 5..].trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = digits.parse::<u64>() {
            let after_digits = rest[digits.len()..].trim_start();
            if after_digits.starts_with("second") || after_digits.starts_with('s') {
                return Some(num.min(MAX_COOLDOWN_SECS));
            } else if after_digits.starts_with("minute") {
                return Some((num * 60).min(MAX_COOLDOWN_SECS));
            }
        }
    }

    // 5. "x-ratelimit-reset: <epoch>"
    if let Some(idx) = lower.find("x-ratelimit-reset:") {
        let rest = &combined[idx + 18..].trim_start();
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(reset_epoch) = digits.parse::<u64>() {
            if reset_epoch > now_epoch {
                let diff = reset_epoch.saturating_sub(now_epoch);
                return Some(diff.clamp(1, MAX_COOLDOWN_SECS));
            }
        }
    }

    None
}

fn default_state_path() -> PathBuf {
    runtime_state_dir().join(STATE_FILE_NAME)
}

fn default_telemetry_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        Path::new(&home)
            .join("Library/Logs/dark-factory")
            .join("daemon.jsonl")
    } else {
        PathBuf::from("daemon.jsonl")
    }
}

pub struct GhCircuitBreaker {
    path: PathBuf,
    telemetry_log: PathBuf,
    inner: Mutex<GhCircuitBreakerRecord>,
}

impl GhCircuitBreaker {
    pub fn new(path: PathBuf, telemetry_log: PathBuf) -> Self {
        let record = Self::load_record(&path);
        Self {
            path,
            telemetry_log,
            inner: Mutex::new(record),
        }
    }

    pub fn load_or_default() -> Self {
        Self::new(default_state_path(), default_telemetry_path())
    }

    pub fn load_or_default_at(path: impl AsRef<Path>) -> Self {
        let p = path.as_ref().to_path_buf();
        Self::new(p, default_telemetry_path())
    }

    fn load_record(path: &Path) -> GhCircuitBreakerRecord {
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(record) = serde_json::from_str::<GhCircuitBreakerRecord>(&raw) {
                return record;
            }
        }
        GhCircuitBreakerRecord::default()
    }

    pub fn persist_record(path: &Path, record: &GhCircuitBreakerRecord) -> Result<(), DaemonError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                DaemonError::Config(format!("create gh_circuit_breaker directory: {error}"))
            })?;
        }
        let _lock = StateFileLock::acquire(path)?;
        let json = serde_json::to_string(record)
            .map_err(|error| DaemonError::Parse(format!("serialize gh_circuit_breaker: {error}")))?;
        let nonce = CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!(
            "json.tmp.{}.{}",
            std::process::id(),
            nonce
        ));
        std::fs::write(&tmp, json.as_bytes())
            .map_err(|e| DaemonError::Config(format!("write gh_circuit_breaker: {e}")))?;
        if let Err(error) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(DaemonError::Config(format!(
                "rename gh_circuit_breaker tmp->final: {error}"
            )));
        }
        Ok(())
    }

    pub fn state(&self) -> CircuitBreakerState {
        self.inner.lock().unwrap().state
    }

    pub fn deadline_epoch(&self) -> u64 {
        self.inner.lock().unwrap().deadline_epoch
    }

    pub fn consecutive_rate_limits(&self) -> u32 {
        self.inner.lock().unwrap().consecutive_rate_limits
    }

    pub fn suppressed_calls(&self) -> u64 {
        self.inner.lock().unwrap().suppressed_calls
    }

    pub fn reset(&self) {
        let mut lock = self.inner.lock().unwrap();
        *lock = GhCircuitBreakerRecord::default();
        let _ = Self::persist_record(&self.path, &lock);
    }

    /// Check if a `gh` call is admitted.
    /// If open and cooldown has not expired, increments `suppressed_calls` and returns an error.
    /// If closed or cooldown has expired, allows the request.
    pub fn check_admission(&self, now_epoch: u64) -> Result<(), DaemonError> {
        let mut lock = self.inner.lock().unwrap();
        if lock.state == CircuitBreakerState::Open {
            if now_epoch < lock.deadline_epoch {
                lock.suppressed_calls += 1;
                let _ = Self::persist_record(&self.path, &lock);
                let deadline_iso = epoch_to_iso8601(lock.deadline_epoch);
                return Err(DaemonError::Tool {
                    tool: "gh".to_string(),
                    rc: 403,
                    stderr: format!(
                        "github rate limit circuit breaker open until {deadline_iso} (suppressed {} calls)",
                        lock.suppressed_calls
                    ),
                });
            }
            // Cooldown has expired: allow exactly ONE probe request
            return Ok(());
        }
        Ok(())
    }

    /// Record the outcome of an executed `gh` command.
    pub fn record_result(
        &self,
        rc: i32,
        stdout: &str,
        stderr: &str,
        now_epoch: u64,
    ) -> Result<(), DaemonError> {
        let mut lock = self.inner.lock().unwrap();
        if is_gh_rate_limit_error(rc, stderr, stdout) {
            let retry_after_opt = parse_retry_after(stderr, stdout, now_epoch);
            lock.consecutive_rate_limits += 1;
            let cooldown_secs = match retry_after_opt {
                Some(secs) => secs.clamp(5, MAX_COOLDOWN_SECS),
                None => {
                    let exp = lock.consecutive_rate_limits.saturating_sub(1).min(30);
                    BASE_COOLDOWN_SECS.saturating_mul(1u64 << exp).min(MAX_COOLDOWN_SECS)
                }
            };
            lock.deadline_epoch = now_epoch + cooldown_secs;
            lock.retry_after_secs = retry_after_opt;
            lock.last_reason = if !stderr.is_empty() {
                stderr.chars().take(200).collect()
            } else if !stdout.is_empty() {
                stdout.chars().take(200).collect()
            } else {
                "rate_limit_403".to_string()
            };

            let is_extension = lock.state == CircuitBreakerState::Open;
            lock.state = CircuitBreakerState::Open;
            if !is_extension {
                lock.opened_at_epoch = now_epoch;
                lock.suppressed_calls = 0;
            }

            let _ = Self::persist_record(&self.path, &lock);

            let event_type = if is_extension { EVT_EXTENDED } else { EVT_OPENED };
            let ev = TelemetryEvent {
                timestamp: epoch_to_iso8601(now_epoch),
                bead_id: "system".to_string(),
                attempt_id: 0,
                lifecycle_state: "CIRCUIT_BREAKER".to_string(),
                event_type: event_type.to_string(),
                metrics: serde_json::json!({
                    "suppressedCalls": lock.suppressed_calls,
                    "consecutiveRateLimits": lock.consecutive_rate_limits,
                    "cooldownSecs": cooldown_secs,
                }),
                context: serde_json::json!({
                    "deadline": epoch_to_iso8601(lock.deadline_epoch),
                    "deadlineEpoch": lock.deadline_epoch,
                    "reason": lock.last_reason,
                    "retryAfterSecs": lock.retry_after_secs,
                }),
            };
            let _ = telemetry::emit(&self.telemetry_log, &ev);
            return Ok(());
        }

        // Non-rate-limit result
        if rc == 0 && lock.state == CircuitBreakerState::Open {
            // Cooldown trial succeeded -> close circuit breaker
            let suppressed = lock.suppressed_calls;
            lock.state = CircuitBreakerState::Closed;
            lock.consecutive_rate_limits = 0;
            lock.backoff_step = 0;
            lock.deadline_epoch = 0;
            lock.suppressed_calls = 0;
            lock.retry_after_secs = None;

            let _ = Self::persist_record(&self.path, &lock);

            let ev = TelemetryEvent {
                timestamp: epoch_to_iso8601(now_epoch),
                bead_id: "system".to_string(),
                attempt_id: 0,
                lifecycle_state: "CIRCUIT_BREAKER".to_string(),
                event_type: EVT_CLOSED.to_string(),
                metrics: serde_json::json!({
                    "suppressedCalls": suppressed,
                    "consecutiveRateLimits": 0,
                }),
                context: serde_json::json!({
                    "reason": "success_probe",
                }),
            };
            let _ = telemetry::emit(&self.telemetry_log, &ev);
        }

        Ok(())
    }
}

static GLOBAL_CIRCUIT_BREAKER: Mutex<Option<GhCircuitBreaker>> = Mutex::new(None);

fn with_global_breaker<R>(f: impl FnOnce(&GhCircuitBreaker) -> R) -> R {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap();
    if lock.is_none() {
        *lock = Some(GhCircuitBreaker::load_or_default());
    }
    f(lock.as_ref().unwrap())
}

pub fn check_admission(now_epoch: Option<u64>) -> Result<(), DaemonError> {
    let now = now_epoch.unwrap_or_else(now_epoch_secs);
    with_global_breaker(|cb| cb.check_admission(now))
}

pub fn record_gh_result(
    rc: i32,
    stdout: &str,
    stderr: &str,
    now_epoch: Option<u64>,
) -> Result<(), DaemonError> {
    let now = now_epoch.unwrap_or_else(now_epoch_secs);
    with_global_breaker(|cb| cb.record_result(rc, stdout, stderr, now))
}

pub fn reset_global() {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap();
    if let Some(cb) = lock.as_ref() {
        cb.reset();
    } else {
        let cb = GhCircuitBreaker::load_or_default();
        cb.reset();
        *lock = Some(cb);
    }
}

pub fn set_global_circuit_breaker(cb: GhCircuitBreaker) {
    let mut lock = GLOBAL_CIRCUIT_BREAKER.lock().unwrap();
    *lock = Some(cb);
}

pub fn is_open() -> bool {
    with_global_breaker(|cb| cb.state() == CircuitBreakerState::Open)
}

pub fn suppressed_calls() -> u64 {
    with_global_breaker(|cb| cb.suppressed_calls())
}

pub fn consecutive_rate_limits() -> u32 {
    with_global_breaker(|cb| cb.consecutive_rate_limits())
}

pub fn deadline_epoch() -> u64 {
    with_global_breaker(|cb| cb.deadline_epoch())
}
