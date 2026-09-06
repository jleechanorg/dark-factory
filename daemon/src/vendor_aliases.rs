//! Vendor alias canonicalization — parsed from `config/vendor_aliases.json`.
//!
//! Bead rev-9zrgs: `tick::skeptic_evidence` used to canonicalize the
//! `DARK_FACTORY_CODER_DEFAULT` / `DARK_FACTORY_REVIEWER_DEFAULT` env var
//! into a skeptic-reviewer vendor bucket via an ORDERED 6-arm `.contains()`
//! chain. Because `"claudem"` contains `"claude"` as a substring, the
//! chain's own inline comment flagged the ordering as a live footgun:
//! reordering the arms would silently misclassify a claudem coder as a
//! plain-Anthropic-`claude` coder. This module replaces the substring chain
//! with an EXACT-match lookup against a versioned config so ordering can no
//! longer matter — there is no substring containment step at all.
//!
//! Mirrors the `config/skeptic_reviewer_priority.json` /
//! `daemon/src/reviewer_priority.rs` and `config/session_health_markers.json`
//! / `daemon/src/session_health_markers.rs` pattern used elsewhere in this
//! crate (`include_str!` + `OnceLock` + `serde::Deserialize`).
//!
//! Investigation (full findings in the rev-9zrgs PR body): every real call
//! site that sets `DARK_FACTORY_CODER_DEFAULT` / `DARK_FACTORY_REVIEWER_DEFAULT`
//! (`daemon/tests/tick_integration.rs`, `daemon/systemd/drop-in/README.md`,
//! and the analogous `--ao-agent` default in `runner/__main__.py`) uses a
//! single bare token (`agy`, `antigravity`, `codex`, ...) — never a
//! compound/hyphenated/path-like string. Whole-string exact match (after
//! trim + lowercase) is therefore sufficient; a token-splitting matcher
//! would add complexity with no known real input it would additionally need
//! to handle.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
struct VendorAliasesConfig {
    vendor_aliases: HashMap<String, Vec<String>>,
}

const CONFIG_JSON: &str = include_str!("../../config/vendor_aliases.json");

/// Flat `alias -> canonical vendor` lookup, built once from the structured
/// (canonical-vendor -> aliases) config. Exact match only — see module doc.
fn load_lookup() -> &'static HashMap<String, String> {
    static LOOKUP: OnceLock<HashMap<String, String>> = OnceLock::new();
    LOOKUP.get_or_init(|| {
        let cfg: VendorAliasesConfig =
            serde_json::from_str(CONFIG_JSON).expect("invalid config/vendor_aliases.json");
        assert!(
            !cfg.vendor_aliases.is_empty(),
            "vendor_aliases must be non-empty"
        );
        let mut lookup = HashMap::new();
        for (canonical, aliases) in &cfg.vendor_aliases {
            assert!(
                !aliases.is_empty(),
                "vendor_aliases[{canonical:?}] must list at least one alias"
            );
            for alias in aliases {
                let key = alias.trim().to_ascii_lowercase();
                if let Some(existing) = lookup.insert(key.clone(), canonical.clone()) {
                    panic!(
                        "config/vendor_aliases.json: alias {key:?} maps to both {existing:?} and {canonical:?} — aliases must be unique across vendors"
                    );
                }
            }
        }
        lookup
    })
}

/// Canonicalize a raw coder/reviewer agent identifier (e.g. the value of
/// `DARK_FACTORY_CODER_DEFAULT` / `DARK_FACTORY_REVIEWER_DEFAULT`) into a
/// skeptic-reviewer vendor bucket name recognized by
/// `tick::SKEPTIC_REVIEWER_PRIORITY` (`"claudem"`, `"claude"`, `"agy"`,
/// `"codex"`, `"gemini"`, `"cursor-agent"`), or `""` if unrecognized —
/// mirrors the old `_ => ""` fallback previously inlined in
/// `tick::skeptic_evidence`.
///
/// Matching is exact (trim + lowercase), never substring — see module doc
/// for why.
pub fn canonical_vendor(raw: &str) -> String {
    let key = raw.trim().to_ascii_lowercase();
    load_lookup().get(key.as_str()).cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{canonical_vendor, load_lookup};

    #[test]
    fn config_parses_and_is_non_empty() {
        assert!(!load_lookup().is_empty());
    }

    #[test]
    fn claudem_and_minimax_resolve_to_claudem() {
        assert_eq!(canonical_vendor("claudem"), "claudem");
        assert_eq!(canonical_vendor("minimax"), "claudem");
        // Case-insensitive, mirrors old `.to_ascii_lowercase()` behavior.
        assert_eq!(canonical_vendor("MiniMax"), "claudem");
        assert_eq!(canonical_vendor(" claudem "), "claudem");
    }

    #[test]
    fn bare_claude_resolves_to_claude_not_claudem() {
        assert_eq!(canonical_vendor("claude"), "claude");
    }

    #[test]
    fn agy_and_antigravity_resolve_to_agy() {
        assert_eq!(canonical_vendor("agy"), "agy");
        assert_eq!(canonical_vendor("antigravity"), "agy");
    }

    #[test]
    fn codex_and_gemini_resolve_directly() {
        assert_eq!(canonical_vendor("codex"), "codex");
        assert_eq!(canonical_vendor("gemini"), "gemini");
    }

    #[test]
    fn cursor_family_resolves_to_cursor_agent() {
        assert_eq!(canonical_vendor("cursor"), "cursor-agent");
        assert_eq!(canonical_vendor("cursor-agent"), "cursor-agent");
        assert_eq!(canonical_vendor("agentf"), "cursor-agent");
    }

    #[test]
    fn unrecognized_value_resolves_to_empty() {
        assert_eq!(canonical_vendor("some-unknown-vendor"), "");
        assert_eq!(canonical_vendor(""), "");
        assert_eq!(canonical_vendor("   "), "");
    }

    /// Proves the specific ordering hazard the bead names, rather than just
    /// asserting the new behavior in isolation. Because `"claudem"`
    /// contains `"claude"` as a substring, an ordered `.contains()` chain
    /// that happened to check the `"claude"` arm BEFORE the `"claudem"` arm
    /// would misclassify a claudem coder as plain `"claude"`. The chain
    /// that actually shipped in `tick::skeptic_evidence` avoided this only
    /// by hand-ordering the `"claudem"` check first (see the removed inline
    /// comment "claudem contains claude — check MiniMax aliases first").
    /// Simulate the REORDERED (buggy) variant of that same chain here to
    /// demonstrate the failure mode is real, then show the new exact-match
    /// lookup gets the same input right regardless of any such ordering.
    #[test]
    fn exact_match_is_immune_to_the_ordering_hazard_the_old_contains_chain_had() {
        fn buggy_reordered_contains_chain(agent: &str) -> &'static str {
            let a = agent.to_ascii_lowercase();
            // NOTE: "claude" arm checked BEFORE "claudem" arm on purpose —
            // this reproduces the hazard class the original inline comment
            // warned about; the shipped code had to avoid this exact
            // ordering by hand.
            if a.contains("claude") {
                "claude"
            } else if a.contains("claudem") || a.contains("minimax") {
                "claudem"
            } else {
                ""
            }
        }

        let tricky_input = "claudem";
        assert_eq!(
            buggy_reordered_contains_chain(tricky_input),
            "claude",
            "sanity check: reordering the substring chain really does misclassify claudem as claude"
        );
        assert_eq!(
            canonical_vendor(tricky_input),
            "claudem",
            "exact-match lookup must resolve claudem correctly regardless of arm/key ordering"
        );
    }
}
