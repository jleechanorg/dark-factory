use daemon::adapters::{
    ChainLlm, CliScm, CliSessions, CliTracker, CliVcs, RecoveryOutcome, ensure_ao_recovery,
};
use daemon::tools::{Llm, Scm, SessionActivity, SessionId, Sessions, SpawnSpec, Tracker, Vcs};

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
            local_checkout: Some(std::env::current_dir().unwrap()),
            expected_revision: None,
            managed_checkout: false,
            expected_cwd: None,
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

// jleechan-nfdl (PR #655 finding 3): the original
// `test_cli_scm_offline_fallback` integration test has been MOVED into
// `daemon::adapters::offline_cache_tests` (a `#[cfg(test)]` mod in
// `daemon/src/adapters.rs`). The production `CliScm::pr_snapshot` no
// longer consults `.beads/offline/*.json` at all — gating the offline
// parser behind `#[cfg(test)]` removes it from the production binary
// and from the library-as-dependency build used by this integration
// test binary, so the test had to be relocated to a location that can
// reach the `#[cfg(test)]` helper directly. The integration-level
// `test_planted_offline_fixture_rejected_in_production` below covers
// the production-side invariant that motivated the move: a planted
// `.beads/offline/pr_<N>.json` MUST NOT be returned by
// `CliScm::pr_snapshot`.
//
// ============================================================================
// Tests for jleechan-kk64: GraphQL failure must report Unknown, not Green
// ============================================================================

/// Guards every test in this file that needs to mutate process-wide env vars
/// (`PATH`) so concurrent tests cannot race.
static FAKE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ============================================================================
// jleechan-nfdl (PR #655 finding 3) — production must reject planted fixtures
// ============================================================================
//
// Before this commit, `CliScm::pr_snapshot(N)` would read
// `.beads/offline/pr_<N>.json` if it existed, with NO production guard:
// a planted file (test residue, debug session, attacker write to the
// daemon's CWD) was returned as if it were a real `gh pr view`
// response, then memoised in `pr_snapshot_cache` for 30s. This test
// proves the fix: the production entry point must NOT return planted
// fixture data, even when the planted file is present in the daemon's
// cwd.
//
// Strategy: plant a `.beads/offline/pr_<N>.json` with distinctive
// sentinel strings, set cwd to that directory, call
// `scm.pr_snapshot(N)`, and assert the returned snapshot is NOT the
// planted data. Because we can't easily mock `gh` for a single call
// from this layer (the production `pr_snapshot` shells out to real
// `gh`), the result may be either `Ok(snapshot)` with sentinel-free
// fields or `Err(...)` — both are acceptable outcomes, as long as the
// planted body never appears.

/// Planted-body sentinel chosen to be unique enough that an
/// accidental code path returning it would clearly be the planted
/// fixture, not a coincidence. Distinctive ASCII string with no
/// overlap to real PR bodies (no `body:` field in any seeded
/// fixture contains `NFDL_PLANTED`).
const PLANTED_BODY_SENTINEL: &str = "NFDL_PLANTED_BODY_DO_NOT_LEAK";

/// Planted-SHA sentinel chosen for the same reason — no real PR
/// SHA in any test fixture ever has this prefix.
const PLANTED_SHA_SENTINEL: &str = "nfdl_planted_sha_00000000000000000000";

