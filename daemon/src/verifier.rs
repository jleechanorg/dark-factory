// Task 8: 7/8-green gate assessment (design doc §5, spec §4.2.5). Read-only —
// this module only reads SCM state and returns a `GateReport`; it never
// mutates a branch, closes a PR, or merges (that discipline lives in the
// harness / dispatch layers, not here).
//
// `PrSnapshot` (owned by `tools.rs`) carries gates 1-5's raw inputs (CI,
// mergeable, CodeRabbit, Bugbot, unresolved thread count) but intentionally
// has no fields for the evidence floor (gate 6: non-test changed LOC +
// integration-evidence marker) or the Skeptic verdict (gate 7) — those two
// gates' raw inputs don't come from `gh pr view`/GraphQL the way `PrSnapshot`
// does, and this task's file-ownership boundary is `verifier.rs` only (Task 8
// scope note: never edit `tools.rs`). `PrEvidence` is a `verifier`-local
// input type carrying exactly that data, kept out of `PrSnapshot` so the tool
// trait boundary doesn't grow fields only this task needs.
use crate::config::Config;
use crate::tools::Scm;

/// Non-test changed LOC above this floor requires an integration-evidence
/// marker in the PR body (spec §4.2.5 "Evidence floor").
const EVIDENCE_FLOOR_LOC: u32 = 100;

/// The 7 named gates, in the fixed order `GateReport::results` is reported in
/// (spec §4.2.5, numbered 1-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateName {
    Ci,
    NoConflicts,
    CodeRabbitApproved,
    BugbotClean,
    CommentsResolved,
    EvidenceFloor,
    Skeptic,
}

/// One gate's verdict. `Unknown` and `Red` are deliberately distinct variants
/// (design doc §5: "Unknown ≠ Red: infra vs verdict") — `Unknown` means the
/// gate could not be evaluated (SCM API error, missing/unparseable Skeptic
/// verdict); `Red` means the gate WAS evaluated and failed. Callers (dispatch,
/// re-roll routing) must never treat the two as equivalent: only a `Red`
/// gate is evidence of a real defect, an `Unknown` gate is evidence the
/// verifier itself needs a retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    Green,
    Red(String),
    Unknown(String),
}

impl GateResult {
    fn is_green(&self) -> bool {
        matches!(self, GateResult::Green)
    }
}

/// Full assessment for one PR: every gate's verdict plus the aggregate
/// `all_green` flag. `all_green` is `true` only when every one of the 7
/// gates is `Green` — a single `Unknown` gate forces `all_green=false` in
/// exactly the same way a `Red` gate does (can't-verify is not "pass"), but
/// the two remain distinguishable via `results` for diagnosis/routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    pub results: [(GateName, GateResult); 7],
    pub all_green: bool,
}

/// Stage-1 Skeptic verdict grammar (spec §4.2.5): `pass|warn|fail`. `Warn` is
/// non-blocking — it still counts as `Green` for gate 7 but the caller may
/// choose to surface the warning text elsewhere; only `Fail` makes the gate
/// `Red`, and an absent/unparseable verdict makes it `Unknown` (never `Red`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkepticVerdict {
    Pass,
    Warn(String),
    Fail(String),
}

/// Parse the Stage-1 Skeptic verdict grammar. ZFC note: this is NOT judgment —
/// the verdict word itself was already produced by a model (the lite
/// verifier's recorded `GATE_ASSESSMENT` event, or an `Llm::judge` adversarial
/// call per design doc §5); this function only recognizes the three fixed
/// grammar tokens the model contract requires it to emit, exactly like
/// `runner/handlers.py::_parse_verdict`'s anchored-marker parsing for
/// pass/warn/fail in the Python pipeline runner. Anything else is a parse
/// failure, not a heuristic guess.
pub fn parse_skeptic_verdict(raw: &str) -> Option<SkepticVerdict> {
    let trimmed = raw.trim();
    let (token, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((t, r)) => (t, r.trim()),
        None => (trimmed, ""),
    };
    match token.to_ascii_lowercase().as_str() {
        "pass" => Some(SkepticVerdict::Pass),
        "warn" => Some(SkepticVerdict::Warn(rest.to_string())),
        "fail" => Some(SkepticVerdict::Fail(rest.to_string())),
        _ => None,
    }
}

