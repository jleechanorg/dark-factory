// jleechan-jsby: vendor health ledger + structural-unavailable waiver.
//
// Bead jleechan-jsby (acceptance criteria 1-5): gate assessment must
// distinguish "vendor red/pending" (the existing `GateBlock::Structural`
// path that parks the bead `DISPOSITION_REQUIRED` until external conditions
// resolve) from "vendor structurally unavailable" — the new waiver tier,
// where the vendor is rate-limited or quota-capped for an extended window,
// and re-rolling the coder (or holding the bead indefinitely) cannot make
// the vendor deliver a verdict.
//
// This module owns three contracts:
//
//   1. The vendor health ledger (`VendorHealthLedger`). A small in-memory
//      state machine that records each vendor's most recent assessment
//      outcomes; N consecutive `capped` results auto-escalate the vendor
//      to `VendorStatus::Waived`, a single successful review auto-clears.
//      Production threshold `CONSECUTIVE_CAPPED_FOR_WAIVER = 3` mirrors the
//      matching escalation-dedup ledger precedent (PR #447 / 1s2q).
//
//   2. The `VendorWaiver` token (`<vendor>:waived_vendor_unavailable`),
//      the merge-authority substitute the task data explicitly mandates.
//      Compensating coverage (skeptic Pass with >=2 families AND /er Pass)
//      is the documented requirement to GREEN under a waiver — the
//      waiver does NOT confer a free pass.
//
//   3. The skeptic-prompt contract: the prompt construction
//      (`skeptic_prompt_with_waivers`) takes the waived-vendor list and
//      tells the skeptic reviewer not to penalize the lane for the waived
//      vendor's missing deliverable. This is acceptance criterion 3.
//
// Telemetry: `emit_vendor_waived` writes one `VENDOR_WAIVED` event to the
// daemon JSONL channel so operators can see when a vendor was waived and
// when it auto-recovered (acceptance criterion 4).
//
// Design notes:
//
//   - ZFC discipline: the waiver state machine keys on STRUCTURED inputs
//     (vendor name + assessment outcome bool + epoch), never on
//     free-text heuristics. Auto-escalation requires exactly
//     `CONSECUTIVE_CAPPED_FOR_WAIVER` consecutive capped assessments;
//     auto-clear requires a single successful review. There is no keyword
//     matching on the vendor response text.
//
//   - State: the ledger is intentionally in-memory (and per-Ledger,
//     single-bead scope) — the global vendor health state lives in the
//     daemon's state store separately (bead jleechan-jsby r2 deferral,
//     not in scope here). The per-bead ledger is sufficient for the
//     skeptic-prompt construction; the global ledger is a separate
//     concurrent-races concern.
//
//   - Merge authority substitution: the SUBSTITUTION happens in
//     `verifier.rs::assess` (gate tier) and `classify_chain` (chain tier).
//     This module only OWNS the data shapes and the prompt contract; the
//     gate logic stays in verifier.rs to keep the file-ownership boundary
//     the verifier.rs module doc comment enforces.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Bead jleechan-jsby: the canonical waiver token the task data mandates.
/// The merge-authority key set substitutes this in place of the vendor's
/// review presence when the vendor is structurally unavailable.
///
/// Format: `<vendor>:waived_vendor_unavailable`. Constructed dynamically
/// per vendor via [`waiver_token_for`] so the gate logic can
/// round-trip-assert the exact string.
pub const WAIVER_TOKEN_SUFFIX: &str = "waived_vendor_unavailable";

/// Bead jleechan-jsby / issue: N consecutive capped assessments required
/// to auto-escalate a vendor from `Degraded` to `Waived`. Mirrors the
/// escalation-dedup precedent in `state.rs` (PR #447 / 1s2q) — that work
/// chose 3 as the smallest number that rejects noise without producing
/// outages, and the same trade-off applies here. A vendor that recovers
/// on assessment N+1 resets the streak immediately.
pub const CONSECUTIVE_CAPPED_FOR_WAIVER: u32 = 3;

/// Bead jleechan-jsby: the telemetry event name (key in the JSONL
/// `eventType` field). Telemetry ingestors (factory-overlay.sh,
/// auto-merge-guard.sh) key on this string.
pub const TELEMETRY_EVENT_VENDOR_WAIVED: &str = "VENDOR_WAIVED";

