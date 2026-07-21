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
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmExtractorResponse {
    inhibition_specs: Vec<String>,
    positive_assertions: Vec<String>,
    security_redaction_encountered: bool,
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

    let prompt = format!(
        "You are the Constraint Extractor for an autonomous coding factory.\n         Analyze the following rejection review feedback:\n\n         \"\"\"\n         {}\n         \"\"\"\n\n         Extract any positive assertions (what the code MUST do) and inhibition specs (what the code MUST NOT do, which get priority).\n         Also, verify if there are any holdout test internals or leaked holdout details in the feedback. If so, set securityRedactionEncountered to true.\n         Respond with exactly one JSON object as the last thing in your reply, in this format:\n         {{\n           \"inhibitionSpecs\": [\"...\"],\n           \"positiveAssertions\": [\"...\"],\n           \"securityRedactionEncountered\": true|false\n         }}",
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

    /// Recording fake LLM: captures the prompt the constraint-extract
    /// path passes to `judge()` so the test can assert the verbatim
    /// text reaches the LLM-extract prompt. This is the Rust side of
    /// the r3 end-to-end invariant from issue #386 gap 6: a contract-
    /// failed gate MUST emit a failure reason that contains the
    /// verbatim acceptance-item text, and that text MUST flow through
    /// `constraints::extract`'s LLM prompt so the next-round worker's
    /// constraint block carries the exact problem, not a paraphrase.
    struct RecordingLlm {
        last_prompt: std::sync::Mutex<String>,
        reply: String,
    }
    impl RecordingLlm {
        fn new(reply: String) -> Self {
            Self {
                last_prompt: std::sync::Mutex::new(String::new()),
                reply,
            }
        }
        fn last_prompt(&self) -> String {
            self.last_prompt.lock().unwrap().clone()
        }
    }
    impl Llm for RecordingLlm {
        fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
            *self.last_prompt.lock().unwrap() = prompt.to_string();
            Ok(self.reply.clone())
        }
    }

    /// End-to-end Rust test for the r3 contract-echo redispatch loop
    /// (issue #386 gap 6): the failure reason emitted by a contract-
    /// failed gate (Python SkepticResult.reason) is fed to the
    /// daemon's `constraints::extract` as `review_text`. The verbatim
    /// acceptance-item text MUST reach the LLM-extract prompt so the
    /// next-round worker's constraints carry the exact problem.
    ///
    /// This mirrors `tests/test_skeptic_contract_echo_redispatch.py`
    /// on the Rust side and proves the wiring without spawning the
    /// daemon subprocess.
    #[test]
    fn test_extract_receives_unaddressed_verbatim_from_contract_failed_gate() {
        // Simulate SkepticResult.reason from a contract-failed gate
        // whose required=true acceptance item was N-A'd away. The
        // text "required=true acceptance items must NOT be N-A-
        // eligible" is the exact acceptance-item text from the bead.
        let review_text = "\
            UNADDRESSED ACCEPTANCE ITEMS:\n\
            - A2 [REQUIRED]: required=true acceptance items must NOT be N-A-eligible\n\
            \n\
            Reviewer returned N-A for required=true item A2; \
            gate fails closed per spec §4.2.5. Required items cannot \
            be skipped.\n";
        let reply = r#"{"inhibitionSpecs":[],"positiveAssertions":["required=true acceptance items must NOT be N-A-eligible"],"securityRedactionEncountered":false}"#.to_string();
        let llm = RecordingLlm::new(reply);
        let ext = extract(&llm, review_text).unwrap();
        // The verbatim acceptance-item text MUST appear in the LLM
        // prompt that the daemon's extractor sends. If it doesn't,
        // the next-round worker only sees a paraphrase, which is
        // exactly the failure mode r3 fixes.
        let prompt = llm.last_prompt();
        assert!(
            prompt.contains("required=true acceptance items must NOT be N-A-eligible"),
            "verbatim acceptance-item text must reach the constraint-extract LLM prompt; got prompt: {prompt:?}",
        );
        // Sanity: the extractor must surface that verbatim text as a
        // positive assertion (the LLM mirrored it back), which is what
        // gets appended to the bead's spec.toml for the next roll.
        assert!(
            ext.positive_assertions.iter().any(|s| s.contains("required=true acceptance items must NOT be N-A-eligible")),
            "verbatim acceptance-item text must surface in positive_assertions; got: {:?}",
            ext.positive_assertions,
        );
    }

    /// Companion to the test above: even when the LLM reply contains
    /// NO usable JSON, the prompt must still carry the verbatim text
    /// (so a misbehaving LLM doesn't lose the constraint). The
    /// extractor will return Err, but the prompt was correct.
    #[test]
    fn test_extract_prompt_carries_verbatim_text_on_unparseable_reply() {
        let review_text = "\
            UNADDRESSED ACCEPTANCE ITEMS:\n\
            - A1 [REQUIRED]: the wire format MUST carry the bead ID\n";
        let llm = RecordingLlm::new("not json".to_string());
        let _ = extract(&llm, review_text);
        let prompt = llm.last_prompt();
        assert!(
            prompt.contains("the wire format MUST carry the bead ID"),
            "verbatim text must reach the prompt even on unparseable reply; got: {prompt:?}",
        );
    }

    /// End-to-end Rust test for the r8 3-item round-trip fixture
    /// (issue #386 r4 ATTEMPT GUIDANCE gap 6): a contract-failed gate
    /// emits a failure reason that lists THREE acceptance items —
    /// A1 ADDRESSED, A2 ADDRESSED, A3 NOT-ADDRESSED. The verbatim
    /// A3 text MUST reach the constraint-extract LLM prompt, AND
    /// the A1/A2 verbatim texts (which were already addressed) MUST
    /// NOT contaminate the next-roll constraint block. Only the
    /// unaddressed item flows into the next round — addressing this
    /// is what makes the redispatch loop converge rather than
    /// thrash on already-fixed items.
    #[test]
    fn test_extract_round_trip_three_items_addressed_addressed_not_addressed() {
        let review_text = "\
            UNADDRESSED ACCEPTANCE ITEMS:\n\
            - A3: 3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip \
                  fixture exercises parse + evaluate + extract\n\
            \n\
            Reviewer returned NOT-ADDRESSED for A3; A1 and A2 were \
            ADDRESSED in this round. Gate fails closed per spec §4.2.5 \
            for the unaddressed A3 item only.\n";
        let reply = r#"{
            "inhibitionSpecs": [],
            "positiveAssertions": [
                "3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture exercises parse + evaluate + extract"
            ],
            "securityRedactionEncountered": false
        }"#
        .to_string();
        let llm = RecordingLlm::new(reply);
        let ext = extract(&llm, review_text).unwrap();

        // The verbatim A3 text MUST appear in the LLM prompt.
        let prompt = llm.last_prompt();
        assert!(
            prompt.contains(
                "3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture exercises parse + evaluate + extract"
            ),
            "verbatim A3 text must reach the constraint-extract LLM prompt; got prompt: {prompt:?}",
        );

        // Only the unaddressed item is mirrored back as a positive
        // assertion (the LLM was given the full failure reason but
        // distilled to the unaddressed constraint). The A1/A2
        // verbatim texts, which the reviewer already addressed, are
        // NOT in positive_assertions — the next-round worker reads
        // the exact problem (A3 only), not a noisy restatement of
        // already-addressed items.
        assert_eq!(ext.positive_assertions.len(), 1);
        assert!(
            ext.positive_assertions[0].contains(
                "3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture"
            ),
            "positive_assertions must contain only the verbatim A3 text; got: {:?}",
            ext.positive_assertions,
        );
        // Holdout redaction still false (no holdout path in the
        // fixture review_text).
        assert!(!ext.security_redaction_encountered);
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

    /// End-to-end Rust test for the r8 contract-echo redispatch loop
    /// (issue #386 r4 ATTEMPT GUIDANCE gap 6b): the daemon's
    /// `skeptic_evidence` path produces a SkepticVerdict::Fail when
    /// the reviewer's `CONTRACT_ECHO:` block reports NOT-ADDRESSED.
    /// The verbatim text MUST reach the constraint-extract LLM prompt
    /// so the next-round worker's constraints carry the exact problem.
    ///
    /// Steps exercised end-to-end:
    ///   1. Build a 3-item BeadContract with a required=true item.
    ///   2. Simulate a reviewer reply whose `CONTRACT_ECHO:` block
    ///      addresses A1/A2 and NOT-ADDRESSEDs A3 (required).
    ///   3. The contract_echo::evaluate_contract_echo produces a Fail
    ///      constraint carrying the verbatim A3 text.
    ///   4. `constraints::extract` is fed that constraint block as
    ///      `review_text`. The verbatim A3 text MUST appear in the
    ///      LLM-extract prompt AND surface as a positive assertion
    ///      that the daemon appends to the bead's spec for the next
    ///      round.
    #[test]
    fn test_run_tick_constraint_extraction_redispatch_e2e_three_items() {
        // Step 1 — contract with 3 items, A2 is required=true.
        let contract = crate::contract_echo::BeadContract {
            id: "jleechan-pq08".into(),
            description: "Contract-echo review step".into(),
            notes: vec!["do NOT N-A away acceptance items".into()],
            prior_findings: vec![],
            acceptance_items: vec![
                crate::contract_echo::AcceptanceItem {
                    id: "A1".into(),
                    text: "diff addresses A1".into(),
                    required: false,
                },
                crate::contract_echo::AcceptanceItem {
                    id: "A2".into(),
                    text: "required=true acceptance items must NOT be N-A-eligible".into(),
                    required: true,
                },
                crate::contract_echo::AcceptanceItem {
                    id: "A3".into(),
                    text: "3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture".into(),
                    required: false,
                },
            ],
        };

        // Step 2 — reviewer reply: A1 ADDRESSED, A2 ADDRESSED, A3 NOT-ADDRESSED.
        let reviewer_reply = "VERDICT: PASS\n\
            HEAD_SHA: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
            REPO: jleechanorg/dark-factory\n\
            PR_NUMBER: 386\n\
            REASON: looks fine\n\
            IDENTITY: claude\n\
            TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n\
            LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n\
            GREP_CITES: runner/skeptic_gate.py:1\n\
            HEAD_COMMIT_VERIFIED: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n\
            CONTRACT_ECHO:\n\
            ITEM: A1 VERDICT: ADDRESSED CITE: foo/bar.py:1\n\
            ITEM: A2 VERDICT: ADDRESSED CITE: foo/bar.py:2\n\
            ITEM: A3 VERDICT: NOT-ADDRESSED REASON: handler missing in daemon\n";

        // Step 3 — parse + evaluate the contract-echo block.
        let report = crate::contract_echo::parse_contract_echo(reviewer_reply)
            .expect("CONTRACT_ECHO block must parse");
        let eval =
            crate::contract_echo::evaluate_contract_echo(Some(&report), &contract);
        assert!(!eval.ok);
        assert_eq!(eval.unaddressed_items.len(), 1);
        assert_eq!(eval.unaddressed_items[0].id, "A3");
        // The verbatim A3 text MUST appear in the constraint block —
        // this is what `tick::apply_contract_echo` will hand to the
        // next-round reroll's `red_reasons`.
        assert!(
            eval.constraint
                .contains("3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture"),
            "verbatim A3 text must appear in constraint block: {:?}",
            eval.constraint
        );

        // Step 4 — feed the constraint block into `constraints::extract`.
        // The verbatim A3 text MUST reach the LLM prompt and surface as
        // a positive assertion (mirrors what the Python redispatch test
        // exercises).
        let reply = r#"{
            "inhibitionSpecs": [],
            "positiveAssertions": [
                "3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture"
            ],
            "securityRedactionEncountered": false
        }"#
        .to_string();
        let llm = RecordingLlm::new(reply);
        let ext = extract(&llm, &eval.constraint).unwrap();
        let prompt = llm.last_prompt();
        assert!(
            prompt.contains(
                "3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture"
            ),
            "verbatim A3 text must reach the constraint-extract LLM prompt; got: {prompt:?}",
        );
        assert_eq!(ext.positive_assertions.len(), 1);
        assert!(
            ext.positive_assertions[0]
                .contains("3-item ADDRESSED/ADDRESSED/NOT-ADDRESSED round-trip fixture"),
            "verbatim A3 text must surface in positive_assertions; got: {:?}",
            ext.positive_assertions,
        );
    }
}
