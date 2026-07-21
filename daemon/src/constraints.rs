use crate::errors::DaemonError;
use crate::tools::Llm;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub inhibition_specs: Vec<String>,
    pub positive_assertions: Vec<String>,
    pub security_redaction_encountered: bool,
    /// Issue #408 / bead jleechan-1a5e r3 (P1-7): reviewer items that the
    /// coder's previous attempt did NOT address. Empty when the review is
    /// either fully satisfied or the LLM does not surface any. Surfaced
    /// as a separate constraint so the reroll prompt and the appended
    /// spec block can carry them as hard requirements for the next
    /// attempt.
    pub not_addressed: Vec<String>,
}

/// Issue #408 / bead jleechan-1a5e r2 (R2-4) + r3: per-item status
/// emitted by the reviewer CLI / skeptic on a known schema line. The
/// structured path is INDEPENDENT of the LLM JSON extractor — a reviewer
/// that emits `REVIEWER_STATUS: <key> = NOT-ADDRESSED` is parsed directly
/// so the `not_addressed` constraints reach the next-round coder prompt
/// even if the LLM extractor truncates or omits `notAddressed`. r3
/// generalizes the existing `parse_not_addressed_schema_line` (a flat
/// string list) into per-item {key, status} pairs so the next-round
/// prompt distinguishes NOT-ADDRESSED (in scope, missed) from N-A (out
/// of scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReviewerStatus {
    /// Reviewer confirmed the item was addressed by the coder's PR.
    Addressed,
    /// Reviewer explicitly marked the item as NOT addressed; the next
    /// attempt MUST treat this as a hard constraint.
    NotAddressed,
    /// Item is not in scope for this PR. Distinct from NOT-ADDRESSED so
    /// the next-round coder prompt does not waste cycles on out-of-scope
    /// items.
    NotApplicable,
}