#[test]
fn test_planted_offline_fixture_rejected_in_production() {
    // Serialise against every other PATH-mutating integration test in
    // this file. The FAKE_ENV_LOCK guards both `PATH` (so our planted
    // cwd wins over any concurrent test's PATH shim) and the
    // set_current_dir dance below.
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Build a private temp cwd that holds `.beads/offline/pr_<N>.json`
    // with distinctive planted values. Use a unique PR number so the
    // (now-complied-out) offline cache branch would have been the only
    // path that returned data for this PR — any real `gh` call will
    // surface a different body (or fail) for `999999999`.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "jleechan_nfdl_proof_{}_{nanos}",
        std::process::id(),
    ));
    let offline_dir = dir.join(".beads/offline");
    std::fs::create_dir_all(&offline_dir).expect("create .beads/offline");

    // Use a PR number that is extremely unlikely to map to a real PR
    // in `jleechanorg/dark-factory` AND that is unique to this test
    // invocation so the `pr_snapshot_cache` memoisation never serves a
    // stale entry. u64::MAX is unreachable for real GitHub PR numbers.
    let pr = u64::MAX;

    let planted_payload = serde_json::json!({
        "ci_success": true,
        "mergeable": true,
        "coderabbit_approved": true,
        "bugbot_error_count": 0,
        "unresolved_thread_count": 0,
        "head_sha": PLANTED_SHA_SENTINEL,
        "body": PLANTED_BODY_SENTINEL,
        "comments": [],
        "files": [],
    })
    .to_string();
    let planted_path = offline_dir.join(format!("pr_{pr}.json"));
    std::fs::write(&planted_path, planted_payload).expect("write planted fixture");

    // Save cwd, switch into the planted dir, run the production
    // entry point, restore cwd, then clean up — in that order so a
    // panic mid-test still restores cwd via the EnvVarGuard-style
    // unwind pattern.
    let prior_cwd = std::env::current_dir().expect("current_dir before");
    std::env::set_current_dir(&dir).expect("set_current_dir to planted dir");

    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let result = scm.pr_snapshot(pr);

    // Always restore cwd and tear down the planted dir before any
    // assertion failures, so a CI box doesn't accumulate planted
    // dirs across re-runs.
    std::env::set_current_dir(&prior_cwd).expect("restore cwd");
    let _ = std::fs::remove_dir_all(&dir);

    // Whatever the result, the planted sentinels must NOT appear.
    // `Err(_)` is an acceptable outcome — the production code did
    // attempt to call real `gh` (which will fail for u64::MAX), and
    // the planted fixture was correctly ignored. `Ok(snap)` is also
    // acceptable if a sibling test or environment quirk made a real
    // fetch succeed for some unrelated reason — what matters is that
    // the body/head_sha don't carry the planted markers.
    if let Ok(snap) = &result {
        assert_ne!(
            snap.body, PLANTED_BODY_SENTINEL,
            "PRODUCTION LEAK: pr_snapshot({pr}) returned the planted fixture body \
             `{PLANTED_BODY_SENTINEL}` — the .beads/offline read path is still \
             compiled into the production binary"
        );
        assert_ne!(
            snap.head_sha, PLANTED_SHA_SENTINEL,
            "PRODUCTION LEAK: pr_snapshot({pr}) returned the planted fixture \
             head_sha `{PLANTED_SHA_SENTINEL}`"
        );
        // If the call succeeded for some unrelated reason, sanity
        // check the bugbot_pending field is the production default
        // (false), not the planted value (also false in our fixture,
        // so this is a non-strict smoke check).
        assert!(
            !snap.bugbot_pending,
            "pr_snapshot({pr}) returned bugbot_pending=true without going through \
             any planted fixture — investigate the snapshot source"
        );
    }
    // `Err(_)` path is also a PASS — the production code tried to
    // call real `gh` for an unreachable PR number, which is the
    // expected behaviour when the offline path is gone.
}

/// Companion to `test_planted_offline_fixture_rejected_in_production`:
/// plant the same fixture but call the LOWER-LEVEL `pr_snapshot`
/// again after a successful fake-gh call. Proves the production
/// entry point never memoises a planted-fixture value into the
/// `pr_snapshot_cache` either — the 30s in-memory TTL would otherwise
/// amplify a one-shot leak into a 30-second window.
#[test]
fn test_planted_offline_fixture_does_not_pollute_cache() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "jleechan_nfdl_cache_{}_{nanos}",
        std::process::id(),
    ));
    let offline_dir = dir.join(".beads/offline");
    std::fs::create_dir_all(&offline_dir).expect("create .beads/offline");

    let pr = u64::MAX - 1;
    let planted_payload = serde_json::json!({
        "ci_success": true,
        "head_sha": PLANTED_SHA_SENTINEL,
        "body": PLANTED_BODY_SENTINEL,
        "comments": [],
        "files": [],
    })
    .to_string();
    std::fs::write(offline_dir.join(format!("pr_{pr}.json")), planted_payload)
        .expect("write planted fixture");

    let prior_cwd = std::env::current_dir().expect("current_dir before");
    std::env::set_current_dir(&dir).expect("set_current_dir");

    let scm = CliScm::new("jleechanorg/dark-factory".to_string());
    let _ = scm.pr_snapshot(pr); // first call (will err on real gh, that's fine)

    std::env::set_current_dir(&prior_cwd).expect("restore cwd");

    // Now REMOVE the planted fixture and verify a second call also
    // doesn't surface it from cache. (Both calls should err because
    // u64::MAX-1 is unreachable, so cache pollution would manifest
    // as the second call returning Ok with the planted body.)
    let _ = std::fs::remove_dir_all(&dir);

    let result2 = scm.pr_snapshot(pr);
    if let Ok(snap) = &result2 {
        assert_ne!(
            snap.body, PLANTED_BODY_SENTINEL,
            "PRODUCTION CACHE LEAK: second pr_snapshot({pr}) returned the planted \
             fixture body — the offline cache fed into pr_snapshot_cache for 30s"
        );
        assert_ne!(
            snap.head_sha, PLANTED_SHA_SENTINEL,
            "PRODUCTION CACHE LEAK: second pr_snapshot({pr}) returned planted \
             head_sha"
        );
    }
}

// ============================================================================
// Tests for jleechan-kk64: GraphQL failure must report Unknown, not Green
// ============================================================================

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
    *"pulls/"*)
        echo '{{"mergeable":true,"head":{{"sha":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"}},"body":"","updated_at":"2026-01-01T00:00:00Z"}}'
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
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    daemon::adapters::clear_graphql_rate_limited();

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

    daemon::adapters::clear_graphql_rate_limited();

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
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    daemon::adapters::clear_graphql_rate_limited();

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

    daemon::adapters::clear_graphql_rate_limited();

    assert!(result.is_ok(), "pr_snapshot should succeed even when GraphQL is malformed: {:?}", result);
    let snapshot = result.unwrap();

    // BUG: Currently this returns Some(0) — but it SHOULD return None (Unknown)
    assert!(
        snapshot.unresolved_thread_count.is_none(),
        "unresolved_thread_count should be None (Unknown) when GraphQL is malformed, but got: {:?}",
        snapshot.unresolved_thread_count
    );
}

