mod common;

use common::{FakeLlm, FakeScm, FakeStateStore, FakeTracker, FakeVcs};
use daemon::adapters::CliSessions;
use daemon::config::{Config, RepoConfig};
use daemon::dispatch::{self, DriveBranchDecision};
use daemon::errors::DaemonError;
use daemon::intake;
use daemon::router;
use daemon::tools::{Sessions, SpawnSpec};

static PROCESS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    saved: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let saved = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, saved }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

#[test]
fn adopted_pr_rejects_sibling_worktree_before_spawn() {
    let _env_lock = PROCESS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    let spawn_log = temp_root.join("spawn.log");
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

if args and args[0] == "spawn":
    with open("{spawn_log}", "a", encoding="utf-8") as f:
        f.write(" ".join(args) + "\n")
    return_wt = os.environ.get("TEST_RETURN_WORKTREE", "{managed}")
    print("SESSION=wa-session-3551")
    print(f"  Worktree: {{return_wt}}")
    print("  Branch:   factory/wa-3551-fix")
    sys.exit(0)

print(f"unknown command: {{args}}", file=sys.stderr)
sys.exit(1)
"#,
            kill_log = kill_log.display(),
            spawn_log = spawn_log.display(),
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

    let _path_guard = EnvVarGuard::set("PATH", &new_path);

    // 1. PRE-SPAWN NEGATIVE CONTROL: local_checkout points to sibling worktree (fails BEFORE AO spawn)
    spec.local_checkout = Some(sibling_worktree.clone());
    let _ = std::fs::remove_file(&spawn_log);
    let res_pre_spawn_sibling = sessions.spawn(&spec);
    assert!(res_pre_spawn_sibling.is_err(), "pre-spawn sibling local_checkout must be rejected");
    match res_pre_spawn_sibling.unwrap_err() {
        DaemonError::WorktreeCwdMismatch { expected, actual } => {
            assert!(expected.contains("managed-wa-worktree"));
            assert!(actual.contains("sibling-wa-worktree"));
        }
        DaemonError::SpawnFallbackExhausted(list) => {
            assert!(list.iter().any(|(_, e)| match e {
                DaemonError::WorktreeCwdMismatch { expected, actual } => {
                    expected.contains("managed-wa-worktree") && actual.contains("sibling-wa-worktree")
                }
                _ => false,
            }));
        }
        other => panic!("expected WorktreeCwdMismatch pre-spawn, got: {other:?}"),
    }
    assert!(!spawn_log.exists(), "pre-spawn rejection must execute ZERO AO spawns");

    // 2. PRE-SPAWN NEGATIVE CONTROL: Stale/drifted expected revision fails BEFORE AO spawn
    spec.local_checkout = Some(managed_worktree.clone());
    spec.expected_revision = Some("deadbeef00000000000000000000000000000000".to_string());
    let _ = std::fs::remove_file(&spawn_log);
    let res_drifted_sha = sessions.spawn(&spec);
    assert!(res_drifted_sha.is_err(), "stale expected revision must be rejected pre-spawn");
    assert!(!spawn_log.exists(), "stale revision rejection must execute ZERO AO spawns");

    // 3. POST-SPAWN DRIFT NEGATIVE CONTROL: AO spawn returns sibling worktree -> kills session immediately
    spec.expected_revision = Some(head_rev.clone());
    let _ = std::fs::remove_file(&kill_log);
    let _ = std::fs::remove_file(&spawn_log);
    let _return_worktree_guard = EnvVarGuard::set(
        "TEST_RETURN_WORKTREE",
        sibling_worktree.to_string_lossy().as_ref(),
    );
    let res_post_drift = sessions.spawn(&spec);
    assert!(res_post_drift.is_err(), "spawn returning sibling worktree must be rejected");
    match res_post_drift.unwrap_err() {
        DaemonError::WorktreeCwdMismatch { expected, actual } => {
            assert!(expected.contains("managed-wa-worktree"));
            assert!(actual.contains("sibling-wa-worktree"));
        }
        DaemonError::SpawnFallbackExhausted(list) => {
            assert!(list.iter().any(|(_, e)| match e {
                DaemonError::WorktreeCwdMismatch { expected, actual } => {
                    expected.contains("managed-wa-worktree") && actual.contains("sibling-wa-worktree")
                }
                _ => false,
            }));
        }
        other => panic!("expected WorktreeCwdMismatch post-spawn, got: {other:?}"),
    }
    let kills = std::fs::read_to_string(&kill_log).unwrap_or_default();
    assert!(kills.contains("session kill wa-session-3551"), "drifted session must be killed immediately");

    // 4. POSITIVE CONTROL: exact managed worktree and head revision succeeds cleanly
    let _ = std::fs::remove_file(&kill_log);
    let _ = std::fs::remove_file(&spawn_log);
    unsafe {
        std::env::set_var("TEST_RETURN_WORKTREE", managed_worktree.to_string_lossy().as_ref());
    }
    let res_ok = sessions.spawn(&spec);
    assert!(res_ok.is_ok(), "exact managed target worktree must succeed, got: {res_ok:?}");
    assert_eq!(res_ok.unwrap().0, "wa-session-3551");

    let _ = std::fs::remove_dir_all(&temp_root);
}

