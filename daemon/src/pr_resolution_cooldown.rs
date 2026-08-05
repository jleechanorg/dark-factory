// Bead jleechan-48ou: daemon-process-wide cooldown for the per-bead
// `pr_number_for_branch` lookup so a sustained `gh` rate-limit does not
// hammer the GraphQL endpoint every slow-tier tick for every DISPATCHED
// bead. Mirrors the jtg8-r4 intake-sweep rate-limit awareness
// (`intake::normalize_labeled_prs_outcome`) — which short-circuits the
// whole PR list query on rate-limit — but at the per-bead resolution
// level: a single `gh pr list --head <branch> --repo <repo>` call per
// DISPATCHED bead per tick, across N dispatched beads, multiplies into
// N GraphQL hits per slow tick. Live incident 2026-08-05 19:35-20:05Z:
// jleechan-htf7 alone logged ~30 `PR_NUMBER_REREZOLVE_TRANSIENT_ERROR`
// events over 30 min; the wider tail logged 287 across many beads.
//
// `daemon/src/tick.rs` calls `deps.scm.pr_number_for_branch(...)` from
// three sites (the DISPATCHED→ATTESTED promotion path at ~line 3487
// and two ATTESTED pre-gate-validation drift paths at ~lines 3727 and
// 3816). All three now route through `PrResolutionCooldown::run` so the
// cooldown state is shared across the daemon process, not per-bead.
//
// The cooldown is intentionally process-wide (not per-repo / per-branch):
// the GraphQL rate-limit bucket is shared across repos/queries, so
// isolating per branch would still leak. The intake sweep's
// `AdoptionProbeCache` already isolates its own per-PR probes from the
// rate-limit window; this cooldown is a different layer — it gates the
// per-bead branch→PR lookup that the slow-tier DISPATCHED→ATTESTED
// promotion relies on.

/// Length of the cooldown set when `pr_number_for_branch` returns a
/// `DaemonError::is_gh_rate_limit()` error. Chosen to outlast the slow
/// tier's normal tick cadence (`slow_tick_secs`, default 300s) so the
/// next attempt does not immediately re-fire during the same rate-limit
/// window, but short enough that recovery is not stuck for hours when
/// the GraphQL bucket regenerates. 60s matches the typical `gh` rate-
/// limit reset window after a small burst and is a fraction of the slow
/// tick so at most one extra cooldown elapses before the next probe.
pub const PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS: u64 = 60;

/// Process-wide rate-limit cooldown state for `pr_number_for_branch`.
///
/// Stored behind a `Mutex` in `TickDeps` so the daemon's poll loop and
/// any concurrent tick callers share one instance, identical to the
/// `vendor_health::VendorHealthLedger` pattern. When `TickDeps` is
/// constructed with `pr_resolution_cooldown: None` (legacy / test sites
/// that don't exercise the cooldown), `tick.rs` falls through to the
/// pre-fix fail-closed `Err` behavior so existing tests stay green.
#[derive(Debug, Default)]
pub struct PrResolutionCooldown {
    /// Epoch seconds at which the cooldown expires. `0` means
    /// "no cooldown active"; any value <= `now_epoch` is treated as
    /// expired (the cooldown is a single forward timestamp, not a
    /// boolean).
    pub until_epoch: u64,
    /// Telemetry-only: count of cooldown-skipped lookups since the
    /// last clear, surfaced in the TICK summary so operators can see
    /// whether the daemon is still bleeding on rate-limit even after
    /// recovery.
    pub skips_since_clear: u64,
}

impl PrResolutionCooldown {
    /// True iff the cooldown is currently active AND has not yet
    /// expired. Caller passes the daemon's `now_epoch` so this method
    /// stays pure (testable without wall-clock).
    pub fn is_active(&self, now_epoch: u64) -> bool {
        self.until_epoch > now_epoch
    }

