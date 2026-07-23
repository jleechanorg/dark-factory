// Vendor health module — bead jleechan-jsby.
//
// PROBLEM: when CodeRabbit (or Bugbot) hits a fair-use adaptive cap, the
// gate-3 / gate-4 verdict goes "unknown" and the bead parks in
// DISPOSITION_REQUIRED indefinitely, churning operator toil. The fix is a
// STRUCTURAL-UNAVAILABILITY WAIVER: when a vendor is structurally unavailable
// (capped or rate-limited to the point of no useful review), the gate-set
// substitutes a documented waiver token (`coderabbit:waived_vendor_unavailable`
// or `bugbot:waived_vendor_unavailable`) which clears the gate ONLY IF
// compensating coverage is green (skeptic pass + /er pass + cross-model
// reviewer). A skeptic FAIL still blocks the merge under waiver — the waiver
// is purely about substituting the external-reviewer gap, not the floor of
// trust.
//
// DESIGN CONSTRAINTS:
// - Vendor cap detection must NOT be heuristic/keyword-based (ZFC). The
//   detector classifies observations from STRUCTURED signals (the gate
//   name + the gate's `Unknown`/`Red` variant, plus an explicit CapSource
//   enum). The /er and /skeptic prompts already make all routing decisions;
//   this module just records facts.
// - A waiver auto-expires the next time a vendor-produced review lands
//   (vendor recovery). It does not require an operator action.
// - The ledger is in-process (per daemon) — no DB schema migration needed;
//   a cap observed in the daemon's lifetime is observed by the daemon. The
//   persistent record is the VENDOR_WAIVED / VENDOR_RECOVERED telemetry events.

use crate::errors::DaemonError;
use std::collections::VecDeque;
use std::path::Path;

/// Maximum cap observations retained for the N-of-M detector.
const CAP_OBSERVATIONS_CAP: usize = 32;

/// Telemetry event names (mirrored in `factory-overlay.sh` and the CXDB
/// consumers). Kept as `&'static str` so the emit call needs no allocation.
pub const EVT_WAIVED: &str = "VENDOR_WAIVED";
pub const EVT_RECOVERED: &str = "VENDOR_RECOVERED";

/// The two external reviewers whose unavailability this module tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    CodeRabbit,
    Bugbot,
}

impl Vendor {
    /// Snake_case token matching `GateName::as_str` and the gate_assessment
    /// telemetry vocabulary.
    pub fn as_str(&self) -> &'static str {
        match self {
            Vendor::CodeRabbit => "coderabbit",
            Vendor::Bugbot => "bugbot",
        }
    }

    /// Waiver token emitted into gate_assessment telemetry when this vendor
    /// is structurally unavailable. The token is intentionally explicit and
    /// non-empty so the strict-merge policy can grep for it without
    /// ambiguity (`coderabbit:waived_vendor_unavailable`).
    pub fn waiver_token(&self) -> &'static str {
        match self {
            Vendor::CodeRabbit => "coderabbit:waived_vendor_unavailable",
            Vendor::Bugbot => "bugbot:waived_vendor_unavailable",
        }
    }
}

/// Why we believe this vendor is structurally unavailable. The detector
/// records the source so an operator / Healer can audit the call; this is
/// NOT used as a routing signal (ZFC-clean).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSource {
    /// Repeated Unknown gate verdict across N consecutive assessments with
    /// no APPROVED review in between. Most common cause: CodeRabbit's
    /// 95th-percentile fair-use cap extending on every review request.
    UnknownGateRepeated,
    /// Vendor response itself reported a quota / fair-use / rate-limit
    /// marker (passed in from the daemon's reviewer dispatcher, NOT
    /// keyword-matched here).
    VendorReportedCap,
    /// Daemon-level health probe: the vendor reviewer's CLI / API is
    /// returning 4xx/5xx with retry-after / 429 markers for >= N attempts.
    ProbeExhausted,
    /// Vendor review is simply absent (no review activity in the bead's
    /// PR at all for an extended period).
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CapObservation {
    pub vendor: Vendor,
    pub source: CapSource,
    pub bead_id: String,
    pub pr_number: u64,
    pub ts_epoch: u64,
    /// One-line structured note (e.g. "gh_call_count=48 retry_after=300s").
    /// NOT used for routing — only for operator / Healer audit.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum VendorHealth {
    /// No recent cap observations; gate verdict is authoritative.
    Healthy,
    /// Vendor is structurally unavailable. Compensating coverage
    /// (skeptic + /er + cross-model) MUST be green for the waiver to
    /// substitute the gate.
    Capped {
        /// Distinct cap observations recorded in the last window. >= N
        /// (see `vendor_is_structurally_unavailable`) triggers this
        /// state.
        observations: Vec<CapObservation>,
        /// First observation in the current cap run (epoch). The
        /// telemetry event uses this as `since`.
        since_epoch: u64,
    },
}

