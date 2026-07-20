//! Task Router (design doc Task 7, spec Appendix C item 1).
//!
//! ZFC (Zero-Framework Cognition): routing judgment lives ENTIRELY in the
//! model's JSON response. This module never inspects the bead title/body for
//! keywords, never counts files, never applies a heuristic score — it only
//! renders a prompt, calls `Llm::judge`, and strictly parses the contracted
//! JSON shape out of the reply. A reply that doesn't parse is a typed error,
//! never a silently-assumed verdict.

use crate::errors::DaemonError;
use crate::tools::{Bead, Llm};
use serde::Deserialize;

/// Routing decision for a bead, per spec Appendix C item 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingVerdict {
    SmallPath,
    StandardPath,
    ResearchPath,
    GenericPath,
}

/// Wire shape of the router LLM's JSON reply (spec Appendix C item 1):
/// `{ "routingVerdict": "SMALL_PATH" | "STANDARD_PATH" | "RESEARCH_PATH" | "GENERIC_PATH", "justification": "<one sentence>" }`.
#[derive(Debug, Deserialize)]
struct RouterResponse {
    #[serde(rename = "routingVerdict")]
    routing_verdict: String,
    #[allow(dead_code)]
    justification: String,
}

/// Render the router prompt for a bead (contract = spec Appendix C item 1:
/// input is the bead's full title/description/file-tree shape; output is
/// judged, not computed here).
///
/// Includes `bead.description` (the full `br list --json` body, not just the
/// one-line title) and `bead.file_tree_summary` (a pre-rendered listing of
/// the repo paths the bead is expected to touch) so the LLM has the "whole
/// shape of the task" this prompt asks it to judge from, rather than a bare
/// title. Both fields render as an explicit "(none provided)" rather than
/// being silently omitted when empty, so a missing description is never
/// confused with an intentionally short one.
fn render_prompt(bead: &Bead) -> String {
    let description = if bead.description.trim().is_empty() {
        "(none provided)"
    } else {
        bead.description.trim()
    };
    let file_tree = if bead.file_tree_summary.trim().is_empty() {
        "(none provided)"
    } else {
        bead.file_tree_summary.trim()
    };

    format!(
        "You are the Task Router for an autonomous coding factory.\n\
         Judge which path this task should be routed to:\n\
         - \"SMALL_PATH\": Small, direct single-shot code implementation.\n\
         - \"STANDARD_PATH\": Standard multi-gate feature implementation.\n\
         - \"RESEARCH_PATH\": Read-only research, analysis, or codebase investigation.\n\
         - \"GENERIC_PATH\": General tasks that do not fit direct code implementation (e.g. documentation, testing, or spec generation).\n\n\
         Base your judgment on the whole shape of the task below — do not apply \
         keyword or file-count rules; use your own judgment of complexity, \
         ambiguity, and blast radius.\n\n\
         Bead ID: {}\n\
         Title: {}\n\n\
         Description:\n{}\n\n\
         Relevant file tree:\n{}\n\n\
         Respond with exactly one JSON object as the last thing in your reply, \
         of the form:\n\
         {{\"routingVerdict\": \"SMALL_PATH\" | \"STANDARD_PATH\" | \"RESEARCH_PATH\" | \"GENERIC_PATH\", \"justification\": \"<one sentence>\"}}",
        bead.id, bead.title, description, file_tree,
    )
}

/// Find the LAST `{...}` JSON block in `reply` and strictly parse it as a
/// `RouterResponse`. Returns `DaemonError::Parse` on any failure — no
/// heuristic fallback, no default verdict.
fn parse_last_json_block(reply: &str) -> Result<RouterResponse, DaemonError> {
    let last_close = reply
        .rfind('}')
        .ok_or_else(|| DaemonError::Parse(format!("no JSON object found in reply: {reply:?}")))?;
    let prefix = &reply[..=last_close];
    let last_open = prefix
        .rfind('{')
        .ok_or_else(|| DaemonError::Parse(format!("no JSON object found in reply: {reply:?}")))?;
    let candidate = &prefix[last_open..=last_close];

    serde_json::from_str::<RouterResponse>(candidate).map_err(|e| {
        DaemonError::Parse(format!(
            "router reply did not contain a valid routing verdict object: {e} (reply: {reply:?})"
        ))
    })
}

