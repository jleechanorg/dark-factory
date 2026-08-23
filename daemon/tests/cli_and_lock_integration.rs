use daemon::lock::DaemonLock;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn get_daemon_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_daemon"))
}

fn create_test_config(dir: &std::path::Path) -> PathBuf {
    let cfg_path = dir.join("test_daemon.toml");
    let cfg_content = r#"
target_repo = "jleechanorg/dark-factory"
base_branch = "main"
stage = 1
max_workers = 5
max_batch = 2
fast_tick_secs = 10
slow_tick_secs = 60
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs"
pre_gate_validation_enabled = false
escalation_refire_secs = 3600
"#;
    fs::write(&cfg_path, cfg_content).unwrap();
    cfg_path
}

#[test]
fn test_daemon_help_and_h_flags() {
    let bin = get_daemon_bin();

    let output = Command::new(&bin).arg("--help").output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Dark Factory daemon"));
    assert!(stdout.contains("Usage: daemon [OPTIONS] [COMMAND]"));
    assert!(stdout.contains("--once"));
    assert!(stdout.contains("--dry-run"));

    let output_h = Command::new(&bin).arg("-h").output().unwrap();
    assert!(output_h.status.success());
    assert_eq!(output_h.status.code(), Some(0));
    let stdout_h = String::from_utf8_lossy(&output_h.stdout);
    assert!(stdout_h.contains("Usage: daemon [OPTIONS] [COMMAND]"));
}

#[test]
fn test_daemon_version_and_v_flags() {
    let bin = get_daemon_bin();

    let output = Command::new(&bin).arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("daemon 0.1.0"));

    let output_v = Command::new(&bin).arg("-V").output().unwrap();
    assert!(output_v.status.success());
    assert_eq!(output_v.status.code(), Some(0));
    let stdout_v = String::from_utf8_lossy(&output_v.stdout);
    assert!(stdout_v.contains("daemon 0.1.0"));
}

#[test]
fn test_daemon_subcommand_help() {
    let bin = get_daemon_bin();

    let output_rec = Command::new(&bin)
        .args(["recover-held", "--help"])
        .output()
        .unwrap();
    assert!(output_rec.status.success());
    assert_eq!(output_rec.status.code(), Some(0));
    let stdout_rec = String::from_utf8_lossy(&output_rec.stdout);
    assert!(stdout_rec.contains("Usage: daemon recover-held --db <PATH> --telemetry-log <PATH>"));

    let output_gates = Command::new(&bin)
        .args(["gates-compute", "-h"])
        .output()
        .unwrap();
    assert!(output_gates.status.success());
    assert_eq!(output_gates.status.code(), Some(0));
    let stdout_gates = String::from_utf8_lossy(&output_gates.stdout);
    assert!(stdout_gates.contains("Usage: daemon gates-compute --pr <PR_NUMBER>"));
}

#[test]
fn test_daemon_unknown_argument_rejected_nonzero() {
    let bin = get_daemon_bin();

    let output = Command::new(&bin).arg("--bogus-argument").output().unwrap();
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("auto-factory daemon: args: Unknown argument: --bogus-argument"));

    let output_rec = Command::new(&bin)
        .args(["recover-held", "--bogus"])
        .output()
        .unwrap();
    assert!(!output_rec.status.success());
    assert_eq!(output_rec.status.code(), Some(1));
    let stderr_rec = String::from_utf8_lossy(&output_rec.stderr);
    assert!(stderr_rec.contains("Unknown argument for recover-held: --bogus"));

    let output_gates = Command::new(&bin)
        .args(["gates-compute", "--bogus"])
        .output()
        .unwrap();
    assert!(!output_gates.status.success());
    assert_eq!(output_gates.status.code(), Some(1));
    let stderr_gates = String::from_utf8_lossy(&output_gates.stderr);
    assert!(stderr_gates.contains("Unknown argument for gates-compute: --bogus"));
}

#[test]
fn test_concurrent_daemon_processes_single_instance_lock() {
    let temp_dir = std::env::temp_dir().join(format!("df_cli_lock_test_{}", std::process::id()));
    fs::create_dir_all(&temp_dir).unwrap();

    let bin = get_daemon_bin();
    let cfg_path = create_test_config(&temp_dir);
    let db_path = temp_dir.join("test_cxdb.sqlite");
    let lock_path = db_path.with_extension("lock");
    let tel_path = temp_dir.join("test_telemetry.jsonl");

    // 1. Process 1 (simulated by this test holding the lock) acquires lock on lock_path
    let lock1 = DaemonLock::acquire(&lock_path, "jleechanorg/dark-factory").unwrap();
    let owner_pid = lock1.metadata.pid;
    assert_eq!(owner_pid, std::process::id());

    // 2. Process 2 attempts to run daemon pointing to the same db
    let p2_output = Command::new(&bin)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--db",
            db_path.to_str().unwrap(),
            "--telemetry-log",
            tel_path.to_str().unwrap(),
            "--once",
            "--dry-run",
        ])
        .output()
        .unwrap();

    // Process 2 must exit nonzero
    assert!(!p2_output.status.success());
    assert_eq!(p2_output.status.code(), Some(1));
    let p2_stderr = String::from_utf8_lossy(&p2_output.stderr);
    assert!(p2_stderr.contains("lock acquisition failed"));
    assert!(p2_stderr.contains(&format!("already locked by PID {owner_pid}")));

    // Process 2 must not have written any telemetry
    assert!(!tel_path.exists() || fs::read_to_string(&tel_path).unwrap().is_empty());

    // 3. Process 1 releases lock (simulating termination/crash recovery)
    drop(lock1);

    // 4. Process 3 runs and successfully acquires lock, executes one tick, and emits telemetry
    let p3_output = Command::new(&bin)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--db",
            db_path.to_str().unwrap(),
            "--telemetry-log",
            tel_path.to_str().unwrap(),
            "--once",
            "--dry-run",
        ])
        .output()
        .unwrap();

    assert!(
        p3_output.status.success(),
        "Process 3 failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&p3_output.stdout),
        String::from_utf8_lossy(&p3_output.stderr)
    );
    assert_eq!(p3_output.status.code(), Some(0));

    // Telemetry must exist and contain both DAEMON_STARTED and TICK events with valid instance UUID
    assert!(tel_path.exists());
    let tel_content = fs::read_to_string(&tel_path).unwrap();
    let lines: Vec<&str> = tel_content.lines().collect();
    assert!(lines.len() >= 2);

    let started_ev: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(started_ev["eventType"], "DAEMON_STARTED");
    let instance_id = started_ev["context"]["instance_id"].as_str().unwrap();
    assert_eq!(instance_id.len(), 36);
    assert_eq!(started_ev["context"]["lock_acquired"], true);

    let tick_ev: serde_json::Value = serde_json::from_str(lines[lines.len() - 1]).unwrap();
    assert_eq!(tick_ev["eventType"], "TICK");
    assert_eq!(tick_ev["context"]["instance_id"], instance_id);

    let _ = fs::remove_dir_all(&temp_dir);
}
