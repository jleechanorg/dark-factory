// Issue #408 P1-7: NOT-ADDRESSED items extracted from reviewer feedback flow
// into reroll constraints. `constraints::extract` must surface them so the
// next-round coder prompt is briefed on the unresolved gaps.

use daemon::constraints::extract;
use daemon::tools::Llm;
use daemon::errors::DaemonError;

struct FakeLlm(String);
impl Llm for FakeLlm {
    fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
        Ok(self.0.clone())
    }
}

#[test]
fn not_addressed_items_are_extracted() {
    let reply = r#"
        I parsed the feedback.
        {"inhibitionSpecs":["no global variables"],"positiveAssertions":["must compile"],"notAddressed":["the 3-check coverage","the manifest-path","the #[ignore] handling"],"securityRedactionEncountered":false}
    "#
    .to_string();
    let llm = FakeLlm(reply);
    let ext = extract(&llm, "you must address: the 3-check coverage; the manifest-path; the #[ignore] handling").unwrap();
    assert_eq!(ext.inhibition_specs, vec!["no global variables"]);
    assert_eq!(ext.positive_assertions, vec!["must compile"]);
    // P1-7: NOT-ADDRESSED must surface as a distinct Vec<String> field.
    assert_eq!(ext.not_addressed, vec![
        "the 3-check coverage".to_string(),
        "the manifest-path".to_string(),
        "the #[ignore] handling".to_string()
    ]);
}

#[test]
fn not_addressed_defaults_to_empty_when_absent() {
    // Backward compat: a reviewer reply that omits `notAddressed` (older LLM
    // output) must NOT trip the extractor — default to an empty list.
    let reply = r#"
        {"inhibitionSpecs":[],"positiveAssertions":[],"securityRedactionEncountered":false}
    "#
    .to_string();
    let llm = FakeLlm(reply);
    let ext = extract(&llm, "no not-addressed items here").unwrap();
    assert!(ext.not_addressed.is_empty());
}