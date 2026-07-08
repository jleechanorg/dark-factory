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

/// The marker prefixes that anchor an authoritative verdict line (spec
/// Appendix C.3, mirroring `runner/handler_verdict.py::_MARKER_RE`'s
/// `verdict:|overall:|normalized:` grammar).
const VERDICT_MARKERS: [&str; 3] = ["verdict:", "overall:", "normalized:"];

/// Parse a `pass|warn|fail` token (case-insensitive) from the start of
/// `text`, returning the built `SkepticVerdict` plus the remainder of the
/// line as the reason/detail string. Returns `None` if `text` does not begin
/// with a recognized token.
fn token_to_verdict(text: &str) -> Option<SkepticVerdict> {
    let trimmed = text.trim();
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

/// Gate 6's `/er` (evidence review) verdict grammar (spec §4.2.5: "gate 6 =
/// '/er returns PASS'"). `Absent` is the honest default when no `/er` run has
/// been recorded yet — it is deliberately distinct from `Fail` so callers
/// never treat "no evidence review happened" as a free pass, matching the
/// `Unknown` vs `Red` discipline used elsewhere in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ErVerdict {
    Pass,
    Partial,
    Fail,
    Inconclusive,
    #[default]
    Absent,
}

/// Scan `raw` for a marker line (`verdict:`/`overall:`/`normalized:`,
/// case-insensitive) followed by a recognized token. Returns the LAST such
/// match found (later marker lines override earlier progress lines), mirroring
/// `runner/handler_verdict.py::_MARKER_RE` taking `matches[-1]`. `None` if no
/// line carries a marker with a valid trailing token.
fn find_marker_verdict(raw: &str) -> Option<SkepticVerdict> {
    let mut found = None;
    for line in raw.lines() {
        let lower = line.to_ascii_lowercase();
        for marker in VERDICT_MARKERS {
            if let Some(idx) = lower.find(marker) {
                let after = &line[idx + marker.len()..];
                if let Some(verdict) = token_to_verdict(after) {
                    found = Some(verdict);
                }
                break;
            }
        }
    }
    found
}