/// `verifier`-local input for the two gates `PrSnapshot` doesn't cover: the
/// evidence floor (gate 6) and the Skeptic review (gate 7). See the module
/// doc comment for why these live here instead of on `tools::PrSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrEvidence {
    /// Non-test changed LOC in the diff (spec §4.2.5 evidence floor).
    pub non_test_changed_loc: u32,
    /// Whether the PR body/comments carry an integration-evidence marker
    /// (Layer-2+ proof, not unit-only) for diffs over the LOC floor.
    pub has_integration_evidence_marker: bool,
    /// Recorded verdict for gate 7, already parsed by `parse_skeptic_verdict`
    /// (or produced directly by an `Llm` adversarial call under Stage 1).
    /// `None` means no verdict is available yet (gate 7 -> `Unknown`).
    pub skeptic_verdict: Option<SkepticVerdict>,
}

fn evidence_floor_gate(evidence: &PrEvidence) -> GateResult {
    if evidence.non_test_changed_loc <= EVIDENCE_FLOOR_LOC {
        return GateResult::Green;
    }
    if evidence.has_integration_evidence_marker {
        GateResult::Green
    } else {
        GateResult::Red("evidence floor".to_string())
    }
}

fn skeptic_gate(evidence: &PrEvidence) -> GateResult {
    match &evidence.skeptic_verdict {
        None => GateResult::Unknown("no Skeptic verdict recorded".to_string()),
        Some(SkepticVerdict::Pass) => GateResult::Green,
        Some(SkepticVerdict::Warn(_)) => GateResult::Green, // warn is non-blocking (spec §4.2.5)
        Some(SkepticVerdict::Fail(reason)) => GateResult::Red(reason.clone()),
    }
}