/// Bead jleechan-jsby: the canonical reason string emitted when a vendor
/// is auto-escalated to `Waived` because of three consecutive capped
/// assessments. Operators triaging VENDOR_WAIVED telemetry look for this
/// exact reason before investigating vendor-level outages.
pub const AUTO_ESCALATION_REASON: &str =
    "quota_capped_3_consecutive_assessments";

// ---------------------------------------------------------------------------
// Vendor state machine
// ---------------------------------------------------------------------------

/// The health ledger's view of one vendor's recent assessment outcomes.
///
/// The three variants match the production taxonomy from
/// `verifier::GateBlock` (bead jleechan-jsby / acceptance criterion 1):
/// the gate assessment already distinguishes `Green` (Healthy) from
/// `Unknown` (Degraded) from `Unknown`-with-no-recovery-in-sight
/// (Waived). Adding the third variant is the minimum needed to render
/// the vendor health signal as first-class data; ad-hoc string checks
/// on the gate's `reason` field are explicitly forbidden by ZFC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VendorStatus {
    /// All recent assessments were successful. The vendor is healthy —
    /// no waiver, no compensating coverage needed.
    Healthy,
    /// One or more recent assessments were capped, but the streak has
    /// NOT yet reached `CONSECUTIVE_CAPPED_FOR_WAIVER`. The gate still
    /// surfaces `Unknown` (per `classify_nongreen_gate`), and the chain
    /// enters `HoldDisposition` — the pre-bead behavior is preserved for
    /// this tier.
    Degraded,
    /// `CONSECUTIVE_CAPPED_FOR_WAIVER` consecutive capped assessments.
    /// The vendor is structurally unavailable; the merge-authority key
    /// set substitutes the explicit waiver token and the gate logic in
    /// `verifier::assess` consults the compensating coverage (skeptic +
    /// /er + cross-model) before promoting a PR to all_green.
    Waived { token: String },
}

/// Per-vendor health record. Tracks (a) the consecutive capped assessment
/// count (reset by any successful assessment) and (b) the most-recent
/// assessment epoch (the audit trail `emit_vendor_waived` writes into
/// telemetry).
#[derive(Debug, Clone, Default)]
struct VendorRecord {
    consecutive_capped: u32,
    last_assessment_epoch: u64,
}

impl VendorRecord {
    fn new() -> Self {
        Self {
            consecutive_capped: 0,
            last_assessment_epoch: 0,
        }
    }
}

/// The vendor-health ledger. One instance per bead (the gating entry point
/// the skeptic prompt consults); the daemon-level cross-bead ledger is a
/// separate, concurrent concern (deferred to r2).
///
/// ZFC: this state machine is pure-data — it accepts `(vendor, ok, epoch)`
/// tuples, applies the deterministic threshold (`3 capped -> Waived`,
/// `1 ok -> Healthy`), and exposes a single read API (`status`,
/// `waivers`/`context`). No keyword matching, no free-text inspection
/// of vendor responses.
#[derive(Debug, Clone, Default)]
pub struct VendorHealthLedger {
    records: HashMap<String, VendorRecord>,
}

impl VendorHealthLedger {
    /// Record one assessment outcome for `vendor`.
    ///
    /// Semantics:
    ///   - `ok = true` resets the consecutive-capped counter and returns
    ///     the vendor to `Healthy`.
    ///   - `ok = false` increments the consecutive-capped counter.
    ///     Reaching `CONSECUTIVE_CAPPED_FOR_WAIVER` escalates the vendor
    ///     to `Waived`.
    ///   - Each call updates `last_assessment_epoch` regardless of `ok`.
    ///
    /// The function returns the post-call `VendorStatus` so the caller
    /// can react to transitions (e.g. emit a VENDOR_WAIVED event on the
    /// Healthy -> Waived edge).
    pub fn record_assessment(
        &mut self,
        vendor: &str,
        ok: bool,
        epoch: u64,
    ) -> VendorStatus {
        let prior = self
            .records
            .entry(vendor.to_string())
            .or_default();
        prior.last_assessment_epoch = epoch;
        if ok {
            prior.consecutive_capped = 0;
        } else {
            prior.consecutive_capped = prior.consecutive_capped.saturating_add(1);
        }
        let consecutive = prior.consecutive_capped;
        self.compute_status(vendor, consecutive)
    }

