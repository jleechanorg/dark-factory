use crate::errors::DaemonError;
use crate::telemetry::{self, TelemetryEvent};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_MIN_COOLDOWN_SECS: u64 = 60;
pub const DEFAULT_MAX_COOLDOWN_SECS: u64 = 1800; // 30 minutes

pub const EVT_GH_CIRCUIT_BREAKER_OPENED: &str = "GH_CIRCUIT_BREAKER_OPENED";
pub const EVT_GH_CIRCUIT_BREAKER_EXTENDED: &str = "GH_CIRCUIT_BREAKER_EXTENDED";
pub const EVT_GH_CIRCUIT_BREAKER_CLOSED: &str = "GH_CIRCUIT_BREAKER_CLOSED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitSignal {
    pub is_secondary: bool,
    pub retry_after: Option<Duration>,
    pub reason: String,
    pub matched_phrase: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PersistedCircuitBreakerState {
    pub deadline_epoch_secs: Option<u64>,
    pub consecutive_trips: u32,
    pub suppressed_calls: u64,
    pub reason: String,
    pub updated_at_epoch_secs: u64,
}

pub fn parse_retry_after(text: &str) -> Option<Duration> {
    let lower = text.to_ascii_lowercase();

    // Check "retry-after:" or "retry-after"
    if let Some(idx) = lower.find("retry-after") {
        let tail = &lower[idx + "retry-after".len()..];
        let tail = tail.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        if let Some(num_str) = tail.split(|c: char| !c.is_ascii_digit()).next() {
            if let Ok(secs) = num_str.parse::<u64>() {
                if secs > 0 {
                    return Some(Duration::from_secs(secs));
                }
            }
        }
    }

    // Check "retry after <N>" or "retry in <N>"
    for marker in &["retry after", "retry in"] {
        if let Some(idx) = lower.find(marker) {
            let tail = lower[idx + marker.len()..].trim_start();
            let mut parts = tail.split_whitespace();
            if let Some(num_str) = parts.next() {
                let clean_num = num_str.trim_matches(|c: char| !c.is_ascii_digit());
                if let Ok(secs) = clean_num.parse::<u64>() {
                    if secs > 0 {
                        if let Some(unit) = parts.next() {
                            let clean_unit = unit.trim_matches(|c: char| !c.is_ascii_alphanumeric());
                            if clean_unit.starts_with("min") || clean_unit == "m" {
                                return Some(Duration::from_secs(secs * 60));
                            }
                            if clean_unit.starts_with("hour") || clean_unit == "h" {
                                return Some(Duration::from_secs(secs * 3600));
                            }
                        }
                        return Some(Duration::from_secs(secs));
                    }
                }
            }
        }
    }

    // Check "please wait <N> minutes/seconds" or "wait <N> minutes/seconds"
    for marker in &["please wait", "wait"] {
        if let Some(idx) = lower.find(marker) {
            let tail = lower[idx + marker.len()..].trim_start();
            let mut parts = tail.split_whitespace();
            if let Some(num_str) = parts.next() {
                let clean_num = num_str.trim_matches(|c: char| !c.is_ascii_digit());
                if let Ok(val) = clean_num.parse::<u64>() {
                    if val > 0 {
                        if let Some(unit) = parts.next() {
                            let clean_unit = unit.trim_matches(|c: char| !c.is_ascii_alphanumeric());
                            if clean_unit.starts_with("min") || clean_unit == "m" {
                                return Some(Duration::from_secs(val * 60));
                            }
                            if clean_unit.starts_with("sec") || clean_unit == "s" {
                                return Some(Duration::from_secs(val));
                            }
                            if clean_unit.starts_with("hour") || clean_unit == "h" {
                                return Some(Duration::from_secs(val * 3600));
                            }
                        }
                    }
                }
            }
        }
    }

    // Check "x-ratelimit-reset:"
    if let Some(idx) = lower.find("x-ratelimit-reset") {
        let tail = &lower[idx + "x-ratelimit-reset".len()..];
        let tail = tail.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        if let Some(num_str) = tail.split(|c: char| !c.is_ascii_digit()).next() {
            if let Ok(reset_epoch) = num_str.parse::<u64>() {
                let now_epoch = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if reset_epoch > now_epoch {
                    return Some(Duration::from_secs(reset_epoch - now_epoch));
                }
            }
        }
    }

    None
}