/// Assess all 7 gates for `pr` (spec §4.2.5, design doc §5). `cfg` is accepted
/// per the design-doc signature for future per-repo gate config (unused today
/// — Stage 1 has no per-gate config knobs yet); `#[allow(unused_variables)]`
/// documents that rather than dropping the parameter ahead of a design-doc
/// revision.
#[allow(unused_variables)]
pub fn assess(
    scm: &dyn Scm,
    pr: u64,
    cfg: &Config,
    evidence: &PrEvidence,
) -> Result<GateReport, crate::errors::DaemonError> {
    let snapshot = match scm.pr_snapshot(pr) {
        Ok(snapshot) => snapshot,
        Err(e) => {
            // The whole snapshot fetch failed (e.g. the GraphQL thread query
            // errored) — every SCM-sourced gate is unverifiable, not merely
            // gate 5. This is the honest Unknown reading: we cannot say the
            // PR is bad, only that we could not check it this tick.
            let reason = format!("SCM pr_snapshot fetch failed: {e}");
            let results = [
                (GateName::Ci, GateResult::Unknown(reason.clone())),
                (GateName::NoConflicts, GateResult::Unknown(reason.clone())),
                (
                    GateName::CodeRabbitApproved,
                    GateResult::Unknown(reason.clone()),
                ),
                (GateName::BugbotClean, GateResult::Unknown(reason.clone())),
                (GateName::CommentsResolved, GateResult::Unknown(reason)),
                (GateName::EvidenceFloor, evidence_floor_gate(evidence)),
                (GateName::Skeptic, skeptic_gate(evidence)),
            ];
            let all_green = results.iter().all(|(_, r)| r.is_green());
            return Ok(GateReport { results, all_green });
        }
    };

    let ci = if snapshot.ci_success {
        GateResult::Green
    } else {
        GateResult::Red("CI check-run(s) not all success".to_string())
    };

    let no_conflicts = if snapshot.mergeable {
        GateResult::Green
    } else {
        GateResult::Red("PR is not mergeable (conflicts)".to_string())
    };

    let coderabbit = if snapshot.coderabbit_approved {
        GateResult::Green
    } else {
        GateResult::Red("CodeRabbit review is not APPROVED".to_string())
    };

    let bugbot = if snapshot.bugbot_error_count == 0 {
        GateResult::Green
    } else {
        GateResult::Red(format!(
            "{} Bugbot error-severity comment(s)",
            snapshot.bugbot_error_count
        ))
    };

    let comments_resolved = if snapshot.unresolved_thread_count == 0 {
        GateResult::Green
    } else {
        GateResult::Red(format!(
            "{} unresolved review thread(s)",
            snapshot.unresolved_thread_count
        ))
    };

    let evidence_floor = evidence_floor_gate(evidence);
    let skeptic = skeptic_gate(evidence);

    let results = [
        (GateName::Ci, ci),
        (GateName::NoConflicts, no_conflicts),
        (GateName::CodeRabbitApproved, coderabbit),
        (GateName::BugbotClean, bugbot),
        (GateName::CommentsResolved, comments_resolved),
        (GateName::EvidenceFloor, evidence_floor),
        (GateName::Skeptic, skeptic),
    ];
    let all_green = results.iter().all(|(_, r)| r.is_green());

    Ok(GateReport { results, all_green })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::DaemonError;
    use crate::tools::{Issue, Permission, PrSnapshot};
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Minimal in-module `Scm` fake (table-driven tests only need
    /// `pr_snapshot`; the other four methods are unused stubs). Kept local
    /// to `verifier.rs` rather than importing `daemon::tests::common`,
    /// which is a `tests/` integration-test-only module the lib crate
    /// itself cannot see from `src/`.
    #[derive(Default)]
    struct FakeScm {
        snapshots: HashMap<u64, PrSnapshot>,
        calls: RefCell<Vec<String>>,
    }

    impl Scm for FakeScm {
        fn labeled_issues(&self, _label: &str) -> Result<Vec<Issue>, DaemonError> {
            Ok(Vec::new())
        }

        fn collaborator_permission(&self, _login: &str) -> Result<Permission, DaemonError> {
            Ok(Permission::None)
        }

        fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
            self.calls.borrow_mut().push(format!("pr_snapshot({pr})"));
            self.snapshots
                .get(&pr)
                .cloned()
                .ok_or_else(|| DaemonError::Tool {
                    tool: "gh".into(),
                    rc: 1,
                    stderr: format!("no scripted snapshot for pr {pr}"),
                })
        }

        fn close_pr(&self, _pr: u64, _comment: &str) -> Result<(), DaemonError> {
            Ok(())
        }
    }

    fn all_green_snapshot(pr: u64) -> PrSnapshot {
        PrSnapshot {
            pr_number: pr,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: 0,
            head_sha: "deadbeef".into(),
        }
    }

    fn test_cfg() -> Config {
        crate::config::load(std::path::Path::new("contracts/daemon.toml.example")).unwrap()
    }

    fn all_green_evidence() -> PrEvidence {
        PrEvidence {
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        }
    }

    fn gate(report: &GateReport, name: GateName) -> &GateResult {
        &report
            .results
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("gate {name:?} missing from report"))
            .1
    }

    #[test]
    fn all_green_snapshot_yields_all_green_report() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();

        let report = assess(&scm, 7, &cfg, &all_green_evidence()).unwrap();

        assert!(report.all_green, "expected all_green, got {report:?}");
        for (name, result) in &report.results {
            assert!(result.is_green(), "gate {name:?} not green: {result:?}");
        }
    }

    #[test]
    fn failing_ci_makes_gate_one_red_without_flipping_others() {
        let mut scm = FakeScm::default();
        let mut snapshot = all_green_snapshot(7);
        snapshot.ci_success = false;
        scm.snapshots.insert(7, snapshot);
        let cfg = test_cfg();

        let report = assess(&scm, 7, &cfg, &all_green_evidence()).unwrap();

        assert!(!report.all_green);
        assert!(matches!(gate(&report, GateName::Ci), GateResult::Red(_)));
        assert!(gate(&report, GateName::NoConflicts).is_green());
        assert!(gate(&report, GateName::CodeRabbitApproved).is_green());
        assert!(gate(&report, GateName::BugbotClean).is_green());
        assert!(gate(&report, GateName::CommentsResolved).is_green());
    }

    #[test]
    fn snapshot_fetch_error_marks_thread_gate_unknown_not_red() {
        // Simulates an SCM API error while gathering thread-resolution data
        // (gate 5's `pr_snapshot` includes `unresolved_thread_count`, so a
        // fetch failure surfaces here). No snapshot scripted for pr 9 at all,
        // so `pr_snapshot` returns `Err` exactly like a real GraphQL error.
        let scm = FakeScm::default();
        let cfg = test_cfg();

        let report = assess(&scm, 9, &cfg, &all_green_evidence()).unwrap();

        assert!(!report.all_green);
        let gate5 = gate(&report, GateName::CommentsResolved);
        assert!(
            matches!(gate5, GateResult::Unknown(_)),
            "expected Unknown, got {gate5:?}"
        );

        // Unknown and Red must be distinct variants with distinct reasons —
        // assert this is genuinely not the Red variant.
        assert_ne!(
            std::mem::discriminant(gate5),
            std::mem::discriminant(&GateResult::Red(String::new()))
        );
        match gate5 {
            GateResult::Unknown(reason) => assert!(!reason.is_empty()),
            other => panic!("expected Unknown(reason), got {other:?}"),
        }
    }

    #[test]
    fn unknown_gate_forces_all_green_false_same_as_red() {
        let scm = FakeScm::default(); // no snapshot -> every SCM gate Unknown
        let cfg = test_cfg();

        let report = assess(&scm, 42, &cfg, &all_green_evidence()).unwrap();

        assert!(!report.all_green);
    }

    #[test]
    fn large_diff_without_evidence_marker_is_red_evidence_floor() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            non_test_changed_loc: 150,
            has_integration_evidence_marker: false,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::EvidenceFloor) {
            GateResult::Red(reason) => assert_eq!(reason, "evidence floor"),
            other => panic!("expected Red(\"evidence floor\"), got {other:?}"),
        }
    }

    #[test]
    fn large_diff_with_evidence_marker_is_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            non_test_changed_loc: 150,
            has_integration_evidence_marker: true,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(gate(&report, GateName::EvidenceFloor).is_green());
        assert!(report.all_green);
    }

    #[test]
    fn small_diff_is_green_regardless_of_evidence_marker() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            non_test_changed_loc: 100, // exactly at the floor, not over it
            has_integration_evidence_marker: false,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(gate(&report, GateName::EvidenceFloor).is_green());
        assert!(report.all_green);
    }

    #[test]
    fn skeptic_fail_is_red_with_reason() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            skeptic_verdict: Some(SkepticVerdict::Fail("wrong fix".into())),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::Skeptic) {
            GateResult::Red(reason) => assert_eq!(reason, "wrong fix"),
            other => panic!("expected Red, got {other:?}"),
        }
    }

    #[test]
    fn skeptic_warn_is_non_blocking_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            skeptic_verdict: Some(SkepticVerdict::Warn("minor nit".into())),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(gate(&report, GateName::Skeptic).is_green());
        assert!(report.all_green);
    }

    #[test]
    fn missing_skeptic_verdict_is_unknown_not_red() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            skeptic_verdict: None,
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        assert!(matches!(
            gate(&report, GateName::Skeptic),
            GateResult::Unknown(_)
        ));
    }

    #[test]
    fn parse_skeptic_verdict_grammar() {
        assert_eq!(parse_skeptic_verdict("pass"), Some(SkepticVerdict::Pass));
        assert_eq!(parse_skeptic_verdict("PASS"), Some(SkepticVerdict::Pass));
        assert_eq!(
            parse_skeptic_verdict("warn looks risky"),
            Some(SkepticVerdict::Warn("looks risky".into()))
        );
        assert_eq!(
            parse_skeptic_verdict("fail wrong approach"),
            Some(SkepticVerdict::Fail("wrong approach".into()))
        );
        assert_eq!(parse_skeptic_verdict("maybe?"), None);
        assert_eq!(parse_skeptic_verdict(""), None);
    }
}