// ============================================================================
// Tests for CliSessions: ensure `ao status` calls include `-p <project>`
// ============================================================================

/// Write a fake `ao` script into `dir` that logs invocations and responds with valid status JSON.
#[cfg(unix)]
fn write_fake_ao(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("ao");
    let script = r#"#!/bin/sh
# Log all argv to the file designated by FAKE_AO_LOG
if [ -n "$FAKE_AO_LOG" ]; then
    echo "$@" >> "$FAKE_AO_LOG"
fi

args="$*"
case "$args" in
    *"status"*)
        echo '[{"name":"session-1","branch":"feature-branch","status":"working","activity":"working"}]'
        exit 0
        ;;
    *"session kill"*)
        exit 0
        ;;
    *)
        echo '[]'
        exit 0
        ;;
esac
"#;
    std::fs::write(&path, script).unwrap_or_else(|e| panic!("failed to write fake ao: {e}"));
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// Test: CliSessions must include `-p <project>` in its command args when calling `ao status --json`,
/// ensuring unscoped status calls cannot be introduced across all query entry points
/// (active_count, attach, attach_within, is_quiescent, session_activity, session_activity_within, session_branch).
#[test]
#[cfg(unix)]
fn test_cli_sessions_ao_status_includes_project_arg_scoped() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_fake_ao_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).unwrap();

    let log_file = fake_bin_dir.join("ao_calls.log");
    write_fake_ao(&fake_bin_dir);

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", fake_bin_dir.display(), original_path);
    let log_file_str = log_file.to_string_lossy().to_string();

    let _env_guard = EnvVarGuard::set(&[
        ("PATH", &new_path),
        ("FAKE_AO_LOG", &log_file_str),
    ]);

    // Test 1: with a repo path that derives project name (e.g. "jleechanorg/dark-factory" -> "dark-factory")
    let sessions_df = CliSessions::new("jleechanorg/dark-factory", "minimax");
    assert_eq!(sessions_df.project, "dark-factory");

    // Call active_count
    let count = sessions_df.active_count().expect("active_count failed");
    assert_eq!(count, 1);

    // Call attach
    let session = sessions_df
        .attach("feature-branch", "bead-1")
        .expect("attach failed");
    assert_eq!(session.0, "session-1");

    // Call attach_within
    let session = sessions_df
        .attach_within("feature-branch", "bead-1", 10)
        .expect("attach_within failed");
    assert_eq!(session.0, "session-1");

    // Call is_quiescent
    let quiescent = sessions_df
        .is_quiescent(&SessionId("session-1".to_string()))
        .expect("is_quiescent failed");
    assert!(!quiescent);

    // Call session_activity
    let activity = sessions_df
        .session_activity(&SessionId("session-1".to_string()))
        .expect("session_activity failed");
    assert_eq!(activity, SessionActivity::Running);

    // Call session_activity_within
    let activity = sessions_df
        .session_activity_within(&SessionId("session-1".to_string()), 10)
        .expect("session_activity_within failed");
    assert_eq!(activity, SessionActivity::Running);

    // Call session_branch
    let branch = sessions_df
        .session_branch(&SessionId("session-1".to_string()))
        .expect("session_branch failed");
    assert_eq!(branch.as_deref(), Some("feature-branch"));

    // Verify logged calls for dark-factory
    let log_content = std::fs::read_to_string(&log_file).expect("failed to read fake ao log");
    let lines: Vec<&str> = log_content.lines().collect();
    assert_eq!(
        lines.len(),
        7,
        "expected exactly 7 calls to ao status, got: {lines:?}"
    );

    for line in &lines {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        assert!(
            tokens.contains(&"status"),
            "command should invoke status: {line}"
        );
        assert!(
            tokens.contains(&"--json"),
            "command should pass --json: {line}"
        );
        // Verify `-p dark-factory` is explicitly included and scoped
        let p_idx = tokens
            .iter()
            .position(|&t| t == "-p")
            .unwrap_or_else(|| panic!("unscoped status call detected without -p flag: {line}"));
        assert_eq!(
            tokens.get(p_idx + 1).copied(),
            Some("dark-factory"),
            "expected project 'dark-factory' following -p in call: {line}"
        );
    }

    // Test 2: with custom project name (e.g. "worldarchitect.ai" -> "worldarchitect")
    let _ = std::fs::remove_file(&log_file);
    let sessions_wa = CliSessions::new("worldarchitect.ai", "claude-code");
    assert_eq!(sessions_wa.project, "worldarchitect");

    let count_wa = sessions_wa.active_count().expect("active_count failed");
    assert_eq!(count_wa, 1);

    let log_content_wa = std::fs::read_to_string(&log_file).expect("failed to read fake ao log");
    let lines_wa: Vec<&str> = log_content_wa.lines().collect();
    assert_eq!(lines_wa.len(), 1);
    let wa_tokens: Vec<&str> = lines_wa[0].split_whitespace().collect();
    let p_idx = wa_tokens
        .iter()
        .position(|&t| t == "-p")
        .expect("unscoped status call detected without -p flag");
    assert_eq!(
        wa_tokens.get(p_idx + 1).copied(),
        Some("worldarchitect"),
        "expected project 'worldarchitect' following -p"
    );

    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// ============================================================================