/// Classification of a failed `gh` invocation.
///
/// Bead rev-x92c8: the circuit breaker used to funnel *every* `gh` failure
/// whose stderr merely CONTAINED a rate-limit-ish token (`rate limit`,
/// `rate_limit`, `ratelimit`, `retry-after`) into `primary_rate_limit`.
/// That substring net catches things that are not rate limits at all:
/// the `https://api.github.com/rate_limit` probe URL echoed back inside a
/// timeout/DNS error, an `x-ratelimit-remaining: 4209` header echoed inside a
/// permission 403, and — worst — this breaker's OWN suppression stderr
/// ("gh call suppressed by rate limit circuit breaker ..."), which made the
/// breaker self-feeding. Live symptom: `GH_CIRCUIT_BREAKER_OPENED`
/// `reason=primary_rate_limit`, `retry_after_secs=null` every few seconds
/// while `gh api rate_limit` reported core 2692/5000 and graphql 4209/5000
/// remaining.
///
/// Every failure now gets its own reason string, and only genuine GitHub
/// rate-limit evidence maps to a rate-limit reason. Note the inverse of the
/// 2026-08-17 incident: there, real quota exhaustion arrived as a 403 and was
/// misread as *auth* failure. Both directions are handled here — a 403 whose
/// body carries a rate-limit phrase IS a rate limit; a 403 without one is
/// `gh_forbidden`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhFailureKind {
    /// GitHub's primary (hourly quota) rate limit is exhausted.
    PrimaryRateLimit,
    /// GitHub's secondary / abuse-detection rate limit fired.
    SecondaryRateLimit,
    /// This daemon's own breaker suppressed the call (not a GitHub signal).
    CircuitBreakerSuppressed,
    /// 401/403 that carries no rate-limit evidence: permissions, SAML, token.
    Forbidden,
    /// 404 / missing resource.
    NotFound,
    /// The call timed out.
    Timeout,
    /// DNS/TLS/connection/empty-response failure.
    Network,
    /// Anything else.
    Other,
}

impl GhFailureKind {
    /// Stable reason string used in circuit-breaker telemetry.
    pub fn reason(self) -> &'static str {
        match self {
            Self::PrimaryRateLimit => "primary_rate_limit",
            Self::SecondaryRateLimit => "secondary_rate_limit",
            Self::CircuitBreakerSuppressed => "circuit_breaker_suppressed",
            Self::Forbidden => "gh_forbidden",
            Self::NotFound => "gh_not_found",
            Self::Timeout => "gh_timeout",
            Self::Network => "gh_network_error",
            Self::Other => "gh_error",
        }
    }

    pub fn is_rate_limit(self) -> bool {
        matches!(self, Self::PrimaryRateLimit | Self::SecondaryRateLimit)
    }
}

/// Phrases GitHub actually uses when the primary quota is exhausted.
const PRIMARY_EXHAUSTION_PHRASES: &[&str] = &[
    "api rate limit exceeded",
    "rate limit exceeded",
    "rate limit already exceeded",
    "exceeded your rate limit",
    "exceeded the rate limit",
    "exceeded a rate limit",
    "rate limit reached",
];

/// True iff stderr echoes an `x-ratelimit-remaining` header whose value is 0.
/// A non-zero value (e.g. `x-ratelimit-remaining: 4209`) is *proof the quota
/// is healthy* and must never be read as exhaustion.
fn ratelimit_remaining_is_zero(lower: &str) -> bool {
    let Some(idx) = lower.find("x-ratelimit-remaining") else {
        return false;
    };
    let tail = &lower[idx + "x-ratelimit-remaining".len()..];
    let tail = tail.trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace());
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u64>().map(|n| n == 0).unwrap_or(false)
}

