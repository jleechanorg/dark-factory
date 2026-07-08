use daemon::adapters::{ChainLlm, CliScm, CliSessions, CliTracker, CliVcs};
use daemon::tools::{Llm, Scm, Sessions, Tracker, Vcs};

#[test]
fn test_cli_vcs_real_git() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let vcs = CliVcs;
    let sha = vcs.base_head("main").expect("base_head main failed");
    assert_eq!(sha.len(), 40);

    let sha2 = vcs.head_sha("main").expect("head_sha main failed");
    assert_eq!(sha, sha2);

    let temp_branch = format!("temp-test-branch-{}", std::process::id());
    let _ = std::process::Command::new("git")
        .args(["branch", "-D", &temp_branch])
        .status();

    vcs.create_branch_at(&temp_branch, &sha).expect("create_branch_at failed");

    let check_sha = vcs.head_sha(&temp_branch).expect("head_sha on temp branch failed");
    assert_eq!(sha, check_sha);

    let status = std::process::Command::new("git")
        .args(["branch", "-D", &temp_branch])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore] // Requires real `br` CLI — run locally with `cargo test -- --ignored`
fn test_cli_tracker_real_br() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let tracker = CliTracker;
    let res = tracker.fetch_candidates();
    assert!(res.is_ok(), "fetch_candidates failed: {:?}", res);
}

#[test]
#[ignore] // Requires real `gh` CLI — run locally with `cargo test -- --ignored`
fn test_cli_scm_real_gh() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let res = scm.labeled_issues("factory");
    assert!(res.is_ok(), "labeled_issues failed: {:?}", res);
}

#[test]
#[ignore] // Requires real `ao` CLI — run locally with `cargo test -- --ignored`
fn test_cli_sessions_real_ao() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let sessions = CliSessions::new("dark-factory", "claude-code");
    let count = sessions.active_count();
    assert!(count.is_ok(), "active_count failed: {:?}", count);
}

#[test]
#[ignore] // Requires real codex/claude/agy CLIs — run locally with `cargo test -- --ignored`
fn test_chain_llm_real_fallback() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let llm = ChainLlm;
    let res = llm.judge("Respond with exactly: hello");
    assert!(res.is_ok(), "LLM judge failed: {:?}", res);
    let text = res.unwrap();
    assert!(!text.trim().is_empty());
}

#[test]
#[ignore] // Requires real `ao` CLI — run locally with `cargo test -- --ignored`
// Stage-2 prereq #1 (jleechan-hna3): CliSessions::attach is the entry point
// the spec's "attach, remediate, quiesce" lifecycle depends on. It used to be
// an unconditional stub that always returned Err — this test used to assert
// that stub behavior (gate self-certification: "no error" would trivially
// hold for any Err variant). Now that attach() is a real reverse lookup over
// `ao status --json` (branch -> SessionId), this asserts the ERROR PATH still
// behaves correctly: a branch nothing is tracking must still fail, with a
// specific, non-generic message (this string ends up directly in
// reroll.rs's `Held(format!("failed to attach to session: {e}"))`, which
// surfaces in HumanHeld telemetry a human reads).
fn test_cli_sessions_real_attach_errors_for_unknown_branch() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let sessions = CliSessions::new("dark-factory", "minimax");
    let bogus_branch = format!("no-such-branch-jleechan-hna3-{}", std::process::id());
    let result = sessions.attach(&bogus_branch, "smoke-test-bead");
    assert!(
        result.is_err(),
        "CliSessions::attach unexpectedly found a session for a branch that \
         cannot possibly be tracked ({bogus_branch}): {result:?}",
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains(&bogus_branch),
        "attach()'s not-found error should name the branch it searched for \
         (this string is surfaced directly to a human via reroll.rs's \
         HumanHeld telemetry), got: {msg}"
    );
    assert!(
        !msg.contains("disabled") && !msg.contains("unimplemented"),
        "attach() is regressing to the old always-stub error message: {msg}"
    );
}

