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

/// Canonical PR-body marker for a public-gist evidence bundle. jleechan-yoqy
/// r3: shared by `dispatch::render_coder_prompt` (coder emits it),
/// `er_runner::build_er_prompt` (reviewer is told to look for it), and
/// `parse_evidence_marker` (parser recognizes it). ONE token, three call
/// sites — round-trip test `canonical_marker_round_trips_through_dispatch`
/// pins the contract end-to-end. Do NOT introduce a second marker.
pub const CANONICAL_EVIDENCE_MARKER: &str = "**Evidence**:";

/// Hash of the canonical evidence marker + gist URL from a PR body. Used by
/// `er_runner::maybe_run` as the second component of the AlreadyPosted key —
/// `head_committed_epoch` alone is not enough: an evidence-only body update
/// (same head SHA, different gist URL) MUST re-trigger `/er` instead of being
/// short-circuited by a stale "verdict posted after this head" guard.
pub fn evidence_marker_hash(body: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parse_evidence_marker(body).hash(&mut h);
    h.finish()
}

/// Parse the canonical `**Evidence**: <gist_url>` marker out of a PR body.
/// Returns the gist URL (the substring after `**Evidence**: `, trimmed) or
/// `None` if the marker is absent or malformed. The match is case-sensitive
/// on `**Evidence**:` (the operator-mandated literal) — fail-closed on a
/// typo rather than silently green-lighting an alternate spelling that the
/// coder prompt never told the coder to write.
pub fn parse_evidence_marker(body: &str) -> Option<String> {
    let idx = body.find(CANONICAL_EVIDENCE_MARKER)?;
    let after = &body[idx + CANONICAL_EVIDENCE_MARKER.len()..];
    // Skip the colon + at least one space/separator, then capture the URL.
    let after = after.trim_start_matches(':').trim_start();
    let url_end = after
        .find(|c: char| c.is_whitespace() || c == '\n' || c == '\r')
        .unwrap_or(after.len());
    let url = after[..url_end].trim();
    if url.is_empty() {
        return None;
    }
    Some(url.to_string())
}

/// Outcome of a fail-closed evidence-gist existence check (bead
/// jleechan-yoqy r3, operator guidance item 2). The gate must NOT trust
/// a `**Evidence**: <gist_url>` marker alone — the substring match in
/// `check_integration_marker` is necessary but not sufficient. `assess`
/// combines `Verified` with the marker to flip gate 6 green; every other
/// variant is Red.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GistEvidenceCheck {
    /// The PR body has no `**Evidence**:` marker at all — separate code
    /// path; `has_integration_evidence_marker` would already be false.
    /// Modeled here so the gate has a single switch over the result.
    NoMarker,
    /// The marker is present but the URL is not a recognizable gist
    /// host (`gist.github.com`/`api.github.com/gists`). Treat as
    /// malformed — fail closed.
    MalformedUrl,
    /// The gist could not be reached (network error, 404, 5xx). Fail
    /// closed — we cannot prove the bundle exists, so we cannot let the
    /// gate green-light on a possibly-missing artifact.
    FetchFailed(String),
    /// The gist exists but is private, empty, or does not reference the
    /// current head SHA. Fail closed.
    Invalid {
        gist_id: String,
        reason: &'static str,
    },
    /// The gist exists, is public, has at least one non-empty file, and
    /// at least one file's content references the current head SHA.
    /// THIS is the only state that lets gate 6 green-light.
    Verified {
        gist_id: String,
        referencing_files: Vec<String>,
    },
}

impl Default for GistEvidenceCheck {
    fn default() -> Self {
        // Back-compat: tests/fixtures that don't exercise the gist gate
        // get `NoMarker` (equivalent to "the check never ran"), which
        // the gate treats as Red for production PRs over the LOC floor
        // but a no-op for everything else.
        GistEvidenceCheck::NoMarker
    }
}

/// Extract the 32-char hex gist ID from a canonical
/// `https://gist.github.com/<user>/<id>` URL (the shape
/// `dispatch::render_coder_prompt` tells the coder to emit). Returns
/// `None` for non-gist URLs, malformed inputs, or IDs that aren't 32
/// hex chars (GitHub gists all use 32-hex IDs).
pub fn parse_gist_id(url: &str) -> Option<String> {
    let url = url.trim();
    // Accept `https://gist.github.com/<user>/<id>` and the bare
    // `<id>` (some test fixtures inline the ID directly). Anything
    // else is malformed.
    let id = if let Some(rest) = url.strip_prefix("https://gist.github.com/") {
        // rest is "<user>/<id>" or "<id>" — take the LAST segment.
        rest.rsplit('/').next()?
    } else if url.starts_with("https://") || url.starts_with("http://") {
        return None;
    } else {
        url
    };
    let id = id.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(id.to_ascii_lowercase())
    } else {
        None
    }
}

