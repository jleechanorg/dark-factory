use daemon::adapters::{ChainLlm, CliScm, CliSessions, CliTracker, CliVcs};
use daemon::tools::{Llm, Scm, Sessions, SpawnSpec, Tracker, Vcs};

/// Guard for setting environment variables during tests.
/// SAFETY: must be used with a mutex lock to prevent concurrent test interference.
struct EnvVarGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvVarGuard {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::new();
        for (k, v) in vars {
            saved.push((*k, std::env::var(k).ok()));
            unsafe { std::env::set_var(k, v) };
        }
        Self { saved }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }
}

#[test]
fn test_cli_vcs_real_git() {
    if std::env::var("GITHUB_ACTIONS").is_ok() {
        return;
    }
    let vcs = CliVcs::new("jleechanorg/dark-factory".to_string());
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
#[ignore] // Creates one real AO worker; run explicitly with --ignored --exact.
fn test_cli_sessions_real_spawn_v013_contract() {
    assert_eq!(
        std::env::var("DARK_FACTORY_RUN_REAL_AO_SPAWN").as_deref(),
        Ok("1"),
        "set DARK_FACTORY_RUN_REAL_AO_SPAWN=1 to run this ignored real-service test"
    );
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    let branch = format!("factory/uald-real-spawn-{nonce}");
    let marker = format!("UALD_REAL_SPAWN_PROBE_{nonce}");
    let prompt = format!(
        "{marker}: This is a benign adapter integration probe. Do not edit files, commit, push, open a PR, or run product commands. Print `PROBE_MARKER: {marker}` and `BRANCH: <current git branch>`, then exit."
    );
    let sessions = CliSessions::new("jleechanorg/dark-factory", "minimax");
    let session = sessions
        .spawn(&SpawnSpec {
            bead_id: format!("uald-real-spawn-{nonce}"),
            branch: branch.clone(),
            prompt,
            repo: "jleechanorg/dark-factory".to_string(),
            ao_project: "dark-factory".to_string(),
            remote: "origin".to_string(),
        })
        .expect("real AO v0.1.3 adapter spawn failed");

    let observed_branch = sessions.session_branch(&session);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut worker_evidence = None;
    while std::time::Instant::now() < deadline {
        let targets = std::process::Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output();
        if let Ok(targets) = targets {
            let targets = String::from_utf8_lossy(&targets.stdout);
            if let Some(target) = targets
                .lines()
                .find(|target| *target == session.0 || target.ends_with(&format!("-{}", session.0)))
            {
                if let Ok(captured) = std::process::Command::new("tmux")
                    .args(["capture-pane", "-p", "-t", target, "-S", "-300"])
                    .output()
                {
                    let captured = String::from_utf8_lossy(&captured.stdout).into_owned();
                    if captured.contains(&format!("PROBE_MARKER: {marker}"))
                        && captured.contains(&format!("BRANCH: {branch}"))
                    {
                        worker_evidence = Some(captured);
                        break;
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    println!("REAL_AO_SESSION={}", session.0);
    println!("REAL_AO_BRANCH={branch}");
    let cleanup = sessions.stop(&session);
    assert!(cleanup.is_ok(), "real AO probe cleanup failed: {cleanup:?}");
    let observed_branch = observed_branch.expect("AO branch lookup failed after spawn");
    assert_eq!(observed_branch.as_deref(), Some(branch.as_str()));
    let worker_evidence = worker_evidence.unwrap_or_else(|| {
        panic!("worker did not emit the unique marker and exact branch within 180 seconds")
    });
    println!("REAL_AO_WORKER_EVIDENCE_BEGIN\n{worker_evidence}\nREAL_AO_WORKER_EVIDENCE_END");
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

// ============================================================================
// Tests for jleechan-kk64: GraphQL failure must report Unknown, not Green
// ============================================================================

/// Guards every test in this section that needs to mutate process-wide env vars
/// (`PATH`) so a future second such test can't race this one.
static FAKE_GH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write a fake `gh` script into `dir` that responds to different commands.
/// The script branches on its argv to return different responses for:
/// - `gh pr view ...` → valid JSON
/// - `gh pr checks ...` → valid check JSON
/// - `gh api graphql` → controlled by FAKE_GH_GRAPHQL_MODE env var
/// - `gh api repos/.../commits/...` → date string
#[cfg(unix)]
fn write_fake_gh(dir: &std::path::Path, graphql_mode: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("gh");
    let script = format!(
        r#"#!/bin/sh
# Fake gh for testing GraphQL failure handling
# graphql_mode: {graphql_mode}

# `gh pr view`/`gh pr checks` arrive as separate argv words ("pr" "view" ...),
# so match against the joined argv string rather than iterating word-by-word
# (a per-word loop can never see the two-word substring "pr view").
args="$*"
case "$args" in
    *"pr view"*)
        echo '{{"mergeable":"MERGEABLE","reviews":[],"headRefOid":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef","body":"","comments":[],"files":[],"updatedAt":"2026-01-01T00:00:00Z"}}'
        exit 0
        ;;
    *"pr checks"*)
        echo '[{{"state":"SUCCESS","bucket":"pass","name":"build"}}]'
        exit 0
        ;;
    *"api graphql"*)
        mode="${{FAKE_GH_GRAPHQL_MODE:-fail}}"
        if [ "$mode" = "fail" ]; then
            echo "gh: GraphQL API rate limit exceeded (HTTP 403)" >&2
            exit 1
        elif [ "$mode" = "malformed" ]; then
            echo '{{"data": "truncated'
            exit 0
        else
            echo '{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[]}}}}}}}}}}'
            exit 0
        fi
        ;;
    *"commits/"*)
        echo '2026-01-01T00:00:00Z'
        exit 0
        ;;
    *)
        echo '[]'
        exit 0
        ;;
esac
"#
    );
    std::fs::write(&path, script).unwrap_or_else(|e| panic!("failed to write fake gh: {e}"));
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// Test: GraphQL command failure should report Unknown, not Green.
/// This test PROVES the bug: currently the code returns 0 (Green), but it should return None (Unknown).
#[test]
#[cfg(unix)]
fn test_cli_scm_pr_snapshot_graphql_command_failure_reports_unknown_not_green() {
    let _lock = FAKE_GH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Unique PR number to avoid cache/collision
    let pr_num = 900001;

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_gh_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();

    // Set up fake gh with FAIL mode
    write_fake_gh(&fake_bin_dir, "fail");

    // Prepend fake gh to PATH
    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    // Guard both PATH and the mode env var our fake gh reads together so a
    // panic mid-test still restores prior state via Drop.
    let _env_guard = EnvVarGuard::set(&[("PATH", &new_path), ("FAKE_GH_GRAPHQL_MODE", "fail")]);

    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let result = scm.pr_snapshot(pr_num);

    // The snapshot fetch should succeed (only the thread count is unknown)
    assert!(result.is_ok(), "pr_snapshot should succeed even when GraphQL fails: {:?}", result);
    let snapshot = result.unwrap();

    // BUG: Currently this returns Some(0) — but it SHOULD return None (Unknown)
    // This assertion will FAIL against the current buggy code, proving the bug exists.
    assert!(
        snapshot.unresolved_thread_count.is_none(),
        "unresolved_thread_count should be None (Unknown) when GraphQL fails, but got: {:?}",
        snapshot.unresolved_thread_count
    );
}

/// Test: GraphQL malformed output should report Unknown, not Green.
#[test]
#[cfg(unix)]
fn test_cli_scm_pr_snapshot_graphql_malformed_output_reports_unknown_not_green() {
    let _lock = FAKE_GH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pr_num = 900002;

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_gh_malformed_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();

    write_fake_gh(&fake_bin_dir, "malformed");

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);

    let _env_guard =
        EnvVarGuard::set(&[("PATH", &new_path), ("FAKE_GH_GRAPHQL_MODE", "malformed")]);

    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let result = scm.pr_snapshot(pr_num);

    assert!(result.is_ok(), "pr_snapshot should succeed even when GraphQL is malformed: {:?}", result);
    let snapshot = result.unwrap();

    // BUG: Currently this returns Some(0) — but it SHOULD return None (Unknown)
    assert!(
        snapshot.unresolved_thread_count.is_none(),
        "unresolved_thread_count should be None (Unknown) when GraphQL is malformed, but got: {:?}",
        snapshot.unresolved_thread_count
    );
}