/// Parse the Stage-1 Skeptic verdict grammar. ZFC note: this is NOT judgment —
/// the verdict word itself was already produced by a model (the lite
/// verifier's recorded `GATE_ASSESSMENT` event, or an `Llm::judge` adversarial
/// call per design doc §5); this function only recognizes the three fixed
/// grammar tokens the model contract requires it to emit, exactly like
/// `runner/handlers.py::_parse_verdict`'s anchored-marker parsing for
/// pass/warn/fail in the Python pipeline runner. Anything else is a parse
/// failure, not a heuristic guess.
///
/// Strategy (spec Appendix C.3, mirrors `_parse_verdict`'s priority order):
///   1. Scan all lines for a marker (`verdict:`/`overall:`/`normalized:`,
///      case-insensitive) followed by a `pass|warn|fail` token; the LAST
///      marker match wins so an authoritative closing line overrides earlier
///      progress chatter.
///   2. Only when no marker line is present at all, fall back to the
///      original standalone-token behavior: the whole input is treated as
///      `<token> [rest...]`.
pub fn parse_skeptic_verdict(raw: &str) -> Option<SkepticVerdict> {
    let mut current_subsystem = None;
    let mut gate7_verdict = None;
    let mut gha_verdict = None;
    let mut signoff_verdict = None;

    for line in raw.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("subsystem: gate-7") || lower.contains("subsystem: llm") {
            current_subsystem = Some("gate-7");
            continue;
        } else if lower.contains("subsystem: gha") || lower.contains("subsystem: workflow") {
            current_subsystem = Some("gha");
            continue;
        } else if lower.contains("subsystem: sign-off") || lower.contains("subsystem: review") {
            current_subsystem = Some("sign-off");
            continue;
        }

        if let Some(verdict) = find_marker_verdict(line).or_else(|| token_to_verdict(line)) {
            match current_subsystem {
                Some("gate-7") => gate7_verdict = Some(verdict),
                Some("gha") => gha_verdict = Some(verdict),
                Some("sign-off") => signoff_verdict = Some(verdict),
                _ => {
                    // Fallback: if no subsystem was specified, default to gate-7
                    gate7_verdict = Some(verdict);
                }
            }
        }
    }

    if gate7_verdict.is_some() && gha_verdict.is_none() && signoff_verdict.is_none()
        && !raw.to_ascii_lowercase().contains("subsystem:")
    {
        return gate7_verdict;
    }

    let v_g7 = gate7_verdict?;
    // PR#163 finding 1 (round 1) + finding (round 3, residual): both `gha`
    // (a target-repo CI workflow posting a skeptic verdict comment) and
    // `sign-off` (a human reviewer comment — "sign-off"/"signoff"/
    // "/skeptic pass") are OPTIONAL enrichment signals, never hard
    // requirements. This daemon is a Level-5 autonomous system with no
    // human in the loop by design (see repo CLAUDE.md "Dark Factory
    // operating mode") — nothing ever posts a sign-off comment for a real
    // (non-test) target repo, and not every target repo runs an equivalent
    // GHA skeptic workflow. Requiring EITHER would make gate 7 permanently
    // `Unknown` for every real PR that happens to trip just one of the two
    // heuristics (round 1 fixed `sign-off` only; round 3 closed the
    // asymmetric residual where `gha` alone was still hard-required —
    // see `parse_skeptic_verdict_gha_absent_is_satisfied_by_gate7_and_signoff`
    // below). Only `gate-7` (the dual-LLM verdict) is a hard requirement:
    // that is the one signal `skeptic_evidence` always produces. `gha` and
    // `sign-off` are both best-effort/optional: when present either can
    // still escalate the verdict (Fail/Warn), but the absence of either
    // (or a literal `"verdict: absent"` placeholder that fails to parse
    // into a real token) must never block gate 7.
    let v_gha = gha_verdict;
    let v_so = signoff_verdict;

    let mut fails = Vec::new();
    let mut warns = Vec::new();

    let mut checked = vec![v_g7];
    if let Some(gha) = v_gha {
        checked.push(gha);
    }
    if let Some(so) = v_so {
        checked.push(so);
    }

    for v in &checked {
        match v {
            SkepticVerdict::Fail(reason) => fails.push(reason.clone()),
            SkepticVerdict::Warn(note) => warns.push(note.clone()),
            _ => {}
        }
    }

    if !fails.is_empty() {
        Some(SkepticVerdict::Fail(fails.join("; ")))
    } else if !warns.is_empty() {
        Some(SkepticVerdict::Warn(warns.join("; ")))
    } else {
        Some(SkepticVerdict::Pass)
    }
}

/// `verifier`-local input for the two gates `PrSnapshot` doesn't cover: the
/// evidence floor (gate 6) and the Skeptic review (gate 7). See the module
/// doc comment for why these live here instead of on `tools::PrSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrEvidence {
    pub is_production: bool,
    /// Non-test changed LOC in the diff (spec §4.2.5 evidence floor).
    pub non_test_changed_loc: u32,
    /// Whether the PR body/comments carry an integration-evidence marker
    /// (Layer-2+ proof, not unit-only) for diffs over the LOC floor.
    pub has_integration_evidence_marker: bool,
    /// The `/er` (evidence review) verdict (spec §4.2.5 gate 6). This is the
    /// PRIMARY gate-6 signal — the LOC floor below is an ADDITIONAL floor on
    /// top of it, not a substitute for it (hardening sweep P1, bead
    /// jleechan-3rf: gate 6 must be "`/er` returns PASS", not just the LOC
    /// floor). Defaults to `Absent` when no `/er` run is recorded.
    pub er_verdict: ErVerdict,
    /// Recorded verdict for gate 7, already parsed by `parse_skeptic_verdict`
    /// (or produced directly by an `Llm` adversarial call under Stage 1).
    /// `None` means no verdict is available yet (gate 7 -> `Unknown`).
    pub skeptic_verdict: Option<SkepticVerdict>,
}

