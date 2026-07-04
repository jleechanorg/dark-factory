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
}

/// Wire shape of the router LLM's JSON reply (spec Appendix C item 1):
/// `{ "routingVerdict": "SMALL_PATH" | "STANDARD_PATH", "justification": "<one sentence>" }`.
#[derive(Debug, Deserialize)]
struct RouterResponse {
    #[serde(rename = "routingVerdict")]
    routing_verdict: String,
    #[allow(dead_code)]
    justification: String,
}

/// Render the router prompt for a bead (contract = spec Appendix C item 1:
/// input is the bead title/description; output is judged, not computed here).
fn render_prompt(bead: &Bead) -> String {
    format!(
        "You are the Task Router for an autonomous coding factory.\n\
         Judge whether this task is small enough for a single-shot \"small path\" \
         implementation, or needs the full multi-gate \"standard path\".\n\
         Base your judgment on the whole shape of the task below — do not apply \
         keyword or file-count rules; use your own judgment of complexity, \
         ambiguity, and blast radius.\n\n\
         Bead ID: {}\n\
         Title/description:\n{}\n\n\
         Respond with exactly one JSON object as the last thing in your reply, \
         of the form:\n\
         {{\"routingVerdict\": \"SMALL_PATH\" | \"STANDARD_PATH\", \"justification\": \"<one sentence>\"}}",
        bead.id, bead.title,
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
    /// integration-test-only `tests/common` path.
    #[derive(Default)]
    struct FakeLlm {
        response: RefCell<Option<Result<String, String>>>,
    }

    impl FakeLlm {
        fn scripted(text: &str) -> Self {
            Self {
                response: RefCell::new(Some(Ok(text.to_string()))),
            }
        }
    }

    impl Llm for FakeLlm {
        fn judge(&self, _prompt: &str) -> Result<String, DaemonError> {
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
    fn prose_reply_is_parse_error_never_defaulted() {
        let llm = FakeLlm::scripted("it looks small to me");
        let result = route(&llm, &bead());
        match result {
            Err(DaemonError::Parse(_)) => {}
            Err(other) => panic!("expected Parse error, got {other:?}"),
            Ok(verdict) => panic!(
                "prose reply must never be silently defaulted to a verdict, got {verdict:?}"
            ),
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
        let llm =
            FakeLlm::scripted(r#"{"routingVerdict":"MEDIUM_PATH","justification":"unsure"}"#);
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
}
