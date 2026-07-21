use crate::errors::DaemonError;
use crate::tools::Llm;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Issue #408 / bead jleechan-1a5e r2 (R2-4): per-item status emitted by
/// the reviewer CLI / skeptic on a known schema line. The structured
/// path is INDEPENDENT of the LLM JSON extractor — a reviewer that emits
/// `REVIEWER_STATUS: <key> = NOT-ADDRESSED` is parsed directly so the
/// `not_addressed` constraints reach the next-round coder prompt even if
/// the LLM extractor truncates or omits `notAddressed`. R2-4 closes the
/// r1 "fires on gaps is still LLM-JSON dependent, not structured" gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReviewerStatus {
    /// Reviewer confirmed the item was addressed by the coder's PR.
    Addressed,
    /// Reviewer explicitly marked the item as NOT addressed; the next
    /// attempt MUST treat this as a hard constraint.
    NotAddressed,
    /// Item is not in scope for this PR (reviewer confirmed it is out
    /// of scope). Distinct from NOT-ADDRESSED so the next-round coder
    /// prompt does not waste cycles on out-of-scope items.
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

/// One structured entry from the reviewer's per-item status block.
/// Carries the `key` (the item identifier the reviewer used) and the
/// status the reviewer assigned it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReviewerStatusEntry {
    pub key: String,
    pub status: ReviewerStatus,
}

/// Issue #408 / bead jleechan-1a5e r2 (R2-4): parse the structured
/// `REVIEWER_STATUS: <key> = <status>` lines emitted by the reviewer CLI
/// / skeptic into typed entries. Pure text — NO LLM call. This is the
/// deterministic path operator required: even if the LLM extractor
/// truncates, the structured lines surface in `not_addressed`.
///
/// Accepted line shapes (whitespace tolerant):
///   REVIEWER_STATUS: 3-check coverage = NOT-ADDRESSED
///   REVIEWER_STATUS: manifest-path = ADDRESSED
///   `REVIEWER_STATUS: foo = N-A`
///
/// Malformed lines are silently skipped — the LLM extractor remains a
/// fallback for unstructured prose.
pub fn parse_reviewer_status_lines(review_text: &str) -> Vec<ReviewerStatusEntry> {
    let mut out: Vec<ReviewerStatusEntry> = Vec::new();
    let mut seen: BTreeSet<(String, ReviewerStatus)> = BTreeSet::new();
    for line in review_text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("REVIEWER_STATUS:") else {
            continue;
        };
        let Some((key_raw, status_raw)) = rest.split_once('=') else {
            continue;
        };
        let key = key_raw.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let Some(status) = ReviewerStatus::parse_wire_token(status_raw) else {
            continue;
        };
        let entry = ReviewerStatusEntry {
            key: key.clone(),
            status,
        };
        // Dedupe: if the reviewer repeats the same (key, status), keep
        // only one. A later contradiction (same key, different status)
        // is resolved by LATER-WINS — the reviewer's most recent
        // judgment supersedes an earlier one.
        seen.insert((key, status));
        // Re-build the deduped vector in input order: remove any
        // earlier occurrence of the same key with a different status,
        // then append the current entry.
        out.retain(|e| e.key != entry.key);
        out.push(entry);
    }
    out
}

/// Extract just the NOT-ADDRESSED items from structured status lines.
/// Convenience helper for callers that only need the structured
/// NOT-ADDRESSED subset (matches the r2 `Extracted::not_addressed`
/// field shape).
pub fn not_addressed_keys_from_status_lines(
    review_text: &str,
) -> Vec<String> {
    parse_reviewer_status_lines(review_text)
        .into_iter()
        .filter(|e| e.status == ReviewerStatus::NotAddressed)
        .map(|e| e.key)
        .collect()
}

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
    /// attempt. R2-4 also folds structured `REVIEWER_STATUS:` lines
    /// (with status NOT-ADDRESSED) into this list — the deterministic
    /// path runs FIRST so a truncated LLM reply does NOT silently drop
    /// gaps.
    pub not_addressed: Vec<String>,
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

    // R2-4: parse structured `REVIEWER_STATUS: <key> = <status>` lines
    // FIRST (no LLM). Items the reviewer marked NOT-ADDRESSED are
    // already present in the merged result before the LLM extractor
    // runs, so a truncated or schema-divergent LLM reply cannot
    // silently drop them. The LLM extractor then ADDs any additional
    // NOT-ADDRESSED items it surfaces from free-form prose.
    let structured_not_addressed = not_addressed_keys_from_status_lines(review_text);

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

    // R2-4 merge: structured NOT-ADDRESSED keys come first (they are
    // authoritative when present), then any LLM-surfaced NOT-ADDRESSED
    // items are appended, with dedup. A truncated LLM reply cannot
    // drop the structured set because it was already captured before
    // the LLM was even called.
    let mut merged_not_addressed: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for key in structured_not_addressed
        .into_iter()
        .chain(parsed.not_addressed.into_iter())
    {
        let trimmed = key.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.clone()) {
            merged_not_addressed.push(trimmed);
        }
    }

    Ok(Extracted {
        inhibition_specs: parsed.inhibition_specs,
        positive_assertions: parsed.positive_assertions,
        security_redaction_encountered: parsed.security_redaction_encountered
            || programmatic_encountered,
        // P1-7 + R2-4: NOT-ADDRESSED items — structured status lines
        // (deterministic) merged with LLM-surfaced items (fallback),
        // deduplicated. The structured set runs first so the LLM
        // extractor cannot drop it on a truncated reply.
        not_addressed: merged_not_addressed,
    })
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
}