/// The words "rate limit" / "rate-limit" as a PHRASE. Deliberately excludes
/// the `rate_limit` (REST probe URL path) and `ratelimit` (HTTP header) token
/// spellings: both appear constantly in failures that are not rate limits.
fn mentions_rate_limit_phrase(lower: &str) -> bool {
    lower.contains("rate limit") || lower.contains("rate-limit")
}

/// Classify a failed `gh` invocation from its stderr and process exit code.
///
/// `rc` is the *process* exit code (gh exits 1 for most API errors), except
/// for the synthetic suppression error this module raises with `rc = 403`.
/// HTTP status therefore has to be read out of the stderr text.
pub fn classify_gh_failure(stderr: &str, rc: i32) -> GhFailureKind {
    let lower = stderr.to_ascii_lowercase();

    // 0. Our own suppression error. Must be first: its text mentions "rate
    //    limit", so any later check would re-classify it as a GitHub signal
    //    and let the breaker re-trip on its own output.
    if lower.contains("circuit breaker") {
        return GhFailureKind::CircuitBreakerSuppressed;
    }

    // 1. Secondary / abuse-detection limit — explicit GitHub wording.
    if lower.contains("secondary rate limit")
        || lower.contains("abuse detection")
        || lower.contains("please wait a few minutes before you try again")
    {
        return GhFailureKind::SecondaryRateLimit;
    }

    // 2. Primary limit — requires explicit exhaustion evidence, never a bare
    //    token match.
    let has_403 = rc == 403 || lower.contains("403");
    // Bare "429" is not usable: hex SHAs echoed in gh stderr contain digit
    // runs. Require the HTTP status in context.
    let has_429 = rc == 429 || lower.contains("http 429") || lower.contains("too many requests");
    let is_primary = PRIMARY_EXHAUSTION_PHRASES.iter().any(|p| lower.contains(p))
        || ratelimit_remaining_is_zero(&lower)
        // GraphQL error type RATE_LIMITED (distinct from the `rate_limit`
        // REST path, which has no trailing "ed").
        || lower.contains("rate_limited")
        || has_429
        // A 403 whose body talks about rate limiting IS a rate limit — the
        // 2026-08-17 direction. A 403 without that phrase is not.
        || (has_403 && mentions_rate_limit_phrase(&lower))
        // An explicit Retry-After header alongside a throttling status.
        || (lower.contains("retry-after") && has_403 && parse_retry_after(stderr).is_some());
    if is_primary {
        return GhFailureKind::PrimaryRateLimit;
    }

    // 3. Everything else gets its own reason instead of being laundered into
    //    a rate-limit trip.
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline exceeded")
    {
        return GhFailureKind::Timeout;
    }
    if lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("could not resolve host")
        || lower.contains("no such host")
        || lower.contains("dial tcp")
        || lower.contains("tls handshake")
        || lower.contains("network error")
        || lower.contains("empty response")
    {
        return GhFailureKind::Network;
    }
    if has_403
        || lower.contains("401")
        || lower.contains("bad credentials")
        || lower.contains("resource not accessible")
    {
        return GhFailureKind::Forbidden;
    }
    if lower.contains("404") || lower.contains("not found") {
        return GhFailureKind::NotFound;
    }
    GhFailureKind::Other
}

pub fn parse_rate_limit_error(stderr: &str, rc: i32) -> Option<RateLimitSignal> {
    let kind = classify_gh_failure(stderr, rc);
    if !kind.is_rate_limit() {
        return None;
    }

    let reason = kind.reason().to_string();
    Some(RateLimitSignal {
        is_secondary: kind == GhFailureKind::SecondaryRateLimit,
        retry_after: parse_retry_after(stderr),
        reason: reason.clone(),
        matched_phrase: reason,
    })
}

pub fn compute_cooldown(consecutive_trips: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(retry_after) = retry_after {
        if retry_after.as_secs() > 0 {
            return retry_after;
        }
    }
    // Exponential backoff: BASE * 2^consecutive_trips, clamped between MIN and MAX
    let shift = consecutive_trips.min(10);
    let mult = 1u64.checked_shl(shift).unwrap_or(1024);
    let secs = (DEFAULT_MIN_COOLDOWN_SECS.saturating_mul(mult))
        .clamp(DEFAULT_MIN_COOLDOWN_SECS, DEFAULT_MAX_COOLDOWN_SECS);
    Duration::from_secs(secs)
}