impl ReviewerStatus {
    /// Wire token used by the reviewer / skeptic when emitting a
    /// structured status line. Stable contract — DO NOT rename without
    /// coordinating with the reviewer CLI prompts.
    pub const fn as_wire_token(self) -> &'static str {
        match self {
            ReviewerStatus::Addressed => "ADDRESSED",
            ReviewerStatus::NotAddressed => "NOT-ADDRESSED",
            ReviewerStatus::NotApplicable => "N-A",
        }
    }

    /// Parse a wire token back into the enum. Tolerant of the case the
    /// reviewer happens to emit; the canonical form is uppercase.
    pub fn parse_wire_token(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "ADDRESSED" => Some(ReviewerStatus::Addressed),
            "NOT-ADDRESSED" | "NOT_ADDRESSED" | "NOTADDRESSED" => {
                Some(ReviewerStatus::NotAddressed)
            }
            "N-A" | "NA" | "NOT-APPLICABLE" | "NOT_APPLICABLE" => {
                Some(ReviewerStatus::NotApplicable)
            }
            _ => None,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmExtractorResponse {
    inhibition_specs: Vec<String>,
    positive_assertions: Vec<String>,
    security_redaction_encountered: bool,
    /// Optional on the wire so a reviewer LLM that does not surface
    /// `notAddressed` (older prompts, smaller models) still parses
    /// cleanly. Defaults to `[]` when absent.
    #[serde(default)]
    not_addressed: Vec<String>,
}

/// r3 (cursor-agent review of PR #410 / issue #408 — fix (d)):
/// deterministic schema-line parser for the structured NOT-ADDRESSED
/// output the reviewer CLI / skeptic is now contractually required to
/// emit. The exact line shape is:
///
/// ```text
/// NOT-ADDRESSED: ["item 1", "item 2", "item 3"]
/// ```
///
/// (single space after the colon, JSON-style array of strings, one per
/// unresolved reviewer item). The parser is regex-free; it locates the
/// marker on its own line, balances brackets, splits on `","` while
/// respecting `\"` escapes inside each string, and strips the surrounding
/// `"` quotes. Returns `None` when the marker is absent so callers fall
/// back to the legacy LLM-JSON path (kept for backward compat with
/// older reviewer LLMs that predate this contract).
///
/// Why a schema line instead of LLM JSON: the operator's r2 review
/// noted that r1's `LlmExtractorResponse.not_addressed` field put the
/// hot path through a model whose JSON output drifts across vendors.
/// A reviewer is already emitting a verdict line (`verdict: pass|warn|
/// fail`) and a structured schema line at the bottom of its reply; the
/// NOT-ADDRESSED list belongs next to those, not nested inside another
/// JSON object that the extractor has to guess at.
pub fn parse_not_addressed_schema_line(text: &str) -> Option<Vec<String>> {
    let line = text.lines().find(|l| {
        let t = l.trim_start();
        t.starts_with("NOT-ADDRESSED:")
    })?;
    let after = line
        .trim_start()
        .trim_start_matches("NOT-ADDRESSED:")
        .trim_start();
    // Strip a leading `[` and trailing `]`.
    let inner = after.trim();
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let inner = inner.strip_suffix(']').unwrap_or(inner);
    // Walk char-by-char; split on commas that appear OUTSIDE `"..."` and
    // outside `\"` escapes.
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut in_str = false;
    let mut escape = false;
    for ch in inner.chars() {
        if escape {
            buf.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_str => {
                buf.push(ch);
                escape = true;
            }
            '"' => {
                in_str = !in_str;
                // do not include the quote in the parsed string
            }
            ',' if !in_str => {
                let trimmed = buf.trim().trim_matches('"').to_string();
                if !trimmed.is_empty() {
                    out.push(trimmed);
                }
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    let trimmed = buf.trim().trim_matches('"').to_string();
    if !trimmed.is_empty() {
        out.push(trimmed);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Screens the reviewer feedback text for holdout test internals or subpaths and redacts them.
pub fn redact_holdouts(text: &str) -> (String, bool) {
    let mut result = String::new();
    let mut encountered = false;

    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            result.push(chars.next().unwrap());
        } else {
            let mut word = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_whitespace() {
                    break;
                }
                word.push(chars.next().unwrap());
            }

            // Check for surrounding quotes or punctuation (e.g. at the start/end)
            let mut start_idx = 0;
            while start_idx < word.len() && (word.as_bytes()[start_idx] == b'"' || word.as_bytes()[start_idx] == b'\'' || word.as_bytes()[start_idx] == b'(' || word.as_bytes()[start_idx] == b'[' || word.as_bytes()[start_idx] == b'{') {
                start_idx += 1;
            }
            let mut end_idx = word.len();
            while end_idx > start_idx && (word.as_bytes()[end_idx - 1] == b'"' || word.as_bytes()[end_idx - 1] == b'\'' || word.as_bytes()[end_idx - 1] == b')' || word.as_bytes()[end_idx - 1] == b']' || word.as_bytes()[end_idx - 1] == b'}' || word.as_bytes()[end_idx - 1] == b'.' || word.as_bytes()[end_idx - 1] == b',' || word.as_bytes()[end_idx - 1] == b';' || word.as_bytes()[end_idx - 1] == b':') {
                end_idx -= 1;
            }

            let core = &word[start_idx..end_idx];
            let lower = core.to_lowercase();
            if lower.contains("holdout") {
                encountered = true;
                result.push_str(&word[..start_idx]);
                if lower.contains('/') || lower.contains('\\') || lower.contains('$') {
                    result.push_str("[REDACTED_HOLDOUT_PATH]");
                } else if lower.contains("holdouts") {
                    result.push_str("[REDACTED_HOLDOUTS]");
                } else {
                    result.push_str("[REDACTED_HOLDOUT]");
                }
                result.push_str(&word[end_idx..]);
            } else {
                result.push_str(&word);
            }
        }
    }

    (result, encountered)
}

/// Prompt the LLM using the Constraint Extractor contract and extract positive assertions
/// and inhibition specs.
pub fn extract(llm: &dyn Llm, review_text: &str) -> Result<Extracted, DaemonError> {
    let (redacted_text, programmatic_encountered) = redact_holdouts(review_text);

    // r3 (cursor-agent review of PR #410 / issue #408 — fix (d)):
    // structured NOT-ADDRESSED takes precedence over the LLM JSON
    // `notAddressed` field. The deterministic schema line is the
    // primary source; the legacy LLM JSON is a fallback for older
    // reviewer LLMs that predate this contract (serde defaults the
    // field to `[]` when absent). Telemetry emits a structured
    // CONSTRAINT_NOT_ADDRESSED_EXTRACTED event so operators can audit
    // the per-bead propagation chain (reviewer → extractor → reroll
    // spec block → next-round red dispatch).
    let schema_line_items = parse_not_addressed_schema_line(review_text);
    if let Some(ref items) = schema_line_items {
        if !items.is_empty() {
            let _ = log_not_addressed_telemetry(items);
        }
    }

    let prompt = format!(
        "You are the Constraint Extractor for an autonomous coding factory.\n         Analyze the following rejection review feedback:\n\n         \"\"\"\n         {}\n         \"\"\"\n\n         Extract any positive assertions (what the code MUST do) and inhibition specs (what the code MUST NOT do, which get priority).\n         Also, identify any reviewer items the coder's previous attempt did NOT address (the reviewer asked for these but the coder's PR did not implement them) — these flow into the next attempt's hard constraints.\n         Also, verify if there are any holdout test internals or leaked holdout details in the feedback. If so, set securityRedactionEncountered to true.\n         Respond with exactly one JSON object as the last thing in your reply, in this format:\n         {{\n           \"inhibitionSpecs\": [\"...\"],\n           \"positiveAssertions\": [\"...\"],\n           \"notAddressed\": [\"...\"],\n           \"securityRedactionEncountered\": true|false\n         }}",
        redacted_text
    );

    let reply = llm.judge(&prompt)?;

    let last_close = reply.rfind('}').ok_or_else(|| {
        DaemonError::Parse(format!(
            "no JSON object found in extractor reply: {reply:?}"
        ))
    })?;
    let prefix = &reply[..=last_close];
    let last_open = prefix.rfind('{').ok_or_else(|| {
        DaemonError::Parse(format!(
            "no JSON object found in extractor reply: {reply:?}"
        ))
    })?;
    let candidate = &prefix[last_open..=last_close];

    let parsed: LlmExtractorResponse = serde_json::from_str(candidate).map_err(|e| {
        DaemonError::Parse(format!(
            "extractor reply did not contain a valid response object: {e} (reply: {reply:?})"
        ))
    })?;

    Ok(Extracted {
        inhibition_specs: parsed.inhibition_specs,
        positive_assertions: parsed.positive_assertions,
        security_redaction_encountered: parsed.security_redaction_encountered
            || programmatic_encountered,
        // r3 (fix d): the deterministic schema line takes precedence
        // over the LLM JSON `notAddressed` field. Schema line is parsed
        // at function entry from `review_text`; if absent we fall back
        // to whatever the LLM surfaced (back-compat for older reviewer
        // LLMs that predate the contract — serde defaults the field to
        // `[]` when absent).
        not_addressed: schema_line_items.unwrap_or(parsed.not_addressed),
    })
}

/// r3 (fix d): append a CONSTRAINT_NOT_ADDRESSED_EXTRACTED telemetry
/// line so operators can audit the propagation chain. The helper is a
/// best-effort append (the telemetry directory may not exist; missing
/// permissions are not a daemon-stopper). The exact line shape mirrors
/// the rest of the daemon's emit() calls but is local to constraints.rs
/// because the daemon-wide emit() takes a `TickDeps` we do not have
/// here.
fn log_not_addressed_telemetry(items: &[String]) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let path = std::env::var("CONSTRAINTS_TELEMETRY_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/constraints_not_addressed.jsonl"));
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::json!({
        "event": "CONSTRAINT_NOT_ADDRESSED_EXTRACTED",
        "count": items.len(),
        "items": items,
    });
    writeln!(f, "{line}")?;
    Ok(())
}

/// Appends the extracted constraints block append-only to the bead's spec file.
/// Atomicity is guaranteed via write-temp -> fsync -> rename.
pub fn append_mutation(spec_path: &Path, block: &str) -> Result<(), DaemonError> {
    let parent = spec_path.parent().ok_or_else(|| {
        DaemonError::Config(format!("spec path {} has no parent directory", spec_path.display()))
    })?;

    std::fs::create_dir_all(parent).map_err(|e| DaemonError::Tool {
        tool: "fs".into(),
        rc: -1,
        stderr: format!("create_dir_all: {e}"),
    })?;

    let temp_filename = format!(
        ".{}.tmp.{}",
        spec_path.file_name().and_then(|f| f.to_str()).unwrap_or("spec"),
        std::process::id()
    );
    let temp_path = parent.join(temp_filename);

    {
        let mut temp_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)
            .map_err(|e| DaemonError::Tool {
                tool: "fs".into(),
                rc: -1,
                stderr: format!("open temp file: {e}"),
            })?;

        if spec_path.exists() {
            let existing = std::fs::read_to_string(spec_path).map_err(|e| DaemonError::Tool {
                tool: "fs".into(),
                rc: -1,
                stderr: format!("read existing spec: {e}"),
            })?;
            temp_file.write_all(existing.as_bytes()).map_err(|e| {
                DaemonError::Tool {
                    tool: "fs".into(),
                    rc: -1,
                    stderr: format!("write existing spec: {e}"),
                }
            })?;
        }

        temp_file.write_all(block.as_bytes()).map_err(|e| {
            DaemonError::Tool {
                tool: "fs".into(),
                rc: -1,
                stderr: format!("write block: {e}"),
            }
        })?;

        temp_file.sync_all().map_err(|e| DaemonError::Tool {
            tool: "fs".into(),
            rc: -1,
            stderr: format!("fsync temp file: {e}"),
        })?;
    }

    std::fs::rename(&temp_path, spec_path).map_err(|e| DaemonError::Tool {
        tool: "fs".into(),
        rc: -1,
        stderr: format!("rename temp to target: {e}"),
    })?;

    Ok(())
}

