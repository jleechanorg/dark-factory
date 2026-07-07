#[derive(thiserror::Error, Debug)]
pub enum DaemonError {
    #[error("tool {tool} failed (rc={rc}): {stderr}")]
    Tool { tool: String, rc: i32, stderr: String },
    #[error("parse: {0}")]
    Parse(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("config: {0}")]
    Config(String),
}

impl DaemonError {
    pub fn is_transient(&self) -> bool {
        matches!(self, DaemonError::Tool { .. } | DaemonError::Timeout(_))
    }

    /// Detects `br create --external-ref ...` failing because the ref is
    /// already tracked (`br`'s own uniqueness constraint on `external_ref`),
    /// e.g. `Error: Configuration error: External reference 'owner/repo#42'
    /// already exists on issue jleechan-abcd`.
    ///
    /// This is a *write-time* duplicate signal, distinct from (and more
    /// authoritative than) the *read-time* `known_refs.contains(..)`
    /// pre-check in `intake::normalize` / `intake::normalize_labeled_prs`:
    /// `br create`'s own uniqueness check queries the durable store directly
    /// at write time, so it cannot suffer whatever staleness/pagination-skew
    /// affects a preceding bulk `br list` snapshot (jleechan-u4gb — the
    /// pre-check occasionally missed a ref that a concurrent `br create`
    /// correctly rejected as a duplicate seconds later). Treating this error
    /// shape as "already tracked, not a failure" makes the create-bead path
    /// idempotent by construction instead of relying solely on a racy read,
    /// and stops it from killing the whole tick with an exponential backoff
    /// retry loop that can never succeed (the ref will *always* already
    /// exist on retry).
    ///
    /// Returns the existing bead id parsed out of the error message, if the
    /// error matches this specific shape.
    pub fn duplicate_external_ref_bead_id(&self) -> Option<String> {
        let DaemonError::Tool { stderr, .. } = self else {
            return None;
        };
        let marker = "already exists on issue";
        let idx = stderr.find(marker)?;
        let rest = stderr[idx + marker.len()..].trim_start();
        let id = rest
            .split(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .find(|tok| !tok.is_empty())?;
        Some(id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_tool_and_timeout_as_transient() {
        let tool = DaemonError::Tool {
            tool: "gh".to_string(),
            rc: 1,
            stderr: "temporary unavailable".to_string(),
        };
        let timeout = DaemonError::Timeout("ao status timed out".to_string());

        assert!(tool.is_transient());
        assert!(timeout.is_transient());
    }

    #[test]
    fn classifies_config_and_parse_as_fatal() {
        assert!(!DaemonError::Config("missing config".to_string()).is_transient());
        assert!(!DaemonError::Parse("bad json".to_string()).is_transient());
    }

    #[test]
    fn duplicate_external_ref_bead_id_parses_real_br_error() {
        let err = DaemonError::Tool {
            tool: "br".to_string(),
            rc: 7,
            stderr: "Error: Configuration error: External reference 'jleechanorg/worldarchitect.ai#8227' already exists on issue jleechan-vj89\n".to_string(),
        };
        assert_eq!(
            err.duplicate_external_ref_bead_id(),
            Some("jleechan-vj89".to_string())
        );
    }

    #[test]
    fn duplicate_external_ref_bead_id_none_for_unrelated_tool_error() {
        let err = DaemonError::Tool {
            tool: "br".to_string(),
            rc: 1,
            stderr: "some other failure".to_string(),
        };
        assert_eq!(err.duplicate_external_ref_bead_id(), None);
    }

    #[test]
    fn duplicate_external_ref_bead_id_none_for_non_tool_error() {
        let err = DaemonError::Timeout("br list timed out".to_string());
        assert_eq!(err.duplicate_external_ref_bead_id(), None);
    }
}