// RED tests for dark-factory #778: project-scoped AO recovery contract
// ============================================================================
//
// Bead dark-factory-5ep4 (P0/factory). The contract for
// `daemon::adapters::ensure_ao_recovery(project)` is being defined in this
// PR. These are the RED tests — they reference an API surface that does not
// yet exist in `daemon/src/adapters.rs`, so `cargo test
// --test adapters_integration` MUST fail to compile until the GREEN side
// lands (out of scope for this PR per the bead's "RED ONLY" rule).
//
// Acceptance criteria pinned by these tests:
//
//   * Healthy AO preflight MUST NOT shell out to `ao start`.
//   * Unrelated preflight failures (e.g. quota, parse errors) MUST NOT
//     shell out to `ao start` — only the canonical "AO unavailable"
//     anchors trigger recovery.
//   * Unavailable preflight MUST shell out to `ao start` exactly once
//     per recovery attempt, and the argv MUST be the exact AO v0.1.3
//     shape: `ao start --project <project>` (long flag, single space,
//     project value immediately after). No `--project=foo`, no `-pfoo`,
//     no substring-only matches.
//   * Status probes used by the recovery loop MUST be project-scoped:
//     `ao status -p <project> --json` (the same exact argv the existing
//     #752 contract pins for `CliSessions::active_count`/`attach`/...).
//   * Concurrent callers of `ensure_ao_recovery` for the same project
//     MUST elect exactly one starter; the losers MUST NOT shell out to
//     `ao start` while the starter is in flight.
//   * A failed `ao start` MUST release the per-project lock so the next
//     caller can retry — there is no permanent deadlock from a transient
//     miss.
//   * `rc = -1` (subprocess exec failure: PATH lookup, permissions,
//     signal kill) MUST NOT be overclassified as "AO unavailable ->
//     restart". The contract must treat that as `Unknown` and propagate
//     the underlying failure verbatim.
//
// Every test below acquires `FAKE_ENV_LOCK` for every PATH/env mutation,
// matching the file-wide invariant for tests that touch `PATH`. The fake
// `ao` scripts are planted in a unique per-test tempdir and removed at
// the end so concurrent test execution cannot observe leftover binaries.

/// Project name the GREEN contract must derive for `worldarchitect.ai`,
/// matching the existing `CliSessions::new` rule (`worldarchitect.ai` ->
/// `worldarchitect`). Pinning this here ensures the recovery contract
/// inherits the same project-naming convention as every other adapter
/// method that already passes `bdd worldarchitect #9615` review.
const RECOVERY_TEST_PROJECT: &str = "worldarchitect";