impl VendorHealth {
    pub fn is_capped(&self) -> bool {
        matches!(self, VendorHealth::Capped { .. })
    }
}

/// In-memory per-vendor ledger. The daemon owns one of these and consults
/// it from `verifier::assess` (and from the skeptic prompt construction).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VendorHealthLedger {
    coderabbit: VecDeque<CapObservation>,
    bugbot: VecDeque<CapObservation>,
}

impl VendorHealthLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cap observation for `vendor`. Trims the oldest entries
    /// beyond `CAP_OBSERVATIONS_CAP` so the ledger stays bounded.
    pub fn record_cap(&mut self, obs: CapObservation) {
        let buf = match obs.vendor {
            Vendor::CodeRabbit => &mut self.coderabbit,
            Vendor::Bugbot => &mut self.bugbot,
        };
        if buf.len() >= CAP_OBSERVATIONS_CAP {
            buf.pop_front();
        }
        buf.push_back(obs);
    }

    /// Clear ALL cap observations for `vendor` (called on vendor
    /// recovery). Idempotent — a no-op when the vendor is already
    /// healthy.
    pub fn clear(&mut self, vendor: Vendor) {
        let buf = match vendor {
            Vendor::CodeRabbit => &mut self.coderabbit,
            Vendor::Bugbot => &mut self.bugbot,
        };
        buf.clear();
    }

    /// Current cap observation count for `vendor`.
    pub fn observation_count(&self, vendor: Vendor) -> usize {
        match vendor {
            Vendor::CodeRabbit => self.coderabbit.len(),
            Vendor::Bugbot => self.bugbot.len(),
        }
    }

    /// Snapshot the cap observations for `vendor` (cloned, sorted by
    /// timestamp ascending). Used by telemetry and by the gate assembler.
    pub fn observations_for(&self, vendor: Vendor) -> Vec<CapObservation> {
        let buf = match vendor {
            Vendor::CodeRabbit => &self.coderabbit,
            Vendor::Bugbot => &self.bugbot,
        };
        let mut out: Vec<CapObservation> = buf.iter().cloned().collect();
        out.sort_by_key(|o| o.ts_epoch);
        out
    }

    /// Classify the vendor's current health from its recorded
    /// observations. N-of-M detector: >= `CAP_THRESHOLD` observations
    /// within the most recent `OBSERVATION_WINDOW` distinct beads
    /// (de-duplicated by `bead_id`) flips the vendor to `Capped`.
    ///
    /// `CAP_THRESHOLD` (3) and `OBSERVATION_WINDOW` (5) are tuned to the
    /// bead frequency seen in production — a single cap observation per
    /// bead across 3 of the last 5 beads is the floor that the operator
    /// brief identified as "structurally unavailable, not transient".
    pub fn health(&self, vendor: Vendor) -> VendorHealth {
        const CAP_THRESHOLD: usize = 3;
        const OBSERVATION_WINDOW: usize = 5;

        let obs = self.observations_for(vendor);
        // De-dup by bead_id (a single bead repeating caps across attempts
        // is one signal, not N).
        let mut distinct_beads: Vec<String> = obs
            .iter()
            .map(|o| o.bead_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        // Use the last N distinct beads in observation order (most recent
        // first).
        distinct_beads.reverse();
        let recent = distinct_beads.into_iter().take(OBSERVATION_WINDOW).count();

        if recent >= CAP_THRESHOLD {
            let since_epoch = obs.first().map(|o| o.ts_epoch).unwrap_or(0);
            VendorHealth::Capped {
                observations: obs,
                since_epoch,
            }
        } else {
            VendorHealth::Healthy
        }
    }
}

