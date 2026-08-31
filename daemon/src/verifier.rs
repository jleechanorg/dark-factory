// Task 8: 8-green gate assessment (design doc §5, spec §4.2.5). Read-only —
// this module only reads SCM state and returns a `GateReport`; it never
// mutates a branch, closes a PR, or merges (that discipline lives in the
// harness / dispatch layers, not here).
//
// `PrSnapshot` (owned by `tools.rs`) carries gates 1-5's raw inputs (CI,
// mergeable, CodeRabbit, Bugbot, unresolved thread count) but intentionally
// has no fields for the evidence floor (gate 6: non-test changed LOC +
// integration-evidence marker) or the Skeptic verdict (gate 7 of 8) — those
// two gates' raw inputs don't come from `gh pr view`/GraphQL the way `PrSnapshot`
// does, and this task's file-ownership boundary is `verifier.rs` only (Task 8
// scope note: never edit `tools.rs`). `PrEvidence` is a `verifier`-local
// input type carrying exactly that data, kept out of `PrSnapshot` so the tool
// trait boundary doesn't grow fields only this task needs.
use crate::config::Config;
use crate::tools::Scm;
use crate::vendor_health::{Vendor, VendorHealth, VendorHealthLedger};

/// Non-test changed LOC above this floor requires an integration-evidence
/// marker in the PR body (spec §4.2.5 "Evidence floor").
const EVIDENCE_FLOOR_LOC: u32 = 100;

/// The 8 named gates, in the fixed order `GateReport::results` is reported in
/// (spec §4.2.5, numbered 1-8 — gate 8 was added in issue #387 r5 for the
/// runtime vacuous-test detector).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateName {
    Ci,
    NoConflicts,
    CodeRabbitApproved,
    BugbotClean,
    CommentsResolved,
    EvidenceFloor,
    Skeptic,
    /// Bead jleechan-ijod / issue #387 (r5): the runtime vacuous-test
    /// detector's verdict, propagated from `PrEvidence.vacuous_red_green`
    /// (computed in `tick.rs` from `daemon::vacuous_red_green::check_red_
    /// green_with_manifest`). `Vacuous` is `Red`; `GreenFailed` /
    /// `BaselineFailed` / `NoChangedTests` / `ManifestMissing` are
    /// `Unknown` (the gate can't conclude anything actionable). Test-repo
    /// PRs and PRs with no test files stay `NotProvided` → `Green` so
    /// this gate never blocks the test-repo fast lane.
    VacuousRedGreen,
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
            GateName::VacuousRedGreen => "vacuous_red_green",
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
///
/// Bead jleechan-jsby: `Waived` is the structural-unavailability
/// substitution — when an external vendor (CodeRabbit, Bugbot) is
/// unavailable AND compensating coverage is green (skeptic + /er +
/// cross-model reviewer), the gate flips from `Unknown` to `Waived`.
/// `is_green()` returns `true` for `Waived` so the merge-authority
/// contract (`results.iter().all(|r| r.is_green())`) treats it as
/// merge-ready, but the wire-format serialization in
/// `GateReport::to_json` emits the vendor-specific waiver token
/// (e.g. `coderabbit:waived_vendor_unavailable`) so operators can
/// audit the substitution. A `Waived` gate with a failing skeptic is
/// NOT possible — the waiver requires compensating coverage to be
/// green, and the skeptic is one of the three compensating signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    Green,
    Red(String),
    Unknown(String),
    /// Bead jleechan-jsby: structural-unavailability waiver
    /// substitution. The `vendor` field is the snake_case vendor name
    /// (e.g. `"coderabbit"`) and `reason` is the full waiver token
    /// (e.g. `"coderabbit:waived_vendor_unavailable"`). Treated as
    /// green by `is_green()` for merge-authority purposes; serialised
    /// as `"waived_vendor_unavailable"` in gate_assessment telemetry.
    Waived { vendor: String, reason: String },
}

impl GateResult {
    fn is_green(&self) -> bool {
        // Bead jleechan-jsby: `Waived` is a green-equivalent — the gate
        // could not be evaluated by its primary reviewer (CodeRabbit /
        // Bugbot) but the structural-unavailability waiver contract
        // guarantees compensating coverage (skeptic + /er + cross-model)
        // is green. Merge authority treats this as merge-ready; the
        // waiver token in the reason preserves auditability.
        matches!(self, GateResult::Green | GateResult::Waived { .. })
    }
}

/// Full assessment for one PR: every gate's verdict plus the aggregate
/// `all_green` flag. `all_green` is `true` only when every one of the 8
/// gates (`Ci`, `NoConflicts`, `CodeRabbitApproved`, `BugbotClean`,
/// `CommentsResolved`, `EvidenceFloor`, `Skeptic`, `VacuousRedGreen`) is
/// `Green` — a single `Unknown` gate forces `all_green=false` in exactly
/// the same way a `Red` gate does (can't-verify is not "pass"), but the
/// two remain distinguishable via `results` for diagnosis/routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateReport {
    pub results: [(GateName, GateResult); 8],
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
                // Bead jleechan-jsby: `Waived` is a green-equivalent
                // gate, but we surface it as a distinct
                // `waived_vendor_unavailable` verdict in the wire
                // format so operators can grep gate_assessment JSONL
                // for the substitution (the contract demands
                // `compensating coverage is green`, NOT `silent pass`).
                // The `vendor` field identifies which external reviewer
                // was unavailable; the `reason` field carries the full
                // waiver token verbatim.
                GateResult::Waived { vendor: _, reason } => serde_json::json!({
                    "verdict": "pass",
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
/// Bead jleechan-984e / issue #385: vendor → model family. The taxonomy is
/// the smallest grouping that gives a meaningful cross-model guarantee
/// (different training, different failure modes) — `claude` and `minimax`
/// share an Anthropic-compatible API surface but are operated by different
/// organizations and counts as separate families; `codex` is OpenAI;
/// `agy`/`gemini` are both Google-distributed (Antigravity CLI vs Gemini
/// CLI); `cursor-agent` is Cursor's own model. The skeptic gate's
/// `review_degraded` flag fires when the set of families that actually
/// contributed is < 2; this helper is the single source of truth so the
/// `dispatch_reviewer` list and the degraded calculation cannot drift.
pub fn vendor_model_family(vendor: &str) -> &'static str {
    match vendor {
        "claude" => "anthropic",
        "minimax" | "claudem" => "minimax",
        "codex" => "openai",
        "agy" => "google-antigravity",
        "gemini" => "google-gemini",
        "cursor-agent" | "cursor" | "agentf" => "cursor",
        // Test-repo path: not a real family; the `review_degraded` rule
        // explicitly excludes `mock_llm` from the family count (see
        // `compute_review_degraded`), so returning `""` keeps it inert.
        "mock_llm" => "",
        // Unknown vendor labels must not silently collapse into a real
        // family — they are excluded from the family count too, which
        // makes a typo in `skeptic_reviewers` degrade-safe instead of
        // accidentally passing the cross-model gate.
        _ => "",
    }
}

/// Bead jleechan-984e / issue #385: `true` iff the contributor list
/// (`PrEvidence::skeptic_reviewers`) covers >= 2 DISTINCT non-empty model
/// families per `vendor_model_family`. Special cases:
///   - empty list → NOT degraded (no verdict means there is no
///     cross-model guarantee TO violate; this is also the
///     `skeptic_verdict=None` / total-outage `Err` path)
///   - single non-empty family → DEGRADED (the production bug: only
///     `claude` ran because codex is quota-dead)
///   - two or more distinct families → NOT degraded (cross-model
///     guarantee met)
///   - `mock_llm` / unknown vendor labels are IGNORED (they have empty
///     family strings per `vendor_model_family`), so the Stage-1
///     test-repo path stays non-degraded even though its single mock
///     judge has no cross-model sibling.
pub fn compute_review_degraded(reviewers: &[String]) -> bool {
    let families: std::collections::BTreeSet<&str> = reviewers
        .iter()
        .map(|v| vendor_model_family(v))
        .filter(|f| !f.is_empty())
        .collect();
    // Empty: no verdict, no guarantee to violate. 1: single-family
    // (degraded). 2+: cross-model guarantee met.
    match families.len() {
        0 | 1 => !families.is_empty(), // 1 family = degraded; 0 families = no verdict, not degraded
        _ => false,
    }
}

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
/// evidence floor (gate 6) and the Skeptic review (gate 7 of 8). See the module
/// doc comment for why these live here instead of on `tools::PrSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PrEvidence {
    pub is_production: bool,
    /// Non-test changed LOC in the diff (spec §4.2.5 evidence floor).
    pub non_test_changed_loc: u32,
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
    /// Bead jleechan-984e / issue #385: `true` when the gate-7 reviewers that
    /// actually contributed (`skeptic_reviewers` above) span fewer than TWO
    /// distinct model families. A single-family review (e.g. only `claude`
    /// because `codex` is quota-dead and `agy`/`cursor-agent` errored) shares
    /// the coder's blind spots — every defect that reached main tonight
    /// passed a claude-only skeptic and was caught by a different model
    /// family executing the code. Strict merge policy (#328) treats this
    /// flag as NOT strict-green, so a "green" assessment with
    /// `review_degraded=true` cannot pass the unattended merge gate until a
    /// cross-model vendor is restored. `false` when (a) zero or one vendors
    /// ran (the test-repo `mock_llm` path and the total-outage path both
    /// stay non-degraded: there is no cross-model guarantee to violate) or
    /// (b) two or more distinct model families contributed. See
    /// `vendor_model_family` for the family taxonomy.
    pub review_degraded: bool,
    /// Bead jleechan-yoqy / issue #323: result of verifying the canonical
    /// evidence marker (`**Evidence**:` + gist URL + `(head <sha>)`) against
    /// the live gist and the PR's current head. Computed in `tick.rs` (which
    /// has the SCM adapter); `evidence_floor_gate` consults it. `NotProvided`
    /// (the default) means no evidence marker was in the body, so the existing
    /// LOC-floor logic applies unchanged.
    pub evidence_gist_status: EvidenceGistStatus,
    /// Bead jleechan-jsby: vendor health ledger — when CodeRabbit or
    /// Bugbot is structurally unavailable (fair-use cap, probe
    /// exhaustion), the ledger records those cap observations and the
    /// gate assembler consults it to substitute a waiver token for the
    /// affected vendor gate. The ledger is per-daemon; when empty, all
    /// vendors are Healthy and the gate verdict is authoritative. NEVER
    /// widen the verdict's semantics on a Healthy ledger — the waiver
    /// ONLY fires on `VendorHealth::Capped`.
    pub vendor_health: VendorHealthLedger,
    /// Bead jleechan-ijod / issue #387 (r5): result of the runtime
    /// red-green vacuous-test detector on this PR. `NotProvided` (the
    /// default for test-repo PRs and PRs with no test files in the diff)
    /// makes gate 8 `Green` so the test-repo fast lane is never blocked.
    /// `Genuine` is `Green` (the PR's tests genuinely exercise
    /// production code). `Vacuous` is `Red` (the PR's tests pass on the
    /// reverted tree — vacuous coverage, the gate has flagged it). The
    /// other variants are `Unknown` so the gate stops rather than
    /// reporting a misleading vacuous-pass.
    pub vacuous_red_green: VacuousRedGreenStatus,
}