/// Issue #408 / bead jleechan-1a5e r3: the structured NOT-ADDRESSED
/// constraints the previous round wrote into `spec.toml` (via
/// `format_reroll_block` / `append_mutation`) MUST be re-consumed by
/// the next-round coder prompt. Operator requires a regression test
/// asserting this end-to-end propagation — without it, structured
/// NOT-ADDRESSED is dead text. This struct is the in-memory shape
/// `parse_latest_reroll_block` returns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PriorRerollBlock {
    pub reviewer: String,
    pub attempt: u32,
    /// Flat list of NOT-ADDRESSED items — merged structured (deterministic)
    /// plus LLM (fallback). Backward-compatible with parsers that only
    /// know the flat shape.
    pub not_addressed: Vec<String>,
    /// Structured per-item records from `REVIEWER_STATUS:` lines,
    /// preserving the distinction NOT-ADDRESSED (in scope, missed) vs
    /// N-A (out of scope) so the next-round coder prompt can act
    /// differently on each.
    pub not_addressed_structured: Vec<(String, ReviewerStatus)>,
    pub inhibition_specs: Vec<String>,
    pub positive_assertions: Vec<String>,
}

/// Parse the LAST `[[reroll]]` block appended to the bead's spec file by
/// `format_reroll_block`. Returns `None` when:
///   * the file does not exist
///   * no `[[reroll]]` block is present
///   * the file is malformed (we silently treat malformed spec files as
///     "no prior reroll" rather than panicking — the coder prompt should
///     still render, just without the prior constraints section).
///
/// Multiple `[[reroll]]` blocks may exist (one per failed attempt); the
/// operator brief requires we surface the LATEST so the next coder sees
/// the most recent reviewer's NOT-ADDRESSED list.
///
/// Pure text/TOML — NO LLM call. The structured `not_addressed_structured`
/// array is the deterministic source of truth; `not_addressed` is the
/// backward-compatible flat list.
pub fn parse_latest_reroll_block(spec_path: &Path) -> Option<PriorRerollBlock> {
    let raw = std::fs::read_to_string(spec_path).ok()?;
    let value: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return None,
    };
    // The spec file is a single TOML document; each `[[reroll]]` table
    // appears as a sibling key. We collect them all then take the last
    // one. The key name is exactly `reroll` per `format_reroll_block`.
    let rerolls = value.get("reroll")?;
    let rerolls = rerolls.as_array()?;
    let last = rerolls.last()?;
    let attempt = last
        .get("attempt")
        .and_then(|v| v.as_integer())
        .map(|n| n as u32)
        .unwrap_or(0);
    let reviewer = last
        .get("reviewer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let not_addressed = last
        .get("not_addressed")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let not_addressed_structured = last
        .get("not_addressed_structured")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let key = item.get("key").and_then(|k| k.as_str())?.to_string();
                    let status_raw = item.get("status").and_then(|s| s.as_str())?;
                    let status = ReviewerStatus::parse_wire_token(status_raw)?;
                    Some((key, status))
                })
                .collect()
        })
        .unwrap_or_default();
    let inhibition_specs = last
        .get("inhibition_specs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let positive_assertions = last
        .get("positive_assertions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    Some(PriorRerollBlock {
        reviewer,
        attempt,
        not_addressed,
        not_addressed_structured,
        inhibition_specs,
        positive_assertions,
    })
}

