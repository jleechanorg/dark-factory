//! Session-health fatal marker list — parsed from
//! `config/session_health_markers.json`.
//!
//! Bead rev-cbzll: `adapters::parse_session_health_pane` used to hand-maintain
//! this list as an inline `&[&str]` literal with no test pinning it to
//! vendors' actual banner wording. Moving it to a versioned JSON config
//! mirrors the `config/skeptic_reviewer_priority.json` /
//! `daemon/src/reviewer_priority.rs` pattern used elsewhere in this crate.
//!
//! ponytail: this remains pane-text keyword scraping — a marker not
//! exercised here is a silent false-negative the next time a vendor CLI
//! changes its banner text. The durable upgrade path is self-reported
//! health from the coder process itself rather than scraping tmux output.

use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
struct SessionHealthMarkersConfig {
    markers: Vec<String>,
}

const CONFIG_JSON: &str = include_str!("../../config/session_health_markers.json");

fn load_markers() -> &'static [String] {
    static MARKERS: OnceLock<Vec<String>> = OnceLock::new();
    MARKERS.get_or_init(|| {
        let cfg: SessionHealthMarkersConfig = serde_json::from_str(CONFIG_JSON)
            .expect("invalid config/session_health_markers.json");
        assert!(
            !cfg.markers.is_empty(),
            "session_health_markers.markers must be non-empty"
        );
        cfg.markers
    })
}

/// Fatal auth/quota/error substrings scanned in tmux pane text by
/// `adapters::parse_session_health_pane`. Shared, versioned config —
/// see the module doc comment above.
pub fn session_health_markers() -> &'static [String] {
    load_markers()
}

#[cfg(test)]
mod tests {
    use super::session_health_markers;

    #[test]
    fn config_parses_and_yields_fourteen_markers() {
        let markers = session_health_markers();
        assert_eq!(
            markers.len(),
            14,
            "expected exactly 14 markers, got {}: {markers:?}",
            markers.len()
        );
    }

    #[test]
    fn markers_are_lowercase_and_non_empty() {
        for marker in session_health_markers() {
            assert!(!marker.is_empty(), "marker must be non-empty");
            assert_eq!(
                marker.to_ascii_lowercase(),
                *marker,
                "marker {marker:?} must already be lowercase — parse_session_health_pane matches against a lowercased pane"
            );
        }
    }
}
