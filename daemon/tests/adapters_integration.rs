use daemon::adapters::{ChainLlm, CliScm, CliSessions, CliTracker, CliVcs};
use daemon::tools::{Llm, Scm, Sessions, Tracker, Vcs};

#[test]
fn test_cli_vcs_real_git() {
    let vcs = CliVcs;
    let sha = vcs.base_head("main").expect("base_head main failed");
    assert_eq!(sha.len(), 40);

    let sha2 = vcs.head_sha("main").expect("head_sha main failed");
    assert_eq!(sha, sha2);

    let temp_branch = format!("temp-test-branch-{}", std::process::id());
    let _ = std::process::Command::new("git")
        .args(&["branch", "-D", &temp_branch])
        .status();

    vcs.create_branch_at(&temp_branch, &sha).expect("create_branch_at failed");

    let check_sha = vcs.head_sha(&temp_branch).expect("head_sha on temp branch failed");
    assert_eq!(sha, check_sha);

    let status = std::process::Command::new("git")
        .args(&["branch", "-D", &temp_branch])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore] // Requires real `br` CLI — run locally with `cargo test -- --ignored`
fn test_cli_tracker_real_br() {
    let tracker = CliTracker;
    let res = tracker.fetch_candidates();
    assert!(res.is_ok(), "fetch_candidates failed: {:?}", res);
}

#[test]
#[ignore] // Requires real `gh` CLI — run locally with `cargo test -- --ignored`
fn test_cli_scm_real_gh() {
    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let res = scm.labeled_issues("factory");
    assert!(res.is_ok(), "labeled_issues failed: {:?}", res);
}

#[test]
#[ignore] // Requires real `ao` CLI — run locally with `cargo test -- --ignored`
fn test_cli_sessions_real_ao() {
    let sessions = CliSessions::new("jleechanorg/dark-factory", "claude-code");
    let count = sessions.active_count();
    assert!(count.is_ok(), "active_count failed: {:?}", count);
}

#[test]
#[ignore] // Requires real codex/claude/agy CLIs — run locally with `cargo test -- --ignored`
fn test_chain_llm_real_fallback() {
    let llm = ChainLlm;
    let res = llm.judge("Respond with exactly: hello");
    assert!(res.is_ok(), "LLM judge failed: {:?}", res);
    let text = res.unwrap();
    assert!(!text.trim().is_empty());
}

#[test]
fn test_cli_scm_offline_fallback() {
    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let offline_dir = std::path::Path::new(".beads/offline");
    std::fs::create_dir_all(&offline_dir).unwrap();
    
    let pr_file = offline_dir.join("pr_9999.json");
    std::fs::write(&pr_file, r#"{
        "ci_success": true,
        "mergeable": true,
        "coderabbit_approved": false,
        "bugbot_error_count": 2,
        "unresolved_thread_count": 1,
        "head_sha": "abc123sha",
        "body": "offline body",
        "comments": [],
        "files": []
    }"#).unwrap();
    
    let snap = scm.pr_snapshot(9999).unwrap();
    assert_eq!(snap.head_sha, "abc123sha");
    assert_eq!(snap.body, "offline body");
    assert_eq!(snap.bugbot_error_count, 2);
    assert!(!snap.coderabbit_approved);
    
    let _ = std::fs::remove_file(pr_file);
}