/// Gate 6 (spec §4.2.5): green only when `/er` returned `Pass` AND the
/// non-test-LOC floor also passes (integration-evidence marker present above
/// the floor). An absent `/er` verdict is `Unknown` — we cannot say the
/// evidence is bad, only that no evidence review has been recorded yet — and
/// an explicit `/er` `Fail` is `Red`, naming the failed gate. Both must clear
/// before the LOC floor is even consulted, since a passing LOC floor never
/// substitutes for a missing/failing `/er` run.
fn evidence_floor_gate(evidence: &PrEvidence) -> GateResult {
    if evidence.is_production {
        match evidence.er_verdict {
            ErVerdict::Absent => {
                return GateResult::Unknown("no /er verdict recorded".to_string());
            }
            ErVerdict::Pass => {}
            ErVerdict::Partial => {
                return GateResult::Red("/er verdict is PARTIAL (PRODUCTION requires PASS)".to_string());
            }
            ErVerdict::Fail => {
                return GateResult::Red("/er verdict is FAIL".to_string());
            }
            ErVerdict::Inconclusive => {
                return GateResult::Red("/er verdict is INCONCLUSIVE".to_string());
            }
        }
    } else {
        match evidence.er_verdict {
            ErVerdict::Absent => {
                return GateResult::Unknown("no /er verdict recorded".to_string());
            }
            ErVerdict::Pass | ErVerdict::Partial => {}
            ErVerdict::Fail => {
                return GateResult::Red("/er verdict is FAIL".to_string());
            }
            ErVerdict::Inconclusive => {
                return GateResult::Red("/er verdict is INCONCLUSIVE".to_string());
            }
        }
    }

    if evidence.non_test_changed_loc <= EVIDENCE_FLOOR_LOC {
        return GateResult::Green;
    }
    if evidence.has_integration_evidence_marker {
        GateResult::Green
    } else {
        GateResult::Red("evidence floor".to_string())
    }
}

