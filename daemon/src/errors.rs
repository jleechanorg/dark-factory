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
}