pub fn default_state_file_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DARK_FACTORY_GH_CIRCUIT_BREAKER_PATH") {
        return PathBuf::from(path);
    }
    crate::intake::runtime_state_dir().join("gh_circuit_breaker.json")
}

pub fn default_telemetry_log_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DARK_FACTORY_TELEMETRY_LOG") {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home)
            .join("Library/Logs/dark-factory")
            .join("daemon.jsonl");
    }
    std::env::temp_dir().join("dark-factory").join("daemon.jsonl")
}

fn emit_transition_telemetry(
    log_path: Option<&Path>,
    event_type: &str,
    metrics: serde_json::Value,
    context: serde_json::Value,
) {
    let fallback_path = default_telemetry_log_path();
    let path = log_path.unwrap_or(&fallback_path);

    let ts = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => format!("{}s", d.as_secs()),
        Err(_) => "1970-01-01T00:00:00Z".to_string(),
    };
    let event = TelemetryEvent {
        timestamp: ts,
        host: telemetry::local_hostname(),
        bead_id: "system".to_string(),
        attempt_id: 0,
        lifecycle_state: "SYSTEM".to_string(),
        event_type: event_type.to_string(),
        metrics,
        context,
    };
    let _ = telemetry::emit(path, &event);
}

pub struct GhCircuitBreaker {
    pub deadline: Option<SystemTime>,
    pub consecutive_trips: u32,
    pub suppressed_calls: u64,
    pub last_reason: Option<String>,
    pub last_retry_after: Option<u64>,
    pub state_file_path: Option<PathBuf>,
    pub telemetry_log_path: Option<PathBuf>,
}

impl Default for GhCircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

impl GhCircuitBreaker {
    pub fn new() -> Self {
        let mut cb = Self {
            deadline: None,
            consecutive_trips: 0,
            suppressed_calls: 0,
            last_reason: None,
            last_retry_after: None,
            state_file_path: None,
            telemetry_log_path: None,
        };
        cb.load_from_disk();
        cb
    }

    pub fn check_admission(&mut self) -> Result<(), DaemonError> {
        let now = SystemTime::now();
        if let Some(deadline) = self.deadline {
            if now < deadline {
                self.suppressed_calls += 1;
                let remaining_secs = deadline
                    .duration_since(now)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let deadline_epoch = deadline
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                self.save_to_disk();
                return Err(DaemonError::Tool {
                    tool: "gh".to_string(),
                    rc: 403,
                    stderr: format!(
                        "gh call suppressed by rate limit circuit breaker (cooldown active for {}s until epoch {}, suppressed_calls={})",
                        remaining_secs, deadline_epoch, self.suppressed_calls
                    ),
                });
            }
        }
        Ok(())
    }

