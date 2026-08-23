use crate::errors::DaemonError;
use std::io::Write;
use std::path::Path;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEvent {
    pub timestamp: String,
    pub bead_id: String,
    pub attempt_id: u32,
    pub lifecycle_state: String,
    pub event_type: String,
    pub metrics: serde_json::Value,
    pub context: serde_json::Value,
}

pub fn default_telemetry_log() -> std::path::PathBuf {
    std::env::var_os("DARK_FACTORY_TELEMETRY_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            Path::new(&home)
                .join("Library/Logs/dark-factory")
                .join("daemon.jsonl")
        })
}

pub fn emit(log_path: &Path, ev: &TelemetryEvent) -> Result<(), DaemonError> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| DaemonError::Config(format!("telemetry open: {e}")))?;
    let line = serde_json::to_string(ev).map_err(|e| DaemonError::Parse(e.to_string()))?;
    writeln!(f, "{line}").map_err(|e| DaemonError::Config(format!("telemetry write: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emits_schema_compliant_jsonl() {
        let dir = std::env::temp_dir().join("afd_tel_test");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon.jsonl");
        let _ = std::fs::remove_file(&log);
        let ev = TelemetryEvent {
            timestamp: "2026-07-03T19:00:00Z".into(),
            bead_id: "b1".into(),
            attempt_id: 1,
            lifecycle_state: "QUEUED".into(),
            event_type: "INTAKE_BEAD_CREATED".into(),
            metrics: serde_json::json!({}),
            context: serde_json::json!({"title":"t"}),
        };
        emit(&log, &ev).unwrap();
        emit(&log, &ev).unwrap();
        let body = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        for key in [
            "timestamp",
            "beadId",
            "attemptId",
            "lifecycleState",
            "eventType",
            "metrics",
            "context",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }
}