/// Format the prior-attempt constraints block that flows into the
/// next-round coder prompt. Returns the empty string when there is no
/// prior `[[reroll]]` block (so the prompt is byte-identical to the
/// pre-r3 renderer for beads without a failed prior attempt).
///
/// The block groups structured NOT-ADDRESSED items first (they are
/// authoritative — the reviewer explicitly marked them not-addressed),
/// then N-A items (out-of-scope, informational only — do NOT chase),
/// then the flat `not_addressed` list for backward-compat visibility.
pub fn format_prior_constraints_block(prior: &PriorRerollBlock) -> String {
    if prior.not_addressed.is_empty()
        && prior.not_addressed_structured.is_empty()
        && prior.inhibition_specs.is_empty()
        && prior.positive_assertions.is_empty()
    {
        return String::new();
    }
    let mut s = String::from("\nPREVIOUS ATTEMPT CONSTRAINTS (authoritative — must be addressed in this round unless explicitly marked N-A):\n");
    if !prior.inhibition_specs.is_empty() {
        s.push_str("\nInhibition specs (MUST NOT do):\n");
        for spec in &prior.inhibition_specs {
            s.push_str(&format!("- {spec}\n"));
        }
    }
    if !prior.positive_assertions.is_empty() {
        s.push_str("\nPositive assertions (MUST do):\n");
        for spec in &prior.positive_assertions {
            s.push_str(&format!("- {spec}\n"));
        }
    }
    if !prior.not_addressed_structured.is_empty() {
        s.push_str("\nNOT-ADDRESSED items from previous reviewer (must address):\n");
        for (key, status) in &prior.not_addressed_structured {
            if *status == ReviewerStatus::NotApplicable {
                continue;
            }
            s.push_str(&format!("- {key}\n"));
        }
        let na_items: Vec<&str> = prior
            .not_addressed_structured
            .iter()
            .filter(|(_, s)| *s == ReviewerStatus::NotApplicable)
            .map(|(k, _)| k.as_str())
            .collect();
        if !na_items.is_empty() {
            s.push_str("\nOut-of-scope (N-A — do NOT chase):\n");
            for k in na_items {
                s.push_str(&format!("- {k}\n"));
            }
        }
    } else if !prior.not_addressed.is_empty() {
        // No structured entries — fall back to the flat list, which is
        // what every prior round produced.
        s.push_str("\nNOT-ADDRESSED items from previous reviewer (must address):\n");
        for spec in &prior.not_addressed {
            s.push_str(&format!("- {spec}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Llm;

    struct FakeLlm(String);
    impl Llm for FakeLlm {
        fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn test_redact_holdouts() {
        let text = "Fail: test in $DARK_FACTORY_HOLDOUTS/scenario_1.py failed";
        let (redacted, enc) = redact_holdouts(text);
        assert!(enc);
        assert_eq!(
            redacted,
            "Fail: test in [REDACTED_HOLDOUT_PATH] failed"
        );

        let text2 = "Check holdouts/test_foo.py";
        let (redacted2, enc2) = redact_holdouts(text2);
        assert!(enc2);
        assert_eq!(redacted2, "Check [REDACTED_HOLDOUT_PATH]");

        let text3 = "Check holdout/test_foo.py";
        let (redacted3, enc3) = redact_holdouts(text3);
        assert!(enc3);
        assert_eq!(redacted3, "Check [REDACTED_HOLDOUT_PATH]");

        let text4 = "Normal feedback, no leak.";
        let (redacted4, enc4) = redact_holdouts(text4);
        assert!(!enc4);
        assert_eq!(redacted4, text4);
    }

    #[test]
    fn test_extract_success() {
        let reply = r#"
            I processed your request.
            {"inhibitionSpecs":["no global variables"],"positiveAssertions":["must compile"],"securityRedactionEncountered":false}
        "#.to_string();
        let llm = FakeLlm(reply);
        let ext = extract(&llm, "Do not use global variables. Make sure it compiles.").unwrap();
        assert_eq!(ext.inhibition_specs, vec!["no global variables"]);
        assert_eq!(ext.positive_assertions, vec!["must compile"]);
        assert!(!ext.security_redaction_encountered);
    }

    #[test]
    fn test_extract_programmatic_redaction_wins() {
        let reply = r#"
            {"inhibitionSpecs":[],"positiveAssertions":[],"securityRedactionEncountered":false}
        "#.to_string();
        let llm = FakeLlm(reply);
        // Even though LLM says false, our programmatic redact_holdouts detects holdout and sets it to true
        let ext = extract(&llm, "Check holdouts/test.py").unwrap();
        assert!(ext.security_redaction_encountered);
    }

    #[test]
    fn test_append_mutation() {
        let temp_dir = std::env::temp_dir().join("acd_constraints_test");
        let spec_file = temp_dir.join("spec.toml");
        let _ = std::fs::remove_file(&spec_file);

        append_mutation(&spec_file, "block1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&spec_file).unwrap(), "block1\n");

        append_mutation(&spec_file, "block2\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&spec_file).unwrap(),
            "block1\nblock2\n"
        );

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    /// Issue #408 / bead jleechan-1a5e r3: round-trip a `[[reroll]]`
    /// block through append + parse so the regression test for
    /// "next-round red dispatch consumes structured NOT-ADDRESSED"
    /// has a fast unit-level companion. The full end-to-end test
    /// lives in `tests/vacuous_red_green_r2_integration.rs`.
    #[test]
    fn r3_parse_latest_reroll_block_round_trips_structured_not_addressed() {
        let temp_dir = std::env::temp_dir().join(format!(
            "acd_constraints_r3_{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let spec = temp_dir.join("bead.toml");
        let _ = std::fs::remove_file(&spec);
        // The remote r3 baseline inlines the [[reroll]] block format
        // inside `execute()` rather than exposing a helper. Construct
        // the same TOML shape here so the parser is exercised against
        // a realistic spec.toml.
        let block = r#"
[[reroll]]
         reviewer = "claude"
         attempt = 2
         inhibition_specs = [
           "no global mutable state",
         ]
         positive_assertions = [
           "must call redact_holdouts",
         ]
         not_addressed = [
           "3-check coverage",
         ]
         not_addressed_structured = [
           { key = "3-check coverage", status = "NOT-ADDRESSED" },
           { key = "out of scope item", status = "N-A" },
         ]
         raw_feedback = """
         raw reviewer text
         """
"#;
        append_mutation(&spec, block).unwrap();
        let prior = parse_latest_reroll_block(&spec)
            .expect("parse must surface the [[reroll]] block");
        assert_eq!(prior.attempt, 2);
        assert_eq!(prior.reviewer, "claude");
        assert_eq!(prior.not_addressed, vec!["3-check coverage".to_string()]);
        assert_eq!(prior.inhibition_specs, vec!["no global mutable state".to_string()]);
        assert_eq!(prior.positive_assertions, vec!["must call redact_holdouts".to_string()]);
        assert_eq!(prior.not_addressed_structured.len(), 2);
        assert!(
            prior.not_addressed_structured.iter().any(
                |(k, s)| k == "3-check coverage" && *s == ReviewerStatus::NotAddressed
            ),
            "structured NOT-ADDRESSED entry missing; got {:?}",
            prior.not_addressed_structured
        );
        assert!(
            prior.not_addressed_structured.iter().any(
                |(k, s)| k == "out of scope item" && *s == ReviewerStatus::NotApplicable
            ),
            "structured N-A entry missing; got {:?}",
            prior.not_addressed_structured
        );
        let rendered = format_prior_constraints_block(&prior);
        assert!(rendered.contains("PREVIOUS ATTEMPT CONSTRAINTS"));
        assert!(rendered.contains("3-check coverage"));
        assert!(rendered.contains("Out-of-scope"));
        assert!(rendered.contains("out of scope item"));
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn r3_parse_latest_reroll_block_returns_none_for_missing_or_malformed() {
        // Missing file → None.
        let missing = std::env::temp_dir().join(format!(
            "acd_constraints_r3_missing_{:?}.toml",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&missing);
        assert!(parse_latest_reroll_block(&missing).is_none());

        // Malformed TOML → None (NOT a panic).
        let temp_dir = std::env::temp_dir().join(format!(
            "acd_constraints_r3_malformed_{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let malformed = temp_dir.join("malformed.toml");
        std::fs::write(&malformed, "this is not valid TOML = [[[").unwrap();
        assert!(parse_latest_reroll_block(&malformed).is_none());
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn r3_parse_latest_reroll_block_returns_latest_when_multiple_blocks() {
        // Multiple `[[reroll]]` blocks must yield the LATEST one. The
        // operator's brief specifically calls out that the next-round
        // coder must see the MOST RECENT reviewer's NOT-ADDRESSED list,
        // not the earliest. Older blocks are inert history.
        let temp_dir = std::env::temp_dir().join(format!(
            "acd_constraints_r3_multi_{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let spec = temp_dir.join("bead.toml");
        let _ = std::fs::remove_file(&spec);

        let block_old = r#"
[[reroll]]
         reviewer = "claude"
         attempt = 1
         inhibition_specs = [
         ]
         positive_assertions = [
         ]
         not_addressed = [
           "old item",
         ]
         not_addressed_structured = [
           { key = "old item", status = "NOT-ADDRESSED" },
         ]
         raw_feedback = """
         first-round
         """
"#;
        append_mutation(&spec, block_old).unwrap();

        let block_new = r#"
[[reroll]]
         reviewer = "cursor-agent"
         attempt = 2
         inhibition_specs = [
         ]
         positive_assertions = [
         ]
         not_addressed = [
           "newer item",
         ]
         not_addressed_structured = [
           { key = "newer item", status = "NOT-ADDRESSED" },
         ]
         raw_feedback = """
         second-round
         """
"#;
        append_mutation(&spec, block_new).unwrap();

        let prior = parse_latest_reroll_block(&spec).expect("parse");
        assert_eq!(prior.attempt, 2, "must return the LATEST block");
        assert_eq!(prior.reviewer, "cursor-agent");
        assert!(
            prior.not_addressed_structured.iter().any(|(k, _)| k == "newer item"),
            "must surface the latest reviewer's NOT-ADDRESSED items; got {:?}",
            prior.not_addressed_structured
        );
        assert!(
            !prior.not_addressed_structured.iter().any(|(k, _)| k == "old item"),
            "older reviewer's items must NOT bleed into the latest block; got {:?}",
            prior.not_addressed_structured
        );
        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