#[test]
#[ignore] // Requires a real `ao` CLI with at least one active session — run locally with `cargo test -- --ignored`
// Companion to the error-path test above: proves the SUCCESS path is a
// genuine reverse lookup (branch -> SessionId) against ground truth pulled
// directly from `ao status --json`, not just "returns Ok sometimes". Also
// confirms the returned SessionId is real/usable by feeding it into
// `is_quiescent`, exactly as `reroll.rs` does immediately after `attach()`.
fn test_cli_sessions_real_attach_finds_session_by_branch() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let Some(candidates) = live_ao_sessions_with_branch() else {
        eprintln!("skipping: could not query/parse `ao status --json`");
        return;
    };
    let Some((expected_name, branch)) = candidates.into_iter().next() else {
        eprintln!("skipping: no active ao session with a branch found — nothing to attach to");
        return;
    };

    let sessions = CliSessions::new("dark-factory", "minimax");
    let result = sessions.attach(&branch, "smoke-test-bead-attach");
    assert!(
        result.is_ok(),
        "attach() failed to find the real, currently-tracked session for \
         branch '{branch}' (ground truth from `ao status --json` says it's \
         session '{expected_name}'): {result:?}"
    );
    let session_id = result.unwrap();
    assert_eq!(
        session_id.0, expected_name,
        "attach() returned the wrong SessionId for branch '{branch}'"
    );

    // reroll.rs feeds attach()'s return value straight into is_quiescent()
    // before stop() — confirm that round-trip doesn't error on a bogus id.
    let quiescent = sessions.is_quiescent(&session_id);
    assert!(
        quiescent.is_ok(),
        "is_quiescent() errored on attach()-returned SessionId {session_id:?}: {quiescent:?}"
    );
}

#[test]
#[ignore] // Requires a real `ao` CLI with >=2 active sessions on distinct branches — run locally with `cargo test -- --ignored`
// Mutation-test guard: a broken implementation that ignores `branch` and
// always returns e.g. the first entry in `ao status --json` would still
// pass `test_cli_sessions_real_attach_finds_session_by_branch` whenever the
// matching session happens to be first in the array. This test specifically
// targets the second (non-first) candidate so an "always return the first
// match" mutation is caught: it must return candidate #2's name, not #1's.
fn test_cli_sessions_real_attach_returns_matching_session_not_arbitrary_one() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let Some(candidates) = live_ao_sessions_with_branch() else {
        eprintln!("skipping: could not query/parse `ao status --json`");
        return;
    };
    if candidates.len() < 2 {
        eprintln!(
            "skipping: need >=2 active ao sessions with distinct branches to \
             prove attach() isn't just returning an arbitrary/first match \
             (found {})",
            candidates.len()
        );
        return;
    }

    let (name1, _branch1) = &candidates[0];
    let (name2, branch2) = &candidates[1];

    let sessions = CliSessions::new("dark-factory", "minimax");
    let result = sessions
        .attach(branch2, "smoke-test-bead-attach-2")
        .expect("attach() should find the second candidate's session");

    assert_eq!(
        &result.0, name2,
        "attach() should return the session matching branch2, not an arbitrary one"
    );
    assert_ne!(
        &result.0, name1,
        "attach() returned session #1's name for session #2's branch — looks \
         like it's ignoring `branch` and just returning the first match \
         (the exact bug class this test exists to catch)"
    );
}

/// Ground-truth helper shared by the real-`ao` attach tests: queries `ao
/// status --json` directly (independent of `CliSessions::attach`'s own
/// implementation) and returns `(name, branch)` pairs for every entry that
/// has both fields non-empty. Returns `None` if the command or its JSON
/// output can't be obtained/parsed at all (distinct from `Some(vec![])`,
/// which means "ao is reachable but nothing has a branch right now").
fn live_ao_sessions_with_branch() -> Option<Vec<(String, String)>> {
    let out = std::process::Command::new("ao")
        .args(["status", "--json"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout.find('[')?;
    let data: serde_json::Value = serde_json::from_str(&stdout[json_start..]).ok()?;
    let arr = data.as_array()?;
    Some(
        arr.iter()
            .filter_map(|entry| {
                let name = entry.get("name")?.as_str()?;
                let branch = entry.get("branch")?.as_str()?;
                if name.is_empty() || branch.is_empty() {
                    return None;
                }
                Some((name.to_string(), branch.to_string()))
            })
            .collect(),
    )
}

#[test]
fn test_cli_scm_offline_fallback() {
    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let offline_dir = std::path::Path::new(".beads/offline");
    std::fs::create_dir_all(offline_dir).unwrap();

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