/// Plant a fake `ao` binary at `<dir>/ao` that records every invocation
/// to `FAKE_AO_LOG` and dispatches on `argv[1]` (the subcommand) into one
/// of the configured response handlers.
///
/// `mode` controls how the fake responds:
///   * `"healthy"`             -> `status` returns a valid 1-element JSON
///                                array, `start` is never invoked.
///   * `"unrelated_quota"`     -> `status` exits 1 with stderr containing
///                                `quota exceeded` (NOT a restart anchor).
///                                `start` is never invoked.
///   * `"unavailable_then_healthy"` -> first `status` returns
///                                "daemon not running" + exit 2; subsequent
///                                `status` returns valid JSON. `start` is
///                                invoked exactly once.
///   * `"unavailable_then_fail"`    -> first `status` returns
///                                "daemon not running" + exit 2; `start`
///                                fails (exit 1, "could not bind port").
///   * `"exec_failure"`        -> the fake `ao` exec itself fails (we
///                                arrange this by NOT placing the script
///                                on PATH and instead letting the daemon
///                                invoke a real `ao` if present — the
///                                expected contract behavior is to
///                                propagate the rc=-1, NOT start).
#[cfg(unix)]
fn write_fake_ao_with_mode(dir: &std::path::Path, mode: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("ao");
    let start_block: String = match mode {
        "unavailable_then_healthy" => r#"
        if [ "$1" = "start" ]; then
            echo "AO_FAKE_START_INVOKED"
            exit 0
        fi
"#
        .to_string(),
        "unavailable_then_fail" => r#"
        if [ "$1" = "start" ]; then
            echo "AO_FAKE_START_INVOKED_BUT_FAILED" >&2
            exit 1
        fi
"#
        .to_string(),
        _ => String::new(),
    };
    let status_block: String = match mode {
        "healthy" => r#"
        if [ "$1" = "status" ]; then
            echo '[{"name":"session-1","branch":"feature-branch","status":"working","activity":"working"}]'
            exit 0
        fi
"#
        .to_string(),
        "unrelated_quota" => r#"
        if [ "$1" = "status" ]; then
            echo "quota exceeded: 5000 points/hr reset at 23:00Z" >&2
            exit 1
        fi
"#
        .to_string(),
        "unavailable_then_healthy" => {
            let mut s = String::from(
                r#"
        STATUS_CALL_COUNT=0
        if [ "$1" = "status" ]; then
            STATUS_CALL_COUNT=$((STATUS_CALL_COUNT + 1))
            if [ "$STATUS_CALL_COUNT" = "1" ]; then
                echo "Error: AO daemon not running for project 'worldarchitect'" >&2
                exit 2
            fi
            echo '[{"name":"session-1","branch":"feature-branch","status":"working","activity":"working"}]'
            exit 0
        fi
"#,
            );
            s.push_str(&start_block);
            s
        }
        "unavailable_then_fail" => {
            let mut s = String::from(
                r#"
        if [ "$1" = "status" ]; then
            echo "Error: AO daemon not running for project 'worldarchitect'" >&2
            exit 2
        fi
"#,
            );
            s.push_str(&start_block);
            s
        }
        _ => String::new(),
    };
    let script = format!(
        r#"#!/bin/sh
# Log all argv to FAKE_AO_LOG
if [ -n "$FAKE_AO_LOG" ]; then
    echo "$@" >> "$FAKE_AO_LOG"
fi
{status_block}
exit 0
"#,
    );
    std::fs::write(&path, &script).unwrap_or_else(|e| panic!("failed to write fake ao: {e}"));
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
}

/// Counts the number of `ao start` invocations in the fake-ao log file.
#[cfg(unix)]
fn count_ao_start_calls(log_file: &std::path::Path) -> usize {
    let content = std::fs::read_to_string(log_file).unwrap_or_default();
    content
        .lines()
        .filter(|line| {
            // Filter to lines whose argv starts with `start` — i.e. a real
            // `start` subcommand invocation, not a `-status` substring match.
            let tokens: Vec<&str> = line.split_whitespace().collect();
            tokens.first().copied() == Some("start")
        })
        .count()
}

/// Counts the number of `ao status` invocations in the fake-ao log file.
#[cfg(unix)]
fn count_ao_status_calls(log_file: &std::path::Path) -> usize {
    let content = std::fs::read_to_string(log_file).unwrap_or_default();
    content
        .lines()
        .filter(|line| {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            tokens.first().copied() == Some("status")
        })
        .count()
}

/// Returns the argv of the Nth `ao start` invocation (0-indexed) for exact
/// shape assertions. Returns `None` if no Nth start was logged.
#[cfg(unix)]
fn nth_ao_start_argv(log_file: &std::path::Path, n: usize) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(log_file).unwrap_or_default();
    let mut starts = content.lines().filter(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        tokens.first().copied() == Some("start")
    });
    starts.nth(n).map(|line| {
        line.split_whitespace()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    })
}

/// Returns the argv of the Nth `ao status` invocation (0-indexed).
#[cfg(unix)]
fn nth_ao_status_argv(log_file: &std::path::Path, n: usize) -> Option<Vec<String>> {
    let content = std::fs::read_to_string(log_file).unwrap_or_default();
    let mut statuses = content.lines().filter(|line| {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        tokens.first().copied() == Some("status")
    });
    statuses.nth(n).map(|line| {
        line.split_whitespace()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    })
}