    pub fn record_result(&mut self, result: &Result<String, DaemonError>) {
        let now = SystemTime::now();
        let now_epoch = now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        match result {
            Ok(_) => {
                if self.deadline.is_some() || self.consecutive_trips > 0 {
                    let total_suppressed = self.suppressed_calls;
                    let had_deadline = self.deadline.is_some();
                    self.deadline = None;
                    self.consecutive_trips = 0;
                    self.suppressed_calls = 0;
                    self.last_reason = None;
                    self.last_retry_after = None;
                    self.save_to_disk();

                    if had_deadline {
                        emit_transition_telemetry(
                            self.telemetry_log_path.as_deref(),
                            EVT_GH_CIRCUIT_BREAKER_CLOSED,
                            serde_json::json!({
                                "suppressed_calls": total_suppressed,
                                "consecutive_trips": 0
                            }),
                            serde_json::json!({
                                "resumed_at_epoch": now_epoch,
                                "total_suppressed": total_suppressed
                            }),
                        );
                    }
                }
            }
            Err(e) => {
                if let DaemonError::Tool { tool, stderr, rc } = e {
                    if tool == "gh" {
                        if let Some(signal) = parse_rate_limit_error(stderr, *rc) {
                            let was_open = self.deadline.map(|d| now < d).unwrap_or(false);
                            let cooldown = compute_cooldown(self.consecutive_trips, signal.retry_after);
                            let cooldown_secs = cooldown.as_secs();
                            let new_deadline = now + cooldown;
                            let new_deadline_epoch = new_deadline
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);

                            self.consecutive_trips += 1;
                            self.deadline = Some(new_deadline);
                            self.last_reason = Some(signal.reason.clone());
                            self.last_retry_after = signal.retry_after.map(|d| d.as_secs());
                            self.save_to_disk();

                            let event_type = if was_open {
                                EVT_GH_CIRCUIT_BREAKER_EXTENDED
                            } else {
                                EVT_GH_CIRCUIT_BREAKER_OPENED
                            };

                            emit_transition_telemetry(
                                self.telemetry_log_path.as_deref(),
                                event_type,
                                serde_json::json!({
                                    "cooldown_secs": cooldown_secs,
                                    "consecutive_trips": self.consecutive_trips,
                                    "suppressed_calls": self.suppressed_calls
                                }),
                                serde_json::json!({
                                    "reason": signal.reason,
                                    "deadline_epoch": new_deadline_epoch,
                                    "retry_after_secs": signal.retry_after.map(|d| d.as_secs()),
                                    "is_secondary": signal.is_secondary
                                }),
                            );
                        }
                    }
                }
            }
        }
    }

    pub fn save_to_disk(&self) {
        let path = self
            .state_file_path
            .clone()
            .unwrap_or_else(default_state_file_path);

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let now_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let deadline_epoch = self.deadline.map(|d| {
            d.duration_since(UNIX_EPOCH)
                .map(|dur| dur.as_secs())
                .unwrap_or(0)
        });

        let persisted = PersistedCircuitBreakerState {
            deadline_epoch_secs: deadline_epoch,
            consecutive_trips: self.consecutive_trips,
            suppressed_calls: self.suppressed_calls,
            reason: self.last_reason.clone().unwrap_or_default(),
            updated_at_epoch_secs: now_epoch,
        };

        if let Ok(json) = serde_json::to_string_pretty(&persisted) {
            let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    pub fn load_from_disk(&mut self) {
        let path = self
            .state_file_path
            .clone()
            .unwrap_or_else(default_state_file_path);

        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(persisted) = serde_json::from_str::<PersistedCircuitBreakerState>(&raw) {
                let now_epoch = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                if let Some(deadline_epoch) = persisted.deadline_epoch_secs {
                    if deadline_epoch > now_epoch {
                        let remaining_dur = Duration::from_secs(deadline_epoch - now_epoch);
                        self.deadline = Some(SystemTime::now() + remaining_dur);
                        self.consecutive_trips = persisted.consecutive_trips;
                        self.suppressed_calls = persisted.suppressed_calls;
                        self.last_reason = if persisted.reason.is_empty() {
                            None
                        } else {
                            Some(persisted.reason)
                        };
                        return;
                    }
                }
                self.deadline = None;
                self.consecutive_trips = persisted.consecutive_trips;
                self.suppressed_calls = persisted.suppressed_calls;
            }
        }
    }
}

fn global_cb() -> &'static Mutex<GhCircuitBreaker> {
    static CB: OnceLock<Mutex<GhCircuitBreaker>> = OnceLock::new();
    CB.get_or_init(|| Mutex::new(GhCircuitBreaker::new()))
}

pub fn admit_or_suppress(cmd: &str) -> Result<(), DaemonError> {
    if cmd == "gh" {
        let mut cb = global_cb().lock().unwrap();
        cb.check_admission()
    } else {
        Ok(())
    }
}

pub fn record_result(cmd: &str, result: &Result<String, DaemonError>) {
    if cmd == "gh" {
        let mut cb = global_cb().lock().unwrap();
        cb.record_result(result);
    }
}

pub fn is_rate_limited() -> bool {
    let cb = global_cb().lock().unwrap();
    if let Some(deadline) = cb.deadline {
        if SystemTime::now() < deadline {
            return true;
        }
    }
    false
}