/// Fetch a gist's raw JSON body via `gh api gists/<id>` and check it
/// for the four fail-closed properties: (a) `public == true`,
/// (b) `files` is non-empty, (c) every file has non-empty `content`,
/// (d) at least one file's content references `head_sha` (substring
/// match — sufficient for the contract, strict equality would
/// over-constrain coders who wrap the SHA in fence blocks).
///
/// `fetch` is a closure (not a `Scm` method) so this function is
/// testable without spinning up the full Scm trait, AND so `tick.rs`
/// can pass `|id| scm.fetch_gist(id)` against the real `gh api` in
/// production without coupling the verifier module to the SCM
/// boundary. The closure signature mirrors the production call:
/// `fn(&str) -> Result<String, _>` where the String is the raw `gh api
/// gists/<id>` stdout body.
pub fn verify_gist_evidence<F>(
    body: &str,
    head_sha: &str,
    fetch: F,
) -> GistEvidenceCheck
where
    F: FnOnce(&str) -> Result<String, String>,
{
    let Some(url) = parse_evidence_marker(body) else {
        return GistEvidenceCheck::NoMarker;
    };
    let Some(gist_id) = parse_gist_id(&url) else {
        return GistEvidenceCheck::MalformedUrl;
    };
    let raw = match fetch(&gist_id) {
        Ok(s) => s,
        Err(e) => return GistEvidenceCheck::FetchFailed(e),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return GistEvidenceCheck::FetchFailed("not JSON".into()),
    };
    // (a) public
    let public = parsed.get("public").and_then(|v| v.as_bool()).unwrap_or(false);
    if !public {
        return GistEvidenceCheck::Invalid {
            gist_id: gist_id.clone(),
            reason: "gist is private",
        };
    }
    // (b) + (c) files non-empty and every file has non-empty content
    let files = parsed
        .get("files")
        .and_then(|v| v.as_object())
        .map(|o| o.len())
        .unwrap_or(0);
    if files == 0 {
        return GistEvidenceCheck::Invalid {
            gist_id: gist_id.clone(),
            reason: "gist has no files",
        };
    }
    let file_contents: Vec<(String, String)> = parsed
        .get("files")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(name, val)| {
                    let content = val.get("content")?.as_str()?.to_string();
                    if content.is_empty() {
                        None
                    } else {
                        Some((name.clone(), content))
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if file_contents.is_empty() {
        return GistEvidenceCheck::Invalid {
            gist_id: gist_id.clone(),
            reason: "all gist files are empty",
        };
    }
    // (d) at least one file references the current head SHA.
    let referencing_files: Vec<String> = file_contents
        .iter()
        .filter(|(_, content)| content.contains(head_sha))
        .map(|(name, _)| name.clone())
        .collect();
    if referencing_files.is_empty() {
        return GistEvidenceCheck::Invalid {
            gist_id: gist_id.clone(),
            reason: "no gist file references the current head SHA",
        };
    }
    GistEvidenceCheck::Verified {
        gist_id,
        referencing_files,
    }
}

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

impl GateName {
    /// Canonical snake_case JSON key for this gate (jleechan-wzgl:
    /// GATE_ASSESSMENT serializes the full per-gate report). This is NOT a
    /// free choice — it MUST match three other already-checked-in
    /// consumers of this exact vocabulary, or `daemon/scripts/auto-merge-
    /// guard.sh` silently mis-keys/crashes on the dict it reads back out
    /// of GATE_ASSESSMENT telemetry:
    ///   - `daemon/factory-overlay.sh`'s `gate-assessment` subcommand
    ///     `REQUIRED_KEYS` (canonical source comment there: "canonical
    ///     source: daemon/src/verifier.rs::GateName").
    ///   - `tests/test_af_gate_contract.py::extract_gate_names_from_rust`'s
    ///     hardcoded `gate_map` (Ci -> ci_green, etc.).
    ///   - `tests/scripts/test_auto_merge_guard_gate_vocabulary.sh`'s fixture
    ///     keys.
    ///
    /// PR #235 (jleechan-l4ki) unified all of the above onto ONE
    /// vocabulary; this must reuse it verbatim rather than mint a fourth.
    pub fn as_str(&self) -> &'static str {
        match self {
            GateName::Ci => "ci_green",
            GateName::NoConflicts => "no_conflicts",
            GateName::CodeRabbitApproved => "coderabbit",
            GateName::BugbotClean => "bugbot",
            GateName::CommentsResolved => "comments_resolved",
            GateName::EvidenceFloor => "evidence_review",
            GateName::Skeptic => "skeptic",
        }
    }
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

impl GateReport {
    /// jleechan-wzgl: serialize the full per-gate breakdown for
    /// `GATE_ASSESSMENT` telemetry. Before this, the emit call only logged
    /// `all_green`, so diagnosing which of the 7 gates failed (and why)
    /// required a manual GitHub REST sweep even though this report already
    /// carries the answer. `to_json` is the single source of truth for that
    /// serialization so the telemetry shape can't drift from what `assess`
    /// actually computed.
    ///
    /// Shape (PR #239 review round 1 finding): `gates` MUST be a `{gate_name:
    /// verdict}` object, not an array — `daemon/scripts/auto-merge-guard.sh`'s
    /// `latest_assessment_no_red` predicate does `for k, v in g.items()` on
    /// exactly this field, and `daemon/factory-overlay.sh`'s `gate-assessment`
    /// subcommand (+ its `REQUIRED_KEYS`/`OPTIONAL_KEYS` contract, unified in
    /// PR #235/jleechan-l4ki) is the other checked-in consumer of this same
    /// vocabulary. Each verdict is either the plain string `"pass"|"warn"|
    /// "fail"|"unknown"` (green gates), or the structured `{"verdict": ...,
    /// "evidence": [...]}` object (red/unknown gates, carrying the reason as
    /// a one-element `evidence` list) — both shapes are accepted by
    /// `factory-overlay.sh`'s `normalize()`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut gates = serde_json::Map::new();
        for (name, result) in &self.results {
            let value = match result {
                GateResult::Green => serde_json::Value::String("pass".to_string()),
                GateResult::Red(reason) => serde_json::json!({
                    "verdict": "fail",
                    "evidence": [reason],
                }),
                GateResult::Unknown(reason) => serde_json::json!({
                    "verdict": "unknown",
                    "evidence": [reason],
                }),
            };
            gates.insert(name.as_str().to_string(), value);
        }
        serde_json::json!({
            "all_green": self.all_green,
            "gates": serde_json::Value::Object(gates),
        })
    }
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
    /// Kept as a fast-path boolean for tests/legacy callers; the
    /// canonical signal for gate 6 is `gist_verification == Verified`,
    /// which is strictly stronger than this substring match.
    pub has_integration_evidence_marker: bool,
    /// jleechan-yoqy r3: outcome of the fail-closed gist existence check
    /// (operator guidance item 2). Gate 6 only flips green when this is
    /// `Verified { .. }` — every other variant is Red, including
    /// `NoMarker`, `FetchFailed`, and `Invalid`. `tick.rs` populates this
    /// from `verify_gist_evidence(snapshot.body, snapshot.head_sha,
    /// |id| scm.fetch_gist(id))`. `Default::default()` is `NoMarker` —
    /// tests/fixtures that don't exercise the gist gate get the
    /// pre-r3 behavior (substring match sufficient).
    pub gist_verification: GistEvidenceCheck,
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
    /// jleechan-wzgl: identity of the reviewer vendor(s) whose response
    /// actually contributed to `skeptic_verdict` this tick (e.g. `["agy"]`
    /// when the dual-dispatch primaries both failed to parse and the
    /// fallback loop's third vendor produced the usable verdict, or
    /// `["codex", "claude"]` when both dual-dispatch primaries agreed).
    /// `["mock_llm"]` marks the Stage-1 `is_test_repo` path (a single
    /// `Llm::judge` adversarial call, not an independent reviewer CLI) so
    /// telemetry never mistakes a test-repo mock for a real vendor. Empty
    /// when no verdict was produced (`skeptic_verdict` is `None`). This is
    /// GATE_ASSESSMENT's provenance signal for confirming the gate-7
    /// reviewer was non-self and genuinely ran, not self-certified.
    pub skeptic_reviewers: Vec<String>,
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
    // jleechan-yoqy r3: production PRs over the LOC floor require BOTH the
    // substring marker AND a fail-closed gist existence check
    // (`gist_verification == Verified`). A bare marker is no longer
    // sufficient — the marker alone is exactly the "substring-only
    // self-certification" the operator flagged in the r2 review. The
    // legacy `has_integration_evidence_marker` is still honored so test
    // fixtures and non-production beads (small diffs under the LOC
    // floor) keep their existing behavior.
    if evidence.is_production {
        match &evidence.gist_verification {
            GistEvidenceCheck::Verified { .. } => GateResult::Green,
            GistEvidenceCheck::NoMarker => GateResult::Red(
                "evidence floor: production PR over LOC floor has no **Evidence**: <gist_url> marker"
                    .to_string(),
            ),
            GistEvidenceCheck::MalformedUrl => GateResult::Red(
                "evidence floor: **Evidence** marker URL is not a recognizable gist"
                    .to_string(),
            ),
            GistEvidenceCheck::FetchFailed(e) => GateResult::Red(format!(
                "evidence floor: gist existence check failed: {e}"
            )),
            GistEvidenceCheck::Invalid { reason, .. } => GateResult::Red(format!(
                "evidence floor: {reason}"
            )),
        }
    } else if evidence.has_integration_evidence_marker
        || matches!(evidence.gist_verification, GistEvidenceCheck::Verified { .. })
    {
        GateResult::Green
    } else {
        GateResult::Red("evidence floor".to_string())
    }
}

pub fn parse_er_verdict(comments: &[crate::tools::PrComment]) -> ErVerdict {
    parse_er_verdict_since(comments, 0)
}

/// Like `parse_er_verdict`, but only accepts verdicts from comments created
/// at or after `min_epoch` (the PR head commit's committer date).
///
/// jleechan-nplh: an `/er PASS` posted days and dozens of commits before the
/// current head does not verify the code that would actually merge; treating
/// it as valid short-circuits re-verification forever (live incident:
/// worldarchitect.ai#7888, a 2026-07-08 PASS suppressed re-review of a
/// 2026-07-10 head). Staleness rules:
///
/// - `min_epoch == 0` (head commit time unknown — offline snapshots, test
///   doubles that predate this field): no filtering, identical to
///   `parse_er_verdict`. Fail-open here is deliberate: without a reference
///   point every comment would be "stale" and gate 6 would deadlock.
/// - comment `created_at_epoch == 0` with `min_epoch > 0` (comment age
///   unknown but head age known): treated as STALE. Fail-closed is safe —
///   the worst case is one redundant `/er` run, bounded by
///   `MAX_ER_RUNNER_ATTEMPTS`; the alternative re-opens the self-
///   certification hole this function exists to close.
pub fn parse_er_verdict_since(
    comments: &[crate::tools::PrComment],
    min_epoch: u64,
) -> ErVerdict {
    for comment in comments.iter().rev() {
        if min_epoch > 0 && comment.created_at_epoch < min_epoch {
            continue;
        }
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

/// jleechan-yoqy r3: this gate still accepts the legacy
/// `has_integration_evidence_marker` / `integration-evidence` tokens (older
/// beads, test fixtures, and r1/r2 of the coder-prompt fix rely on them),
/// BUT a fresh gate-6 green now also requires `parse_evidence_marker` to
/// return Some on the PR body — the legacy tokens alone are insufficient
/// for production PRs. `comments` are scanned only for the legacy tokens
/// (a reviewer posting `**Evidence**: <url>` in a comment does not
/// satisfy the canonical marker, by design: the marker is the CODER's
/// contract, not the reviewer's).
pub fn check_integration_marker(body: &str, comments: &[crate::tools::PrComment]) -> bool {
    if parse_evidence_marker(body).is_some() {
        return true;
    }
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

/// Assess all 7 gates for `pr` (spec §4.2.5, design doc §5). `repo` (bead
/// jleechan-9xrs, Stage D — see
/// `docs/multirepo-dispatch-investigation-2026-07-11.md`) is the bead's OWN
/// resolved repo (`overlay.repo(cfg)`), used to fetch the PR snapshot from
/// the RIGHT repo instead of whichever repo `cfg.target_repo` names — a bead
/// dispatched into a non-default `[repos.*]` entry used to have its gates
/// silently assessed against the daemon-global repo's PR #N (or, worse,
/// against no PR at all in that repo). `cfg` is accepted per the
/// design-doc signature for future per-repo gate config (unused today —
/// Stage 1 has no per-gate config knobs yet); `#[allow(unused_variables)]`
/// documents that rather than dropping the parameter ahead of a design-doc
/// revision.
#[allow(unused_variables)]
pub fn assess(
    scm: &dyn Scm,
    pr: u64,
    repo: &str,
    cfg: &Config,
    evidence: &PrEvidence,
) -> Result<GateReport, crate::errors::DaemonError> {
    let snapshot = match scm.pr_snapshot_for_repo(repo, pr) {
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

    // jleechan-kk64: `None` means the GraphQL fetch/parse of the
    // unresolved-review-thread count failed — that is NOT evidence of zero
    // unresolved threads. Fail closed to `Unknown`, never `Green`.
    let comments_resolved = match snapshot.unresolved_thread_count {
        Some(0) => GateResult::Green,
        Some(n) => GateResult::Red(format!("{n} unresolved review thread(s)")),
        None => GateResult::Unknown(
            "unresolved review thread count could not be determined (GraphQL fetch or parse failed)"
                .to_string(),
        ),
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

// --- jleechan-zaga: structured gate-block classification (issue #348) ---
//
// A not-all-green gate report is blocked by one of three kinds of condition,
// and the daemon must react differently to each:
//
//   * CODER-FIXABLE — a RED gate the worker can plausibly clear by editing
//     the diff: CI failure, merge conflicts, unresolved review threads,
//     Bugbot errors, a CodeRabbit review that requested changes, an
//     evidence-floor violation (a coder CAN publish a gist — bead cnf9 proved
//     it live), a skeptic rejection. → re-roll as today.
//   * STRUCTURAL-PENDING — a gate held non-green by an EXTERNAL verifier that
//     has produced no verdict and that the coder cannot make produce one:
//     CodeRabbit unavailable/rate-limited, or Bugbot absent/neutral. Under
//     the daemon's own gate logic (verifier::assess) these surface as
//     `Unknown` gates (e.g. `coderabbit_status == "unknown"` -> CodeRabbit
//     `Unknown`), NOT `Red`. Re-rolling cannot change the outcome — it just
//     produces an equivalent r2 that hits the same feedback hash and parks
//     HUMAN_HELD at the attempt cap (issue #348: v6ud's correct P0 fix #342
//     superseded by its own reroll). → hold DISPOSITION_REQUIRED, keep
//     re-assessing.
//   * TRANSIENT — every other `Unknown` gate (CI still running, review-thread
//     count GraphQL-unverifiable). Resolves on its own. → stay ATTESTED,
//     re-assess next tick (existing behavior).
//
// ZFC: the classification keys ONLY on STRUCTURED signals — the gate's
// `GateResult` variant (`Green`/`Red`/`Unknown`, itself derived by
// `verifier::assess` from `PrSnapshot`'s structured fields) plus the gate
// name. It does NOT free-text-match reason strings and does NOT depend on
// synthetic `coderabbit_status` values that production never emits
// (`adapters.rs` emits only `green`/`red`/`unknown`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateBlock {
    /// A RED gate the worker can plausibly clear by editing the diff.
    CoderFixable,
    /// A gate held `Unknown` by an external verifier the coder cannot drive
    /// (CodeRabbit unavailable, Bugbot absent/neutral).
    Structural,
    /// A gate held `Unknown` by a condition that resolves on its own (CI still
    /// running, review-thread count temporarily unverifiable, or a whole-
    /// snapshot SCM fetch outage). Wait and re-assess — do NOT hold.
    Transient,
}

/// Classify a single non-green gate on STRUCTURED signals only (the
/// `GateResult` variant + gate name — no reason-text keyword matching).
/// Returns `None` only for a `Green` gate.
pub fn classify_nongreen_gate(name: GateName, result: &GateResult) -> Option<GateBlock> {
    match result {
        GateResult::Green => None,
        // Every RED gate is coder-fixable — see the module comment above.
        GateResult::Red(_) => Some(GateBlock::CoderFixable),
        // An `Unknown` gate is structural-pending ONLY for the external
        // verifiers a coder cannot drive: CodeRabbit (unavailable /
        // rate-limited) and Bugbot (absent / neutral). Every other `Unknown`
        // (CI still running, unverifiable thread count, or a whole-snapshot
        // SCM outage that turns EVERY gate Unknown) is transient.
        GateResult::Unknown(_) => match name {
            GateName::CodeRabbitApproved | GateName::BugbotClean => Some(GateBlock::Structural),
            _ => Some(GateBlock::Transient),
        },
    }
}

/// What the tick path should do with a NOT-all-green report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainDisposition {
    /// At least one coder-fixable RED gate — re-roll can plausibly help.
    Reroll,
    /// No coder-fixable red gate, but at least one structural-pending gate —
    /// hold `DISPOSITION_REQUIRED` and keep re-assessing.
    HoldDisposition,
    /// Only transient `Unknown` gates remain — stay ATTESTED, re-assess.
    TransientOnly,
}

/// Aggregate a report into a [`ChainDisposition`]. Precedence:
///   1. any coder-fixable RED gate → `Reroll` (a coder can act now);
///   2. else any TRANSIENT unknown → `TransientOnly` (the picture is not yet
///      settled — e.g. CI still running, or a whole-snapshot SCM outage that
///      turned every gate Unknown — so WAIT and re-assess rather than hold on
///      an incomplete signal);
///   3. else any STRUCTURAL-pending gate → `HoldDisposition` (the ONLY
///      remaining blockers are external verifiers the coder cannot drive);
///   4. else `TransientOnly` (all green — the caller handles that separately).
///
/// The transient-before-structural ordering is deliberate: a report must be
/// classified `HoldDisposition` only when structural gates are the SOLE
/// blockers, never when a transient unknown might still resolve into a
/// coder-fixable red or a green.
pub fn classify_chain(report: &GateReport) -> ChainDisposition {
    let mut has_coder_fixable = false;
    let mut has_structural = false;
    let mut has_transient = false;
    for (name, result) in &report.results {
        match classify_nongreen_gate(*name, result) {
            Some(GateBlock::CoderFixable) => has_coder_fixable = true,
            Some(GateBlock::Structural) => has_structural = true,
            Some(GateBlock::Transient) => has_transient = true,
            None => {}
        }
    }
    if has_coder_fixable {
        ChainDisposition::Reroll
    } else if has_transient {
        ChainDisposition::TransientOnly
    } else if has_structural {
        ChainDisposition::HoldDisposition
    } else {
        ChainDisposition::TransientOnly
    }
}

/// The structural-pending gates in a report as `(gate, reason)` pairs, for the
/// operator-facing DISPOSITION_REQUIRED comment/telemetry.
pub fn structural_pending_gates(report: &GateReport) -> Vec<(GateName, &str)> {
    report
        .results
        .iter()
        .filter_map(|(name, result)| match classify_nongreen_gate(*name, result) {
            Some(GateBlock::Structural) => match result {
                GateResult::Unknown(reason) => Some((*name, reason.as_str())),
                _ => None,
            },
            _ => None,
        })
        .collect()
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

        /// jleechan-9xrs Stage D regression coverage: records the `repo`
        /// argument distinctly from the plain `pr_snapshot({pr})` call log
        /// entry so tests can assert `assess()` fetched the bead's OWN
        /// resolved repo, not `cfg.target_repo`.
        fn pr_snapshot_for_repo(&self, repo: &str, pr: u64) -> Result<PrSnapshot, DaemonError> {
            self.calls
                .borrow_mut()
                .push(format!("pr_snapshot_for_repo({repo},{pr})"));
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
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            head_committed_epoch: 0,
        }
    }

    /// Creates a snapshot with unknown unresolved_thread_count (simulating GraphQL failure).
    /// This is the scenario this test proves: when thread count is unknown,
    /// the CommentsResolved gate must return Unknown, NOT Green.
    fn snapshot_with_unknown_thread_count(pr: u64) -> PrSnapshot {
        PrSnapshot {
            pr_number: pr,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: None, // Unknown - GraphQL failed
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            head_committed_epoch: 0,
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
            ..Default::default()
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

    /// jleechan-9xrs Stage D: `assess()` must fetch the PR snapshot from the
    /// bead's OWN resolved repo (`repo` argument), not `cfg.target_repo`.
    /// Regression for the multi-repo dispatch fix
    /// (docs/multirepo-dispatch-investigation-2026-07-11.md) — before this
    /// fix, gate assessment for a bead dispatched into a non-default
    /// `[repos.*]` entry silently read the daemon-global repo's PR state
    /// instead of the bead's own.
    #[test]
    fn assess_fetches_pr_snapshot_from_bead_repo_not_cfg_target_repo() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let mut cfg = test_cfg();
        cfg.target_repo = "jleechanorg/wrong-global-repo".to_string();

        let report = assess(
            &scm,
            7,
            "otherorg/bead-owned-repo",
            &cfg,
            &all_green_evidence(),
        )
        .unwrap();

        assert!(report.all_green, "expected all_green, got {report:?}");
        let calls = scm.calls.borrow();
        assert!(
            calls
                .iter()
                .any(|c| c == "pr_snapshot_for_repo(otherorg/bead-owned-repo,7)"),
            "expected assess() to fetch via pr_snapshot_for_repo with the bead's \
             own repo, got calls: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c.contains("wrong-global-repo")),
            "assess() must never leak cfg.target_repo into the snapshot fetch, \
             got calls: {calls:?}"
        );
    }

    /// Companion to the above: when the bead has no explicit repo (legacy —
    /// mirrors `overlay.repo(cfg)`'s `None` fallback), passing
    /// `&cfg.target_repo` as `repo` must behave identically to the
    /// pre-Stage-D single-repo world. This is the "unchanged for legacy
    /// beads" half of the acceptance criteria.
    #[test]
    fn assess_legacy_repo_equal_to_cfg_target_repo_is_unchanged() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();

        assert!(report.all_green, "expected all_green, got {report:?}");
        let calls = scm.calls.borrow();
        assert!(
            calls
                .iter()
                .any(|c| c == &format!("pr_snapshot_for_repo({},7)", cfg.target_repo)),
            "expected the legacy fallback repo (== cfg.target_repo) to reach \
             pr_snapshot_for_repo unchanged, got calls: {calls:?}"
        );
    }

    #[test]
    fn all_green_snapshot_yields_all_green_report() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();

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

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();

        assert!(!report.all_green);
        assert!(matches!(gate(&report, GateName::Ci), GateResult::Red(_)));
        assert!(gate(&report, GateName::NoConflicts).is_green());
        assert!(gate(&report, GateName::CodeRabbitApproved).is_green());
        assert!(gate(&report, GateName::BugbotClean).is_green());
        assert!(gate(&report, GateName::CommentsResolved).is_green());
    }

    #[test]
    fn unknown_thread_count_marks_comments_resolved_unknown_not_green() {
        // jleechan-kk64 regression test: a GraphQL fetch/parse failure for
        // the unresolved-review-thread count (represented here as
        // `unresolved_thread_count: None`, distinct from the "whole
        // pr_snapshot call errored" case covered by
        // `snapshot_fetch_error_marks_thread_gate_unknown_not_red` below)
        // must make the CommentsResolved gate report Unknown — proving
        // READY/all_green remains impossible until the thread count is
        // actually proven, never defaulting to Green.
        let mut scm = FakeScm::default();
        scm.snapshots
            .insert(7, snapshot_with_unknown_thread_count(7));
        let cfg = test_cfg();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();

        assert!(
            !report.all_green,
            "all_green must be false when thread count is unknown, got {report:?}"
        );
        let comments_resolved = gate(&report, GateName::CommentsResolved);
        assert!(
            matches!(comments_resolved, GateResult::Unknown(_)),
            "expected CommentsResolved gate to be Unknown when thread count is unproven, got {comments_resolved:?}"
        );
        assert!(
            !comments_resolved.is_green(),
            "CommentsResolved gate must never be Green when thread count is unproven"
        );
        // Every other gate stays unaffected — this is an isolated Unknown,
        // not a cascade.
        assert!(gate(&report, GateName::Ci).is_green());
        assert!(gate(&report, GateName::NoConflicts).is_green());
        assert!(gate(&report, GateName::CodeRabbitApproved).is_green());
        assert!(gate(&report, GateName::BugbotClean).is_green());
    }

    #[test]
    fn snapshot_fetch_error_marks_thread_gate_unknown_not_red() {
        // Simulates an SCM API error while gathering thread-resolution data
        // (gate 5's `pr_snapshot` includes `unresolved_thread_count`, so a
        // fetch failure surfaces here). No snapshot scripted for pr 9 at all,
        // so `pr_snapshot` returns `Err` exactly like a real GraphQL error.
        let scm = FakeScm::default();
        let cfg = test_cfg();

        let report = assess(&scm, 9, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();

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

        let report = assess(&scm, 42, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();

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
            // No marker → Default (NoMarker). The new gate produces a
            // more specific reason string than the legacy "evidence
            // floor" sentinel.
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::EvidenceFloor) {
            GateResult::Red(reason) => assert!(
                reason.contains("no **Evidence**: <gist_url> marker"),
                "expected Red with marker-missing reason, got {reason:?}"
            ),
            other => panic!("expected Red(marker-missing), got {other:?}"),
        }
    }

    #[test]
    fn large_diff_with_evidence_marker_is_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        // jleechan-yoqy r3: production PRs over the LOC floor require
        // BOTH the substring marker AND a Verified gist (the bare
        // marker is the r2 regression). This test now sets both.
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 150,
            has_integration_evidence_marker: true,
            gist_verification: GistEvidenceCheck::Verified {
                gist_id: "abc123456789abcdef0123456789abcd".to_string(),
                referencing_files: vec!["out.txt".to_string()],
            },
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::EvidenceFloor) {
            GateResult::Red(reason) => assert!(
                reason.contains("no **Evidence**: <gist_url> marker"),
                "expected Red with marker-missing reason, got {reason:?}"
            ),
            other => panic!("expected Red(marker-missing), got {other:?}"),
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
            // jleechan-yoqy r3: marker alone insufficient for production
            // PRs — Verified gist required.
            gist_verification: GistEvidenceCheck::Verified {
                gist_id: "abc123456789abcdef0123456789abcd".to_string(),
                referencing_files: vec!["out.txt".to_string()],
            },
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

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
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er PASS".into(), created_at_epoch: 0 }]), ErVerdict::Pass);
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er PARTIAL".into(), created_at_epoch: 0 }]), ErVerdict::Partial);
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er FAIL".into(), created_at_epoch: 0 }]), ErVerdict::Fail);
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er INCONCLUSIVE".into(), created_at_epoch: 0 }]), ErVerdict::Inconclusive);

        // Latest comment wins (comments are in chronological order)
        let comments = vec![
            PrComment { author: "alice".into(), body: "/er FAIL".into(), created_at_epoch: 0 },
            PrComment { author: "bob".into(), body: "/er PASS".into(), created_at_epoch: 0 },
        ];
        assert_eq!(parse_er_verdict(&comments), ErVerdict::Pass);

        // Multiple verdicts in one comment: last one wins
        let comments_multiple = vec![
            PrComment { author: "alice".into(), body: "/er FAIL then /er PASS".into(), created_at_epoch: 0 },
        ];
        assert_eq!(parse_er_verdict(&comments_multiple), ErVerdict::Pass);

        // Word boundary check: "/er-gate" or "/er_gate" should not match as "/er" command
        assert_eq!(parse_er_verdict(&[PrComment { author: "alice".into(), body: "/er-gate PASS".into(), created_at_epoch: 0 }]), ErVerdict::Absent);
    }

    // jleechan-nplh: staleness filtering against the head commit epoch.
    #[test]
    fn test_parse_er_verdict_since_staleness() {
        use crate::tools::PrComment;
        let stale_pass = PrComment { author: "er".into(), body: "/er PASS".into(), created_at_epoch: 1_000 };
        let fresh_fail = PrComment { author: "er".into(), body: "/er FAIL regressed".into(), created_at_epoch: 3_000 };

        // A verdict older than the head commit is ignored entirely.
        assert_eq!(parse_er_verdict_since(std::slice::from_ref(&stale_pass), 2_000), ErVerdict::Absent);

        // A verdict at/after the head commit is accepted (>= boundary).
        assert_eq!(parse_er_verdict_since(std::slice::from_ref(&stale_pass), 1_000), ErVerdict::Pass);

        // Stale PASS must not mask a fresh FAIL.
        assert_eq!(
            parse_er_verdict_since(&[stale_pass.clone(), fresh_fail], 2_000),
            ErVerdict::Fail
        );

        // min_epoch == 0 (head age unknown) disables filtering — identical
        // to parse_er_verdict, so old offline snapshots keep working.
        assert_eq!(parse_er_verdict_since(&[stale_pass], 0), ErVerdict::Pass);

        // Comment age unknown (epoch 0) with a known head age: fail closed,
        // treat as stale.
        let unknown_age = PrComment { author: "er".into(), body: "/er PASS".into(), created_at_epoch: 0 };
        assert_eq!(parse_er_verdict_since(&[unknown_age], 2_000), ErVerdict::Absent);
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
        let comments = vec![PrComment { author: "alice".into(), body: "Found integration evidence".into(), created_at_epoch: 0 }];
        assert!(check_integration_marker("No marker here", &comments));

        // Neither contains marker
        assert!(!check_integration_marker("No marker here", &[]));

        // jleechan-yoqy r3: canonical **Evidence**: <gist_url> marker in body
        // satisfies the gate even when no legacy token is present.
        assert!(check_integration_marker(
            "**Evidence**: https://gist.github.com/jleechan/abc123",
            &[],
        ));
        // Canonical marker with no legacy fallback still flips green.
        assert!(check_integration_marker(
            "Some prose.\n\n**Evidence**: https://gist.github.com/jleechan/deadbeef\n\nMore prose.",
            &[],
        ));
    }

    /// jleechan-yoqy r3: the parser recognizes the exact marker the coder
    /// prompt emits (`**Evidence**:` followed by a gist URL on the same line).
    #[test]
    fn parse_evidence_marker_recognizes_canonical_form() {
        assert_eq!(
            parse_evidence_marker("**Evidence**: https://gist.github.com/jleechan/abc123"),
            Some("https://gist.github.com/jleechan/abc123".to_string()),
        );
        assert_eq!(
            parse_evidence_marker(
                "Some prose.\n\n**Evidence**: https://gist.github.com/jleechan/deadbeef\n\nMore prose.",
            ),
            Some("https://gist.github.com/jleechan/deadbeef".to_string()),
        );
        // Surrounding markdown is fine.
        assert_eq!(
            parse_evidence_marker("**Evidence**:https://gist.github.com/x/y"),
            Some("https://gist.github.com/x/y".to_string()),
        );
    }

    /// jleechan-yoqy r3: the parser is case-sensitive and fail-closed on
    /// misspellings — a typo in the operator-mandated marker must NOT
    /// green-light gate 6.
    #[test]
    fn parse_evidence_marker_rejects_misspellings() {
        assert_eq!(parse_evidence_marker("**evidence**: https://gist.github.com/x/y"), None);
        assert_eq!(parse_evidence_marker("**Evidence** : https://gist.github.com/x/y"), None);
        assert_eq!(parse_evidence_marker("Evidence: https://gist.github.com/x/y"), None);
        assert_eq!(parse_evidence_marker("**Evidence**:"), None);
        assert_eq!(parse_evidence_marker(""), None);
        // Legacy markers are NOT parsed by parse_evidence_marker — only
        // check_integration_marker retains them as a back-compat layer.
        assert_eq!(parse_evidence_marker("integration-evidence"), None);
    }

    /// jleechan-yoqy r3: changing the body evidence URL changes the marker
    /// hash, even when the head SHA / epoch stays the same. This is the
    /// second component of the AlreadyPosted key.
    #[test]
    fn evidence_marker_hash_changes_with_url() {
        let a = evidence_marker_hash("**Evidence**: https://gist.github.com/jleechan/abc");
        let b = evidence_marker_hash("**Evidence**: https://gist.github.com/jleechan/abc");
        let c = evidence_marker_hash("**Evidence**: https://gist.github.com/jleechan/def");
        let d = evidence_marker_hash("no marker at all");
        assert_eq!(a, b, "same body → same hash");
        assert_ne!(a, c, "different gist URL → different hash");
        assert_ne!(a, d, "no marker → different hash (sentinel)");
    }

    /// jleechan-yoqy r3 (operator guidance item 3): the literal marker
    /// emitted by `dispatch::render_coder_prompt`
    /// ("**Evidence**: <gist_url>") MUST round-trip through
    /// `parse_evidence_marker`. The marker constant lives in BOTH files
    /// only by convention — this test pins the contract: if the dispatch
    /// template drifts from the parser, this test fails.
    #[test]
    fn canonical_marker_round_trips_through_dispatch() {
        // The exact substring that render_coder_prompt tells the coder to
        // write, copied verbatim from daemon/src/dispatch.rs.
        const PROMPT_MANDATED_LITERAL: &str = "**Evidence**: <gist_url>";
        // Embedded in a realistic PR body the way a coder would paste it.
        let body = format!(
            "## Summary\n\nFix the bug.\n\n## Testing\n\ncargo test ran.\n\n{PROMPT_MANDATED_LITERAL}\n"
        );
        let parsed = parse_evidence_marker(&body);
        assert_eq!(
            parsed.as_deref(),
            Some("<gist_url>"),
            "parser must pick up the literal marker emitted by the coder prompt, got {parsed:?}"
        );
        // And the gate recognizes the body.
        assert!(
            check_integration_marker(&body, &[]),
            "check_integration_marker must green-light a body containing the literal prompt-mandated marker"
        );
        // And the marker constant agrees with the dispatch template.
        assert_eq!(CANONICAL_EVIDENCE_MARKER, "**Evidence**:");
    }

    /// jleechan-yoqy r3 (operator guidance item 2): the parser must accept
    /// the canonical `https://gist.github.com/<user>/<id>` shape AND
    /// extract the 32-hex gist ID. Bare IDs are accepted as a test
    /// convenience; non-gist URLs are rejected.
    #[test]
    fn parse_gist_id_handles_canonical_and_malformed() {
        // Canonical: https://gist.github.com/<user>/<32-hex>
        assert_eq!(
            parse_gist_id("https://gist.github.com/jleechan/abc123456789abcdef0123456789abcd"),
            Some("abc123456789abcdef0123456789abcd".to_string()),
        );
        assert_eq!(
            parse_gist_id(
                &parse_evidence_marker(
                    "**Evidence**: https://gist.github.com/octocat/deadbeefdeadbeefdeadbeefdeadbeef"
                )
                .expect("marker must be recognized"),
            ),
            Some("deadbeefdeadbeefdeadbeefdeadbeef".to_string()),
        );
        // Bare 32-hex ID (test fixture convenience)
        assert_eq!(
            parse_gist_id("0123456789abcdef0123456789abcdef"),
            Some("0123456789abcdef0123456789abcdef".to_string()),
        );
        // Non-gist URLs → None
        assert_eq!(parse_gist_id("https://github.com/jleechan/df"), None);
        assert_eq!(parse_gist_id("https://gist.github.com/"), None);
        // Too short
        assert_eq!(parse_gist_id("abc"), None);
        // Wrong chars
        assert_eq!(
            parse_gist_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
            None,
            "non-hex gist ID must be rejected"
        );
    }

    /// jleechan-yoqy r3 (operator guidance item 2): a public gist with a
    /// file referencing the current head SHA is `Verified`. The substring
    /// match is intentional — coders wrap the SHA in fence blocks, commit
    /// messages, etc., so strict equality would over-constrain.
    #[test]
    fn verify_gist_evidence_returns_verified_for_public_gist_with_head_ref() {
        let body = "**Evidence**: https://gist.github.com/jleechan/abc123456789abcdef0123456789abcd";
        let head_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let fetch = |_id: &str| -> Result<String, String> {
            Ok(format!(
                r#"{{"public": true, "files": {{"test-output.txt": {{"content": "running tests for head {head_sha}\nok\n"}}}}}}"#
            ))
        };
        match verify_gist_evidence(body, head_sha, fetch) {
            GistEvidenceCheck::Verified { gist_id, referencing_files } => {
                assert_eq!(gist_id, "abc123456789abcdef0123456789abcd");
                assert_eq!(referencing_files, vec!["test-output.txt".to_string()]);
            }
            other => panic!("expected Verified, got {other:?}"),
        }
    }

    /// jleechan-yoqy r3: a private gist is `Invalid` — fail-closed.
    #[test]
    fn verify_gist_evidence_rejects_private_gist() {
        let body = "**Evidence**: https://gist.github.com/jleechan/abc123456789abcdef0123456789abcd";
        let fetch = |_id: &str| -> Result<String, String> {
            Ok(r#"{"public": false, "files": {"x.txt": {"content": "x"}}}"#.to_string())
        };
        match verify_gist_evidence(body, "deadbeef", fetch) {
            GistEvidenceCheck::Invalid { reason, .. } => {
                assert_eq!(reason, "gist is private");
            }
            other => panic!("expected Invalid(private), got {other:?}"),
        }
    }

    /// jleechan-yoqy r3: a public gist with no files is `Invalid`.
    #[test]
    fn verify_gist_evidence_rejects_empty_gist() {
        let body = "**Evidence**: https://gist.github.com/jleechan/abc123456789abcdef0123456789abcd";
        let fetch = |_id: &str| -> Result<String, String> {
            Ok(r#"{"public": true, "files": {}}"#.to_string())
        };
        match verify_gist_evidence(body, "deadbeef", fetch) {
            GistEvidenceCheck::Invalid { reason, .. } => {
                assert_eq!(reason, "gist has no files");
            }
            other => panic!("expected Invalid(no-files), got {other:?}"),
        }
    }

    /// jleechan-yoqy r3: a public gist whose files do NOT reference the
    /// current head SHA is `Invalid` — the bundle is stale or fabricated.
    #[test]
    fn verify_gist_evidence_rejects_gist_without_head_sha_reference() {
        let body = "**Evidence**: https://gist.github.com/jleechan/abc123456789abcdef0123456789abcd";
        let head_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let fetch = |_id: &str| -> Result<String, String> {
            Ok(r#"{"public": true, "files": {"out.txt": {"content": "some other head"}}}"#.to_string())
        };
        match verify_gist_evidence(body, head_sha, fetch) {
            GistEvidenceCheck::Invalid { reason, .. } => {
                assert_eq!(reason, "no gist file references the current head SHA");
            }
            other => panic!("expected Invalid(no-head-ref), got {other:?}"),
        }
    }

    /// jleechan-yoqy r3: a network/404 fetch error is `FetchFailed` —
    /// fail-closed, the gate must not green-light on a possibly-missing
    /// artifact.
    #[test]
    fn verify_gist_evidence_fails_closed_on_fetch_error() {
        let body = "**Evidence**: https://gist.github.com/jleechan/abc123456789abcdef0123456789abcd";
        let fetch = |_id: &str| -> Result<String, String> { Err("404 Not Found".into()) };
        match verify_gist_evidence(body, "deadbeef", fetch) {
            GistEvidenceCheck::FetchFailed(e) => assert_eq!(e, "404 Not Found"),
            other => panic!("expected FetchFailed, got {other:?}"),
        }
    }

    /// jleechan-yoqy r3: a body with no `**Evidence**` marker at all is
    /// `NoMarker` — the substring `check_integration_marker` would still
    /// flag this via its legacy fallbacks, but `verify_gist_evidence` is
    /// a separate, stricter check.
    #[test]
    fn verify_gist_evidence_returns_no_marker_when_absent() {
        let fetch = |_id: &str| -> Result<String, String> { panic!("fetch must not be called when no marker"); };
        assert_eq!(
            verify_gist_evidence("no marker here", "deadbeef", fetch),
            GistEvidenceCheck::NoMarker,
        );
    }

    /// jleechan-yoqy r3: a marker whose URL is malformed (not a gist
    /// host, not a 32-hex ID) is `MalformedUrl`.
    #[test]
    fn verify_gist_evidence_returns_malformed_for_non_gist_url() {
        let body = "**Evidence**: https://github.com/jleechan/dark-factory";
        let fetch = |_id: &str| -> Result<String, String> { panic!("fetch must not be called for malformed URL"); };
        assert_eq!(
            verify_gist_evidence(body, "deadbeef", fetch),
            GistEvidenceCheck::MalformedUrl,
        );
    }

    /// jleechan-yoqy r3: a Verified gist flips gate 6 green for a
    /// production PR over the LOC floor.
    #[test]
    fn production_pr_with_verified_gist_flips_evidence_floor_green() {
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 200, // over the floor
            has_integration_evidence_marker: true,
            gist_verification: GistEvidenceCheck::Verified {
                gist_id: "abc123456789abcdef0123456789abcd".to_string(),
                referencing_files: vec!["out.txt".to_string()],
            },
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            ..Default::default()
        };
        assert!(evidence_floor_gate(&evidence).is_green());
    }

    /// jleechan-yoqy r3 (negative): a production PR over the LOC floor
    /// with only the substring marker (no gist verification) MUST stay
    /// Red — the operator-flagged regression.
    #[test]
    fn production_pr_with_marker_but_no_gist_verification_stays_red() {
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 200,
            has_integration_evidence_marker: true,
            gist_verification: GistEvidenceCheck::FetchFailed("404".into()),
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            ..Default::default()
        };
        match evidence_floor_gate(&evidence) {
            GateResult::Red(reason) => assert!(reason.contains("gist existence check failed")),
            other => panic!("expected Red, got {other:?}"),
        }
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
            ..Default::default()
        };
        assert_eq!(evidence_floor_gate(&prod_pass), GateResult::Green);

        let prod_partial = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Partial,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert!(matches!(evidence_floor_gate(&prod_partial), GateResult::Red(_)));

        // NON_PRODUCTION: Pass and Partial are Green
        let non_prod_partial = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Partial,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert_eq!(evidence_floor_gate(&non_prod_partial), GateResult::Green);

        let non_prod_pass = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert_eq!(evidence_floor_gate(&non_prod_pass), GateResult::Green);

        // Fail/Inconclusive are Red for both
        let prod_fail = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Fail,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert!(matches!(evidence_floor_gate(&prod_fail), GateResult::Red(_)));

        let non_prod_inconclusive = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            has_integration_evidence_marker: false,
            er_verdict: ErVerdict::Inconclusive,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert!(matches!(evidence_floor_gate(&non_prod_inconclusive), GateResult::Red(_)));
    }

    // --- jleechan-zaga: structured gate-block classification tests (#348) ---
    // The classifier keys ONLY on the `GateResult` variant + gate name (the
    // structured signal), NOT on reason-text keywords or synthetic
    // `coderabbit_status` values production never emits. These tests use real
    // production evidence shapes: a CodeRabbit stall is `coderabbit_status =
    // "unknown"` (→ an `Unknown` CodeRabbit gate), never a fabricated
    // "blocked"/"usage_limit".

    #[test]
    fn classify_green_gate_is_none() {
        assert_eq!(
            classify_nongreen_gate(GateName::Ci, &GateResult::Green),
            None
        );
    }

    #[test]
    fn classify_every_red_gate_is_coder_fixable() {
        for name in [
            GateName::Ci,
            GateName::NoConflicts,
            GateName::CodeRabbitApproved,
            GateName::BugbotClean,
            GateName::CommentsResolved,
            GateName::EvidenceFloor,
            GateName::Skeptic,
        ] {
            assert_eq!(
                classify_nongreen_gate(name, &GateResult::Red("whatever".into())),
                Some(GateBlock::CoderFixable),
                "red {name:?} must be coder-fixable"
            );
        }
    }

    #[test]
    fn classify_coderabbit_unknown_is_structural() {
        // Production shape: coderabbit_status == "unknown" makes the gate
        // `Unknown` (verifier::assess). A coder cannot make CodeRabbit run.
        assert_eq!(
            classify_nongreen_gate(
                GateName::CodeRabbitApproved,
                &GateResult::Unknown("CodeRabbit review is still pending/unknown".into())
            ),
            Some(GateBlock::Structural)
        );
    }

    #[test]
    fn classify_bugbot_unknown_is_structural() {
        assert_eq!(
            classify_nongreen_gate(
                GateName::BugbotClean,
                &GateResult::Unknown("Bugbot absent".into())
            ),
            Some(GateBlock::Structural)
        );
    }

    #[test]
    fn classify_transient_unknowns_are_transient() {
        // CI still running and an unverifiable review-thread count are
        // transient — neither structural nor a reroll trigger. They resolve on
        // their own, so they must classify `Transient` (which keeps a chain in
        // the wait/re-assess path, NOT the DISPOSITION_REQUIRED hold).
        for name in [
            GateName::Ci,
            GateName::CommentsResolved,
            GateName::Skeptic,
            GateName::NoConflicts,
            GateName::EvidenceFloor,
        ] {
            assert_eq!(
                classify_nongreen_gate(name, &GateResult::Unknown("pending".into())),
                Some(GateBlock::Transient),
                "unknown {name:?} must be transient, not structural"
            );
        }
    }

    #[test]
    fn classify_chain_total_scm_outage_is_transient_not_hold() {
        // A whole-snapshot SCM fetch failure turns EVERY gate Unknown —
        // including CodeRabbit and Bugbot. That is a transient outage, NOT a
        // structural CodeRabbit-unavailable condition, so the chain must WAIT
        // (TransientOnly), never hold DISPOSITION_REQUIRED. Regression guard
        // for qdw_assess_refetch_failure_stays_attested_and_never_closes_pr.
        let report = GateReport {
            all_green: false,
            results: [
                (GateName::Ci, GateResult::Unknown("fetch failed".into())),
                (GateName::NoConflicts, GateResult::Unknown("fetch failed".into())),
                (
                    GateName::CodeRabbitApproved,
                    GateResult::Unknown("fetch failed".into()),
                ),
                (GateName::BugbotClean, GateResult::Unknown("fetch failed".into())),
                (
                    GateName::CommentsResolved,
                    GateResult::Unknown("fetch failed".into()),
                ),
                (GateName::EvidenceFloor, GateResult::Unknown("fetch failed".into())),
                (GateName::Skeptic, GateResult::Unknown("fetch failed".into())),
            ],
        };
        assert_eq!(classify_chain(&report), ChainDisposition::TransientOnly);
    }

    #[test]
    fn classify_chain_all_green_is_transient_only() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();
        assert!(report.all_green);
        assert_eq!(classify_chain(&report), ChainDisposition::TransientOnly);
    }

    #[test]
    fn classify_chain_coderabbit_unknown_only_holds_disposition() {
        // The #348 production scenario: CodeRabbit unavailable
        // (coderabbit_status="unknown" -> Unknown gate) is the ONLY non-green
        // gate. No coder-fixable red -> HoldDisposition.
        let mut scm = FakeScm::default();
        let mut snapshot = all_green_snapshot(7);
        snapshot.coderabbit_approved = false;
        snapshot.coderabbit_status = "unknown".to_string();
        scm.snapshots.insert(7, snapshot);
        let cfg = test_cfg();
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();
        assert!(!report.all_green);
        assert_eq!(classify_chain(&report), ChainDisposition::HoldDisposition);

        // The structural gate is surfaced for the operator comment.
        let structural = structural_pending_gates(&report);
        assert_eq!(structural.len(), 1);
        assert_eq!(structural[0].0, GateName::CodeRabbitApproved);
    }

    #[test]
    fn classify_chain_mixed_red_plus_structural_rerolls() {
        // CI red (coder-fixable) + CodeRabbit unknown (structural). A
        // coder-fixable red wins -> Reroll (issue #348: "mixed red → reroll").
        let mut scm = FakeScm::default();
        let mut snapshot = all_green_snapshot(7);
        snapshot.ci_success = false;
        snapshot.ci_status = "red".to_string();
        snapshot.coderabbit_approved = false;
        snapshot.coderabbit_status = "unknown".to_string();
        scm.snapshots.insert(7, snapshot);
        let cfg = test_cfg();
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();
        assert_eq!(classify_chain(&report), ChainDisposition::Reroll);
    }

    #[test]
    fn classify_chain_all_coder_fixable_rerolls() {
        let mut scm = FakeScm::default();
        let mut snapshot = all_green_snapshot(7);
        snapshot.ci_success = false;
        snapshot.ci_status = "red".to_string();
        snapshot.mergeable = false;
        scm.snapshots.insert(7, snapshot);
        let cfg = test_cfg();
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();
        assert_eq!(classify_chain(&report), ChainDisposition::Reroll);
    }

    #[test]
    fn classify_chain_ci_unknown_only_is_transient() {
        // CI still running is transient, NOT structural — it must not hold
        // DISPOSITION_REQUIRED (that's for external verifiers a coder can't
        // drive, not for a check that resolves on its own).
        let mut scm = FakeScm::default();
        let mut snapshot = all_green_snapshot(7);
        snapshot.ci_success = false;
        snapshot.ci_status = "unknown".to_string();
        scm.snapshots.insert(7, snapshot);
        let cfg = test_cfg();
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();
        assert_eq!(classify_chain(&report), ChainDisposition::TransientOnly);
    }
}