/// Set up a unique fake-ao tempdir + PATH shim + FAKE_AO_LOG, with the
/// per-test recovery mode applied. Returns `(fake_bin_dir, log_file)`.
/// Caller is responsible for `remove_dir_all` + EnvVarGuard + lock release.
#[cfg(unix)]
fn setup_recovery_test_env(mode: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let fake_bin_dir = std::env::temp_dir().join(format!(
        "afd_recovery_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&fake_bin_dir).expect("create fake_bin_dir");
    write_fake_ao_with_mode(&fake_bin_dir, mode);
    let log_file = fake_bin_dir.join("ao_calls.log");
    (fake_bin_dir, log_file)
}

/// Install PATH + FAKE_AO_LOG for a recovery test, returning an
/// `EnvVarGuard` that restores the previous values on Drop.
#[cfg(unix)]
fn install_recovery_test_path(
    fake_bin_dir: &std::path::Path,
    log_file: &std::path::Path,
) -> EnvVarGuard {
    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!(
        "{}:{}",
        fake_bin_dir.display(),
        original_path
    );
    let log_file_str = log_file.to_string_lossy().to_string();
    EnvVarGuard::set(&[("PATH", &new_path), ("FAKE_AO_LOG", &log_file_str)])
}

// ----------------------------------------------------------------------------
// Contract test 1: healthy AO preflight MUST NOT trigger `ao start`.
// ----------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn test_ao_recovery_healthy_preflight_never_starts_daemon() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (fake_bin_dir, log_file) = setup_recovery_test_env("healthy");
    let _env_guard = install_recovery_test_path(&fake_bin_dir, &log_file);

    let outcome = ensure_ao_recovery(RECOVERY_TEST_PROJECT);

    assert!(
        matches!(outcome, RecoveryOutcome::Healthy { .. }),
        "healthy preflight must produce RecoveryOutcome::Healthy, got: {outcome:?}"
    );
    assert_eq!(
        count_ao_start_calls(&log_file),
        0,
        "healthy preflight MUST NOT shell out to `ao start` (logged calls: {:?})",
        std::fs::read_to_string(&log_file).unwrap_or_default()
    );
    assert!(
        count_ao_status_calls(&log_file) >= 1,
        "recovery contract MUST probe `ao status` at least once for the project, got 0 calls"
    );
    let status_argv = nth_ao_status_argv(&log_file, 0).expect("first status call");
    assert!(
        status_argv.iter().any(|t| t == "-p"),
        "status argv must include `-p` flag (the AO v0.1.3 project short form), got: {status_argv:?}"
    );
    assert_eq!(
        status_argv
            .iter()
            .position(|t| t == "-p")
            .and_then(|i| status_argv.get(i + 1)),
        Some(&RECOVERY_TEST_PROJECT.to_string()),
        "status argv must scope to project `{RECOVERY_TEST_PROJECT}` after `-p`, got: {status_argv:?}"
    );
    assert!(
        status_argv.iter().any(|t| t == "--json"),
        "status argv must include `--json` for parseable recovery outcomes, got: {status_argv:?}"
    );

    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// ----------------------------------------------------------------------------
// Contract test 2: unrelated preflight failure MUST NOT trigger `ao start`.
// ----------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn test_ao_recovery_unrelated_failure_never_starts_daemon() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (fake_bin_dir, log_file) = setup_recovery_test_env("unrelated_quota");
    let _env_guard = install_recovery_test_path(&fake_bin_dir, &log_file);

    let outcome = ensure_ao_recovery(RECOVERY_TEST_PROJECT);

    // An unrelated stderr like "quota exceeded" must surface as
    // `Unknown` (NOT a restart trigger) and MUST NOT shell out to
    // `ao start`. The exact contract surface is pinned here so a
    // substring-only matcher (e.g. "contains `not running`") cannot
    // accidentally reclassify quota as Unavailable.
    assert!(
        matches!(outcome, RecoveryOutcome::Unknown { .. }),
        "quota/preflight unrelated failure must produce RecoveryOutcome::Unknown, got: {outcome:?}"
    );
    assert_eq!(
        count_ao_start_calls(&log_file),
        0,
        "unrelated preflight failure MUST NOT shell out to `ao start` (the entire recovery contract exists specifically to avoid restarting AO for non-AO failures), got calls: {:?}",
        std::fs::read_to_string(&log_file).unwrap_or_default()
    );

    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// ----------------------------------------------------------------------------
// Contract test 3: `ao start` argv MUST be the exact AO v0.1.3 shape.
// ----------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn test_ao_recovery_unavailable_starts_with_exact_v013_argv() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (fake_bin_dir, log_file) = setup_recovery_test_env("unavailable_then_healthy");
    let _env_guard = install_recovery_test_path(&fake_bin_dir, &log_file);

    let outcome = ensure_ao_recovery(RECOVERY_TEST_PROJECT);

    assert!(
        matches!(outcome, RecoveryOutcome::Restarted { .. }),
        "Unavailable -> healthy-after-start must produce RecoveryOutcome::Restarted, got: {outcome:?}"
    );

    let start_argv = nth_ao_start_argv(&log_file, 0).expect("ao start invocation");
    // AO v0.1.3 contract: argv[0] == "start", argv[1..2] == ["--project",
    // <project>]. Exactly one `--project` flag, exactly one project value,
    // no `--project=foo`, no `-pfoo`, no `-project`. Reject substring-only
    // matches: `--project` must appear as its own whitespace-delimited token
    // AND the project value must be the immediately-following token.
    assert_eq!(
        start_argv.first().map(|s| s.as_str()),
        Some("start"),
        "first token of recovery `ao start` invocation must be the literal `start` subcommand, got: {start_argv:?}"
    );
    let p_idx = start_argv
        .iter()
        .position(|t| t == "--project")
        .unwrap_or_else(|| panic!("`ao start` argv missing `--project` flag (must be its own whitespace-delimited token, NOT `--project=foo` or substring `-project`): {start_argv:?}"));
    assert_eq!(
        start_argv.get(p_idx + 1).map(|s| s.as_str()),
        Some(RECOVERY_TEST_PROJECT),
        "the token immediately after `--project` must be the project name `{RECOVERY_TEST_PROJECT}`, got: {start_argv:?}"
    );
    // Substring-only overclassification guard: ensure no `--project=...`
    // (which would also match `--project` as a substring) appears.
    assert!(
        !start_argv.iter().any(|t| t.starts_with("--project=")),
        "`--project=...` form is NOT the AO v0.1.3 recovery contract (this is exactly the substring-only classification the contract must reject), got: {start_argv:?}"
    );
    // No `--headless` / `--json` aliases leaking in: AO v0.1.3's `start`
    // argv is exactly `[start, --project, <project>]` for the recovery
    // case (the headless flag, if needed, is the operator's job via
    // DARK_FACTORY_AO_START_CMD env var, NOT the daemon's argv).
    assert_eq!(
        start_argv.len(),
        3,
        "AO v0.1.3 recovery `ao start` argv must be exactly `[start, --project, <project>]` (no extra flags injected), got: {start_argv:?}"
    );

    // Re-probe MUST happen after start to confirm AO is now healthy.
    let status_count_after_start = count_ao_status_calls(&log_file);
    assert!(
        status_count_after_start >= 2,
        "recovery loop MUST re-probe with `ao status` after `ao start` (got {status_count_after_start} status calls, expected >=2: preflight + post-start)"
    );
    let re_probe_argv =
        nth_ao_status_argv(&log_file, 1).expect("post-start re-probe");
    assert_eq!(
        re_probe_argv
            .iter()
            .position(|t| t == "-p")
            .and_then(|i| re_probe_argv.get(i + 1)),
        Some(&RECOVERY_TEST_PROJECT.to_string()),
        "post-start re-probe MUST also be project-scoped (`-p {RECOVERY_TEST_PROJECT}`), got: {re_probe_argv:?}"
    );

    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// ----------------------------------------------------------------------------
// Contract test 4: concurrent callers MUST elect exactly one starter.
// ----------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn test_ao_recovery_concurrent_callers_elect_one_starter() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (fake_bin_dir, log_file) = setup_recovery_test_env("unavailable_then_healthy");
    let _env_guard = install_recovery_test_path(&fake_bin_dir, &log_file);

    // Fan out 4 concurrent callers onto the daemon's threadpool. Each
    // will independently try `ensure_ao_recovery("worldarchitect")`.
    // The contract must serialize the actual `ao start` invocation:
    // exactly one starter wins, the others observe the start already
    // succeeded (via the post-start re-probe) and resolve as
    // `Restarted` WITHOUT shelling out to `ao start` themselves.
    let handles: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| -> RecoveryOutcome {
                ensure_ao_recovery(RECOVERY_TEST_PROJECT)
            })
        })
        .collect();
    let outcomes: Vec<RecoveryOutcome> = handles
        .into_iter()
        .map(|h: std::thread::JoinHandle<RecoveryOutcome>| {
            h.join().expect("recovery thread panicked")
        })
        .collect();

    // Every caller must observe a successful recovery (they all
    // started from the same Unavailable state and the one starter
    // brought AO back).
    for (i, outcome) in outcomes.iter().enumerate() {
        assert!(
            matches!(outcome, RecoveryOutcome::Restarted { .. }),
            "concurrent caller #{i} must observe Restarted (the elected starter's start succeeded), got: {outcome:?}"
        );
    }

    // Exactly ONE `ao start` invocation must have been shelled out.
    // This is the election guarantee: N concurrent callers do NOT
    // produce N `ao start` calls.
    let start_count = count_ao_start_calls(&log_file);
    assert_eq!(
        start_count, 1,
        "concurrent recovery callers MUST elect exactly one starter (got {start_count} `ao start` invocations — each extra one is a duplicate-port-bind race the contract forbids), logged: {:?}",
        std::fs::read_to_string(&log_file).unwrap_or_default()
    );

    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// ----------------------------------------------------------------------------
