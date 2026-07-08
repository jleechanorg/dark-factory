#![allow(clippy::zombie_processes)]

//! Integration test for the `--child-run-er` argv routing.
//!
//! This test exists because bead `jleechan-2xlo` reported that without the
//! `--child-run-er` handler in `main.rs::parse_args`, the flag fell through
//! to `CommandMode::Daemon(Args::default())` — which runs an infinite tick
//! loop (no `--once`). This caused a fork bomb: every dispatch from the
//! parent spawned a new daemon instance that never exited.
//!
//! The test binary path (`is_test_binary()`) deliberately short-circuits
//! forking when running under `cargo test` (`target/debug/deps/...` path or
//! `--list`/`--exact` flags), which is exactly what let the original bug
//! through undetected — a pure in-process unit test of `run_child_main`'s
//! logic never exercises `main.rs::parse_args`'s routing at all.
//!
//! Spawning the ACTUAL compiled binary (`CARGO_BIN_EXE_daemon`) exercises the
//! production `main()` entry point end-to-end, including the argv-parsing
//! routing bug this PR fixes.
#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Timeout for the child process to exit. Without the fix, it would hang
/// forever (infinite tick loop). With the fix, it should exit within seconds.
const CHILD_TIMEOUT_SECS: u64 = 20;

#[test]
fn child_run_er_routes_correctly_and_exits() {
    // 1. Create fake bin directory with fake `claude` and `gh` executables.
    let fake_bin_dir = std::env::temp_dir().join(format!("fake_bin_{}", std::process::id()));
    fs::create_dir_all(&fake_bin_dir).expect("create temp dir for fake bin");
    let fake_claude_path = fake_bin_dir.join("claude");
    let fake_gh_path = fake_bin_dir.join("gh");

    // Fake `claude` that returns a PASS verdict (short-circuits real LLM work).
    fs::write(
        &fake_claude_path,
        "#!/bin/sh\necho '/er PASS fake-test-verdict'\n",
    )
    .expect("write fake claude script");
    fs::write(
        &fake_gh_path,
        "#!/bin/sh\nexit 0\n",
    )
    .expect("write fake gh script");

    // Make them executable.
    fs::set_permissions(&fake_claude_path, fs::Permissions::from_mode(0o755))
        .expect("chmod fake claude");
    fs::set_permissions(&fake_gh_path, fs::Permissions::from_mode(0o755))
        .expect("chmod fake gh");

    // 2. Create a fresh HOME dir so `spawn_reviewer`'s check for
    // `$HOME/.nvm/versions/node/v22.22.0/bin/claude` does NOT find a real
    // binary, forcing the PATH lookup to resolve to our fake script.
    let fake_home_dir = std::env::temp_dir().join(format!("fake_home_{}", std::process::id()));
    fs::create_dir_all(&fake_home_dir).expect("create temp dir for fake home");

    // 3. Spawn the daemon binary with `--child-run-er ...` args.
    let daemon_bin = env!("CARGO_BIN_EXE_daemon");
    let mut cmd = Command::new(daemon_bin);
    cmd.arg("--child-run-er")
        .arg("--bead-id")
        .arg("test-bead-1")
        .arg("--pr")
        .arg("1")
        .arg("--now-epoch")
        .arg("1000000")
        .arg("--target-repo")
        .arg("test-owner/test-repo")
        // Set PATH to prefer our fake binaries.
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fake_bin_dir.display()),
        )
        // Set HOME to the fake home dir.
        .env("HOME", fake_home_dir.display().to_string())
        // Capture stderr for assertions.
        .stderr(Stdio::piped())
        .stdout(Stdio::null());

    let mut child = cmd.spawn().expect("spawn daemon with --child-run-er");

    // 4. Poll for exit with timeout — this is the critical assertion.
    // Without the fix, this would hang forever (infinite tick loop).
    let start = Instant::now();
    let mut exited = false;
    let mut stderr_output = String::new();

    while start.elapsed() < Duration::from_secs(CHILD_TIMEOUT_SECS) {
        match child.try_wait().expect("try_wait") {
            Some(_status) => {
                // Child exited!
                exited = true;

                // Collect stderr.
                if let Some(stderr) = child.stderr.take() {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines() {
                        stderr_output.push_str(&line.unwrap_or_default());
                        stderr_output.push('\n');
                    }
                }
                break;
            }
            None => {
                // Still running, sleep briefly and check again.
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    // If we timed out, the child is still running — kill it and fail.
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "Child process did NOT exit within {}s — this means \
             --child-run-er fell through to CommandMode::Daemon which runs \
             an infinite tick loop (the original bug jleechan-2xlo)",
            CHILD_TIMEOUT_SECS
        );
    }

    // 5. Assert stderr contains the expected markers from run_child_main.
    assert!(
        stderr_output.contains("er_runner[child]: start"),
        "stderr should contain 'er_runner[child]: start', got:\n{}",
        stderr_output
    );
    assert!(
        stderr_output.contains("er_runner[child]: done"),
        "stderr should contain 'er_runner[child]: done', got:\n{}",
        stderr_output
    );

    // 6. Cleanup (best-effort).
    let _ = fs::remove_dir_all(&fake_bin_dir);
    let _ = fs::remove_dir_all(&fake_home_dir);
}