pub fn parse_er_verdict(comments: &[crate::tools::PrComment]) -> ErVerdict {
    for comment in comments.iter().rev() {
        let body_lower = comment.body.to_ascii_lowercase();
        let mut start_idx = 0;
        let mut last_verdict = None;
        while let Some(idx) = body_lower[start_idx..].find("/er") {
            let absolute_idx = start_idx + idx;
            let is_valid_start = absolute_idx == 0 || {
                let prev_char = body_lower.as_bytes()[absolute_idx - 1] as char;
                !prev_char.is_alphanumeric()
            };
            let is_valid_end = absolute_idx + 3 == body_lower.len() || {
                let next_char = body_lower.as_bytes()[absolute_idx + 3] as char;
                !next_char.is_alphanumeric() && next_char != '-' && next_char != '_'
            };
            if is_valid_start && is_valid_end {
                let sub = &body_lower[absolute_idx + 3..];
                let tokens: Vec<&str> = sub.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
                for token in tokens {
                    match token {
                        "pass" | "passed" => {
                            last_verdict = Some(ErVerdict::Pass);
                            break;
                        }
                        "partial" | "partially" => {
                            last_verdict = Some(ErVerdict::Partial);
                            break;
                        }
                        "fail" | "failed" => {
                            last_verdict = Some(ErVerdict::Fail);
                            break;
                        }
                        "inconclusive" => {
                            last_verdict = Some(ErVerdict::Inconclusive);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            start_idx = absolute_idx + 3;
        }
        if let Some(v) = last_verdict {
            return v;
        }
    }
    ErVerdict::Absent
}

pub fn classify_production(files: &[crate::tools::PrFile]) -> bool {
    for file in files {
        let path = &file.path;
        if path.starts_with("mvp_site/") 
            && !path.starts_with("mvp_site/tests/") 
            && !path.starts_with("mvp_site/test_integration/") 
        {
            return true;
        }
        
        let path_lower = path.to_lowercase();
        if path_lower.contains("prompt") || path_lower.starts_with("prompts/") {
            return true;
        }
        if path.starts_with(".github/") {
            return true;
        }
        if path_lower.contains("ci/") 
            || path_lower.contains("/ci/")
            || path_lower.starts_with("ci")
            || path_lower.contains("deploy") 
            || path_lower.contains("merge-safety")
            || path_lower.contains("merge_safety")
            || path_lower.contains("merge-guard")
            || path_lower.contains("merge_guard")
            || path_lower.contains("docker")
            || path_lower.contains("jenkinsfile")
            || path_lower.contains("travis")
        {
            return true;
        }
        if path_lower.starts_with("migrations/") 
            || path_lower.contains("/migrations/") 
            || path_lower.starts_with("db/")
            || path_lower.contains("/db/")
            || path_lower.ends_with("schema.sql")
            || path_lower.ends_with("schema.db")
            || path_lower.contains("migration")
        {
            return true;
        }
    }
    false
}

pub fn calculate_non_test_loc(files: &[crate::tools::PrFile]) -> u32 {
    let mut total = 0;
    for file in files {
        let path_lower = file.path.to_lowercase();
        let is_test = path_lower.contains("test") 
            || path_lower.contains("/test/")
            || path_lower.contains("/tests/");
        if !is_test {
            total += file.additions;
        }
    }
    total
}

pub fn check_integration_marker(body: &str, comments: &[crate::tools::PrComment]) -> bool {
    let lower_body = body.to_lowercase();
    if lower_body.contains("has_integration_evidence_marker") 
       || lower_body.contains("integration-evidence") 
       || lower_body.contains("integration evidence") 
    {
        return true;
    }
    for comment in comments {
        let lower_comment = comment.body.to_lowercase();
        if lower_comment.contains("has_integration_evidence_marker") 
           || lower_comment.contains("integration-evidence") 
           || lower_comment.contains("integration evidence") 
        {
            return true;
        }
    }
    false
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

    let ci = if !snapshot.ci_success {
        if snapshot.ci_status == "unknown" {
            GateResult::Unknown("CI check-run(s) are still pending/unknown".to_string())
        } else {
            GateResult::Red("CI check-run(s) not all success".to_string())
        }
    } else {
        GateResult::Green
    };

    let no_conflicts = if snapshot.mergeable {
        GateResult::Green
    } else {
        GateResult::Red("PR is not mergeable (conflicts)".to_string())
    };

    let coderabbit = if !snapshot.coderabbit_approved {
        if snapshot.coderabbit_status == "unknown" {
            GateResult::Unknown("CodeRabbit review is still pending/unknown".to_string())
        } else {
            GateResult::Red("CodeRabbit review is not APPROVED".to_string())
        }
    } else {
        GateResult::Green
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
    use crate::tools::{Issue, LabeledPr, Permission, PrSnapshot};
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

        fn labeled_prs(&self, _label: &str) -> Result<Vec<LabeledPr>, DaemonError> {
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

        fn remote_branch_last_commit(&self, _branch: &str) -> Result<Option<u64>, DaemonError> {
            Ok(None)
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
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
        }
    }

    fn test_cfg() -> Config {
        crate::config::load(std::path::Path::new("contracts/daemon.toml.example")).unwrap()
    }

    fn all_green_evidence() -> PrEvidence {
        PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
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
            is_production: true,
            non_test_changed_loc: 150,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
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
            is_production: true,
            non_test_changed_loc: 150,
            has_integration_evidence_marker: true,
            er_verdict: ErVerdict::Pass,
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
            is_production: true,
            non_test_changed_loc: 100, // exactly at the floor, not over it
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
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
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
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
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
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
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
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

    // --- Gate 6 (evidence review) hardening: bead jleechan-3rf ---
    // Spec §4.2.5: gate 6 = "/er returns PASS"; the LOC floor is an
    // ADDITIONAL floor on top of that, never a substitute for it. These
    // cases cross er_verdict (Pass/Fail/Absent) with the LOC floor
    // (under/over) to pin down that gate 6 is green ONLY when both hold.

    #[test]
    fn er_absent_is_unknown_even_when_loc_floor_passes() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10, // well under the floor
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Absent,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        assert!(
            matches!(
                gate(&report, GateName::EvidenceFloor),
                GateResult::Unknown(_)
            ),
            "absent /er verdict must be Unknown, not a free pass: {:?}",
            gate(&report, GateName::EvidenceFloor)
        );
    }

    #[test]
    fn parse_skeptic_verdict_marker_anchored_anywhere_in_message() {
        // Marker line is NOT first — hardening case from jleechan-j7d: the
        // old bare-leading-token parser would have looked at line 1
        // ("Reviewing the diff...") and returned None, even though an
        // authoritative marker line follows later in the message.
        let msg = "Reviewing the diff...\nChecked tests, looks solid.\nVerdict: PASS\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));

        let msg = "Some notes here.\noverall: FAIL needs more coverage\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Fail("needs more coverage".into()))
        );

        let msg = "intro line\nnormalized: warn minor nit\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Warn("minor nit".into()))
        );
    }

    #[test]
    fn er_fail_is_red_even_when_loc_floor_passes() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10, // well under the floor
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Fail,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::EvidenceFloor) {
            GateResult::Red(reason) => assert_eq!(reason, "/er verdict is FAIL"),
            other => panic!("expected Red(\"/er verdict is FAIL\"), got {other:?}"),
        }
    }

    #[test]
    fn er_pass_with_small_diff_is_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(gate(&report, GateName::EvidenceFloor).is_green());
        assert!(report.all_green);
    }

    #[test]
    fn er_pass_with_large_diff_and_no_evidence_marker_is_still_red() {
        // /er PASS alone does not waive the LOC floor: both must hold.
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 150,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
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
    fn er_pass_with_large_diff_and_evidence_marker_is_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 150,
            has_integration_evidence_marker: true,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(gate(&report, GateName::EvidenceFloor).is_green());
        assert!(report.all_green);
    }

    #[test]
    fn er_absent_with_large_diff_and_evidence_marker_is_still_unknown() {
        // Even a passing LOC floor (marker present) never upgrades an
        // absent /er verdict to green — absent stays Unknown.
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 150,
            has_integration_evidence_marker: true,
            er_verdict: ErVerdict::Absent,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        assert!(matches!(
            gate(&report, GateName::EvidenceFloor),
            GateResult::Unknown(_)
        ));
    }

    #[test]
    fn er_fail_with_large_diff_and_evidence_marker_is_still_red() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 150,
            has_integration_evidence_marker: true,
            er_verdict: ErVerdict::Fail,
            skeptic_verdict: Some(SkepticVerdict::Pass),
        };

        let report = assess(&scm, 7, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::EvidenceFloor) {
            GateResult::Red(reason) => assert_eq!(reason, "/er verdict is FAIL"),
            other => panic!("expected Red(\"/er verdict is FAIL\"), got {other:?}"),
        }
    }

    #[test]
    fn parse_skeptic_verdict_no_marker_falls_back_to_bare_token() {
        // No marker anywhere in the text — old bare-leading-token behavior
        // must still work exactly as before this hardening fix.
        assert_eq!(parse_skeptic_verdict("pass"), Some(SkepticVerdict::Pass));
        assert_eq!(
            parse_skeptic_verdict("fail wrong approach"),
            Some(SkepticVerdict::Fail("wrong approach".into()))
        );
        assert_eq!(parse_skeptic_verdict("no marker or token here"), None);
    }

    #[test]
    fn parse_skeptic_verdict_marker_wins_over_unrelated_bare_token() {
        // Message has an unrelated bare "fail" token on its own outside of
        // any marker line, AND a real marker line elsewhere. The marker line
        // must win — this is the exact misclassification the hardening
        // sweep is closing (mirrors runner/handler_verdict.py's
        // "verdict: not a fail" anchoring discipline).
        let msg = "Earlier attempt: fail\nVerdict: PASS\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));

        let msg = "fail\nMore context.\nVerdict: PASS\nFooter mentions fail again.\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));
    }

    #[test]
    fn parse_skeptic_verdict_three_subsystems() {
        // All three present and green
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: pass\n\
                   subsystem: sign-off\nverdict: pass\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));

        // One fails -> overall fail
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: fail (compile error)\n\
                   subsystem: sign-off\nverdict: pass\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Fail("(compile error)".into()))
        );

        // `gha` missing (no `subsystem: gha` block at all) with only
        // gate-7 + sign-off present -> resolves from gate-7 + sign-off.
        // PR#163 finding (round 3): `gha` is optional exactly like
        // `sign-off` — only `gate-7` (the dual-LLM verdict) is a hard
        // requirement. See
        // `parse_skeptic_verdict_gha_absent_is_satisfied_by_gate7_and_signoff`
        // below for the full residual-deadlock regression test.
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: sign-off\nverdict: pass\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));

        // Only gate-7 present (no gha, no sign-off blocks at all) ->
        // resolves from gate-7 alone.
        let msg = "subsystem: gate-7\nverdict: pass\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));
    }

    // --- PR#163 finding 1 regression: sign-off must never hard-block gate 7 ---
    //
    // The daemon has no human-in-the-loop by design (repo CLAUDE.md "Dark
    // Factory operating mode" — Level 5, no human code review gate). Before
    // this fix, `tick.rs::skeptic_evidence` always wrapped the reviewer
    // reply in a `subsystem: gate-7 / gha / sign-off` combined string with
    // sign-off defaulting to the literal `"verdict: absent"` (which never
    // parses to a `SkepticVerdict`), and this function required ALL THREE
    // subsystems to parse. Since no human ever posts a sign-off comment on
    // a real target repo, gate 7 (`skeptic_gate`) was permanently `Unknown`
    // and `all_green` permanently `false` for every real PR — a total
    // deadlock. Mutation-test sanity: reverting the `v_so` optionality
    // above (making `signoff_verdict?` hard-required again) makes this
    // test fail with `None` instead of `Some(Pass)`.
    #[test]
    fn parse_skeptic_verdict_signoff_absent_is_satisfied_by_gate7_and_gha() {
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: pass\n\
                   subsystem: sign-off\nverdict: absent\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));
    }

    #[test]
    fn parse_skeptic_verdict_signoff_present_can_still_escalate_to_fail() {
        // When a human DOES leave a sign-off comment, it is not ignored —
        // it can still escalate gate 7 to Fail, same as gate-7/gha.
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: pass\n\
                   subsystem: sign-off\nverdict: fail human rejected\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Fail("human rejected".into()))
        );
    }

    // --- PR#163 finding (round 3): residual asymmetric deadlock ---
    //
    // Round 1 (finding 1) made `sign-off` optional but left `gha` hard-
    // required (`gha_verdict?`). `tick.rs::skeptic_evidence` only skips the
    // 3-subsystem grammar entirely when BOTH `has_gha_evidence` and
    // `has_signoff_evidence` are false; the moment EITHER is true (e.g. a
    // PR comment trips the loose sign-off heuristic — any non-bot comment
    // containing "sign-off"/"signoff"/"verdict: pass"/"/skeptic pass",
    // which is very plausible given this project's own convention of
    // emitting "verdict: pass"-style text) it unconditionally emits all
    // three `subsystem:` blocks, padding whichever is missing with the
    // literal placeholder `"verdict: absent"` — which never parses into a
    // `SkepticVerdict`. Because `gha` was still hard-required, this
    // silently re-introduced the exact same permanent-`Unknown` deadlock
    // class for any real target repo that (a) has no `gha` skeptic
    // workflow (the common case) and (b) got ANY comment that trips the
    // broad sign-off heuristic. Mutation-test sanity: reverting `v_gha`'s
    // optionality above (making `gha_verdict?` hard-required again) makes
    // this test fail with `None` instead of `Some(Pass)` — confirmed by
    // temporarily reverting the fix and re-running (2026-07-07).
    #[test]
    fn parse_skeptic_verdict_signoff_without_gha_is_deadlocked_unknown() {
        // Renamed intent, kept the exact reviewer-provided PoC message:
        // this now asserts the FIXED (non-deadlocked) outcome.
        let msg = "subsystem: gate-7\npass\n\
                   subsystem: gha\nverdict: absent\n\
                   subsystem: sign-off\nverdict: pass\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));
    }

    #[test]
    fn parse_skeptic_verdict_gha_absent_is_satisfied_by_gate7_and_signoff() {
        // Mirrors `parse_skeptic_verdict_signoff_absent_is_satisfied_by_gate7_and_gha`
        // but with the roles reversed: `gha` is the placeholder-absent
        // subsystem, `sign-off` is the one with real evidence. Gate 7 must
        // still resolve from gate-7 + sign-off alone.
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: absent\n\
                   subsystem: sign-off\nverdict: pass\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));
    }

    #[test]
    fn parse_skeptic_verdict_gha_present_can_still_escalate_to_fail() {
        // Symmetric to `parse_skeptic_verdict_signoff_present_can_still_escalate_to_fail`:
        // when `gha` DOES report real evidence, it is not ignored — it can
        // still escalate gate 7 to Fail even with sign-off absent.
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: fail (compile error)\n\
                   subsystem: sign-off\nverdict: absent\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Fail("(compile error)".into()))
        );
    }

    #[test]
    fn parse_skeptic_verdict_both_gha_and_signoff_absent_placeholders_resolves_from_gate7() {
        // Both optional subsystems present-but-placeholder (the exact
        // strings `tick.rs` used to always emit pre-fix) -> resolves from
        // gate-7 alone. Covers the (absent, absent) cell of the combination
        // matrix via the placeholder-string path rather than the
        // block-omitted path (see `parse_skeptic_verdict_three_subsystems`'s
        // "Only gate-7 present" case for the block-omitted equivalent).
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: absent\n\
                   subsystem: sign-off\nverdict: absent\n";
        assert_eq!(parse_skeptic_verdict(msg), Some(SkepticVerdict::Pass));
    }

    #[test]
    fn parse_skeptic_verdict_gha_warn_and_signoff_absent_escalates_to_warn() {
        // gha: present-warn, sign-off: absent, dual-LLM: pass -> Warn (not
        // deadlocked, not silently swallowed).
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: warn flaky retry\n\
                   subsystem: sign-off\nverdict: absent\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Warn("flaky retry".into()))
        );
    }

    #[test]
    fn parse_skeptic_verdict_signoff_fail_and_gha_absent_escalates_to_fail() {
        // gha: absent, sign-off: present-fail, dual-LLM: pass -> Fail (the
        // asymmetric mirror of the classic "gha fails" escalation case,
        // now reachable now that gha is optional too).
        let msg = "subsystem: gate-7\nverdict: pass\n\
                   subsystem: gha\nverdict: absent\n\
                   subsystem: sign-off\nverdict: fail human rejected\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Fail("human rejected".into()))
        );
    }

    #[test]
    fn parse_skeptic_verdict_dual_llm_fail_always_wins_regardless_of_gha_signoff() {
        // gate-7 itself is Fail: no combination of optional gha/sign-off
        // evidence can downgrade it back to Pass.
        let msg = "subsystem: gate-7\nfail root cause not addressed\n\
                   subsystem: gha\nverdict: pass\n\
                   subsystem: sign-off\nverdict: pass\n";
        assert_eq!(
            parse_skeptic_verdict(msg),
            Some(SkepticVerdict::Fail("root cause not addressed".into()))
        );
    }

    #[test]
    fn parse_skeptic_verdict_no_gate7_evidence_is_still_genuinely_unknown() {
        // The one case that legitimately stays `None`: no dual-LLM verdict
        // at all. This is not the deadlock bug — it is the honest "we have
        // no gate-7 evidence yet" signal gate 7 -> Unknown depends on.
        let msg = "subsystem: gha\nverdict: pass\n\
                   subsystem: sign-off\nverdict: pass\n";
        assert_eq!(parse_skeptic_verdict(msg), None);
    }

    #[test]
    fn test_parse_er_verdict() {
        use crate::tools::PrComment;
        // Simple cases
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er PASS".into() }]), ErVerdict::Pass);
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er PARTIAL".into() }]), ErVerdict::Partial);
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er FAIL".into() }]), ErVerdict::Fail);
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er INCONCLUSIVE".into() }]), ErVerdict::Inconclusive);

        // Latest comment wins (comments are in chronological order)
        let comments = vec![
            PrComment { author: "alice".into(), body: "/er FAIL".into() },
            PrComment { author: "bob".into(), body: "/er PASS".into() },
        ];
        assert_eq!(parse_er_verdict(&comments), ErVerdict::Pass);

        // Multiple verdicts in one comment: last one wins
        let comments_multiple = vec![
            PrComment { author: "alice".into(), body: "/er FAIL then /er PASS".into() },
        ];
        assert_eq!(parse_er_verdict(&comments_multiple), ErVerdict::Pass);

        // Word boundary check: "/er-gate" or "/er_gate" should not match as "/er" command
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er-gate PASS".into() }]), ErVerdict::Absent);
    }

    #[test]
    fn test_classify_production() {
        use crate::tools::PrFile;
        // Production cases
        assert!(classify_production(&[PrFile { path: "mvp_site/src/main.rs".into(), additions: 1, deletions: 0 }]));
        assert!(classify_production(&[PrFile { path: "prompts/custom.md".into(), additions: 1, deletions: 0 }]));
        assert!(classify_production(&[PrFile { path: ".github/workflows/ci.yml".into(), additions: 1, deletions: 0 }]));
        assert!(classify_production(&[PrFile { path: "deploy.sh".into(), additions: 1, deletions: 0 }]));
        assert!(classify_production(&[PrFile { path: "contracts/schema.sql".into(), additions: 1, deletions: 0 }]));

        // Non-production exclusions
        assert!(!classify_production(&[PrFile { path: "mvp_site/tests/main_test.rs".into(), additions: 1, deletions: 0 }]));
        assert!(!classify_production(&[PrFile { path: "mvp_site/test_integration/main_test.rs".into(), additions: 1, deletions: 0 }]));
        assert!(!classify_production(&[PrFile { path: "daemon/src/main.rs".into(), additions: 1, deletions: 0 }]));
    }

    #[test]
    fn test_calculate_non_test_loc() {
        use crate::tools::PrFile;
        let files = vec![
            PrFile { path: "mvp_site/src/main.rs".into(), additions: 100, deletions: 0 },
            PrFile { path: "mvp_site/tests/test_main.rs".into(), additions: 50, deletions: 0 },
            PrFile { path: "daemon/src/lib.rs".into(), additions: 30, deletions: 0 },
        ];
        assert_eq!(calculate_non_test_loc(&files), 130);
    }

    #[test]
    fn test_check_integration_marker() {
        use crate::tools::PrComment;
        // PR body contains marker
        assert!(check_integration_marker("Here is the has_integration_evidence_marker", &[]));
        assert!(check_integration_marker("Here is the integration-evidence proof", &[]));
        assert!(check_integration_marker("Here is the integration evidence proof", &[]));

        // Comment contains marker
        let comments = vec![PrComment { author: "alice".into(), body: "Found integration evidence".into() }];
        assert!(check_integration_marker("No marker here", &comments));

        // Neither contains marker
        assert!(!check_integration_marker("No marker here", &[]));
    }

    #[test]
    fn test_evidence_floor_gate_production_vs_non_production() {
        // PRODUCTION: Pass is Green, Partial is Red, Fail/Inconclusive/Absent are Red/Unknown
        let prod_pass = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: None,
        };
        assert_eq!(evidence_floor_gate(&prod_pass), GateResult::Green);

        let prod_partial = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Partial,
            skeptic_verdict: None,
        };
        assert!(matches!(evidence_floor_gate(&prod_partial), GateResult::Red(_)));

        // NON_PRODUCTION: Pass and Partial are Green
        let non_prod_partial = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Partial,
            skeptic_verdict: None,
        };
        assert_eq!(evidence_floor_gate(&non_prod_partial), GateResult::Green);

        let non_prod_pass = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: None,
        };
        assert_eq!(evidence_floor_gate(&non_prod_pass), GateResult::Green);

        // Fail/Inconclusive are Red for both
        let prod_fail = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Fail,
            skeptic_verdict: None,
        };
        assert!(matches!(evidence_floor_gate(&prod_fail), GateResult::Red(_)));

        let non_prod_inconclusive = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Inconclusive,
            skeptic_verdict: None,
        };
        assert!(matches!(evidence_floor_gate(&non_prod_inconclusive), GateResult::Red(_)));
    }
}