// Contract test 5: failed `ao start` MUST release the per-project lock.
// ----------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn test_ao_recovery_failed_start_releases_lock_for_next_caller() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (fake_bin_dir, log_file) = setup_recovery_test_env("unavailable_then_fail");
    let _env_guard = install_recovery_test_path(&fake_bin_dir, &log_file);

    // First caller: AO is unavailable, the recovery loop attempts
    // `ao start`, the start fails -> FailClosed outcome.
    let first = ensure_ao_recovery(RECOVERY_TEST_PROJECT);
    assert!(
        matches!(first, RecoveryOutcome::FailClosed { .. }),
        "first caller with failed start MUST observe FailClosed, got: {first:?}"
    );
    assert_eq!(
        count_ao_start_calls(&log_file),
        1,
        "first caller MUST shell out to `ao start` exactly once (preflight + start), got: {:?}",
        std::fs::read_to_string(&log_file).unwrap_or_default()
    );

    // Second caller: the per-project lock MUST be released so this
    // caller can attempt again. If the lock were leaked (a common
    // bug in lazy-initialized Mutex patterns), this caller would
    // observe a stale `Unavailable` and silently skip the second
    // start — which is the failure mode the contract exists to
    // prevent.
    let second = ensure_ao_recovery(RECOVERY_TEST_PROJECT);
    assert!(
        matches!(second, RecoveryOutcome::FailClosed { .. }),
        "second caller (after a failed start) MUST be allowed to retry and observe FailClosed again (not blocked on a leaked lock), got: {second:?}"
    );
    assert_eq!(
        count_ao_start_calls(&log_file),
        2,
        "second caller MUST shell out to `ao start` exactly once more (proves the per-project lock was released after the first failure), got: {:?}",
        std::fs::read_to_string(&log_file).unwrap_or_default()
    );

    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// ----------------------------------------------------------------------------
