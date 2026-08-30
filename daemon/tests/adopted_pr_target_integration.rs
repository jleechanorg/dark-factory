mod common;

use daemon::errors::DaemonError;
use daemon::tools::SpawnSpec;

#[test]
fn adopted_pr_rejects_sibling_worktree_before_spawn() {
    let temp_root = std::env::temp_dir().join(format!(
        "afd_adopted_pr_target_integration_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&temp_root);
    std::fs::create_dir_all(&temp_root).unwrap();

    let managed_worktree = temp_root.join("managed-wa-worktree");
    let sibling_worktree = temp_root.join("sibling-wa-worktree");
    std::fs::create_dir_all(&managed_worktree).unwrap();
    std::fs::create_dir_all(&sibling_worktree).unwrap();

    // Negative control: spec expects managed_worktree as cwd, but session was spawned at sibling_worktree
    let spec = SpawnSpec {
        bead_id: "wa-3551-repro".to_string(),
        branch: "wa-3551-fix".to_string(),
        prompt: "remediate adopted PR".to_string(),
        repo: "jleechanorg/worldarchitect.ai".to_string(),
        ao_project: "worldarchitect".to_string(),
        remote: "worldai".to_string(),
        local_checkout: Some(managed_worktree.clone()),
        expected_revision: None,
        managed_checkout: true,
        expected_cwd: Some(managed_worktree.clone()),
    };

    // Verify negative control fails when actual workspace is sibling_worktree
    let result = daemon::tools::check_cwd_guard(
        spec.expected_cwd.as_deref(),
        &sibling_worktree,
    );
    assert!(result.is_err(), "sibling worktree must be rejected before spawn/coding");
    let err = result.unwrap_err();
    match err {
        DaemonError::WorktreeCwdMismatch { expected, actual } => {
            assert!(expected.contains("managed-wa-worktree"));
            assert!(actual.contains("sibling-wa-worktree"));
        }
        other => panic!("expected DaemonError::WorktreeCwdMismatch, got: {other:?}"),
    }

    // Positive control: when actual workspace matches expected_cwd exactly, guard passes
    let ok_res = daemon::tools::check_cwd_guard(
        spec.expected_cwd.as_deref(),
        &managed_worktree,
    );
    assert!(ok_res.is_ok(), "exact managed worktree must pass target binding guard");

    let _ = std::fs::remove_dir_all(&temp_root);
}
