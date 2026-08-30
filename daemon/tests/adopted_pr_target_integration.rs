mod common;

use daemon::adapters::CliSessions;
use daemon::errors::DaemonError;
use daemon::tools::{Sessions, SpawnSpec};

#[test]
fn adopted_pr_rejects_sibling_worktree_before_spawn() {
    let temp_root = std::env::temp_dir().join(format!(
        "afd_adopted_pr_integration_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&temp_root);
    std::fs::create_dir_all(&temp_root).unwrap();

    let fake_bin = temp_root.join("bin");
    let managed_worktree = temp_root.join("managed-wa-worktree");
    let sibling_worktree = temp_root.join("sibling-wa-worktree");
    let kill_log = temp_root.join("kill.log");
    std::fs::create_dir_all(&fake_bin).unwrap();
    std::fs::create_dir_all(&managed_worktree).unwrap();
    std::fs::create_dir_all(&sibling_worktree).unwrap();

    // Initialize managed git worktree
    let _ = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&managed_worktree)
        .status();
    let _ = std::process::Command::new("git")
        .args(["remote", "add", "origin", "https://github.com/jleechanorg/worldarchitect.ai.git"])
        .current_dir(&managed_worktree)
        .status();
    let _ = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=Dark Factory Test",
            "commit",
            "--allow-empty",
            "-m",
            "initial wa commit",
        ])
        .current_dir(&managed_worktree)
        .status();
    let head_rev = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&managed_worktree)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    // Create fake ao script
    let fake_ao = fake_bin.join("ao");
    std::fs::write(
        &fake_ao,
        format!(
            r#"#!/usr/bin/env python3
import os
import sys

args = sys.argv[1:]
if args[:3] == ["status", "-p", "worldarchitect"]:
    print("[]")
    sys.exit(0)

if args[:2] == ["session", "kill"]:
    with open("{kill_log}", "a", encoding="utf-8") as f:
        f.write(" ".join(args) + "\n")
    sys.exit(0)

if args[0] == "spawn":
    return_wt = os.environ.get("TEST_RETURN_WORKTREE", "{managed}")
    print("SESSION=wa-session-3551")
    print(f"  Worktree: {{return_wt}}")
    print("  Branch:   factory/wa-3551-fix")
    sys.exit(0)

print(f"unknown command: {{args}}", file=sys.stderr)
sys.exit(1)
"#,
            kill_log = kill_log.display(),
            managed = managed_worktree.display(),
        ),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&fake_ao).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_ao, permissions).unwrap();
    }

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin.display(), original_path);

    let mut spec = SpawnSpec {
        bead_id: "wa-3551-repro".to_string(),
        branch: "factory/wa-3551-fix".to_string(),
        prompt: "remediate adopted PR".to_string(),
        repo: "jleechanorg/worldarchitect.ai".to_string(),
        ao_project: "worldarchitect".to_string(),
        remote: "worldai".to_string(),
        local_checkout: Some(managed_worktree.clone()),
        expected_revision: Some(head_rev.clone()),
        managed_checkout: true,
        expected_cwd: Some(managed_worktree.clone()),
    };

    let sessions = CliSessions::new("worldarchitect.ai", "minimax");

    // 1. NEGATIVE CONTROL: AO spawns into sibling worktree
    unsafe {
        std::env::set_var("PATH", &new_path);
        std::env::set_var("TEST_RETURN_WORKTREE", sibling_worktree.to_string_lossy().as_ref());
    }
    let res_sibling = sessions.spawn(&spec);
    assert!(res_sibling.is_err(), "spawn into sibling worktree must be rejected");
    let err = res_sibling.unwrap_err();
    let is_mismatch = match &err {
        DaemonError::WorktreeCwdMismatch { expected, actual } => {
            expected.contains("managed-wa-worktree") && actual.contains("sibling-wa-worktree")
        }
        DaemonError::SpawnFallbackExhausted(list) => {
            list.iter().any(|(_, e)| match e {
                DaemonError::WorktreeCwdMismatch { expected, actual } => {
                    expected.contains("managed-wa-worktree") && actual.contains("sibling-wa-worktree")
                }
                _ => false,
            })
        }
        DaemonError::Config(msg) => {
            msg.contains("working directory") || msg.contains("refusing to spawn")
        }
        _ => false,
    };
    assert!(is_mismatch, "error must report WorktreeCwdMismatch for sibling worktree drift, got: {err:?}");
    // Verify session was killed immediately
    let kills = std::fs::read_to_string(&kill_log).unwrap_or_default();
    assert!(kills.contains("session kill wa-session-3551"), "sibling session must be killed on drift");

    // 2. POSITIVE CONTROL: AO spawns into exact managed worktree and head revision
    let _ = std::fs::remove_file(&kill_log);
    unsafe {
        std::env::set_var("TEST_RETURN_WORKTREE", managed_worktree.to_string_lossy().as_ref());
    }
    let res_ok = sessions.spawn(&spec);
    assert!(res_ok.is_ok(), "exact managed target worktree must succeed, got: {res_ok:?}");
    assert_eq!(res_ok.unwrap().0, "wa-session-3551");

    // 3. NEGATIVE CONTROL: Stale/drifted expected revision fails closed
    spec.expected_revision = Some("deadbeef00000000000000000000000000000000".to_string());
    let res_drifted_sha = sessions.spawn(&spec);
    assert!(res_drifted_sha.is_err(), "stale expected revision must be rejected");

    unsafe {
        std::env::set_var("PATH", original_path);
        std::env::remove_var("TEST_RETURN_WORKTREE");
    }
    let _ = std::fs::remove_dir_all(&temp_root);
}