#[test]
fn adopted_pr_rejects_drift_before_ao_spawn() {
    let _env_lock = PROCESS_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let temp_root = std::env::temp_dir().join(format!(
        "afd_adopted_pr_intake_dispatch_{}_{}",
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
    let spawn_log = temp_root.join("spawn.log");
    std::fs::create_dir_all(&fake_bin).unwrap();
    std::fs::create_dir_all(&managed_worktree).unwrap();
    std::fs::create_dir_all(&sibling_worktree).unwrap();

    let init_repo = |path: &std::path::Path, remote: &str| {
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["remote", "add", "origin", remote])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
        let commit_message = format!("initial adopted PR commit for {remote}");
        assert!(std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Dark Factory Test",
                "commit",
                "--allow-empty",
                "-m",
                &commit_message,
            ])
            .current_dir(path)
            .status()
            .unwrap()
            .success());
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let adopted_head = init_repo(
        &managed_worktree,
        "https://github.com/jleechanorg/worldarchitect.ai.git",
    );
    let sibling_head = init_repo(
        &sibling_worktree,
        "https://github.com/other-owner/other-repo.git",
    );
    assert_ne!(adopted_head, sibling_head);

    let fake_ao = fake_bin.join("ao");
    std::fs::write(
        &fake_ao,
        format!(
            r#"#!/usr/bin/env python3
import sys

args = sys.argv[1:]
if args[:3] == ["status", "-p", "worldarchitect"]:
    print("[]")
    sys.exit(0)
if args and args[0] == "spawn":
    with open("{spawn_log}", "a", encoding="utf-8") as f:
        f.write(" ".join(args) + "\n")
    print("SESSION=wa-adopted-3551")
    print("  Worktree: {managed}")
    print("  Branch:   factory/adopted-pr-head")
    sys.exit(0)
print(f"unknown command: {{args}}", file=sys.stderr)
sys.exit(1)
"#,
            spawn_log = spawn_log.display(),
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

    let repo = "jleechanorg/worldarchitect.ai";
    let branch = "factory/adopted-pr-head";
    let cfg = Config {
        target_repo: repo.to_string(),
        ao_project: None,
        base_branch: "main".to_string(),
        stage: 1,
        max_workers: 40,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 600,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 0.0,
        spec_dir: ".factory/specs/".to_string(),
        reroll_head_stability_window_secs: 30,
        reroll_death_confirm_secs: 5,
        held_recheck_cooldown_secs: 900,
        repos: std::collections::HashMap::from([(
            repo.to_string(),
            RepoConfig {
                ao_project: "worldarchitect".to_string(),
                push_remote: "worldai".to_string(),
                local_checkout: Some(managed_worktree.clone()),
            },
        )]),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    };

    let mut scm = FakeScm::new();
    scm.prs.push(daemon::tools::LabeledPr {
        number: 3551,
        title: "adopt existing remediation PR".to_string(),
        body: "Keep remediation on the existing PR head.".to_string(),
        author_login: "alice".to_string(),
        external_ref: format!("{repo}#3551"),
        head_ref_name: branch.to_string(),
        is_cross_repository: false,
        head_repo_full_name: Some(repo.to_string()),
        head_repo_owner_login: Some("jleechanorg".to_string()),
        head_sha: Some(adopted_head.clone()),
        updated_at_epoch: Some(1_700_000_000),
    });
    scm.permissions.insert("alice".to_string(), daemon::tools::Permission::Write);
    let tracker = FakeTracker::new();
    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();
    assert!(outcomes.is_empty(), "the same-repo labeled PR must be adopted");
    assert_eq!(adopted.len(), 1);
    assert_eq!(adopted[0].head_ref_name, branch);
    assert_eq!(adopted[0].head_sha.as_deref(), Some(adopted_head.as_str()));
    assert_eq!(adopted[0].repo, repo);

    let bead = tracker
        .candidates
        .borrow()
        .iter()
        .find(|bead| bead.id == adopted[0].bead_id)
        .cloned()
        .expect("intake-created adopted bead must be routed");
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"STANDARD_PATH","justification":"existing PR remediation"}"#
            .to_string(),
    ));
    let verdict = router::route(&llm, &bead).unwrap();
    assert_eq!(verdict, daemon::router::RoutingVerdict::StandardPath);

    let ready = vec![(
        bead,
        verdict,
        DriveBranchDecision::PrHead(branch.to_string()),
    )];
    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin.display(), original_path);
    let _path_guard = EnvVarGuard::set("PATH", &new_path);
    let sessions = CliSessions::new(repo, "minimax");

    // The public dispatch path pins the PR head SHA in SpawnSpec. A stale
    // VCS answer then fails in CliSessions' pre-spawn target validation, so
    // the fake AO spawn command is never reached.
    let stale_sha = "deadbeef00000000000000000000000000000000";
    let mut stale_vcs = FakeVcs::new();
    stale_vcs
        .heads
        .insert(format!("{repo}@{branch}"), stale_sha.to_string());
    let stale_store = FakeStateStore::new();
    let stale_report = dispatch::dispatch_ready_with_vcs(
        &sessions,
        &stale_store,
        &cfg,
        &ready,
        Some(&stale_vcs),
    )
    .unwrap();
    assert_eq!(stale_report.success_count(), 0);
    assert_eq!(stale_report.failures.len(), 1);
    assert_eq!(stale_report.failures[0].phase, "spawn_failed");
    assert!(
        stale_report.failures[0].error.contains(stale_sha),
        "stale revision must be the pre-spawn rejection, got: {}",
        stale_report.failures[0].error
    );
    assert!(!spawn_log.exists(), "stale adopted head must execute ZERO AO spawns");
    assert!(stale_vcs
        .calls
        .borrow()
        .iter()
        .any(|call| call == &format!("head_sha_within_for_repo({repo},{branch},30)")));
    let stale_overlay = stale_store
        .overlays
        .borrow()
        .get(&adopted[0].bead_id)
        .cloned()
        .expect("dispatch must persist the pre-spawn adopted overlay");
    assert_eq!(stale_overlay.branch.as_deref(), Some(branch));
    assert_eq!(stale_overlay.pre_session_head_sha.as_deref(), Some(stale_sha));

    // A sibling checkout with a different remote is likewise rejected by the
    // same dispatch-produced SpawnSpec before any AO spawn can occur.
    let mut sibling_cfg = cfg.clone();
    sibling_cfg
        .repos
        .get_mut(repo)
        .unwrap()
        .local_checkout = Some(sibling_worktree);
    let mut sibling_vcs = FakeVcs::new();
    sibling_vcs
        .heads
        .insert(format!("{repo}@{branch}"), adopted_head);
    let sibling_store = FakeStateStore::new();
    let sibling_report = dispatch::dispatch_ready_with_vcs(
        &sessions,
        &sibling_store,
        &sibling_cfg,
        &ready,
        Some(&sibling_vcs),
    )
    .unwrap();
    assert_eq!(sibling_report.success_count(), 0);
    assert_eq!(sibling_report.failures.len(), 1);
    assert_eq!(sibling_report.failures[0].phase, "spawn_failed");
    assert!(
        sibling_report.failures[0].error.contains("other-owner/other-repo"),
        "sibling remote drift must be rejected before spawn, got: {}",
        sibling_report.failures[0].error
    );
    assert!(!spawn_log.exists(), "sibling checkout drift must execute ZERO AO spawns");
    let sibling_overlay = sibling_store
        .overlays
        .borrow()
        .get(&adopted[0].bead_id)
        .cloned()
        .expect("sibling rejection must persist the adopted overlay");
    assert_eq!(sibling_overlay.branch.as_deref(), Some(branch));

    let _ = std::fs::remove_dir_all(&temp_root);
}