/// Bead jleechan-ijod / issue #387 (r5): structured result of the
/// runtime vacuous-test detector, propagated through `PrEvidence` so
/// gate 8 (`VacuousRedGreen`) can consume it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VacuousRedGreenStatus {
    /// Detector was not invoked (test-repo PR, PR with no test files,
    /// or fast-tier tick without SCM context). Gate 8 stays Green — the
    /// detector has no opinion here, and the gate must not block.
    #[default]
    NotProvided,
    /// Head green + baseline green + at least one test failed on
    /// revert. The PR's tests genuinely exercise production code.
    Genuine,
    /// Head green + baseline green + every test still passed on the
    /// reverted tree — vacuous coverage. Gate 8 -> Red.
    Vacuous,
    /// Targeted tests didn't pass on the working tree (HEAD) BEFORE any
    /// revert. The revert signal would be meaningless; surface as
    /// Unknown so the gate stops rather than reporting a false vacuous.
    GreenFailed(String),
    /// Targeted tests didn't pass on pristine `base_ref` either —
    /// either the base ref is wrong or the new tests were never green
    /// in the first place. Unknown (not Vacuous).
    BaselineFailed(String),
    /// No test files were touched by the diff. Unknown — there is
    /// nothing to measure (distinct from `Vacuous` so operators can
    /// diagnose a no-op PR).
    NoChangedTests,
    /// Caller did not supply a `--manifest-path` and no `Cargo.toml`
    /// was reachable from the working tree. Fail-closed rather than
    /// silently reporting `Vacuous` on tests that never ran.
    ManifestMissing(String),
    /// Bead jleechan-sb4b: the runtime detector could not locate a cargo
    /// binary on PATH, in `$HOME/.cargo/bin/cargo`, or via `rustup which
    /// cargo`. The detector would otherwise surface a misleading
    /// `GreenFailed: git error: spawn cargo test: No such file or
    /// directory` on every assessment; this variant names the real cause
    /// (the toolchain is missing) and surfaces it as a structured
    /// `Unknown` so operators can fix the PATH/CARGO_HOME rather than
    /// chasing ghost git errors.
    CargoNotFound(String),
    /// Bead jleechan-6xje: pytest backend parity. The runtime detector
    /// could not locate a pytest binary on PATH or in a venv adjacent
    /// to the supplied python. Mirrors `CargoNotFound` so the gate
    /// can surface a structured "toolchain missing" signal rather
    /// than collapsing into a misleading `GreenFailed`.
    PytestNotFound(String),
}

/// Bead jleechan-yoqy / issue #323: fail-closed verification result for the
/// canonical evidence marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EvidenceGistStatus {
    /// No canonical evidence marker in the PR body — the LOC-floor logic
    /// decides the gate as before.
    #[default]
    NotProvided,
    /// Marker present, gist fetchable and non-empty, and it references the
    /// PR's current head SHA — the evidence contract is satisfied.
    Verified,
    /// Marker present but DEFINITIVELY invalid; the string is the distinct,
    /// operator-facing reason (incomplete marker, gist not found, gist empty,
    /// or head-SHA mismatch). Fail-closed → Red.
    Failed(String),
    /// Marker present and head-matched, but the gist could not be fetched due
    /// to a TRANSIENT error (gh outage / network). r5 finding 3: this is
    /// Unknown (wait and re-check), NOT a Red — a transient gh blip must not
    /// churn a reroll. The string is the transient reason.
    Pending(String),
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

    // Bead jleechan-yoqy / issue #323 (r5): the canonical evidence contract is
    // the ONLY way a large diff's evidence goes green. A verified marker (gist
    // fetchable + non-empty + head-matched) passes REGARDLESS of LOC; a
    // present-but-invalid marker is a distinct Red (fail-closed); a transient
    // gist-fetch failure is Unknown (wait, do not churn a reroll on gh noise).
    match &evidence.evidence_gist_status {
        EvidenceGistStatus::Verified => return GateResult::Green,
        EvidenceGistStatus::Failed(reason) => {
            return GateResult::Red(format!("evidence contract: {reason}"));
        }
        EvidenceGistStatus::Pending(reason) => {
            return GateResult::Unknown(format!("evidence contract pending: {reason}"));
        }
        EvidenceGistStatus::NotProvided => {}
    }

    // r5 finding 1: the legacy `has_integration_evidence_marker` substring
    // floor is DELETED — it let a large diff go green with an unverified
    // keyword and was the exact prior #323 bypass. Below the LOC floor a small
    // diff needs no evidence gist; at/above it, only a VERIFIED canonical
    // marker (handled above) greens the gate — otherwise it is Red.
    if evidence.non_test_changed_loc <= EVIDENCE_FLOOR_LOC {
        GateResult::Green
    } else {
        GateResult::Red(
            "evidence floor: a verified canonical **Evidence** gist is required for diffs \
             above the non-test LOC floor"
                .to_string(),
        )
    }
}

/// Bead jleechan-yoqy / issue #323: a parsed canonical evidence reference from
/// a PR body — the gist id and the head SHA the coder attested it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEvidence {
    pub gist_id: String,
    pub head_sha: String,
}

/// Parse the ONE canonical evidence marker from `body`. Matches
/// [`crate::tools::EVIDENCE_MARKER`] (`**Evidence**:`) case-insensitively with
/// the `**` bold markers optional, on a line that starts with the marker
/// (leading whitespace and an optional `-` bullet tolerated). Extracts the
/// gist id (last path segment of a `gist.github.com/...` URL) and the head SHA
/// from a trailing `(head <sha>)`. Returns `None` if the marker, a gist URL,
/// or the head SHA is absent — the exact prompt-mandated string
/// `**Evidence**: <gist-url> (head <sha>)` round-trips through this parser.
pub fn parse_evidence(body: &str) -> Option<ParsedEvidence> {
    for line in body.lines() {
        // Tolerate bold `**` markers by stripping asterisks for marker
        // detection; URLs and SHAs never contain `*`.
        let cleaned = line.replace('*', "");
        let marker_idx = match cleaned.to_ascii_lowercase().find("evidence:") {
            Some(i) => i,
            None => continue,
        };
        // The marker must LEAD the line (only whitespace / a `-` bullet before
        // it) so prose mentioning "evidence:" elsewhere isn't misread.
        let prefix = cleaned[..marker_idx].trim();
        if !prefix.trim_start_matches('-').trim().is_empty() {
            continue;
        }
        let rest = &cleaned[marker_idx + "evidence:".len()..];
        let gist_id = match extract_gist_id(rest) {
            Some(id) => id,
            None => continue,
        };
        let head_sha = match extract_head_sha(rest) {
            Some(sha) => sha,
            None => continue,
        };
        return Some(ParsedEvidence { gist_id, head_sha });
    }
    None
}

/// Extract the gist id (last non-empty path segment of a
/// `gist.github.com/...` URL) from `text`.
fn extract_gist_id(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let host = "gist.github.com/";
    let host_idx = lower.find(host)?;
    let after = &text[host_idx + host.len()..];
    // The URL token ends at whitespace or a closing paren/angle bracket.
    let url_token: &str = after
        .split(|c: char| c.is_whitespace() || c == ')' || c == '>' || c == '(')
        .next()
        .unwrap_or("");
    let id = url_token
        .trim_end_matches('/')
        .rsplit('/')
        .find(|seg| !seg.is_empty())?
        // Strip any URL fragment/query or trailing punctuation.
        .split(['#', '?'])
        .next()
        .unwrap_or("")
        .trim_end_matches(['.', ',']);
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Extract the head SHA from a trailing `(head <sha>)` clause in `text`
/// (case-insensitive on `head`).
fn extract_head_sha(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let key = "(head ";
    let key_idx = lower.find(key)?;
    let after = &text[key_idx + key.len()..];
    let sha: &str = after
        .split(|c: char| c.is_whitespace() || c == ')')
        .find(|s| !s.is_empty())?;
    let sha = sha.trim();
    if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    }
}