pub fn trip(cooldown: Duration, reason: &str) {
    let mut cb = global_cb().lock().unwrap();
    let now = SystemTime::now();
    let was_open = cb.deadline.map(|d| now < d).unwrap_or(false);
    cb.consecutive_trips += 1;
    let new_deadline = now + cooldown;
    cb.deadline = Some(new_deadline);
    cb.last_reason = Some(reason.to_string());
    cb.save_to_disk();

    let event_type = if was_open {
        EVT_GH_CIRCUIT_BREAKER_EXTENDED
    } else {
        EVT_GH_CIRCUIT_BREAKER_OPENED
    };

    let deadline_epoch = new_deadline
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    emit_transition_telemetry(
        cb.telemetry_log_path.as_deref(),
        event_type,
        serde_json::json!({
            "cooldown_secs": cooldown.as_secs(),
            "consecutive_trips": cb.consecutive_trips,
            "suppressed_calls": cb.suppressed_calls
        }),
        serde_json::json!({
            "reason": reason,
            "deadline_epoch": deadline_epoch
        }),
    );
}

pub fn reset() {
    let mut cb = global_cb().lock().unwrap();
    cb.deadline = None;
    cb.consecutive_trips = 0;
    cb.suppressed_calls = 0;
    cb.last_reason = None;
    cb.last_retry_after = None;
    cb.save_to_disk();
}

pub fn suppressed_call_count() -> u64 {
    global_cb().lock().unwrap().suppressed_calls
}

pub fn consecutive_trips() -> u32 {
    global_cb().lock().unwrap().consecutive_trips
}

pub fn current_deadline() -> Option<SystemTime> {
    global_cb().lock().unwrap().deadline
}

pub fn set_state_file_path(path: Option<PathBuf>) {
    let mut cb = global_cb().lock().unwrap();
    cb.state_file_path = path;
}

pub fn set_telemetry_log_path(path: Option<PathBuf>) {
    let mut cb = global_cb().lock().unwrap();
    cb.telemetry_log_path = path;
}

