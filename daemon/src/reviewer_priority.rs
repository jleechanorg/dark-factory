//! Shared skeptic reviewer priority — parsed from `config/skeptic_reviewer_priority.json`.
//!
//! Keep in sync with `runner/reviewer_priority.py`. Integration test
//! `reviewer_priority_parity` in `daemon/tests/reviewer_priority.rs` asserts
//! the JSON matches the Python loader on every `cargo test`.

use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
struct SkepticReviewerConfig {
    reviewer_priority: Vec<String>,
}

const CONFIG_JSON: &str = include_str!("../../config/skeptic_reviewer_priority.json");

fn load_priority() -> &'static [String] {
    static PRIORITY: OnceLock<Vec<String>> = OnceLock::new();
    PRIORITY.get_or_init(|| {
        let cfg: SkepticReviewerConfig =
            serde_json::from_str(CONFIG_JSON).expect("invalid config/skeptic_reviewer_priority.json");
        assert!(
            !cfg.reviewer_priority.is_empty(),
            "reviewer_priority must be non-empty"
        );
        cfg.reviewer_priority
    })
}

/// Ordered reviewer vendor list shared with `runner/reviewer_priority.py`.
pub fn skeptic_reviewer_priority() -> &'static [String] {
    load_priority()
}

#[cfg(test)]
mod tests {
    use super::skeptic_reviewer_priority;

    #[test]
    fn default_priority_excludes_legacy_vendors() {
        let p = skeptic_reviewer_priority();
        assert_eq!(
            p.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["claudem", "agy", "cursor-agent"]
        );
        assert!(!p.iter().any(|v| v == "gemini"));
        assert!(!p.iter().any(|v| v == "codex"));
        assert!(!p.iter().any(|v| v == "claude"));
    }
}