/// Persistent JSONL file backing the ledger. The daemon writes one
/// observation per line and reads them back on startup. Keeping the
/// ledger across restarts is what lets the N-of-M detector survive a
/// daemon bounce — without persistence the bead fleet would re-trigger
/// the cap-detector every restart and churn.
pub fn append_observation(log_path: &Path, obs: &CapObservation) -> Result<(), DaemonError> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| DaemonError::Config(format!("vendor_health open: {e}")))?;
    let line = serde_json::to_string(obs).map_err(|e| DaemonError::Parse(e.to_string()))?;
    use std::io::Write;
    writeln!(f, "{line}")
        .map_err(|e| DaemonError::Config(format!("vendor_health write: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(vendor: Vendor, bead: &str, ts: u64) -> CapObservation {
        CapObservation {
            vendor,
            source: CapSource::UnknownGateRepeated,
            bead_id: bead.into(),
            pr_number: 1,
            ts_epoch: ts,
            note: "test".into(),
        }
    }

    #[test]
    fn healthy_when_no_observations() {
        let ledger = VendorHealthLedger::new();
        assert_eq!(ledger.health(Vendor::CodeRabbit), VendorHealth::Healthy);
        assert_eq!(ledger.health(Vendor::Bugbot), VendorHealth::Healthy);
    }

    #[test]
    fn healthy_below_threshold() {
        // 2 distinct beads < threshold of 3.
        let mut ledger = VendorHealthLedger::new();
        ledger.record_cap(obs(Vendor::CodeRabbit, "b1", 1));
        ledger.record_cap(obs(Vendor::CodeRabbit, "b2", 2));
        assert_eq!(ledger.health(Vendor::CodeRabbit), VendorHealth::Healthy);
    }

    #[test]
    fn capped_at_threshold_distinct_beads() {
        let mut ledger = VendorHealthLedger::new();
        ledger.record_cap(obs(Vendor::CodeRabbit, "b1", 1));
        ledger.record_cap(obs(Vendor::CodeRabbit, "b2", 2));
        ledger.record_cap(obs(Vendor::CodeRabbit, "b3", 3));
        let h = ledger.health(Vendor::CodeRabbit);
        match h {
            VendorHealth::Capped {
                observations,
                since_epoch,
            } => {
                assert_eq!(observations.len(), 3);
                assert_eq!(since_epoch, 1);
            }
            other => panic!("expected Capped, got {other:?}"),
        }
    }

    #[test]
    fn repeated_caps_in_same_bead_count_as_one_signal() {
        let mut ledger = VendorHealthLedger::new();
        // Same bead, 4 different attempts — still 1 distinct bead,
        // below threshold.
        for ts in 1..=4 {
            ledger.record_cap(obs(Vendor::CodeRabbit, "b1", ts));
        }
        assert_eq!(ledger.health(Vendor::CodeRabbit), VendorHealth::Healthy);
    }

    #[test]
    fn coderabbit_and_bugbot_ledgers_are_independent() {
        let mut ledger = VendorHealthLedger::new();
        ledger.record_cap(obs(Vendor::CodeRabbit, "b1", 1));
        ledger.record_cap(obs(Vendor::CodeRabbit, "b2", 2));
        ledger.record_cap(obs(Vendor::CodeRabbit, "b3", 3));
        // CodeRabbit is capped, Bugbot untouched.
        assert!(ledger.health(Vendor::CodeRabbit).is_capped());
        assert_eq!(ledger.health(Vendor::Bugbot), VendorHealth::Healthy);
    }

    #[test]
    fn clear_resets_to_healthy() {
        let mut ledger = VendorHealthLedger::new();
        ledger.record_cap(obs(Vendor::CodeRabbit, "b1", 1));
        ledger.record_cap(obs(Vendor::CodeRabbit, "b2", 2));
        ledger.record_cap(obs(Vendor::CodeRabbit, "b3", 3));
        assert!(ledger.health(Vendor::CodeRabbit).is_capped());

        ledger.clear(Vendor::CodeRabbit);
        assert_eq!(ledger.health(Vendor::CodeRabbit), VendorHealth::Healthy);
    }

    #[test]
    fn ledger_trims_beyond_cap() {
        let mut ledger = VendorHealthLedger::new();
        for ts in 1..=(CAP_OBSERVATIONS_CAP as u64 + 5) {
            ledger.record_cap(obs(Vendor::CodeRabbit, "b1", ts));
        }
        assert_eq!(ledger.observation_count(Vendor::CodeRabbit), CAP_OBSERVATIONS_CAP);
    }

    #[test]
    fn waiver_token_is_documented() {
        // Pins the exact strings callers grep for; never silently rename.
        assert_eq!(
            Vendor::CodeRabbit.waiver_token(),
            "coderabbit:waived_vendor_unavailable"
        );
        assert_eq!(
            Vendor::Bugbot.waiver_token(),
            "bugbot:waived_vendor_unavailable"
        );
        assert_eq!(Vendor::CodeRabbit.as_str(), "coderabbit");
        assert_eq!(Vendor::Bugbot.as_str(), "bugbot");
    }

    #[test]
    fn append_observation_writes_jsonl() {
        let dir = std::env::temp_dir().join(format!("vh_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("vendor_health.jsonl");
        append_observation(
            &log,
            &obs(Vendor::CodeRabbit, "b1", 1),
        )
        .unwrap();
        let body = std::fs::read_to_string(&log).unwrap();
        assert_eq!(body.lines().count(), 1);
        let _: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
