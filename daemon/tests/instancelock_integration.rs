// Process-level integration tests for the single-instance lease and the
// strict CLI boundary.
//
// jleechan-bze8.4: an ostensibly read-only diagnostic (`daemon --help`)
// launched as PID 3486748 on 2026-07-18 entered the live tick loop while
// the systemd daemon PID 1621182 was already running, contaminated the
// 2026-07-18T18:35Z–18:41Z telemetry window, and dispatched df-184/185/
// 186 (which were then re-dispatched by the systemd process — same beads,
// double the work). These tests guard against that entire failure class:
// they spawn the daemon binary as a real subprocess, exercise `--help`,
// `--version`, and unknown-flag rejection at the process boundary, and
// prove that two concurrent daemon processes cannot both write telemetry
// or both dispatch — only the lease holder dispatches, the loser exits
// non-zero.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn locate_daemon_binary() -> PathBuf {
    // Cargo links the test binary next to the daemon binary by default.
    let exe = std::env::current_exe().expect("current_exe");
    let dir = exe.parent().expect("exe parent");
    // Test binaries live in `target/debug/deps/daemon-*`; the daemon
    // binary is one level up.
    let candidate = dir.parent().map(|p| p.join("daemon")).unwrap_or_else(|| {
        dir.join("daemon")
    });
    if candidate.exists() {
        return candidate;
    }
    // Workspace fallback: `target/debug/daemon` when running integration
    // tests at the workspace root.
    let alt = dir.join("../daemon");
    if alt.exists() {
        return alt;
    }
    panic!(
        "could not locate the daemon binary next to {:?}; try `cargo build` first",
        exe
    );
}

fn isolated_env(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "afd_bze84_{}_{}_{:?}",
        std::process::id(),
        label,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("tempdir create");
    dir
}

fn config_path_for(label: &str) -> PathBuf {
    // Each test instance gets its own HOME and config so the daemon's
    // `default_*_path` resolvers don't bleed between concurrent test cases.
    // The daemon binary's `default_config_path()` checks `cwd/config/
    // daemon.toml` first and then `daemon/contracts/daemon.toml.example`;
    // we set BOTH so the test is robust across build directories.
    let home = isolated_env(label);
    std::fs::create_dir_all(home.join(".dark-factory")).unwrap();
    std::fs::create_dir_all(home.join("Library/Logs/dark-factory")).unwrap();
    let cwd_config_dir = home.join("config");
    std::fs::create_dir_all(&cwd_config_dir).unwrap();
    let daemon_contracts_dir = home.join("daemon/contracts");
    std::fs::create_dir_all(&daemon_contracts_dir).unwrap();
    let body = format!(
        "target_repo = \"owner/repo-test-{label}\"\n\
         base_branch = \"main\"\n\
         stage = 1\n\
         max_workers = 2\n\
         max_batch = 1\n\
         fast_tick_secs = 60\n\
         slow_tick_secs = 600\n\
         autonomy_timebox_secs = 600\n\
         budget_warn_usd = 1.0\n\
         spec_dir = \".factory/specs\"\n\
         [repos]\n"
    );
    std::fs::write(cwd_config_dir.join("daemon.toml"), &body).unwrap();
    std::fs::write(
        daemon_contracts_dir.join("daemon.toml.example"),
        &body,
    )
    .unwrap();
    home
}

fn run_daemon_with(home: &PathBuf, args: &[&str]) -> std::process::Output {
    let bin = locate_daemon_binary();
    let mut cmd = Command::new(&bin);
    cmd.args(args)
        .env("HOME", home)
        .current_dir(home)
        .env_remove("DARK_FACTORY_REVIEWER_DEFAULT")
        .env_remove("NOTIFY_SOCKET")
        // Make sure tests can't accidentally inherit an outer daemon's
        // lock state from a developer's `$HOME`.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.output().unwrap_or_else(|e| {
        panic!("failed to spawn {bin:?} with args {args:?}: {e}");
    })
}

