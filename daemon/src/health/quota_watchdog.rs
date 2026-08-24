// Gemini quota watchdog — bead rev-4ou1z.
//
// PROBLEM: when a Gemini-backed coder's individual quota exhausts mid-run,
// the post-spawn health probe (rev-k8hp4 / `parse_session_health_pane`)
// correctly detects the "Individual quota reached" marker, but the fast
// tier's default SESSION_HEALTH_FAILED handling immediately stops the
// session and requeues a fresh spawn. A fresh spawn hits the SAME quota
// wall instantly, so `MAX_TRANSIENT_SPAWN_RETRY` burns out in minutes
// against a quota window that can take hours to reset, parking the bead
// HUMAN_HELD long before the quota actually clears. Live incident:
// 02:15Z 2026-08-23, coder wa-3538 paused at "Individual quota reached.
// Resets in 1h 23m" with no auto-recovery after the reset time passed.
//
// FIX: when a health-check reason names a parseable quota reset duration,
// the tick loop (see `tick::run_fast_tier`) arms this watchdog instead of
// killing the session, and the slow-tier wake sweep
// (`tick::run_quota_watchdog_wake`) sends an Enter keypress to the SAME
// paused pane once `reset_at + WAKE_GRACE_SECS` has passed — no respawn,
// no retry-budget burn.
//
// The ledger is in-process only (mirrors `adapters::GRAPHQL_RATE_LIMITED_UNTIL`
// and the `vendor_health` module's documented rationale): a reset window
// observed during the daemon's lifetime is observed by the daemon, and a
// daemon restart mid-quota-window just re-observes the pane on the next
// health check. Every accessor is keyed and scoped by a single `bead_id` —
// the wake sweep in `tick.rs` only ever queries bead ids the calling
// `TickDeps::store` currently owns (mirroring the fast tier's
// `owned_branches` walk), so two independent tick loops sharing this
// process-wide static (e.g. two `cargo test` integration tests running on
// separate `FakeStateStore`s in parallel) never observe or consume each
// other's entries as long as they use distinct bead ids — which every
// caller already must, since bead ids are globally unique.

use crate::errors::DaemonError;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Grace period after the recorded quota reset time before the daemon pokes
/// the paused coder pane (ACCEPTANCE: "at reset time + 60s").
pub const WAKE_GRACE_SECS: u64 = 60;

fn ledger() -> &'static Mutex<HashMap<String, (String, u64)>> {
    static LEDGER: OnceLock<Mutex<HashMap<String, (String, u64)>>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Parses a "Resets in Xh Ym" / "Resets in Xh" / "Resets in Ym" duration out
/// of a quota-related health-check reason or raw pane string. Returns `None`
/// when there is no "individual quota reached" marker, or the marker has no
/// parseable reset duration nearby.
pub fn parse_quota_reset_duration(text: &str) -> Option<Duration> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("individual quota reached") {
        return None;
    }
    let marker = "resets in";
    let idx = lower.find(marker)?;
    let tail = lower[idx + marker.len()..].trim_start();

    let mut hours: u64 = 0;
    let mut minutes: u64 = 0;
    let mut found_any = false;
    // "Xh Ym" is two whitespace-separated tokens (each optionally followed
    // by trailing punctuation, e.g. "Resets in 1h 23m." or "...23m.)"). Stop
    // at the first token that isn't a recognized "<digits>h"/"<digits>m"
    // unit — that ends the duration (or means there was none to parse).
    for raw_token in tail.split_whitespace().take(3) {
        let token = raw_token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        if let Some(num_str) = token.strip_suffix('h') {
            if let Ok(h) = num_str.parse::<u64>() {
                hours = h;
                found_any = true;
                continue;
            }
        }
        if let Some(num_str) = token.strip_suffix('m') {
            if let Ok(m) = num_str.parse::<u64>() {
                minutes = m;
                found_any = true;
                continue;
            }
        }
        break;
    }
    if !found_any {
        return None;
    }
    Some(Duration::from_secs(hours * 3600 + minutes * 60))
}

/// Records that `bead_id`'s coder session (`session_id`) has a quota reset
/// due at `reset_at_epoch` (unix seconds). A no-op if `bead_id` is already
/// armed — the fast tier re-observes the same stale pane text on every
/// tick while the coder stays paused, and re-arming from that same static
/// text would keep pushing the wake time further into the future forever.
pub fn record_quota_reset(bead_id: &str, session_id: &str, reset_at_epoch: u64) {
    let mut map = ledger().lock().unwrap();
    map.entry(bead_id.to_string())
        .or_insert_with(|| (session_id.to_string(), reset_at_epoch));
}

/// Returns the previously recorded reset epoch for `bead_id`, if any.
pub fn recorded_reset_at(bead_id: &str) -> Option<u64> {
    ledger().lock().unwrap().get(bead_id).map(|(_, at)| *at)
}

/// Removes `bead_id` from the ledger (e.g. once its pane has been woken, or
/// the bead moved on through another path such as manual operator recovery).
pub fn clear(bead_id: &str) {
    ledger().lock().unwrap().remove(bead_id);
}