/// Route a bead by asking the LLM to judge it, then strictly parsing the
/// contracted JSON verdict out of the LAST `{...}` block in the reply.
///
/// Parse failure (prose, missing keys, malformed JSON, unrecognized verdict
/// token) returns `DaemonError::Parse` — the caller is responsible for
/// parking the bead HUMAN_HELD. This function never guesses a verdict.
pub fn route(llm: &dyn Llm, bead: &Bead) -> Result<RoutingVerdict, DaemonError> {
    let prompt = render_prompt(bead);
    let reply = llm.judge(&prompt)?;
    let parsed = parse_last_json_block(&reply)?;

    match parsed.routing_verdict.as_str() {
        "SMALL_PATH" => Ok(RoutingVerdict::SmallPath),
        "STANDARD_PATH" => Ok(RoutingVerdict::StandardPath),
        "RESEARCH_PATH" => Ok(RoutingVerdict::ResearchPath),
        "GENERIC_PATH" => Ok(RoutingVerdict::GenericPath),
        other => Err(DaemonError::Parse(format!(
            "unrecognized routingVerdict token: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Minimal in-file fake `Llm` mirroring `daemon/tests/common/mod.rs`'s
    /// `FakeLlm`, scoped to this module so unit tests don't need the
    /// integration-test-only `tests/common` path. Records every prompt it was
    /// called with so tests can assert on rendered prompt content.
    #[derive(Default)]
    struct FakeLlm {
        response: RefCell<Option<Result<String, String>>>,
        prompts: RefCell<Vec<String>>,
    }

    impl FakeLlm {
        fn scripted(text: &str) -> Self {
            Self {
                response: RefCell::new(Some(Ok(text.to_string()))),
                prompts: RefCell::new(Vec::new()),
            }
        }
    }

    impl Llm for FakeLlm {
        fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
            self.prompts.borrow_mut().push(prompt.to_string());
            match self.response.borrow().as_ref() {
                Some(Ok(text)) => Ok(text.clone()),
                Some(Err(e)) => Err(DaemonError::Parse(e.clone())),
                None => Ok(String::new()),
            }
        }
    }

    fn bead() -> Bead {
        Bead {
            id: "jleechan-test".into(),
            title: "Fix the thing".into(),
            description: String::new(),
            notes: String::new(),
            file_tree_summary: String::new(),
            external_ref: None,
        }
    }

    #[test]
    fn small_path_verdict_parses() {
        let llm = FakeLlm::scripted(r#"{"routingVerdict":"SMALL_PATH","justification":"x"}"#);
        let verdict = route(&llm, &bead()).expect("should parse");
        assert_eq!(verdict, RoutingVerdict::SmallPath);
    }

    #[test]
    fn standard_path_verdict_parses() {
        let llm = FakeLlm::scripted(
            r#"{"routingVerdict":"STANDARD_PATH","justification":"needs more care"}"#,
        );
        let verdict = route(&llm, &bead()).expect("should parse");
        assert_eq!(verdict, RoutingVerdict::StandardPath);
    }

    #[test]
    fn research_path_verdict_parses() {
        let llm = FakeLlm::scripted(
            r#"{"routingVerdict":"RESEARCH_PATH","justification":"just read codebase"}"#,
        );
        let verdict = route(&llm, &bead()).expect("should parse");
        assert_eq!(verdict, RoutingVerdict::ResearchPath);
    }

    #[test]
    fn generic_path_verdict_parses() {
        let llm = FakeLlm::scripted(
            r#"{"routingVerdict":"GENERIC_PATH","justification":"spec edit only"}"#,
        );
        let verdict = route(&llm, &bead()).expect("should parse");
        assert_eq!(verdict, RoutingVerdict::GenericPath);
    }

    #[test]
    fn prose_reply_is_parse_error_never_defaulted() {
        let llm = FakeLlm::scripted("it looks small to me");
        let result = route(&llm, &bead());
        match result {
            Err(DaemonError::Parse(_)) => {}
            Err(other) => panic!("expected Parse error, got {other:?}"),
            Ok(verdict) => {
                panic!("prose reply must never be silently defaulted to a verdict, got {verdict:?}")
            }
        }
    }

    #[test]
    fn json_embedded_in_prose_still_parses_last_block() {
        // The model may reason in prose before emitting the verdict object;
        // the contract is "last {...} block in the reply", not "reply is bare JSON".
        let llm = FakeLlm::scripted(
            "Let me think about this task...\n\
             It touches only one file, so:\n\
             {\"routingVerdict\": \"SMALL_PATH\", \"justification\": \"single file change\"}",
        );
        let verdict = route(&llm, &bead()).expect("should parse trailing JSON block");
        assert_eq!(verdict, RoutingVerdict::SmallPath);
    }

    #[test]
    fn malformed_json_is_parse_error() {
        let llm = FakeLlm::scripted(r#"{"routingVerdict": "SMALL_PATH", "justification": }"#);
        assert!(matches!(route(&llm, &bead()), Err(DaemonError::Parse(_))));
    }

    #[test]
    fn unrecognized_verdict_token_is_parse_error_not_default() {
        let llm = FakeLlm::scripted(r#"{"routingVerdict":"MEDIUM_PATH","justification":"unsure"}"#);
        assert!(matches!(route(&llm, &bead()), Err(DaemonError::Parse(_))));
    }

    #[test]
    fn missing_justification_key_is_parse_error() {
        let llm = FakeLlm::scripted(r#"{"routingVerdict":"SMALL_PATH"}"#);
        assert!(matches!(route(&llm, &bead()), Err(DaemonError::Parse(_))));
    }

    #[test]
    fn llm_error_propagates_without_defaulting() {
        struct ErrLlm;
        impl Llm for ErrLlm {
            fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
                Err(DaemonError::Timeout("llm call timed out".into()))
            }
        }
        assert!(matches!(
            route(&ErrLlm, &bead()),
            Err(DaemonError::Timeout(_))
        ));
    }

    #[test]
    fn rendered_prompt_includes_description_and_file_tree() {
        let mut b = bead();
        b.description = "This bead touches dispatch.rs and router.rs together.".into();
        b.file_tree_summary = "daemon/src/dispatch.rs\ndaemon/src/router.rs".into();

        let prompt = render_prompt(&b);

        assert!(
            prompt.contains("This bead touches dispatch.rs and router.rs together."),
            "prompt must include the full description, not just the title: {prompt:?}"
        );
        assert!(
            prompt.contains("daemon/src/dispatch.rs"),
            "prompt must include the file-tree summary: {prompt:?}"
        );
        assert!(
            prompt.contains(&b.title),
            "prompt must still include the title: {prompt:?}"
        );
        assert!(
            prompt.contains(&b.id),
            "prompt must still include the bead id: {prompt:?}"
        );
    }

    #[test]
    fn rendered_prompt_marks_missing_description_and_file_tree_explicitly() {
        // Empty fields must render as an explicit placeholder, not be silently
        // dropped — a missing description should never look identical to an
        // intentionally short one in the prompt the LLM sees.
        let b = bead(); // description + file_tree_summary both "" by construction
        let prompt = render_prompt(&b);

        assert!(
            prompt.contains("(none provided)"),
            "empty description/file-tree must render an explicit placeholder: {prompt:?}"
        );
    }

    #[test]
    fn llm_receives_the_enriched_prompt_via_route() {
        let mut b = bead();
        b.description = "Needs a multi-file coordinated change.".into();
        b.file_tree_summary = "daemon/src/tools.rs".into();

        let llm = FakeLlm::scripted(r#"{"routingVerdict":"STANDARD_PATH","justification":"x"}"#);
        route(&llm, &b).expect("should parse");

        let prompts = llm.prompts.borrow();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("Needs a multi-file coordinated change."));
        assert!(prompts[0].contains("daemon/src/tools.rs"));
    }
}
