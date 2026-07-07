#[derive(thiserror::Error, Debug)]
pub enum DaemonError {
    #[error("tool {tool} failed (rc={rc}): {stderr}")]
    Tool { tool: String, rc: i32, stderr: String },
    #[error("parse: {0}")]
    Parse(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("config: {0}")]
    Config(String),
    /// Bead `jleechan-qdw` — GitHub returned a rate-limit response (HTTP 429
    /// or 403 with a secondary-rate-limit message). `retry_after_secs` is the
    /// daemon's best estimate of how long to back off (parsed from
    /// `Retry-After` / `X-RateLimit-Reset` when present, otherwise 60s). The
    /// daemon MUST treat this as a transient condition — never exit the
    /// process, never park a bead HUMAN_HELD on a single occurrence, and
    /// back off across all gh-touching operations until the window elapses.
    #[error("rate-limited by {tool}; retry after {retry_after_secs}s")]
    RateLimited { tool: String, retry_after_secs: u64 },
}
