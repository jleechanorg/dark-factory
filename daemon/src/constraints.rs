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

    struct TempCleanup<'a>(&'a Path, bool);
    impl<'a> Drop for TempCleanup<'a> {
        fn drop(&mut self) {
            if self.1 {
                let _ = std::fs::remove_file(self.0);
            }
        }
    }

    let mut cleanup = TempCleanup(&temp_path, false);

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

        cleanup.1 = true;

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

    cleanup.1 = false;

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

    #[test]
    fn reroll_temp_open_failure_preserves_original() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = std::env::temp_dir().join(format!("afd_constraints_temp_open_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let spec_file = temp_dir.join("spec.toml");
        let initial_bytes = "initial_key = \"initial_value\"\n";
        std::fs::write(&spec_file, initial_bytes).unwrap();

        let temp_filename = format!(
            ".{}.tmp.{}",
            spec_file.file_name().and_then(|f| f.to_str()).unwrap_or("spec"),
            std::process::id()
        );
        let temp_path = temp_dir.join(&temp_filename);

        // Make spec_file unreadable (0o000) so temp file opens successfully but read_existing fails
        let mut unreadable_perms = std::fs::metadata(&spec_file).unwrap().permissions();
        unreadable_perms.set_mode(0o000);
        std::fs::set_permissions(&spec_file, unreadable_perms).unwrap();

        let res = append_mutation(&spec_file, "inhibition_specs = [\"fail\"]\n");
        assert!(res.is_err(), "append_mutation must fail when existing spec cannot be read");
        let err = res.unwrap_err();
        match err {
            DaemonError::Tool { tool, stderr, .. } => {
                assert_eq!(tool, "fs");
                assert!(
                    stderr.contains("read existing spec:"),
                    "error must specifically name read failure after temp open, got: {stderr}"
                );
            }
            other => panic!("expected DaemonError::Tool fs error, got: {other:?}"),
        }

        // Assert that TempCleanup actively deleted the opened temp file on drop
        assert!(
            !temp_path.exists(),
            "temp file {} must be deleted by TempCleanup guard on failure",
            temp_path.display()
        );

        // Restore permissions and verify original spec file is completely unmodified
        let mut readable_perms = std::fs::metadata(&spec_file).unwrap().permissions();
        readable_perms.set_mode(0o644);
        std::fs::set_permissions(&spec_file, readable_perms).unwrap();

        assert_eq!(
            std::fs::read_to_string(&spec_file).unwrap(),
            initial_bytes,
            "original spec file must remain completely unmodified on failure"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

