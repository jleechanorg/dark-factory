use crate::errors::DaemonError;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

/// Process-wide daemon instance UUID (`"inst-<hex>"` style). Set exactly
/// once at startup (before any tick emits telemetry) and read by
/// [`instance_uuid`] for every emitted [`TelemetryEvent`].
///
/// jleechan-bze8.4: a single instance UUID per process is what makes
/// telemetry from 2026-07-18 traceable. Contamination from
/// 2026-07-18T18:35Z–18:41Z is excluded from autonomy evidence because the
/// duplicate diagnostic process had no UUID stamped at startup.
static INSTANCE_UUID: OnceLock<String> = OnceLock::new();

/// Stamp this process with its instance UUID. Idempotent: subsequent calls
/// are no-ops so a re-acquired lease after stale-recovery doesn't shadow
/// the original identifier.
pub fn set_instance_uuid(uuid: impl Into<String>) {
    let _ = INSTANCE_UUID.set(uuid.into());
}

/// Returns the instance UUID set by [`set_instance_uuid`], or `"none"` if
/// the daemon started in a context that never called set (e.g. the
/// `recover-held` subcommand, or unit tests).
pub fn instance_uuid() -> &'static str {
    INSTANCE_UUID.get().map(|s| s.as_str()).unwrap_or("none")
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
    // Stamp the per-process instance UUID onto every emitted line.
    // jleechan-bze8.4: this is what lets the daemon prove (in
    // post-incident review) which process emitted which telemetry, and
    // it is the missing field that confirmed
    // 2026-07-18T18:35Z–18:41Z contamination from a duplicate --help
    // invocation was independent of the systemd daemon (PID 1621182).
    let wrapped = serde_json::json!({
        "instanceUuid": instance_uuid(),
        "timestamp": ev.timestamp,
        "beadId": ev.bead_id,
        "attemptId": ev.attempt_id,
        "lifecycleState": ev.lifecycle_state,
        "eventType": ev.event_type,
        "metrics": ev.metrics,
        "context": ev.context,
    });
    let line = serde_json::to_string(&wrapped)
        .map_err(|e| DaemonError::Parse(e.to_string()))?;
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
            "instanceUuid",
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

    #[test]
    fn emit_stamps_instance_uuid_field() {
        crate::telemetry::set_instance_uuid("inst-test-stable");
        let dir = std::env::temp_dir().join("afd_tel_test_uuid");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon.jsonl");
        let _ = std::fs::remove_file(&log);
        let ev = TelemetryEvent {
            timestamp: "2026-07-18T18:35:00Z".into(),
            bead_id: "b1".into(),
            attempt_id: 1,
            lifecycle_state: "QUEUED".into(),
            event_type: "TICK".into(),
            metrics: serde_json::json!({}),
            context: serde_json::json!({"k":"v"}),
        };
        emit(&log, &ev).unwrap();
        let line = std::fs::read_to_string(&log).unwrap();
        assert!(line.contains("\"instanceUuid\":\"inst-test-stable\""));
    }
}