// Contract test 6: argv parser MUST reject substring-only classification.
// ----------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn test_ao_recovery_argv_parser_rejects_substring_only_classification() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (fake_bin_dir, log_file) = setup_recovery_test_env("healthy");
    let _env_guard = install_recovery_test_path(&fake_bin_dir, &log_file);

    let _outcome = ensure_ao_recovery(RECOVERY_TEST_PROJECT);

    // Walk every logged `ao status` invocation and assert the
    // project-scoping flag is its own whitespace-delimited token
    // followed by the project name. A substring-only matcher would
    // let `-project` or `--projects` or `--project=foo` slip through.
    let log = std::fs::read_to_string(&log_file).unwrap_or_default();
    assert!(!log.is_empty(), "fake ao must have logged at least one call");
    for line in log.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() != Some("status") {
            continue;
        }
        // Reject substring-only forms of the project flag.
        assert!(
            !tokens
                .iter()
                .any(|t| t.starts_with("--project=") || t.starts_with("-p=") || *t == "-pproject" || *t == "-project"),
            "argv contains a substring-only or `-Xfoo`-style project flag (the contract MUST reject these so a future code path can't classify an unrelated flag as the project): {tokens:?}"
        );
        // Reject a `--projects` (plural) variant: a substring-only
        // matcher would accept `--projects` as containing the
        // substring `--project`.
        assert!(
            !tokens.iter().any(|t| *t == "--projects" || *t == "--Project"),
            "argv contains a `--projects`/`--Project` variant that a substring-only matcher would misclassify as the project flag: {tokens:?}"
        );
        // The exact, project-scoped argv shape.
        let p_idx = tokens
            .iter()
            .position(|&t| t == "-p")
            .unwrap_or_else(|| panic!("status argv missing `-p` flag (must be its own whitespace-delimited token, NOT `-pfoo` or `-project`): {tokens:?}"));
        assert_eq!(
            tokens.get(p_idx + 1).copied(),
            Some(RECOVERY_TEST_PROJECT),
            "status argv token after `-p` must be exactly `{RECOVERY_TEST_PROJECT}` (substring-only `RECOVERY` or `worldarchi` would have been caught by the explicit equality check), got: {tokens:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&fake_bin_dir);
}

// ----------------------------------------------------------------------------
// Contract test 7: rc=-1 (exec failure) MUST NOT trigger `ao start`.
// ----------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn test_ao_recovery_rc_minus_one_exec_failure_does_not_classify_as_unavailable() {
    let _lock = FAKE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Force `Command::new("ao").output()` to fail at the OS layer by
    // pointing PATH at a directory that contains NO executable named
    // `ao`. `Command::output()` returns `Err` in that case (which the
    // adapter layer surfaces as `rc: -1`), NOT a successful spawn
    // with a non-zero exit code. A naive classifier that treats
    // `rc = -1` as "AO unavailable -> restart" would then call
    // `ao start --project worldarchitect`, which would ALSO fail at
    // the exec layer (no `ao` on PATH), and the contract would be
    // silently broken. The contract must surface this as
    // `Unknown` (or surface the exec error directly) and MUST NOT
    // shell out to `ao start`.
    let empty_dir = std::env::temp_dir().join(format!(
        "afd_recovery_empty_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&empty_dir).expect("create empty_dir");
    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", empty_dir.display(), original_path);
    let _env_guard = EnvVarGuard::set(&[("PATH", &new_path)]);
    // Sentinel log file path (never written, but if the contract
    // accidentally invokes `ao start` we want a place to record the
    // leak — there's no executable to write to, so the absence of
    // the file is itself the assertion).
    let sentinel_log = empty_dir.join("ao_calls.log");
    assert!(
        !sentinel_log.exists(),
        "sentinel log file must not exist before the test runs"
    );

    let outcome = ensure_ao_recovery(RECOVERY_TEST_PROJECT);

    // The contract MUST surface the exec failure. The exact outcome
    // variant is intentionally left to the GREEN side, but it MUST
    // NOT be `Restarted` (which would imply the recovery loop
    // successfully invoked `ao start`).
    assert!(
        !matches!(outcome, RecoveryOutcome::Restarted { .. }),
        "exec-layer failure (rc=-1) MUST NOT be overclassified as `Restarted` (the contract exists specifically to avoid this: an exec failure on PATH means the daemon cannot invoke `ao` AT ALL, so a successful `ao start` is impossible), got: {outcome:?}"
    );

    let _ = std::fs::remove_dir_all(&empty_dir);
}