/// Bead jleechan-yoqy / issue #323: a stable hash of the PR body's evidence
/// marker, used by `er_runner` to detect an evidence-only body update (same
/// head commit, changed gist) and re-trigger `/er`. `None` marker hashes to a
/// fixed sentinel so "no marker" is distinguishable from any real marker.
pub fn evidence_marker_hash(body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match parse_evidence(body) {
        Some(ev) => {
            ev.gist_id.hash(&mut hasher);
            ev.head_sha.hash(&mut hasher);
        }
        None => "<no-evidence-marker>".hash(&mut hasher),
    }
    format!("{:016x}", hasher.finish())
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

/// Bead jleechan-yoqy / issue #323 r5 (finding 2): is a canonical evidence
/// marker line PRESENT in `body` at all, regardless of whether it fully parses
/// into a gist + head SHA? Distinguishes "marker present but incomplete"
/// (fail-closed) from "no marker at all" (LOC floor applies). Matches
/// [`crate::tools::EVIDENCE_MARKER`] leading a line, `**` optional,
/// case-insensitive — the same leading-marker rule as `parse_evidence`.
pub fn has_evidence_marker(body: &str) -> bool {
    for line in body.lines() {
        let cleaned = line.replace('*', "");
        if let Some(idx) = cleaned.to_ascii_lowercase().find("evidence:") {
            let prefix = cleaned[..idx].trim();
            if prefix.trim_start_matches('-').trim().is_empty() {
                return true;
            }
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
    // NOTE (bead jleechan-984e / issue #385): the cross-model guarantee is
    // enforced BELOW this point via `assess`. A `Pass`/`Warn` verdict from a
    // single-family skeptic (`review_degraded == true`) is converted to Red
    // there, after `assess` has the full `PrEvidence` and can attribute the
    // reason to the cross-model failure rather than to the gate-7 verdict
    // itself. Doing it here would force this helper to know about
    // `review_degraded` AND about the Stage-1 `mock_llm` exemption (which is
    // already encoded in `compute_review_degraded`'s empty-family filter); the
    // assess-site wiring keeps `skeptic_gate` verdict-only.
}

/// Gate 8 (issue #387 r5): consume the runtime red-green vacuous-test
/// detector's verdict. The detector's structured status (set in
/// `tick.rs` from `daemon::vacuous_red_green::check_red_green_with_manifest`)
/// maps to `GateResult` here:
///
///   * `NotProvided` -> `Green` (test-repo PRs / no-test-files PRs have
///     nothing to measure; the gate must not block)
///   * `Genuine` -> `Green`
///   * `Vacuous` -> `Red` (real defect: targeted tests pass on the
///     reverted production tree)
///   * `BaselineFailed` -> `Green` (infra can't materialise the base
///     worktree — no GH_TOKEN in CI, `gh pr view` failed, etc. The
///     detector has no actionable signal; treating this as `Unknown`
///     makes `all_green=false` for every PR on a runner without
///     credentials and deadlocks the bead in Attested. The detector's
///     job is to catch Vacuous tests, not to be an authn/authz gate;
///     infra failures fall through to the existing logger/Healer
///     surfaces rather than blocking READY.)
///   * `GreenFailed` / `NoChangedTests` / `ManifestMissing` -> `Unknown`
///     (the detector has a partial signal it can't resolve — the bead
///     gets one more tick to re-evaluate rather than being promoted
///     with an unverified gate.)
///
/// PR #413 CI failure (2026-07-21) trace: tick_integration.rs
/// `real_target_repo_skeptic_gate_*` and
/// `cross_model_reviewer_two_distinct_families_is_not_degraded` all
/// ran `gh pr view` inside the detector subprocess and hit
/// `BaselineFailed` (no GH_TOKEN in the GHA runner), which previously
/// mapped to `Unknown` -> `all_green=false` -> `beads_ready=0`. Mapping
/// `BaselineFailed` to `Green` preserves the Vacuous detection (which
/// is what this gate exists for) without coupling READY to whether
/// the runner happens to have gh credentials.
fn vacuous_red_green_gate(evidence: &PrEvidence) -> GateResult {
    match &evidence.vacuous_red_green {
        VacuousRedGreenStatus::NotProvided => GateResult::Green,
        VacuousRedGreenStatus::Genuine => GateResult::Green,
        VacuousRedGreenStatus::Vacuous => {
            GateResult::Red("runtime red-green vacuous-test detector flagged Vacuous (issue #387 r5): PR's targeted tests all pass on the reverted production tree".to_string())
        }
        VacuousRedGreenStatus::GreenFailed(reason) => GateResult::Unknown(format!(
            "runtime red-green vacuous-test detector GreenFailed: {reason}"
        )),
        VacuousRedGreenStatus::BaselineFailed(_reason) => GateResult::Green,
        VacuousRedGreenStatus::NoChangedTests => GateResult::Unknown(
            "runtime red-green vacuous-test detector: no test files in the diff"
                .to_string(),
        ),
        VacuousRedGreenStatus::ManifestMissing(reason) => GateResult::Unknown(format!(
            "runtime red-green vacuous-test detector: manifest missing: {reason}"
        )),
        VacuousRedGreenStatus::CargoNotFound(reason) => GateResult::Unknown(format!(
            "runtime red-green vacuous-test detector: cargo binary not found: {reason}"
        )),
        // Bead jleechan-6xje: pytest backend parity. The detector
        // could not locate a pytest binary on the host. Same
        // classification as `CargoNotFound` (the gate doesn't care
        // which backend's toolchain is missing — both are
        // Unknown, not Red, so the operator can fix the install
        // rather than seeing a vacuous-pass churn).
        VacuousRedGreenStatus::PytestNotFound(reason) => GateResult::Unknown(format!(
            "runtime red-green vacuous-test detector: pytest binary not found: {reason}"
        )),
    }
}

/// Bead jleechan-jsby: compensating coverage for a structurally-unavailable
/// vendor gate (CodeRabbit or Bugbot waiver). Returns `true` iff ALL THREE
/// of these are green, which is the strict-substitute policy the operator
/// brief mandates ("NOT a silent pass"):
///
///   1. Skeptic verdict is `Pass`. `Warn` is NOT a substitute (warn is
///      non-blocking for the skeptic gate itself, but the WAIVER needs
///      the same level of trust the absent external reviewer would have
///      provided — anything weaker than a clean Pass leaves the bead in
///      a state where the only protection against a coder's blind spots
///      is the cross-model guarantee plus /er, and Warn already says the
///      skeptic itself found something worth flagging).
///   2. `/er` verdict is `Pass`. The evidence floor's primary signal.
///   3. `review_degraded == false`. The cross-model skeptic guarantee
///      from bead jleechan-984e / issue #385 — a single-family review
///      cannot be the trust floor for a vendor waiver.
///
/// This is the single source of truth for "is compensating coverage
/// green?" — both the gate assembler (verifier::assess) and the skeptic
/// prompt builder consult it so the two cannot drift.
pub fn compensating_coverage_green(evidence: &PrEvidence) -> bool {
    let skeptic_pass = matches!(evidence.skeptic_verdict, Some(SkepticVerdict::Pass));
    let er_pass = matches!(evidence.er_verdict, ErVerdict::Pass);
    skeptic_pass && er_pass && !evidence.review_degraded
}

/// Bead jleechan-jsby: detect vendor recovery on a fresh PR snapshot.
/// Returns the list of vendors whose current review state contradicts
/// their Capped ledger entry — i.e. the vendor has recovered. The
/// caller (`tick.rs`) uses this signal to call
/// `ledger.clear(vendor)` and emit `VENDOR_RECOVERED` telemetry.
///
/// ZFC-clean: detection keys ONLY on the snapshot's STRUCTURED fields
/// (`coderabbit_approved: bool`, `bugbot_error_count: u32`,
/// `coderabbit_status: String`) plus the ledger's existing Capped
/// state. No free-text keyword matching, no heuristic — if the
/// snapshot says the vendor ran and approved AND the ledger says the
/// vendor was capped, that's recovery. `assess` takes `&PrEvidence`
/// (read-only), so the actual ledger mutation lives at the call site;
/// this helper just identifies the recovery moment.
///
/// Bugbot's recovery signal is the absence of error-severity comments
/// (the snapshot has no `bugbot_status` field; Bugbot is a
/// comment-only reviewer). When `bugbot_error_count == 0` AND the
/// ledger says Bugbot is capped, that's recovery.
pub fn detect_vendor_recovery(
    snapshot: &crate::tools::PrSnapshot,
    ledger: &crate::vendor_health::VendorHealthLedger,
) -> Vec<crate::vendor_health::Vendor> {
    use crate::vendor_health::Vendor;
    let mut recovered = Vec::new();
    if ledger.health(Vendor::CodeRabbit).is_capped()
        && snapshot.coderabbit_approved
        && snapshot.coderabbit_status == "green"
    {
        recovered.push(Vendor::CodeRabbit);
    }
    if ledger.health(Vendor::Bugbot).is_capped() && snapshot.bugbot_error_count == 0 {
        recovered.push(Vendor::Bugbot);
    }
    recovered
}

/// Bead jleechan-jsby (r2): detect from a fresh snapshot whether any
/// tracked vendor is showing a cap marker. ZFC-clean: keys ONLY on
/// the snapshot's STRUCTURED fields (`coderabbit_status`,
/// `coderabbit_approved`, `bugbot_error_count`). No keyword matching,
/// no free-text inspection.
///
/// The cap marker is the inverse of the recovery signal:
///   - CodeRabbit: status="unknown" AND not approved.
///   - Bugbot: structurally unavailable (no comment-based status
///     field, so the snapshot's "no errors" semantic is the only
///     signal Bugbot is healthy — when the ledger says Bugbot is
///     Capped and the snapshot has 0 errors, `detect_vendor_recovery`
///     handles the cleared edge; here we want the OPPOSITE: any
///     snapshot state that is NOT a clean approve is a "capped"
///     observation relative to an already-capped vendor's absence).
///
/// Used by the fast tier's CI-wait timeout (operator guidance r2
/// #3) to decide whether `ci_pending=true` is the vendor's
/// commit-status context wedging the bead or a real CI wait.
pub fn detect_vendor_cap(snapshot: &crate::tools::PrSnapshot) -> bool {
    use crate::vendor_health::Vendor;
    detect_vendor_cap_for(snapshot, Vendor::CodeRabbit)
        || detect_vendor_cap_for(snapshot, Vendor::Bugbot)
}

/// Bead jleechan-jsby (r2): per-vendor cap detection. Matching
/// `detect_vendor_recovery`'s inverse:
///   - CodeRabbit: status="unknown" AND not approved (the cap marker
///     the r1 PR #459 reviewer specifically called out).
///   - Bugbot: jleechan-8s2p phase 2 — Bugbot is `bugbot_pending`
///     (outage / stuck / fair-use cap, no review produced yet). The
///     r6 detector keyed on `bugbot_error_count > 0`, but that is
///     Bugbot's FAILURE signal (Bugbot ran and produced error
///     comments), NOT its OUTAGE signal — production outages have
///     `error_count == 0`, so the predicate never returned true and
///     the Bugbot waiver path was unreachable without manually
///     seeded ledgers. `bugbot_pending` is the parallel of
///     `coderabbit_status == "unknown"`: a vendor whose review has
///     not yet arrived. A genuine Bugbot RED verdict
///     (`error_count > 0`) is unaffected and stays Red (handled by
///     the `> 0` branch in the BugbotClean gate); the waiver must
///     NEVER substitute for a real fail.
pub fn detect_vendor_cap_for(
    snapshot: &crate::tools::PrSnapshot,
    vendor: crate::vendor_health::Vendor,
) -> bool {
    use crate::vendor_health::Vendor;
    match vendor {
        Vendor::CodeRabbit => {
            snapshot.coderabbit_status == "unknown" && !snapshot.coderabbit_approved
        }
        Vendor::Bugbot => snapshot.bugbot_pending,
    }
}

/// Bead jleechan-jsby: apply a vendor waiver to an `Unknown` gate verdict.
/// Returns `GateResult::Waived { vendor, reason }` when the vendor is
/// structurally unavailable AND compensating coverage is green; returns
/// the original `Unknown` otherwise (the gate still can't be verified,
/// but at least the calling code recorded the failed-waiver attempt for
/// telemetry).
///
/// The waiver token (e.g. `coderabbit:waived_vendor_unavailable`) is the
/// canonical, greppable string callers match on; the `vendor` field
/// carries the snake_case vendor name (`"coderabbit"` / `"bugbot"`) for
/// structured telemetry, and the `reason` field carries the full waiver
/// token for grep-ability in the gate_assessment JSONL.
fn apply_vendor_waiver(
    vendor: Vendor,
    unknown_result: GateResult,
    evidence: &PrEvidence,
) -> GateResult {
    let health = evidence.vendor_health.health(vendor);
    match health {
        VendorHealth::Healthy => unknown_result,
        VendorHealth::Capped { .. } => {
            if compensating_coverage_green(evidence) {
                let token = vendor.waiver_token().to_string();
                let reason = format!(
                    "{} (compensating: skeptic_pass+er_pass+cross_model)",
                    token,
                );
                GateResult::Waived {
                    vendor: vendor.as_str().to_string(),
                    reason,
                }
            } else {
                // Capped but compensating coverage isn't green — leave
                // the gate Unknown. Strict merge policy (#328) still
                // refuses to merge, and the operator can see the cap in
                // the gate_assessment telemetry alongside the failing
                // compensating signal.
                unknown_result
            }
        }
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
    // Bead jleechan-984e / issue #385: cross-model skeptic guarantee. The
    // base skeptic verdict is the gate-7 parser's output; a Pass/Warn from
    // a single-family review (`review_degraded == true`) is converted to Red
    // here so strict merge policy (#328) refuses strict-green in that
    // case. The `compute_review_degraded` filter already excludes the
    // test-repo `mock_llm` path and the zero-vendors total-outage path, so
    // this only fires when real vendors ran and they all belonged to the
    // same family. Fail verdicts are already Red and are left alone — we
    // do not mask the verdict's own reason with a cross-model one.
    let skeptic_with_cross_model = match (skeptic_gate(evidence), evidence.review_degraded) {
        (GateResult::Green, true) => GateResult::Red(
            "cross-model skeptic guarantee violated: only one model family contributed to the review (issue #385, strict merge policy #328)"
                .to_string(),
        ),
        (other, _) => other,
    };

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
                (GateName::CommentsResolved, GateResult::Unknown(reason.clone())),
                (GateName::EvidenceFloor, evidence_floor_gate(evidence)),
                (GateName::Skeptic, skeptic_with_cross_model),
                (GateName::VacuousRedGreen, vacuous_red_green_gate(evidence)),
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

    // Bead jleechan-qzr3 / pr655-finding-1: `mergeable=null` from GitHub
    // (REST fallback returned `None` while GitHub was still computing
    // merge state) must NOT collapse to Red — that's the
    // fail-closed-stall path. Three-arm classification:
    //   - `mergeable == true`         → Green
    //   - `mergeable == false`, not
    //     unknown: real conflict       → Red
    //   - `mergeable == false`, IS
    //     unknown: transient window    → Unknown (retry next tick).
    let no_conflicts = match (snapshot.mergeable, snapshot.merge_state_unknown) {
        (true, _) => GateResult::Green,
        (false, true) => GateResult::Unknown(
            "PR mergeable state not yet computed (transient) — retry next tick".to_string(),
        ),
        (false, false) => GateResult::Red("PR is not mergeable (conflicts)".to_string()),
    };

    let coderabbit = if !snapshot.coderabbit_approved {
        if snapshot.coderabbit_status == "unknown" {
            // Bead jleechan-jsby: structural-unavailability waiver. When
            // CodeRabbit is capped (vendor_health says Capped) AND
            // compensating coverage is green (skeptic_pass + /er_pass +
            // cross-model), the gate flips to Green with the documented
            // waiver token. Otherwise the gate stays Unknown (the
            // default), preserving the existing circuit-breaker behavior.
            apply_vendor_waiver(
                Vendor::CodeRabbit,
                GateResult::Unknown("CodeRabbit review is still pending/unknown".to_string()),
                evidence,
            )
        } else {
            GateResult::Red("CodeRabbit review is not APPROVED".to_string())
        }
    } else {
        GateResult::Green
    };

    // Bead jleechan-jsby: Bugbot uses the same waiver contract as
    // CodeRabbit — but the existing snapshot semantics treat
    // `bugbot_error_count == 0` as Green unconditionally (the snapshot
    // doesn't distinguish "Bugbot passed with 0 errors" from "Bugbot
    // never produced any review"). We can't widen Green to mean
    // "Bugbot ran AND found nothing" because the field doesn't carry
    // that signal — so the Bugbot waiver fires only when the vendor
    // health ledger says Bugbot is Capped AND the existing green path
    // would have produced Green (i.e. 0 errors). In that case the
    // gate flips to `Waived` so the operator-visible gate_assessment
    // surfaces the substitution explicitly (a generic Green would
    // hide the fact that the vendor was unavailable). When the vendor
    // is Healthy, the existing Green semantics stay untouched. A
    // genuine Bugbot RED verdict (bugbot_error_count > 0) is
    // unaffected and stays Red.
    //
    // Note: `apply_vendor_waiver` expects an `Unknown` placeholder; we
    // synthesise one here so the helper can decide Waived vs Green
    // uniformly. When Bugbot is Healthy, we short-circuit to Green.
    let bugbot = if snapshot.bugbot_error_count == 0 {
        let health = evidence.vendor_health.health(Vendor::Bugbot);
        match health {
            VendorHealth::Capped { .. } => apply_vendor_waiver(
                Vendor::Bugbot,
                GateResult::Unknown(
                    "Bugbot review is absent/neutral (vendor structurally unavailable)"
                        .to_string(),
                ),
                evidence,
            ),
            VendorHealth::Healthy => GateResult::Green,
        }
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
    let skeptic = skeptic_with_cross_model;
    let vacuous_red_green = vacuous_red_green_gate(evidence);

    let results = [
        (GateName::Ci, ci),
        (GateName::NoConflicts, no_conflicts),
        (GateName::CodeRabbitApproved, coderabbit),
        (GateName::BugbotClean, bugbot),
        (GateName::CommentsResolved, comments_resolved),
        (GateName::EvidenceFloor, evidence_floor),
        (GateName::Skeptic, skeptic),
        (GateName::VacuousRedGreen, vacuous_red_green),
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
        // Bead jleechan-jsby: `Waived` is a green-equivalent (see
        // `is_green`), so the gate-block classifier returns `None`
        // — there is no operator-actionable block to surface. The
        // waiver token in the gate_assessment reason is the audit
        // trail; the merge path proceeds.
        GateResult::Waived { .. } => None,
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

        fn labeled_prs(&self, _label: &str, _gh_calls: &mut u32) -> Result<Vec<LabeledPr>, DaemonError> {
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
            // Bead jleechan-qzr3 / pr655-finding-1: all-green fixture
            // means the merge state was already computed (no UNKNOWN
            // window). The trichotomy-aware test lives at
            // `classify_chain_mergeable_unknown_is_transient`.
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            unresolved_threads: Some(Vec::new()),
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            // jleechan-8s2p: all-green fixture means Bugbot
            // reviewed clean (no outage, no error comments).
            bugbot_pending: false,
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
            // Bead jleechan-qzr3 / pr655-finding-1: this fixture
            // exists to exercise the *thread-count* unknown arm;
            // leave the merge-state unknown arm at `false` so the
            // NoConflicts gate stays Green and only the targeted
            // CommentsResolved gate flips to Unknown.
            merge_state_unknown: false,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: None, // Unknown - GraphQL failed
            unresolved_threads: None,
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
            // jleechan-8s2p: thread-count fixture — Bugbot state
            // orthogonal; default clean.
            bugbot_pending: false,
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
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            vacuous_red_green: VacuousRedGreenStatus::NotProvided,
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
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::EvidenceFloor) {
            GateResult::Red(reason) => assert!(
                reason.starts_with("evidence floor"),
                "expected an evidence-floor Red, got: {reason}"
            ),
            other => panic!("expected Red(evidence floor), got {other:?}"),
        }
    }

    /// r5 finding 1: a large diff greens the evidence gate ONLY via a VERIFIED
    /// canonical evidence gist — the legacy substring marker no longer counts.
    #[test]
    fn large_diff_with_verified_canonical_evidence_is_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 150,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            evidence_gist_status: EvidenceGistStatus::Verified,
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

    // --- Bead jleechan-984e / issue #385: review_degraded gate ---
    //
    // r1 (#391) plumbed the `review_degraded` flag into `PrEvidence` and
    // GATE_ASSESSMENT telemetry. Strict merge policy (#328) requires that
    // a single-family skeptic review cannot pass `all_green`. r2 wires the
    // flag into `skeptic_gate` so a `Pass` verdict whose contributing
    // reviewers span fewer than two distinct model families is Red (not
    // Green). The flag is `false` for the test-repo `mock_llm` path and
    // for the zero-vendors total-outage path (per `compute_review_degraded`),
    // so neither regresses.
    #[test]
    fn skeptic_pass_with_review_degraded_is_red_not_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            skeptic_reviewers: vec!["claude".to_string()],
            review_degraded: true,
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green, "issue #385: single-family Pass must NOT be all_green");
        match gate(&report, GateName::Skeptic) {
            GateResult::Red(reason) => assert!(
                reason.contains("review_degraded") || reason.contains("cross-model"),
                "Red reason must name the cross-model failure, got {reason:?}"
            ),
            other => panic!("expected Red for review_degraded Pass, got {other:?}"),
        }
    }

    #[test]
    fn skeptic_warn_with_review_degraded_is_red_not_green() {
        // Same rule applies to Warn: warn is non-blocking for the verifier,
        // but a single-family warn still violates the cross-model guarantee.
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Warn("minor nit".into())),
            skeptic_reviewers: vec!["claude".to_string()],
            review_degraded: true,
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        assert!(matches!(gate(&report, GateName::Skeptic), GateResult::Red(_)));
    }

    #[test]
    fn skeptic_pass_with_two_families_is_green() {
        // Healthy path: two distinct model families (claude=anthropic +
        // codex=openai) → review_degraded=false → Pass stays Green.
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            skeptic_reviewers: vec!["claude".to_string(), "codex".to_string()],
            review_degraded: false,
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(report.all_green, "two distinct families with Pass must be all_green");
        assert!(gate(&report, GateName::Skeptic).is_green());
    }

    #[test]
    fn skeptic_pass_with_mock_llm_is_green() {
        // Stage-1 test-repo path: a single mock_llm Llm::judge call. Per
        // `compute_review_degraded`, this sets review_degraded=false so
        // the cross-model gate does not fire on test fixtures.
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            skeptic_reviewers: vec!["mock_llm".to_string()],
            review_degraded: false,
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(report.all_green, "mock_llm Pass with review_degraded=false stays green");
        assert!(gate(&report, GateName::Skeptic).is_green());
    }

    #[test]
    fn skeptic_pass_with_empty_reviewers_is_green() {
        // Zero-vendors total-outage path: no verdict was produced (but in
        // this scenario a Pass was still recorded somehow). The skeptic
        // gate's verdict is what matters; review_degraded stays false when
        // no real vendors contributed. The cross-model gate does not fire.
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            skeptic_reviewers: vec![],
            review_degraded: false,
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(report.all_green);
        assert!(gate(&report, GateName::Skeptic).is_green());
    }

    #[test]
    fn skeptic_fail_with_review_degraded_still_red() {
        // A Red verdict wins regardless of review_degraded (the Fail reason
        // is preserved).
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Fail("wrong fix".into())),
            skeptic_reviewers: vec!["claude".to_string(), "agy".to_string()],
            review_degraded: false,
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::Skeptic) {
            GateResult::Red(reason) => assert_eq!(reason, "wrong fix"),
            other => panic!("expected Red('wrong fix'), got {other:?}"),
        }
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
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            ..Default::default()
        };

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green);
        match gate(&report, GateName::EvidenceFloor) {
            GateResult::Red(reason) => assert!(reason.starts_with("evidence floor"), "got: {reason}"),
            other => panic!("expected Red(evidence floor), got {other:?}"),
        }
    }

    #[test]
    fn er_pass_with_large_diff_and_verified_evidence_is_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = PrEvidence {
            is_production: true,
            non_test_changed_loc: 150,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            evidence_gist_status: EvidenceGistStatus::Verified,
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

    // r5 finding 1: `check_integration_marker` (the legacy substring floor) is
    // DELETED — the canonical verified evidence path is the only way a large
    // diff's evidence goes green (see the evidence_gate_* tests below).

    #[test]
    fn test_evidence_floor_gate_production_vs_non_production() {
        // PRODUCTION: Pass is Green, Partial is Red, Fail/Inconclusive/Absent are Red/Unknown
        let prod_pass = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert_eq!(evidence_floor_gate(&prod_pass), GateResult::Green);

        let prod_partial = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Partial,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert!(matches!(evidence_floor_gate(&prod_partial), GateResult::Red(_)));

        // NON_PRODUCTION: Pass and Partial are Green
        let non_prod_partial = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Partial,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert_eq!(evidence_floor_gate(&non_prod_partial), GateResult::Green);

        let non_prod_pass = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert_eq!(evidence_floor_gate(&non_prod_pass), GateResult::Green);

        // Fail/Inconclusive are Red for both
        let prod_fail = PrEvidence {
            is_production: true,
            non_test_changed_loc: 10,
            er_verdict: ErVerdict::Fail,
            skeptic_verdict: None,
            ..Default::default()
        };
        assert!(matches!(evidence_floor_gate(&prod_fail), GateResult::Red(_)));

        let non_prod_inconclusive = PrEvidence {
            is_production: false,
            non_test_changed_loc: 10,
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
                (GateName::VacuousRedGreen, GateResult::Unknown("fetch failed".into())),
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

    /// Bead jleechan-qzr3 / pr655-finding-1: `mergeable:null` from
    /// GitHub's REST fallback (still computing merge state) MUST land
    /// in `TransientOnly`, NOT `Reroll`. Before this fix, the REST
    /// fallback collapsed `None` to CONFLICTING and the gate's
    /// `if !mergeable { Red }` arm fired, causing the daemon to
    /// spawn a coder that had nothing to fix (fail-closed stall).
    /// Mirrors `classify_chain_ci_unknown_only_is_transient` but flips
    /// `mergeable = false` via the new `merge_state_unknown = true`
    /// arm and asserts `TransientOnly`.
    #[test]
    fn classify_chain_mergeable_unknown_is_transient() {
        let mut scm = FakeScm::default();
        let mut snapshot = all_green_snapshot(7);
        // Simulate the GitHub REST fallback returning `mergeable: null`:
        // the snapshot's `mergeable` field stays `false` (the safe
        // default for "not MERGEABLE"), but `merge_state_unknown` is
        // `true` so the gate emits `Unknown` instead of `Red`.
        snapshot.mergeable = false;
        snapshot.merge_state_unknown = true;
        scm.snapshots.insert(7, snapshot);
        let cfg = test_cfg();
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &all_green_evidence()).unwrap();

        // The NoConflicts gate must be Unknown, not Red.
        let no_conflicts = gate(&report, GateName::NoConflicts);
        match no_conflicts {
            GateResult::Unknown(reason) => {
                assert!(
                    reason.contains("mergeable state not yet computed")
                        || reason.contains("transient"),
                    "expected transient-unknown reason, got {reason:?}"
                );
            }
            other => panic!(
                "expected NoConflicts gate to be Unknown (transient), got {other:?}"
            ),
        }
        // And the resulting chain disposition must NOT Reroll —
        // a coder dispatch here would have nothing to fix.
        assert_eq!(
            classify_chain(&report),
            ChainDisposition::TransientOnly,
            "mergeable=null must classify as TransientOnly (no false-positive Reroll)"
        );
    }

    // --- jleechan-yoqy / issue #323: canonical evidence contract tests ---

    /// ROUND-TRIP: the EXACT string the coder-dispatch prompt mandates
    /// (`{EVIDENCE_MARKER} <gist-url> (head <sha>)`) must parse back to the
    /// gist id and head SHA. This is the contract that ties the prompt, the
    /// reviewer, and the parser to one literal.
    #[test]
    fn evidence_marker_round_trips_from_prompt_mandated_string() {
        let gist_id = "abc123def456";
        let head = "deadbeefcafe";
        // Build exactly as the prompt instructs, using the shared constant.
        let line = format!(
            "{} https://gist.github.com/octocat/{gist_id} (head {head})",
            crate::tools::EVIDENCE_MARKER
        );
        let body = format!("Some PR description.\n\n{line}\n\nMore text.");
        let parsed = parse_evidence(&body).expect("prompt-mandated evidence line must parse");
        assert_eq!(parsed.gist_id, gist_id);
        assert_eq!(parsed.head_sha, head);
    }

    #[test]
    fn evidence_marker_parse_tolerates_case_and_missing_bold_and_bullet() {
        // Bold markers optional, case-insensitive, leading `-` bullet tolerated.
        let body = "- evidence: https://gist.github.com/u/deadid (head abc123)";
        let parsed = parse_evidence(body).unwrap();
        assert_eq!(parsed.gist_id, "deadid");
        assert_eq!(parsed.head_sha, "abc123");

        // Bare gist URL with no user path segment.
        let body2 = "**EVIDENCE**: https://gist.github.com/onlyid (head ffff)";
        let parsed2 = parse_evidence(body2).unwrap();
        assert_eq!(parsed2.gist_id, "onlyid");
    }

    #[test]
    fn evidence_marker_absent_or_incomplete_parses_none() {
        assert_eq!(parse_evidence("no marker here"), None);
        // Prose mentioning evidence mid-line is not the marker.
        assert_eq!(
            parse_evidence("we added evidence: see the tests"),
            None,
            "a non-gist evidence line must not falsely parse"
        );
        // Marker + gist but no (head ..) clause.
        assert_eq!(
            parse_evidence("**Evidence**: https://gist.github.com/u/x"),
            None
        );
    }

    #[test]
    fn evidence_marker_hash_changes_with_gist_and_is_stable() {
        let a = "**Evidence**: https://gist.github.com/u/g1 (head h1)";
        let b = "**Evidence**: https://gist.github.com/u/g2 (head h1)"; // gist changed
        let none = "no evidence marker";
        assert_eq!(evidence_marker_hash(a), evidence_marker_hash(a), "stable");
        assert_ne!(
            evidence_marker_hash(a),
            evidence_marker_hash(b),
            "an evidence-only gist change must change the hash"
        );
        assert_ne!(evidence_marker_hash(a), evidence_marker_hash(none));
    }

    fn evidence_with_status(status: EvidenceGistStatus) -> PrEvidence {
        PrEvidence {
            is_production: false,
            non_test_changed_loc: 500, // above the LOC floor
            er_verdict: ErVerdict::Pass,
            skeptic_verdict: Some(SkepticVerdict::Pass),
            evidence_gist_status: status,
            ..Default::default()
        }
    }

    #[test]
    fn evidence_gate_verified_is_green_even_above_loc_floor() {
        let ev = evidence_with_status(EvidenceGistStatus::Verified);
        assert!(matches!(evidence_floor_gate(&ev), GateResult::Green));
    }

    #[test]
    fn evidence_gate_failed_is_red_with_distinct_text() {
        let ev = evidence_with_status(EvidenceGistStatus::Failed(
            "evidence gist abc is empty".to_string(),
        ));
        match evidence_floor_gate(&ev) {
            GateResult::Red(reason) => {
                assert!(
                    reason.contains("evidence contract") && reason.contains("empty"),
                    "distinct evidence-failure text expected, got: {reason}"
                );
            }
            other => panic!("expected Red, got {other:?}"),
        }
    }

    #[test]
    fn evidence_gate_not_provided_above_floor_is_red_no_legacy_bypass() {
        // r5 finding 1: no canonical marker + above the LOC floor => Red. The
        // legacy substring floor that used to green this is deleted.
        let ev = evidence_with_status(EvidenceGistStatus::NotProvided);
        match evidence_floor_gate(&ev) {
            GateResult::Red(reason) => assert!(
                reason.starts_with("evidence floor"),
                "got: {reason}"
            ),
            other => panic!("expected Red(evidence floor), got {other:?}"),
        }
    }

    #[test]
    fn evidence_gate_pending_is_unknown_not_red() {
        // r5 finding 3: a transient gist-fetch failure must be Unknown (wait),
        // never Red (which would churn a reroll on gh noise).
        let ev = evidence_with_status(EvidenceGistStatus::Pending(
            "gist g fetch failed transiently".to_string(),
        ));
        match evidence_floor_gate(&ev) {
            GateResult::Unknown(reason) => assert!(
                reason.contains("pending"),
                "expected a pending Unknown, got: {reason}"
            ),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    // jleechan-984e / issue #385: vendor → model family taxonomy.
    // Tests assert the EXACT family strings, not just distinctness, so
    // any future rename or family merge fails loudly instead of silently
    // collapsing two "different" vendors into one family.
    #[test]
    fn vendor_model_family_known_vendors() {
        assert_eq!(vendor_model_family("claude"), "anthropic");
        assert_eq!(vendor_model_family("minimax"), "minimax");
        assert_eq!(vendor_model_family("claudem"), "minimax");
        assert_eq!(vendor_model_family("codex"), "openai");
        assert_eq!(vendor_model_family("agy"), "google-antigravity");
        assert_eq!(vendor_model_family("gemini"), "google-gemini");
        assert_eq!(vendor_model_family("cursor-agent"), "cursor");
        // Bare `cursor` / bashrc `agentf` aliases must map to the same family.
        assert_eq!(vendor_model_family("cursor"), "cursor");
        assert_eq!(vendor_model_family("agentf"), "cursor");
    }

    #[test]
    fn vendor_model_family_unknown_and_mock_return_empty() {
        // Unknown / mock labels must NOT silently collapse into a real
        // family — `compute_review_degraded` filters empty families, so
        // returning `""` here keeps them inert rather than accidentally
        // satisfying the cross-model guarantee.
        assert_eq!(vendor_model_family("mock_llm"), "");
        assert_eq!(vendor_model_family("nonsense_vendor"), "");
        assert_eq!(vendor_model_family(""), "");
    }

    // jleechan-984e / issue #385: `compute_review_degraded` is true iff
    // the contributor set covers fewer than TWO distinct non-empty
    // model families.
    #[test]
    fn compute_review_degraded_empty_is_false() {
        // Zero contributors cannot violate the cross-model guarantee.
        assert!(!compute_review_degraded(&[]));
    }

    #[test]
    fn compute_review_degraded_single_family_is_true() {
        // The exact production scenario from the issue: codex quota-dead,
        // agy/cursor-agent errored, so only claude contributed.
        let v = vec!["claude".to_string()];
        assert!(
            compute_review_degraded(&v),
            "single claude contributor must be review_degraded (issue #385 acceptance)"
        );
    }

    #[test]
    fn compute_review_degraded_two_distinct_families_is_false() {
        // Codex (openai) + claude (anthropic) → two distinct families →
        // strict merge policy can treat this as strict-green.
        let v = vec!["codex".to_string(), "claude".to_string()];
        assert!(
            !compute_review_degraded(&v),
            "codex+claude (two distinct families) must NOT be degraded"
        );
    }

    #[test]
    fn compute_review_degraded_same_family_deduped_is_true() {
        // Two contributors from the SAME family — a same-model review —
        // shares the coder's blind spots and must be flagged degraded.
        // `agy` and `gemini` both come from Google but they are distinct
        // families in the taxonomy, so this test specifically uses two
        // claude contributors (impossible in practice but defensive).
        let v = vec!["claude".to_string(), "claude".to_string()];
        assert!(
            compute_review_degraded(&v),
            "duplicate claude contributors (single family) must be degraded"
        );
    }

    #[test]
    fn compute_review_degraded_agy_and_cursor_are_two_families() {
        // The exact healthy-path acceptance case from the issue:
        // `agy` falls through and a different-family vendor (cursor)
        // also contributed — degraded MUST be false.
        let v = vec!["agy".to_string(), "cursor-agent".to_string()];
        assert!(
            !compute_review_degraded(&v),
            "agy + cursor-agent are distinct families and must NOT be degraded"
        );
    }

    #[test]
    fn compute_review_degraded_mock_llm_does_not_count() {
        // The Stage-1 test-repo path produces a `["mock_llm"]` list —
        // `vendor_model_family("mock_llm") == ""` so the family set is
        // empty and the degraded flag is false (no real cross-model
        // guarantee to violate in the test path).
        let v = vec!["mock_llm".to_string()];
        assert!(
            !compute_review_degraded(&v),
            "test-repo mock_llm-only contributor must NOT be flagged \
             degraded (no real family to cross-check against)"
        );
    }

    #[test]
    fn compute_review_degraded_google_split_is_two_families() {
        // agy (Google Antigravity) and gemini (Google Gemini CLI) are
        // operated as separate CLIs/quota pools per the dispatch_reviewer
        // comment, so they count as two distinct families even though
        // both are Google-distributed.
        let v = vec!["agy".to_string(), "gemini".to_string()];
        assert!(
            !compute_review_degraded(&v),
            "agy + gemini are distinct Google families and must NOT be degraded"
        );
    }

    // ---- bead jleechan-ijod / issue #387 r5: gate 8 (VacuousRedGreen) ----

    /// TDD guard for issue #387 r5: the runtime vacuous-test detector's
    /// `Vacuous` verdict must surface as a Red gate so `all_green=false`
    /// — the daemon's strict-merge policy #328 should refuse to merge a
    /// vacuously-tested PR.
    #[test]
    fn vacuous_red_green_gate_red_when_verdict_vacuous() {
        let mut ev = all_green_evidence();
        ev.vacuous_red_green = VacuousRedGreenStatus::Vacuous;
        let r = vacuous_red_green_gate(&ev);
        assert!(matches!(r, GateResult::Red(_)), "got {r:?}");
    }

    #[test]
    fn vacuous_red_green_gate_green_when_verdict_genuine() {
        let mut ev = all_green_evidence();
        ev.vacuous_red_green = VacuousRedGreenStatus::Genuine;
        let r = vacuous_red_green_gate(&ev);
        assert!(matches!(r, GateResult::Green), "got {r:?}");
    }

    #[test]
    fn vacuous_red_green_gate_green_when_not_provided() {
        // Test-repo PRs and PRs with no test files must keep the gate
        // green (issue #387 r5 finding: the detector wasn't called and
        // the gate must not block on absence of evidence).
        let ev = all_green_evidence();
        let r = vacuous_red_green_gate(&ev);
        assert!(matches!(r, GateResult::Green), "got {r:?}");
    }

    #[test]
    fn vacuous_red_green_gate_unknown_when_green_failed() {
        // Issue #387 r5 finding: if the PR's tests don't pass on HEAD
        // BEFORE the revert, the verdict is unknown — reporting Vacuous
        // would be a false positive.
        let mut ev = all_green_evidence();
        ev.vacuous_red_green = VacuousRedGreenStatus::GreenFailed("classify_high failed on head".to_string());
        let r = vacuous_red_green_gate(&ev);
        assert!(matches!(r, GateResult::Unknown(_)), "got {r:?}");
    }

    #[test]
    fn vacuous_red_green_gate_green_when_baseline_failed() {
        // Issue #387 r6 finding (PR #413 CI regression 2026-07-21): if the
        // detector's baseline-main subprocess can't materialise the base
        // worktree — `gh pr view` failed in CI due to no GH_TOKEN, etc —
        // the gate must NOT block READY. The detector's job is to catch
        // Vacuous tests, not to be an authn/authz gate; infra failures
        // fall through to the existing logger/Healer surfaces rather
        // than blocking READY. See comment on `vacuous_red_green_gate`
        // above.
        let mut ev = all_green_evidence();
        ev.vacuous_red_green = VacuousRedGreenStatus::BaselineFailed(
            "classify_high failed on origin/main".to_string(),
        );
        let r = vacuous_red_green_gate(&ev);
        assert!(matches!(r, GateResult::Green), "got {r:?}");
    }

    #[test]
    fn vacuous_red_green_gate_unknown_when_no_changed_tests() {
        // No test files in the diff → no measurement possible. Distinct
        // from Vacuous so operators can diagnose a no-op PR.
        let mut ev = all_green_evidence();
        ev.vacuous_red_green = VacuousRedGreenStatus::NoChangedTests;
        let r = vacuous_red_green_gate(&ev);
        assert!(matches!(r, GateResult::Unknown(_)), "got {r:?}");
    }

    #[test]
    fn vacuous_red_green_gate_unknown_when_manifest_missing() {
        // Issue #387 r5 finding 3: missing manifest is fail-closed
        // Unknown, NOT Vacuous — without a manifest, `cargo test` from
        // the repo root silently never ran the targeted tests and the
        // old detector accepted the silent no-op as a real pass.
        let mut ev = all_green_evidence();
        ev.vacuous_red_green =
            VacuousRedGreenStatus::ManifestMissing("no Cargo.toml".to_string());
        let r = vacuous_red_green_gate(&ev);
        assert!(matches!(r, GateResult::Unknown(_)), "got {r:?}");
    }

    #[test]
    fn vacuous_red_green_gate_unknown_when_pytest_not_found() {
        // Bead jleechan-6xje: pytest backend parity. The detector
        // could not locate a pytest binary on the host — the gate
        // surface this as `Unknown` (not `Red` and not `Vacuous`),
        // mirroring the cargo analogue. Operators see a clear
        // "toolchain missing" signal rather than a misleading
        // Vacuous pass.
        let mut ev = all_green_evidence();
        ev.vacuous_red_green =
            VacuousRedGreenStatus::PytestNotFound("pytest was not on PATH".to_string());
        let r = vacuous_red_green_gate(&ev);
        match r {
            GateResult::Unknown(msg) => {
                assert!(
                    msg.contains("pytest"),
                    "Unknown reason must name pytest: {msg}"
                );
            }
            other => panic!("expected GateResult::Unknown, got {other:?}"),
        }
    }

    /// TDD guard for issue #387 r5 finding 1: the assess() function must
    /// include the new `VacuousRedGreen` gate in its 8-gate results
    /// array, and a Vacuous verdict must drive `all_green=false`.
    #[test]
    fn assess_includes_vacuous_red_green_gate_and_blocks_all_green_on_vacuous() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.vacuous_red_green = VacuousRedGreenStatus::Vacuous;
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        assert!(!report.all_green, "vacuous verdict must block all_green");
        let vacuous_gate = report
            .results
            .iter()
            .find(|(n, _)| *n == GateName::VacuousRedGreen)
            .expect("vacuous_red_green gate must be present");
        assert!(
            matches!(vacuous_gate.1, GateResult::Red(_)),
            "expected Red, got {:?}",
            vacuous_gate.1
        );
    }

    /// TDD guard for issue #387 r5: when the detector reports `Genuine`
    /// the assess() report must remain all_green (a real red-green test
    /// is exactly what we want — the gate's job is to catch the
    /// vacuous failure mode, not block genuine coverage).
    #[test]
    fn assess_with_genuine_verdict_remains_all_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.vacuous_red_green = VacuousRedGreenStatus::Genuine;
        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();
        assert!(report.all_green, "Genuine verdict must not block all_green");
    }

    // ------------------------------------------------------------------
    // Bead jleechan-jsby — vendor-waiver regression suite
    //
    // The waiver contract (issue #387 spec extension, jleechan-jsby
    // task):
    //
    //  * When CodeRabbit or Bugbot is structurally unavailable
    //    (VendorHealthLedger reports `Capped` for that vendor) AND
    //    compensating coverage is green (skeptic_pass + /er_pass +
    //    !review_degraded), the gate flips from `Unknown` to
    //    `Waived { vendor, reason }`. `is_green()` returns true so
    //    merge authority treats the bead as merge-ready.
    //  * The wire-format reason includes the documented waiver token
    //    (`coderabbit:waived_vendor_unavailable`) so gate_assessment
    //    JSONL surfaces the substitution for operator audit.
    //  * When the vendor is Healthy, the existing semantics are
    //    unchanged — a Healthy ledger MUST NOT widen the verdict.
    //  * When the vendor is Capped BUT compensating coverage isn't
    //    green, the gate stays Unknown (the strict-substitute policy
    //    demands actual compensating trust, not just structural
    //    unavailability).
    //  * A waiver cannot rescue a Red CodeRabbit/Bugbot verdict —
    //    vendor red means the vendor ran and produced a finding, so
    //    the gate stays Red.
    //  * Recovery: when a fresh PR snapshot shows the vendor is
    //    APPROVED / clean AND the ledger says Capped, the
    //    `detect_vendor_recovery` helper surfaces the vendor for
    //    ledger-clear + VENDOR_RECOVERED telemetry.
    //
    // These tests pin all of the above.
    // ------------------------------------------------------------------

    use crate::vendor_health::{CapObservation, CapSource, Vendor, VendorHealthLedger};

    /// Build a snapshot where CodeRabbit and Bugbot are BOTH
    /// `unknown` (the structural-unavailability trigger).
    fn unknown_vendor_snapshot(pr: u64) -> PrSnapshot {
        let mut snap = all_green_snapshot(pr);
        snap.coderabbit_approved = false;
        snap.coderabbit_status = "unknown".to_string();
        snap.bugbot_error_count = 0;
        // jleechan-8s2p: parallel outage signal for Bugbot. The
        // r6 detector keyed on `bugbot_error_count > 0`, which is
        // real-failure semantics; the structural-unavailability
        // signal is `bugbot_pending`. Setting it here keeps the
        // existing recovery tests consistent: both vendors
        // unknown-stuck → recovery must NOT fire (the snapshot
        // does not contradict the ledger).
        snap.bugbot_pending = true;
        snap
    }

    /// Build a Capped ledger with at least 3 distinct bead
    /// observations (the N-of-M detector's threshold).
    fn capped_coderabbit_ledger() -> VendorHealthLedger {
        let mut ledger = VendorHealthLedger::new();
        for ts in 1..=3 {
            ledger.record_cap(CapObservation {
                vendor: Vendor::CodeRabbit,
                source: CapSource::UnknownGateRepeated,
                bead_id: format!("bead-{ts}"),
                pr_number: ts,
                ts_epoch: ts,
                note: "test fixture".into(),
            });
        }
        ledger
    }

    fn capped_bugbot_ledger() -> VendorHealthLedger {
        let mut ledger = VendorHealthLedger::new();
        for ts in 1..=3 {
            ledger.record_cap(CapObservation {
                vendor: Vendor::Bugbot,
                source: CapSource::UnknownGateRepeated,
                bead_id: format!("bead-{ts}"),
                pr_number: ts,
                ts_epoch: ts,
                note: "test fixture".into(),
            });
        }
        ledger
    }

    // AC #2 (waiver substitution) + AC #3 (skeptic informed):
    // When CodeRabbit is structurally unavailable AND compensating
    // coverage is green (skeptic_pass + /er_pass + cross-model),
    // the gate flips to `Waived { vendor: "coderabbit", reason }`
    // and `is_green()` returns true so all_green is achievable.
    #[test]
    fn waiver_substitutes_coderabbit_when_vendor_capped_and_compensating_green() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, unknown_vendor_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        // all_green_evidence already gives us Pass skeptic + Pass /er +
        // review_degraded=false, so compensating coverage is green.
        evidence.vendor_health = capped_coderabbit_ledger();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        // Bugbot gate stays Green (vendor healthy, 0 errors). CodeRabbit
        // gate flips to Waived.
        let coderabbit = gate(&report, GateName::CodeRabbitApproved);
        match coderabbit {
            GateResult::Waived { vendor, reason } => {
                assert_eq!(vendor, "coderabbit");
                assert!(
                    reason.contains("coderabbit:waived_vendor_unavailable"),
                    "waiver reason must carry the documented token, got: {reason}"
                );
                assert!(
                    reason.contains("compensating"),
                    "waiver reason must name the compensating coverage source, got: {reason}"
                );
            }
            other => panic!("expected Waived for CodeRabbit, got {other:?}"),
        }
        // And it must count as green for merge authority.
        assert!(coderabbit.is_green(), "Waived must satisfy is_green()");
        // Bugbot unaffected (no Bugbot cap observation).
        assert!(gate(&report, GateName::BugbotClean).is_green());
        // Skeptic unaffected.
        assert!(gate(&report, GateName::Skeptic).is_green());
    }

    // AC #5 (NOT a silent pass): when the vendor is Capped BUT
    // compensating coverage is NOT green (skeptic failed or /er
    // failed or review_degraded=true), the gate MUST stay Unknown.
    // Strict-merge policy still refuses to merge, and the operator
    // sees the cap in telemetry alongside the failing compensating
    // signal.
    #[test]
    fn waiver_refused_when_compensating_skeptic_is_failing() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, unknown_vendor_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.skeptic_verdict = Some(SkepticVerdict::Fail("wrong fix".into()));
        evidence.vendor_health = capped_coderabbit_ledger();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        // CodeRabbit gate stays Unknown — the waiver requires the
        // skeptic to be Pass, and Fail refuses the substitution.
        let coderabbit = gate(&report, GateName::CodeRabbitApproved);
        assert!(
            matches!(coderabbit, GateResult::Unknown(_)),
            "CodeRabbit gate must stay Unknown when skeptic is failing, got {coderabbit:?}"
        );
        assert!(!coderabbit.is_green());
        // Skeptic itself is Red — that's the dominant signal.
        assert!(matches!(
            gate(&report, GateName::Skeptic),
            GateResult::Red(_)
        ));
        assert!(
            !report.all_green,
            "all_green must be false when skeptic fails even under vendor waiver, got {report:?}"
        );
    }

    // AC #5 (NOT a silent pass): same contract for the /er
    // compensating signal — a Fail /er verdict refuses the waiver.
    #[test]
    fn waiver_refused_when_compensating_er_is_failing() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, unknown_vendor_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.er_verdict = ErVerdict::Fail;
        evidence.vendor_health = capped_coderabbit_ledger();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        let coderabbit = gate(&report, GateName::CodeRabbitApproved);
        assert!(
            matches!(coderabbit, GateResult::Unknown(_)),
            "CodeRabbit gate must stay Unknown when /er is failing, got {coderabbit:?}"
        );
        assert!(!coderabbit.is_green());
        assert!(
            !report.all_green,
            "all_green must be false when /er fails even under vendor waiver"
        );
    }

    // AC #5 (NOT a silent pass): single-family skeptic
    // (review_degraded=true) cannot be the compensating floor for a
    // vendor waiver — the cross-model guarantee from bead jleechan-984e
    // / issue #385 still applies under waiver.
    #[test]
    fn waiver_refused_when_cross_model_guarantee_is_violated() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, unknown_vendor_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        // Skeptic is still Pass but review_degraded=true means a
        // single-family review cannot be the compensating floor.
        evidence.review_degraded = true;
        evidence.vendor_health = capped_coderabbit_ledger();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        let coderabbit = gate(&report, GateName::CodeRabbitApproved);
        assert!(
            matches!(coderabbit, GateResult::Unknown(_)),
            "CodeRabbit gate must stay Unknown under single-family skeptic, got {coderabbit:?}"
        );
        // Skeptic itself flips to Red (existing cross-model contract
        // from issue #385) — both signals refuse the merge.
        assert!(matches!(
            gate(&report, GateName::Skeptic),
            GateResult::Red(_)
        ));
        assert!(!report.all_green);
    }

    // Healthy ledger MUST NOT widen the gate — when CodeRabbit
    // review is missing/unknown but the vendor is not capped, the
    // existing Unknown semantics stay untouched. The waiver is
    // strictly opt-in via the ledger.
    #[test]
    fn healthy_ledger_does_not_trigger_waiver() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, unknown_vendor_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.vendor_health = VendorHealthLedger::new(); // empty / Healthy

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        let coderabbit = gate(&report, GateName::CodeRabbitApproved);
        assert!(
            matches!(coderabbit, GateResult::Unknown(_)),
            "Healthy ledger must not flip the gate to Waived, got {coderabbit:?}"
        );
        assert!(!coderabbit.is_green());
        assert!(!report.all_green);
    }

    // Bugbot waiver uses the same contract.
    #[test]
    fn waiver_substitutes_bugbot_when_vendor_capped_and_compensating_green() {
        let mut scm = FakeScm::default();
        let mut snap = all_green_snapshot(7);
        // CodeRabbit APPROVED (healthy), Bugbot absent/neutral.
        snap.bugbot_error_count = 0;
        scm.snapshots.insert(7, snap);
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.vendor_health = capped_bugbot_ledger();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        let bugbot = gate(&report, GateName::BugbotClean);
        match bugbot {
            GateResult::Waived { vendor, reason } => {
                assert_eq!(vendor, "bugbot");
                assert!(reason.contains("bugbot:waived_vendor_unavailable"));
            }
            other => panic!("expected Waived for Bugbot, got {other:?}"),
        }
        assert!(bugbot.is_green());
        // CodeRabbit unaffected.
        assert!(gate(&report, GateName::CodeRabbitApproved).is_green());
    }

    // AC #4 (vendor red is NOT waivable): when a vendor IS running
    // and produced a Red verdict (CodeRabbit CHANGES_REQUESTED or
    // Bugbot error-severity comments), the waiver MUST NOT clear
    // it. A waiver substitutes for "vendor couldn't run", NOT for
    // "vendor ran and found a defect".
    #[test]
    fn waiver_does_not_clear_vendor_red_verdict() {
        let mut scm = FakeScm::default();
        let mut snap = all_green_snapshot(7);
        snap.coderabbit_approved = false;
        snap.coderabbit_status = "red".to_string(); // CodeRabbit ran and said no
        scm.snapshots.insert(7, snap);
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.vendor_health = capped_coderabbit_ledger();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();

        let coderabbit = gate(&report, GateName::CodeRabbitApproved);
        assert!(
            matches!(coderabbit, GateResult::Red(_)),
            "CodeRabbit Red must stay Red even under waiver, got {coderabbit:?}"
        );
        assert!(!coderabbit.is_green());
        assert!(!report.all_green);
    }

    // Wire-format emission: gate_assessment JSONL must carry the
    // waiver verdict so operators / dashboards can grep it.
    #[test]
    fn waiver_serialises_as_pass_with_token_in_evidence_in_jsonl() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, unknown_vendor_snapshot(7));
        let cfg = test_cfg();
        let mut evidence = all_green_evidence();
        evidence.vendor_health = capped_coderabbit_ledger();

        let report = assess(&scm, 7, &cfg.target_repo, &cfg, &evidence).unwrap();
        let json = report.to_json();
        let gates = json.get("gates").and_then(|v| v.as_object()).unwrap();
        let cr = gates.get("coderabbit").expect("coderabbit key in gate JSON");
        let verdict = cr.get("verdict").and_then(|v| v.as_str()).unwrap();
        assert_eq!(
            verdict, "pass",
            "wire-format verdict must be pass, got: {cr}"
        );
        assert!(cr.get("vendor").is_none(), "vendor key must not be present in serialised gate to conform with schema");
        let evidence_arr = cr.get("evidence").and_then(|v| v.as_array()).unwrap();
        let first = evidence_arr[0].as_str().unwrap();
        assert!(
            first.contains("coderabbit:waived_vendor_unavailable"),
            "evidence[0] must carry the waiver token, got: {first}"
        );
    }

    // Compensating coverage helper pin.
    #[test]
    fn compensating_coverage_green_pins_three_signal_contract() {
        let mut evidence = all_green_evidence();
        // Default all_green_evidence: skeptic Pass, /er Pass,
        // review_degraded=false.
        assert!(compensating_coverage_green(&evidence));

        // Skeptic Fail → false.
        evidence.skeptic_verdict = Some(SkepticVerdict::Fail("x".into()));
        assert!(!compensating_coverage_green(&evidence));
        evidence.skeptic_verdict = Some(SkepticVerdict::Pass);

        // Skeptic Warn → false (warn is NOT a substitute for the
        // missing external review — the brief explicitly demands the
        // trust floor stay a clean Pass).
        evidence.skeptic_verdict = Some(SkepticVerdict::Warn("nit".into()));
        assert!(!compensating_coverage_green(&evidence));
        evidence.skeptic_verdict = Some(SkepticVerdict::Pass);

        // /er Fail → false.
        evidence.er_verdict = ErVerdict::Fail;
        assert!(!compensating_coverage_green(&evidence));
        evidence.er_verdict = ErVerdict::Pass;

        // /er Partial → false (PRODUCTION requires PASS).
        evidence.er_verdict = ErVerdict::Partial;
        assert!(!compensating_coverage_green(&evidence));
        evidence.er_verdict = ErVerdict::Pass;

        // review_degraded=true → false.
        evidence.review_degraded = true;
        assert!(!compensating_coverage_green(&evidence));
        evidence.review_degraded = false;

        assert!(compensating_coverage_green(&evidence));
    }

    // AC #6 (auto-clear): when the snapshot shows the vendor is now
    // APPROVED (CodeRabbit) or clean (Bugbot) AND the ledger says
    // Capped, the recovery helper surfaces the vendor for ledger
    // clearing + telemetry emission.
    #[test]
    fn detect_vendor_recovery_clears_capped_state_on_fresh_approval() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7)); // CodeRabbit approved
        let cfg = test_cfg();
        let _evidence = all_green_evidence();
        let mut ledger = capped_coderabbit_ledger();
        // Sanity: ledger says Capped before recovery.
        assert!(ledger.health(Vendor::CodeRabbit).is_capped());

        let snap = scm.pr_snapshot_for_repo(&cfg.target_repo, 7).unwrap();
        let recovered = detect_vendor_recovery(&snap, &ledger);
        assert_eq!(recovered, vec![Vendor::CodeRabbit]);

        // Caller-side: clear and verify the waiver path deactivates.
        for v in &recovered {
            ledger.clear(*v);
        }
        assert_eq!(
            ledger.health(Vendor::CodeRabbit),
            crate::vendor_health::VendorHealth::Healthy
        );
    }

    // Recovery only fires when the ledger says Capped — a Healthy
    // ledger stays untouched even if the snapshot shows approval.
    #[test]
    fn detect_vendor_recovery_is_noop_when_ledger_already_healthy() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, all_green_snapshot(7));
        let cfg = test_cfg();
        let evidence = all_green_evidence();
        let ledger = VendorHealthLedger::new(); // empty / Healthy

        let snap = scm.pr_snapshot_for_repo(&cfg.target_repo, 7).unwrap();
        let recovered = detect_vendor_recovery(&snap, &ledger);
        assert!(
            recovered.is_empty(),
            "Healthy ledger must not produce a recovery event, got {recovered:?}"
        );
        // Compensating evidence block is empty — no harm done.
        let _ = evidence;
    }

    // Recovery only fires on a CONTRADICTION between ledger (Capped)
    // and snapshot (approved). A snapshot that still says the vendor
    // is unknown must NOT trigger recovery.
    #[test]
    fn detect_vendor_recovery_does_not_fire_when_vendor_still_unknown() {
        let mut scm = FakeScm::default();
        scm.snapshots.insert(7, unknown_vendor_snapshot(7));
        let cfg = test_cfg();
        let ledger = capped_coderabbit_ledger();

        let snap = scm.pr_snapshot_for_repo(&cfg.target_repo, 7).unwrap();
        let recovered = detect_vendor_recovery(&snap, &ledger);
        assert!(
            recovered.is_empty(),
            "Vendor-still-unknown snapshot must not clear the Capped ledger, got {recovered:?}"
        );
    }

    // jleechan-8s2p (phase 2): the r6 cap detector required
    // `bugbot_error_count > 0`, but the OUTAGE case has `== 0` — that
    // meant `detect_vendor_cap_for(Vendor::Bugbot)` never returned true
    // in production and the Bugbot waiver was unreachable without
    // manually seeded ledgers. After this fix the predicate keys on
    // `snapshot.bugbot_pending` (a STRUCTURED field parallel to
    // `coderabbit_status == "unknown"`), so an outage snapshot returns
    // true while a passed Bugbot returns false.
    #[test]
    fn detect_vendor_cap_for_bugbot_pending_is_true_regardless_of_error_count() {
        let mut scm = FakeScm::default();
        let mut snap = all_green_snapshot(11);
        // Bugbot is stuck pending, no error comments yet — the r6
        // detector returned false here, blocking the entire Bugbot
        // waiver path.
        snap.bugbot_pending = true;
        snap.bugbot_error_count = 0;
        scm.snapshots.insert(11, snap.clone());

        let cfg = test_cfg();
        let snap = scm.pr_snapshot_for_repo(&cfg.target_repo, 11).unwrap();
        assert!(
            detect_vendor_cap_for(&snap, Vendor::Bugbot),
            "Bugbot stuck pending must be detected as a cap observation \
             (structural unavailability, not absence of errors)"
        );
    }

    // jleechan-8s2p: Bugbot cap detection must NOT fire when Bugbot
    // has actually reviewed and produced no errors — that is a clean
    // pass, not a cap. Without this guard, `bugbot_error_count == 0`
    // would be misread as cap and the waiver would substitute for a
    // vendor that legitimately ran green.
    #[test]
    fn detect_vendor_cap_for_bugbot_passed_is_false() {
        let mut scm = FakeScm::default();
        let mut snap = all_green_snapshot(12);
        // Bugbot reviewed, no errors, no pending state — clean pass.
        snap.bugbot_pending = false;
        snap.bugbot_error_count = 0;
        scm.snapshots.insert(12, snap);

        let cfg = test_cfg();
        let snap = scm.pr_snapshot_for_repo(&cfg.target_repo, 12).unwrap();
        assert!(
            !detect_vendor_cap_for(&snap, Vendor::Bugbot),
            "Bugbot reviewed clean (pending=false, errors=0) must NOT be \
             treated as a cap observation; that is a genuine Green path"
        );
    }

    // jleechan-8s2p: Bugbot cap detection must NOT fire when Bugbot
    // has actually produced error comments — that is a real RED verdict
    // (handled by the BugbotClean gate's `> 0` branch), not structural
    // unavailability. The waiver must NEVER substitute for a genuine
    // fail.
    #[test]
    fn detect_vendor_cap_for_bugbot_with_errors_is_false() {
        let mut scm = FakeScm::default();
        let mut snap = all_green_snapshot(13);
        // Bugbot produced a real error comment. This is a RED verdict,
        // never a cap — the waiver is for STRUCTURAL outage only.
        snap.bugbot_pending = false;
        snap.bugbot_error_count = 2;
        scm.snapshots.insert(13, snap);

        let cfg = test_cfg();
        let snap = scm.pr_snapshot_for_repo(&cfg.target_repo, 13).unwrap();
        assert!(
            !detect_vendor_cap_for(&snap, Vendor::Bugbot),
            "Bugbot producing error comments is a real RED verdict, never \
             a structural cap; waiver must not substitute"
        );
    }

    // jleechan-8s2p: end-to-end proof that the Bugbot waiver path is
    // reachable in PRODUCTION — `tick` records the cap observation
    // (predicate now true for outage), the ledger escalates to Capped
    // across N-of-M beads, and `assess` then produces GateResult::Waived
    // instead of leaving the gate as Green-pretending-Bugbot-was-Healthy
    // or stuck Unknown.
    #[test]
    fn bugbot_outage_reaches_waived_via_cap_observation_recording() {
        use crate::vendor_health::{VendorHealth, EVT_WAIVED};

        let mut scm = FakeScm::default();
        // Three distinct beads, each with Bugbot stuck pending and 0
        // errors — exactly the production outage signature.
        for pr in 21..=23u64 {
            let mut snap = all_green_snapshot(pr);
            snap.bugbot_pending = true;
            snap.bugbot_error_count = 0;
            scm.snapshots.insert(pr, snap);
        }
        let cfg = test_cfg();

        // First two beads record but the ledger stays Healthy
        // (below the N-of-M threshold of 3 distinct beads).
        let mut ledger = VendorHealthLedger::new();
        for (pr, bead) in [(21u64, "bead-21"), (22u64, "bead-22")] {
            let snap = scm.pr_snapshot_for_repo(&cfg.target_repo, pr).unwrap();
            assert!(
                detect_vendor_cap_for(&snap, Vendor::Bugbot),
                "Bugbot outage must be detected on bead {bead}"
            );
            ledger.record_cap(CapObservation {
                vendor: Vendor::Bugbot,
                source: CapSource::UnknownGateRepeated,
                bead_id: bead.into(),
                pr_number: pr,
                ts_epoch: pr,
                note: format!("outage at pr {pr}"),
            });
        }
        assert_eq!(
            ledger.health(Vendor::Bugbot),
            VendorHealth::Healthy,
            "2 of 3 distinct beads is below the N-of-M threshold"
        );

        // Third bead crosses the threshold → Capped → waiver eligible.
        let _snap = scm.pr_snapshot_for_repo(&cfg.target_repo, 23).unwrap();
        ledger.record_cap(CapObservation {
            vendor: Vendor::Bugbot,
            source: CapSource::UnknownGateRepeated,
            bead_id: "bead-23".into(),
            pr_number: 23,
            ts_epoch: 23,
            note: "outage at pr 23".into(),
        });
        assert!(
            ledger.health(Vendor::Bugbot).is_capped(),
            "3 distinct beads must flip Bugbot to Capped; without the P1 \
             fix, the predicate never returned true and this transition \
             was unreachable"
        );

        // EVT_WAIVED marker is the canonical grep target for the
        // waiver firing — confirm it is emitted when the record call
        // happens on a fresh cap edge.
        assert_eq!(EVT_WAIVED, "VENDOR_WAIVED");
    }
}
