use crate::errors::DaemonError;
use std::io::Write;
use std::path::Path;
use std::sync::RwLock;

static GLOBAL_INSTANCE_ID: RwLock<Option<String>> = RwLock::new(None);

pub fn set_instance_id(id: &str) {
    if let Ok(mut lock) = GLOBAL_INSTANCE_ID.write() {
        *lock = Some(id.to_string());
    }
}

pub fn get_instance_id() -> String {
    if let Ok(lock) = GLOBAL_INSTANCE_ID.read() {
        if let Some(ref id) = *lock {
            return id.clone();
        }
    }
    "00000000-0000-0000-0000-000000000000".to_string()
}

pub fn emit_startup(
    telemetry_log: &Path,
    lock: &crate::lock::DaemonLock,
) -> Result<(), DaemonError> {
    emit(
        telemetry_log,
        &TelemetryEvent {
            timestamp: crate::state::now_iso8601(),
            bead_id: "_daemon".to_string(),
            attempt_id: 0,
            lifecycle_state: "RUNNING".to_string(),
            event_type: "DAEMON_STARTED".to_string(),
            metrics: serde_json::json!({}),
            context: serde_json::json!({
                "instance_id": lock.metadata.instance_id,
                "pid": lock.metadata.pid,
                "executable_sha": lock.metadata.executable_sha,
                "config_identity": lock.metadata.config_identity,
                "lock_acquired": true,
                "lock_path": lock.path.display().to_string(),
            }),
        },
    )
}


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