/// If `bead_id` is armed AND its recorded reset time (plus the wake grace)
/// has passed as of `now_epoch`, removes it from the ledger and returns its
/// session id — the caller wakes that session's pane exactly once. Returns
/// `None` (leaving the ledger untouched) when `bead_id` isn't armed, or its
/// wake time hasn't arrived yet. Scoped to a single `bead_id` deliberately:
/// callers (the tick loop's wake sweep) only ever probe bead ids their own
/// `StateStore` currently owns, so this never reacts to another store's
/// entries in this same process-wide ledger.
pub fn take_due_wake(bead_id: &str, now_epoch: u64) -> Option<String> {
    let mut map = ledger().lock().unwrap();
    let due = map
        .get(bead_id)
        .map(|(_, reset_at)| now_epoch >= reset_at.saturating_add(WAKE_GRACE_SECS))
        .unwrap_or(false);
    if !due {
        return None;
    }
    map.remove(bead_id).map(|(session_id, _)| session_id)
}

/// Sends an Enter keypress to the tmux pane backing `session_name`, waking a
/// worker paused at a benign prompt (e.g. after "Individual quota reached"
/// clears past its reset time). Returns `Ok(true)` when a matching tmux
/// session was found and poked, `Ok(false)` when no matching session exists
/// (already reaped/renamed — nothing to wake).
pub fn wake_session_pane_cli(session_name: &str) -> Result<bool, DaemonError> {
    let out = match crate::tools::run_tool("tmux", &["list-sessions", "-F", "#{session_name}"], 5) {
        Ok(o) => o,
        Err(_) => return Ok(false),
    };
    let target_tmux_session = out.lines().find(|line| {
        let trimmed = line.trim();
        trimmed == session_name
            || trimmed.ends_with(&format!("-{session_name}"))
            || trimmed.contains(session_name)
    });
    let Some(tmux_session) = target_tmux_session else {
        return Ok(false);
    };
    let pane_target = format!("{}:0", tmux_session.trim());
    crate::tools::run_tool("tmux", &["send-keys", "-t", &pane_target, "Enter"], 5)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hours_and_minutes() {
        let sample = "⚠ Individual quota reached. Resets in 1h 23m";
        assert_eq!(
            parse_quota_reset_duration(sample),
            Some(Duration::from_secs(3600 + 23 * 60))
        );
    }

    #[test]
    fn parses_minutes_only() {
        let sample = "Individual quota reached. Resets in 45m.";
        assert_eq!(
            parse_quota_reset_duration(sample),
            Some(Duration::from_secs(45 * 60))
        );
    }

    #[test]
    fn parses_hours_only() {
        let sample = "Individual quota reached. Resets in 2h";
        assert_eq!(
            parse_quota_reset_duration(sample),
            Some(Duration::from_secs(2 * 3600))
        );
    }

    #[test]
    fn returns_none_without_quota_marker() {
        let sample = "PR URL: https://github.com/example/repo/pull/1";
        assert_eq!(parse_quota_reset_duration(sample), None);
    }

    #[test]
    fn returns_none_when_quota_marker_present_but_no_duration() {
        let sample = "terminal session error in tmux pane: individual quota reached";
        assert_eq!(parse_quota_reset_duration(sample), None);
    }

    #[test]
    fn ledger_records_and_wakes_after_grace_period() {
        clear("bead-rev4ou1z-1");
        record_quota_reset("bead-rev4ou1z-1", "wa-3538", 1_000);

        assert_eq!(recorded_reset_at("bead-rev4ou1z-1"), Some(1_000));
        // Before reset time: nothing due.
        assert_eq!(take_due_wake("bead-rev4ou1z-1", 999), None);
        // At reset time, still inside the 60s grace window: nothing due.
        assert_eq!(take_due_wake("bead-rev4ou1z-1", 1_000), None);
        assert_eq!(take_due_wake("bead-rev4ou1z-1", 1_059), None);
        // Reset time + 60s grace has passed: due, and removed from the ledger.
        assert_eq!(
            take_due_wake("bead-rev4ou1z-1", 1_060),
            Some("wa-3538".to_string())
        );
        assert_eq!(recorded_reset_at("bead-rev4ou1z-1"), None);
        // Woken exactly once — a second sweep at the same time finds nothing.
        assert_eq!(take_due_wake("bead-rev4ou1z-1", 2_000), None);
    }

    #[test]
    fn record_is_idempotent_first_observation_wins() {
        clear("bead-rev4ou1z-2");
        record_quota_reset("bead-rev4ou1z-2", "wa-1", 5_000);
        // A later re-observation of the same stale pane text must NOT push
        // the wake time forward every tick.
        record_quota_reset("bead-rev4ou1z-2", "wa-1", 999_999);
        assert_eq!(recorded_reset_at("bead-rev4ou1z-2"), Some(5_000));
        clear("bead-rev4ou1z-2");
        assert_eq!(recorded_reset_at("bead-rev4ou1z-2"), None);
    }
}