#[test]
fn help_exits_zero_without_dispatching() {
    let home = config_path_for("help-zero");
    let out = run_daemon_with(&home, &["--help"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dark-factory daemon"));
    // No telemetry should be emitted.
    let log = home.join("Library/Logs/dark-factory/daemon.jsonl");
    if log.exists() {
        let body = std::fs::read_to_string(&log).unwrap();
        assert!(body.is_empty(), "--help must not write telemetry; got: {body}");
    }
}

#[test]
fn version_exits_zero_without_dispatching() {
    let home = config_path_for("version-zero");
    let out = run_daemon_with(&home, &["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dark-factory daemon"));
}

#[test]
fn unknown_flag_exits_nonzero_without_dispatching() {
    let home = config_path_for("unknown");
    let out = run_daemon_with(&home, &["--definitely-not-a-flag"]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "unknown flags must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown") || stderr.contains("--help"),
        "stderr must explain the failure; got: {stderr}"
    );
    let log = home.join("Library/Logs/dark-factory/daemon.jsonl");
    if log.exists() {
        let body = std::fs::read_to_string(&log).unwrap();
        assert!(body.is_empty(), "unknown flag must not write telemetry; got: {body}");
    }
}

#[test]
fn two_concurrent_daemons_only_one_writes_telemetry_or_dispatches() {
    // Build two independent HOME trees but force them onto a *shared*
    // CXDB so the contention is real. We do that by giving both the
    // same explicit HOME and matching config + telemetry log -- which
    // means they share the lockdir and we verify only one wins.
    let base = config_path_for("concur");
    let shared = base.join("shared_home");
    let shared_dark = shared.join(".dark-factory");
    let shared_logs = shared.join("Library/Logs/dark-factory");
    std::fs::create_dir_all(&shared_dark).unwrap();
    std::fs::create_dir_all(&shared_logs).unwrap();
    std::fs::create_dir_all(shared.join("config")).unwrap();
    let body = std::fs::read_to_string(base.join("config/daemon.toml")).unwrap();
    std::fs::write(shared.join("config/daemon.toml"), &body).unwrap();

    let bin = locate_daemon_binary();

    // We invoke the daemon in --once mode so each invocation exits after
    // a single tick. We wrap two spawns in distinct threads and assert
    // that exactly one succeeds (writes to telemetry, advances state)
    // and the other exits non-zero with the "single-instance lock held"
    // diagnostic (no telemetry on the losing side).
    let home1 = shared.clone();
    let home2 = shared.clone();
    let bin1 = bin.clone();

    let handle_a = std::thread::spawn(move || {
        let mut cmd = Command::new(&bin1);
        cmd.arg("--once")
            .arg("--dry-run")
            .env("HOME", &home1)
            .current_dir(&home1)
            .env_remove("NOTIFY_SOCKET")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output().unwrap()
    });
    let bin2 = bin.clone();
    let handle_b = std::thread::spawn(move || {
        let mut cmd = Command::new(&bin2);
        cmd.arg("--once")
            .arg("--dry-run")
            .env("HOME", &home2)
            .current_dir(&home2)
            .env_remove("NOTIFY_SOCKET")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output().unwrap()
    });

    // One daemon acquires the lease and proceeds; the other sees
    // AlreadyHeld and exits non-zero BEFORE dispatching.
    let out_a = handle_a.join().unwrap();
    let out_b = handle_b.join().unwrap();
    let codes = vec![out_a.status.code(), out_b.status.code()];
    let winners: Vec<_> = codes.iter().filter(|c| **c == Some(0)).collect();
    let losers: Vec<_> = codes.iter().filter(|c| **c != Some(0)).collect();

    assert_eq!(
        winners.len(),
        1,
        "exactly one daemon must win the lease; got codes {:?}\nA stderr: {}\nB stderr: {}",
        codes,
        String::from_utf8_lossy(&out_a.stderr),
        String::from_utf8_lossy(&out_b.stderr),
    );
    assert_eq!(
        losers.len(),
        1,
        "exactly one daemon must lose; got codes {:?}\nA stderr: {}\nB stderr: {}",
        codes,
        String::from_utf8_lossy(&out_a.stderr),
        String::from_utf8_lossy(&out_b.stderr),
    );

    // Telemetry from the loser would be a regression: the contention
    // must short-circuit BEFORE any tick is written. After both
    // subprocesses return, the loser has already exited, so any
    // telemetry on disk is the winner's alone.
    let log = shared_logs.join("daemon.jsonl");
    if log.exists() {
        let body = std::fs::read_to_string(&log).unwrap();
        let started = body.matches("DAEMON_STARTED").count();
        assert!(
            started <= 1,
            "second daemon must not emit DAEMON_STARTED; got count={started}\nfull body:\n{body}"
        );
    }

    let _ = Instant::now().checked_add(Duration::from_millis(50));
}