    /// Remaining seconds on the cooldown, or 0 if expired / inactive.
    pub fn remaining_secs(&self, now_epoch: u64) -> u64 {
        self.until_epoch.saturating_sub(now_epoch)
    }

    /// Set the cooldown to expire `PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS`
    /// from `now_epoch` ONLY IF no cooldown is currently active. If a
    /// cooldown is already in the future (i.e. still active), leave it
    /// alone — otherwise every retry would reset the clock and the
    /// daemon would never probe again. Returns the (possibly unchanged)
    /// `until_epoch` for telemetry.
    pub fn record_rate_limit(&mut self, now_epoch: u64) -> u64 {
        if self.until_epoch <= now_epoch {
            self.until_epoch = now_epoch.saturating_add(PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS);
        }
        self.until_epoch
    }

    /// Clear the cooldown. Called when `pr_number_for_branch` returns
    /// `Ok(_)` so the very next tick resumes the lookup the moment
    /// `gh` is responsive again — without waiting out the full window.
    /// Resets `skips_since_clear` so the next rate-limit window starts
    /// a fresh skip-count for operators.
    pub fn clear(&mut self) {
        self.until_epoch = 0;
        self.skips_since_clear = 0;
    }

    /// Increment the skip counter. Called when `is_active()` returned
    /// true and the lookup was suppressed.
    pub fn record_skip(&mut self) {
        self.skips_since_clear = self.skips_since_clear.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_by_default() {
        let c = PrResolutionCooldown::default();
        assert!(!c.is_active(0));
        assert!(!c.is_active(1_000_000));
        assert_eq!(c.remaining_secs(0), 0);
    }

    #[test]
    fn record_rate_limit_sets_until_and_is_active_within_window() {
        let mut c = PrResolutionCooldown::default();
        let now = 1_000;
        let until = c.record_rate_limit(now);
        assert_eq!(until, now + PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS);
        assert!(c.is_active(now));
        assert_eq!(c.remaining_secs(now), PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS);
    }

    #[test]
    fn expires_at_until() {
        let mut c = PrResolutionCooldown::default();
        let now = 1_000;
        let until = c.record_rate_limit(now);
        assert!(c.is_active(until - 1));
        assert!(!c.is_active(until));
        assert_eq!(c.remaining_secs(until - 1), 1);
        assert_eq!(c.remaining_secs(until), 0);
    }

    #[test]
    fn record_rate_limit_window_starts_at_first_hit() {
        // First rate-limit hit at T sets the window to T+60s. Subsequent
        // rate-limit hits within that window do NOT extend it (otherwise
        // every retry would reset the clock and the daemon would never
        // probe again). A hit AFTER the window expires starts a fresh
        // 60s window from the new `now`.
        let mut c = PrResolutionCooldown::default();
        let t0 = 1_000;
        let until_first = c.record_rate_limit(t0);
        assert_eq!(until_first, t0 + PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS);

        // Within window: hit does not extend.
        let until_within = c.record_rate_limit(t0 + 30);
        assert_eq!(
            until_within, until_first,
            "rate-limit hit within the window must NOT extend the cooldown"
        );

        // After window expires: hit starts a fresh window.
        let until_after = c.record_rate_limit(t0 + PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS + 1);
        assert_eq!(
            until_after,
            t0 + PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS + 1 + PR_RESOLUTION_RATE_LIMIT_COOLDOWN_SECS,
            "rate-limit hit after window expiry starts a fresh window"
        );
    }

    #[test]
    fn clear_resets_until_and_skip_counter() {
        let mut c = PrResolutionCooldown::default();
        let now = 1_000;
        c.record_rate_limit(now);
        c.record_skip();
        c.record_skip();
        assert_eq!(c.skips_since_clear, 2);
        c.clear();
        assert!(!c.is_active(now));
        assert_eq!(c.remaining_secs(now), 0);
        assert_eq!(c.skips_since_clear, 0);
    }
}
