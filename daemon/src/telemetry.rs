use crate::errors::DaemonError;
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEvent {
    pub timestamp: String,
    /// Host that emitted this event (rev-377j4: the 2026-08-23 PR-merge-storm
    /// postmortem took 1+ hour of cross-host detective work to establish
    /// which host performed a batch of unattended merges, because no
    /// telemetry event carried any host identity at all).
    pub host: String,
    pub bead_id: String,
    pub attempt_id: u32,
    pub lifecycle_state: String,
    pub event_type: String,
    pub metrics: serde_json::Value,
    pub context: serde_json::Value,
}

static LOCAL_HOSTNAME: OnceLock<String> = OnceLock::new();

/// Best-effort local hostname for telemetry/merge-provenance, cached once
/// per process so repeated telemetry emission never repeatedly shells out.
/// Deliberately avoids adding a new external crate dependency (neither
/// `hostname` nor `gethostname` is already a daemon dependency): tries
/// `$HOSTNAME` first, then the `hostname` binary, and falls back to the
/// literal string "unknown" rather than failing.
pub fn local_hostname() -> String {
    LOCAL_HOSTNAME
        .get_or_init(|| {
            if let Ok(h) = std::env::var("HOSTNAME") {
                let h = h.trim().to_string();
                if !h.is_empty() {
                    return h;
                }
            }
            if let Ok(out) = std::process::Command::new("hostname").output() {
                if out.status.success() {
                    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
            "unknown".to_string()
        })
        .clone()
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
            host: "test-host".into(),
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
            "host",
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
    fn emitted_event_host_matches_local_hostname_value() {
        // rev-377j4: prove the `host` field round-trips through JSON with
        // exactly the value `local_hostname()` computes, not a placeholder.
        let dir = std::env::temp_dir().join("afd_tel_test_host");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon.jsonl");
        let _ = std::fs::remove_file(&log);
        let host = local_hostname();
        let ev = TelemetryEvent {
            timestamp: "2026-08-23T00:00:00Z".into(),
            host: host.clone(),
            bead_id: "b1".into(),
            attempt_id: 1,
            lifecycle_state: "QUEUED".into(),
            event_type: "MERGE_PROVENANCE_TEST".into(),
            metrics: serde_json::json!({}),
            context: serde_json::json!({}),
        };
        emit(&log, &ev).unwrap();
        let body = std::fs::read_to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(v["host"].as_str().unwrap(), host);
        let _ = std::fs::remove_file(&log);
    }

    #[test]
    fn local_hostname_is_non_empty_and_stable_across_calls() {
        // rev-377j4: OnceLock caching must not change the value between
        // calls within the same process, and the fallback chain must never
        // produce an empty string (empty provenance is as useless as none).
        let first = local_hostname();
        let second = local_hostname();
        assert!(!first.is_empty(), "local_hostname() must never be empty");
        assert_eq!(first, second, "cached hostname must be stable per-process");
    }
}