    /// Compute the current status without mutating state (read-only).
    pub fn status(&self, vendor: &str) -> VendorStatus {
        let consecutive = self
            .records
            .get(vendor)
            .map(|r| r.consecutive_capped)
            .unwrap_or(0);
        self.compute_status(vendor, consecutive)
    }

    fn compute_status(&self, vendor: &str, consecutive: u32) -> VendorStatus {
        if consecutive >= CONSECUTIVE_CAPPED_FOR_WAIVER {
            VendorStatus::Waived {
                token: waiver_token_for(vendor),
            }
        } else if consecutive > 0 {
            VendorStatus::Degraded
        } else {
            VendorStatus::Healthy
        }
    }

    /// Build the context the skeptic-prompt builder consumes. Mirrors the
    /// production pattern of pushing structured data into reviewer prompts
    /// rather than free-text (cf. `parse_skeptic_verdict`'s subsystem
    /// tagging in `verifier.rs`).
    pub fn context(&self) -> VendorWaiverContext {
        let waived_vendors = self
            .records
            .iter()
            .filter_map(|(vendor, record)| {
                if record.consecutive_capped >= CONSECUTIVE_CAPPED_FOR_WAIVER {
                    Some(vendor.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        VendorWaiverContext { waived_vendors }
    }
}

/// The skeptic-prompt contract's data — the list of waived vendors and
/// the canonical token for each. The skeptic-prompt builder
/// (`skeptic_prompt_with_waivers`) reads this and injects a structured
/// "the following vendors are waived for structural unavailability; do
/// not fail the lane for their missing review" section. Default impl
/// is the empty case — no waivers active, prompt is unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorWaiverContext {
    pub waived_vendors: Vec<String>,
}

impl VendorWaiverContext {
    pub fn is_empty(&self) -> bool {
        self.waived_vendors.is_empty()
    }

    /// Merge two contexts — the merged `VendorWaiverContext` has the union
    /// of waived vendors (no duplicates). Used to combine the per-bead
    /// ledger's context with any operator-supplied waivers.
    pub fn merge(&self, other: &VendorWaiverContext) -> VendorWaiverContext {
        let mut all: Vec<String> = self.waived_vendors.clone();
        for v in &other.waived_vendors {
            if !all.contains(v) {
                all.push(v.clone());
            }
        }
        VendorWaiverContext {
            waived_vendors: all,
        }
    }
}

/// Bead jleechan-jsby: the per-bead waiver payload the gate logic
/// consults during `assess`. Combines the vendor identity with the
/// canonical token, the compensating-coverage flag (set when skeptic +
/// /er + cross-model collectively demonstrate non-vendor coverage), and
/// the bead for telemetry attribution. The `compensating_pass` flag is
/// what prevents the waiver from acting as a free pass — see the
/// `assess_waived_vendor_without_compensating_green_*` regression tests
/// in `verifier.rs` for the fail-closed semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorWaiver {
    pub vendor: String,
    pub token: String,
    pub compensating_pass: bool,
    pub bead_id: String,
}

impl VendorWaiver {
    /// Construct the per-vendor waiver token. Format:
    /// `<vendor>:waived_vendor_unavailable`. Round-trip-stable for the
    /// merge-authority substitute assertion in the gate logic.
    pub fn token_for(vendor: &str) -> String {
        waiver_token_for(vendor)
    }
}

/// Helper: build the canonical waiver token for `vendor`. Exposed
/// `pub(crate)` so the gate logic in `verifier.rs` can construct the same
/// string verbatim from the `vendor` field of a `GateResult::Unknown(_)`
/// reason without depending on the `VendorHealthLedger` instance.
pub(crate) fn waiver_token_for(vendor: &str) -> String {
    format!("{vendor}:{WAIVER_TOKEN_SUFFIX}")
}

// ---------------------------------------------------------------------------
// Skeptic-prompt contract
// ---------------------------------------------------------------------------

/// Build the skeptic prompt with the waived-vendor list injected. When
/// `ctx` is empty, the prompt is just the diff body unchanged (preserves
/// the pre-bead behavior; gates with no active waivers are unaffected).
/// When `ctx` is non-empty, a structured WAIVED-VENDORS section is
/// injected so the skeptic reviewer does not fail the lane for missing
/// reviews from the waived vendor.
///
/// The injected section is the EXACT contract from bead jleechan-jsby
/// acceptance criterion 3 — by name, not summary, so operator audits
/// can grep for it in prompt records.
pub fn skeptic_prompt_with_waivers(diff_body: &str, ctx: &VendorWaiverContext) -> String {
    if ctx.is_empty() {
        return diff_body.to_string();
    }
    // The injected section is structured (markdown heading + bullet list)
    // so the skeptic reviewer can emit it back as the waived-vendors
    // portion of their structured verdict — accepted by the merge
    // authority as evidence it consulted the waiver context.
    let list = ctx
        .waived_vendors
        .iter()
        .map(|v| format!("- {v}:{WAIVER_TOKEN_SUFFIX}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{diff_body}\n\n\
         <!-- WAIVED-VENDORS -->\n\
         The following vendors are structurally unavailable (waiver active).\n\
         Do not penalize this lane for missing reviews from these vendors:\n\
         {list}\n\
         <!-- /WAIVED-VENDORS -->\n"
    )
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// Emit a `VENDOR_WAIVED` event to the daemon JSONL channel. Call this
/// from `vendor_health` consumers on the Healthy -> Waived edge so
/// operators see when a vendor was waived.
///
/// Wire format (one line per emission; `serde_json`'s default `Value` to
/// keep the schema simple and aligned with `telemetry::TelemetryEvent`):
///
/// ```json
/// {
///   "eventType": "VENDOR_WAIVED",
///   "beadId": "...",
///   "context": {
///     "vendor": "coderabbit",
///     "reason": "quota_capped_3_consecutive_assessments",
///     "waiver_token": "coderabbit:waived_vendor_unavailable",
///     "epoch": 1700000000
///   }
/// }
/// ```
///
/// Failure mode: returns `Err` on filesystem errors (matching
/// `telemetry::emit`'s contract). Callers may ignore — emission is
/// observational, never blocking — but logging the error surfaces audit-
/// trail gaps.
pub fn emit_vendor_waived(
    log_path: &Path,
    bead_id: &str,
    vendor: &str,
    reason: &str,
    waiver_token: &str,
    epoch: u64,
) -> Result<(), crate::errors::DaemonError> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let payload = serde_json::json!({
        "eventType": TELEMETRY_EVENT_VENDOR_WAIVED,
        "beadId": bead_id,
        "context": {
            "vendor": vendor,
            "reason": reason,
            "waiver_token": waiver_token,
            "epoch": epoch,
        }
    });
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| crate::errors::DaemonError::Config(format!("vendor_waived open: {e}")))?;
    let line = serde_json::to_string(&payload)
        .map_err(|e| crate::errors::DaemonError::Parse(format!("vendor_waived encode: {e}")))?;
    writeln!(f, "{line}")
        .map_err(|e| crate::errors::DaemonError::Config(format!("vendor_waived write: {e}")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiver_token_for_uses_canonical_format() {
        assert_eq!(waiver_token_for("coderabbit"), "coderabbit:waived_vendor_unavailable");
        assert_eq!(waiver_token_for("bugbot"), "bugbot:waived_vendor_unavailable");
        // The exact literal matches the task data's mandate verbatim.
        assert!(waiver_token_for("coderabbit").ends_with(WAIVER_TOKEN_SUFFIX));
    }

    #[test]
    fn ledger_starts_healthy() {
        let l = VendorHealthLedger::default();
        assert_eq!(l.status("coderabbit"), VendorStatus::Healthy);
        assert_eq!(l.status("bugbot"), VendorStatus::Healthy);
    }

    #[test]
    fn ledger_degraded_then_waived_at_threshold() {
        let mut l = VendorHealthLedger::default();
        // 1 capped → Degraded (not yet Waived).
        l.record_assessment("coderabbit", false, 0);
        assert_eq!(l.status("coderabbit"), VendorStatus::Degraded);
        // 2 capped → Degraded.
        l.record_assessment("coderabbit", false, 1);
        assert_eq!(l.status("coderabbit"), VendorStatus::Degraded);
        // 3 capped → Waived (auto-escalation).
        l.record_assessment("coderabbit", false, 2);
        assert_eq!(
            l.status("coderabbit"),
            VendorStatus::Waived {
                token: "coderabbit:waived_vendor_unavailable".into()
            }
        );
    }

    #[test]
    fn ledger_auto_clears_on_single_success() {
        let mut l = VendorHealthLedger::default();
        for i in 0..3 {
            l.record_assessment("coderabbit", false, i);
        }
        assert!(matches!(l.status("coderabbit"), VendorStatus::Waived { .. }));
        // One successful review auto-clears the waiver.
        l.record_assessment("coderabbit", true, 99);
        assert_eq!(l.status("coderabbit"), VendorStatus::Healthy);
    }

    #[test]
    fn ledger_per_vendor_independence() {
        let mut l = VendorHealthLedger::default();
        for i in 0..3 {
            l.record_assessment("coderabbit", false, i);
        }
        // bugbot has not yet hit the threshold — Healthy.
        assert_eq!(l.status("bugbot"), VendorStatus::Healthy);
        assert_eq!(
            l.status("coderabbit"),
            VendorStatus::Waived {
                token: "coderabbit:waived_vendor_unavailable".into()
            }
        );
    }

    #[test]
    fn ledger_context_collects_all_waived_vendors() {
        let mut l = VendorHealthLedger::default();
        for i in 0..3 {
            l.record_assessment("coderabbit", false, i);
            l.record_assessment("bugbot", false, i);
        }
        let ctx = l.context();
        assert_eq!(ctx.waived_vendors.len(), 2);
        assert!(ctx.waived_vendors.contains(&"coderabbit".into()));
        assert!(ctx.waived_vendors.contains(&"bugbot".into()));
    }

    #[test]
    fn ledger_context_empty_when_no_waivers() {
        let l = VendorHealthLedger::default();
        let ctx = l.context();
        assert!(ctx.is_empty());
    }

    #[test]
    fn context_merge_unions_vendors_without_duplicates() {
        let a = VendorWaiverContext {
            waived_vendors: vec!["coderabbit".into()],
        };
        let b = VendorWaiverContext {
            waived_vendors: vec!["bugbot".into(), "coderabbit".into()],
        };
        let merged = a.merge(&b);
        assert_eq!(merged.waived_vendors.len(), 2);
        assert!(merged.waived_vendors.contains(&"coderabbit".into()));
        assert!(merged.waived_vendors.contains(&"bugbot".into()));
    }

    #[test]
    fn skeptic_prompt_includes_waiver_section_when_ctx_nonempty() {
        let ctx = VendorWaiverContext {
            waived_vendors: vec!["coderabbit".into(), "bugbot".into()],
        };
        let prompt = skeptic_prompt_with_waivers("Review this diff...", &ctx);
        assert!(prompt.contains("Review this diff..."));
        assert!(prompt.contains("WAIVED-VENDORS"));
        assert!(prompt.contains("coderabbit:waived_vendor_unavailable"));
        assert!(prompt.contains("bugbot:waived_vendor_unavailable"));
    }

    #[test]
    fn skeptic_prompt_unchanged_when_ctx_empty() {
        let ctx = VendorWaiverContext::default();
        let body = "Review this diff...\nVerdict: pass\n";
        let prompt = skeptic_prompt_with_waivers(body, &ctx);
        // Empty waivers → prompt is the diff body verbatim, no extra
        // section injected.
        assert_eq!(prompt, body);
    }

    #[test]
    fn emit_vendor_waived_writes_jsonl_line() {
        let dir = std::env::temp_dir().join("vendor_waiver_emit_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon.jsonl");
        emit_vendor_waived(
            &log,
            "b-1",
            "coderabbit",
            AUTO_ESCALATION_REASON,
            &waiver_token_for("coderabbit"),
            1_800_000_000,
        )
        .unwrap();
        let body = std::fs::read_to_string(&log).unwrap();
        let line = body.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["eventType"], TELEMETRY_EVENT_VENDOR_WAIVED);
        assert_eq!(v["beadId"], "b-1");
        assert_eq!(v["context"]["vendor"], "coderabbit");
        assert_eq!(v["context"]["reason"], AUTO_ESCALATION_REASON);
        assert_eq!(
            v["context"]["waiver_token"],
            "coderabbit:waived_vendor_unavailable"
        );
        assert_eq!(v["context"]["epoch"], 1_800_000_000);
    }
}