pub fn reload() {
    let mut cb = global_cb().lock().unwrap();
    cb.load_from_disk();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_constructor_matches_new() {
        let from_default = GhCircuitBreaker::default();
        let from_new = GhCircuitBreaker::new();

        assert_eq!(from_default.deadline.is_some(), from_new.deadline.is_some());
        assert_eq!(from_default.consecutive_trips, from_new.consecutive_trips);
        assert_eq!(from_default.suppressed_calls, from_new.suppressed_calls);
        assert_eq!(from_default.last_reason, from_new.last_reason);
        assert_eq!(from_default.last_retry_after, from_new.last_retry_after);
        assert_eq!(from_default.state_file_path, from_new.state_file_path);
        assert_eq!(from_default.telemetry_log_path, from_new.telemetry_log_path);
    }

    #[test]
    fn test_parse_primary_rate_limit() {
        let stderr = "gh: API rate limit exceeded for installation ID 123456";
        let signal = parse_rate_limit_error(stderr, 1).expect("should parse primary rate limit");
        assert!(!signal.is_secondary);
        assert_eq!(signal.reason, "primary_rate_limit");
    }

    #[test]
    fn test_generic_forbidden_is_not_rate_limit() {
        let stderr = "HTTP 403 Forbidden: repository access denied";
        assert!(parse_rate_limit_error(stderr, 403).is_none());
    }

    /// Bead rev-x92c8: the three shapes that used to be laundered into
    /// `primary_rate_limit` by the old bare-substring net. Pinned here, at
    /// the classifier's home, so a future widening of the match cannot
    /// silently reintroduce the false-trip loop.
    #[test]
    fn test_non_rate_limit_failures_get_their_own_reason() {
        // Healthy quota proven by the echoed header (4209 of 5000 left).
        let headers = "HTTP 403: Resource not accessible by integration\nx-ratelimit-remaining: 4209";
        assert_eq!(classify_gh_failure(headers, 1), GhFailureKind::Forbidden);
        assert!(parse_rate_limit_error(headers, 1).is_none());

        // The `gh api rate_limit` probe URL echoed inside a timeout.
        let probe = "gh: Get \"https://api.github.com/rate_limit\": net/http: request canceled (Client.Timeout exceeded while awaiting headers)";
        assert_eq!(classify_gh_failure(probe, 1), GhFailureKind::Timeout);
        assert!(parse_rate_limit_error(probe, 1).is_none());

        // This breaker's OWN suppression stderr must never re-trip it.
        let suppressed = "gh call suppressed by rate limit circuit breaker (cooldown active for 47s until epoch 1756000000, suppressed_calls=3)";
        assert_eq!(
            classify_gh_failure(suppressed, 403),
            GhFailureKind::CircuitBreakerSuppressed
        );
        assert!(parse_rate_limit_error(suppressed, 403).is_none());
    }

    /// Zero remaining IS exhaustion, even without an "exceeded" phrase.
    #[test]
    fn test_zero_ratelimit_remaining_is_primary() {
        let stderr = "HTTP 403: Forbidden\nx-ratelimit-limit: 5000\nx-ratelimit-remaining: 0";
        let signal = parse_rate_limit_error(stderr, 1).expect("zero remaining is exhaustion");
        assert_eq!(signal.reason, "primary_rate_limit");
    }

    #[test]
    fn test_parse_secondary_rate_limit() {
        let stderr = "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.";
        let signal = parse_rate_limit_error(stderr, 403).expect("should parse secondary rate limit");
        assert!(signal.is_secondary);
        assert_eq!(signal.reason, "secondary_rate_limit");
    }

    #[test]
    fn test_parse_retry_after_header() {
        let stderr = "HTTP 403 Forbidden\nRetry-After: 120\nAPI rate limit exceeded";
        let signal = parse_rate_limit_error(stderr, 403).expect("should parse signal with retry after");
        assert_eq!(signal.retry_after, Some(Duration::from_secs(120)));
    }

    #[test]
    fn test_parse_please_wait_minutes() {
        let stderr = "HTTP 403: You have exceeded a secondary rate limit. Please wait 5 minutes before you try again.";
        let signal = parse_rate_limit_error(stderr, 403).expect("should parse please wait minutes");
        assert!(signal.is_secondary);
        assert_eq!(signal.retry_after, Some(Duration::from_secs(300)));
    }

    #[test]
    fn test_exponential_backoff_bounds() {
        let cooldown0 = compute_cooldown(0, None);
        assert_eq!(cooldown0, Duration::from_secs(60));

        let cooldown1 = compute_cooldown(1, None);
        assert_eq!(cooldown1, Duration::from_secs(120));

        let cooldown2 = compute_cooldown(2, None);
        assert_eq!(cooldown2, Duration::from_secs(240));

        let cooldown3 = compute_cooldown(3, None);
        assert_eq!(cooldown3, Duration::from_secs(480));

        let cooldown10 = compute_cooldown(10, None);
        assert_eq!(cooldown10, Duration::from_secs(DEFAULT_MAX_COOLDOWN_SECS));
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = std::env::temp_dir().join(format!("cb_persist_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("gh_circuit_breaker.json");

        let cb1 = GhCircuitBreaker {
            deadline: Some(SystemTime::now() + Duration::from_secs(300)),
            consecutive_trips: 2,
            suppressed_calls: 5,
            last_reason: Some("secondary_rate_limit".to_string()),
            last_retry_after: Some(300),
            state_file_path: Some(state_path.clone()),
            telemetry_log_path: None,
        };
        cb1.save_to_disk();

        let mut cb2 = GhCircuitBreaker {
            deadline: None,
            consecutive_trips: 0,
            suppressed_calls: 0,
            last_reason: None,
            last_retry_after: None,
            state_file_path: Some(state_path.clone()),
            telemetry_log_path: None,
        };
        cb2.load_from_disk();

        assert!(cb2.deadline.is_some());
        assert_eq!(cb2.consecutive_trips, 2);
        assert_eq!(cb2.suppressed_calls, 5);
        assert_eq!(cb2.last_reason, Some("secondary_rate_limit".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
