// Task 5 Step 4: exercise each scripted fake (call-log recording + scripted
// response) so downstream tasks (intake, router, dispatch, verifier) can trust
// `tests/common/mod.rs` as a stand-in for real subprocess-backed tools.
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeTracker, FakeVcs};
use daemon::tools::Llm;
use daemon::tools::{
    Bead, Issue, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs,
};

#[test]
fn fake_tracker_records_calls_and_returns_scripted_response() {
    let fake = FakeTracker {
        candidates: std::cell::RefCell::new(vec![Bead {
            id: "b1".into(),
            title: "t".into(),
            description: String::new(),
            notes: String::new(),
            file_tree_summary: String::new(),
            external_ref: Some("owner/repo#5".into()),
        }]),
        create_bead_result: std::cell::RefCell::new(Some(Ok("bead-42".into()))),
        create_bead_duplicate_of: Default::default(),
        create_bead_fail_for_ref: Default::default(),
        fail_next_fetch_candidates: Default::default(),
        fail_next_comment: Default::default(),
        calls: Default::default(),
    };

    let candidates = fake.fetch_candidates().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "b1");

    let id = fake.create_bead("title", "body", "owner/repo#5").unwrap();
    assert_eq!(id, "bead-42");

    fake.comment_external("owner/repo#5", "hello").unwrap();

    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], "fetch_candidates");
    assert!(calls[1].starts_with("create_bead("));
    assert!(calls[2].starts_with("comment_external("));
}

#[test]
fn fake_scm_returns_scripted_permission_and_records_call() {
    let mut fake = FakeScm::new();
    fake.permissions.insert("alice".into(), Permission::Write);
    fake.issues.push(Issue {
        number: 5,
        title: "issue".into(),
        body: "body".into(),
        author_login: "alice".into(),
        external_ref: "owner/repo#5".into(),
    });
    fake.pr_snapshots.insert(
        7,
        PrSnapshot {
            pr_number: 7,
            ci_success: true,
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "deadbeef".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "green".to_string(),
            coderabbit_status: "green".to_string(),
            bugbot_status: "green".to_string(),
            ci_pending: false,
            head_committed_epoch: 0,
            pending_check_names: vec![],
            check_names_and_buckets: vec![],
        },
    );

    let perm = fake.collaborator_permission("alice").unwrap();
    assert!(perm.is_write_tier());

    let issues = fake.labeled_issues("factory").unwrap();
    assert_eq!(issues.len(), 1);

    let snapshot = fake.pr_snapshot(7).unwrap();
    assert!(snapshot.ci_success);

    fake.close_pr(7, "superseded").unwrap();

    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 4);
}

#[test]
fn fake_sessions_spawn_attach_stop_quiescent_roundtrip() {
    let fake = FakeSessions {
        active_count: 3,
        next_session_id: "sess-9".into(),
        quiescent: true,
        fail_spawn_for: Default::default(),
        panic_after_spawn_for: Default::default(),
        fail_spawn_cleanup_for: Default::default(),
        fail_stop_for: Default::default(),
        fail_spawn_deferred_for: Default::default(),
        spawn_prompts: Default::default(),
        calls: Default::default(),
        branch_for: Default::default(),
        terminal_at: Default::default(),
        quiescence_check_error: Default::default(),
        attach_not_found_for: Default::default(),
        fail_attach_permanent_for: Default::default(),
        fail_attach_transient_for: Default::default(),
        orphan_after_stop: Default::default(),
        stop_succeeded: Default::default(),
        fail_stop_permanent_for: Default::default(),
        activity: Default::default(),
        activity_permanent_error: Default::default(),
        activity_sequence: Default::default(),
        worktree_remote_override: Default::default(),
        transcript_activity_for: Default::default(),
    };

    assert_eq!(fake.active_count().unwrap(), 3);

    let spec = SpawnSpec {
        bead_id: "b1".into(),
        branch: "factory/b1-r1".into(),
        prompt: "do the thing".into(),
        repo: "owner/repo".into(),
        ao_project: "repo".into(),
        remote: "origin".into(),
    };
    let id = fake.spawn(&spec).unwrap();
    assert_eq!(id, SessionId("sess-9".into()));
    assert_eq!(
        fake.spawn_prompts.borrow().as_slice(),
        &[("b1".to_string(), "do the thing".to_string())]
    );

    let attached = fake.attach("factory/b1-r1", "b1").unwrap();
    assert_eq!(attached, SessionId("sess-9".into()));

    assert!(fake.is_quiescent(&id).unwrap());
    fake.stop(&id).unwrap();

    let calls = fake.calls.borrow();
    assert_eq!(
        calls.as_slice(),
        &[
            "active_count",
            "spawn(b1)",
            "attach(factory/b1-r1,b1)",
            "is_quiescent(sess-9)",
            "stop(sess-9)",
        ]
    );
}

#[test]
fn fake_vcs_returns_scripted_head_shas() {
    let mut fake = FakeVcs::new();
    fake.heads.insert("main".into(), "abc123".into());

    let sha = fake.base_head("main").unwrap();
    assert_eq!(sha, "abc123");

    fake.create_branch_at("factory/b1-r1", "abc123").unwrap();

    let head = fake.head_sha("main").unwrap();
    assert_eq!(head, "abc123");

    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 3);
}

#[test]
fn fake_llm_returns_scripted_judgment_regardless_of_prompt() {
    let fake = FakeLlm {
        response: std::cell::RefCell::new(Some(Ok("StandardPath".into()))),
        calls: Default::default(),
    };

    let verdict = fake.judge("some prompt text").unwrap();
    assert_eq!(verdict, "StandardPath");

    let verdict2 = fake.judge("a completely different prompt").unwrap();
    assert_eq!(verdict2, "StandardPath");

    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 2);
}

#[test]
fn fake_vcs_missing_head_is_tool_error() {
    let fake = FakeVcs::new();
    let err = fake.base_head("nonexistent").unwrap_err();
    assert!(matches!(err, daemon::errors::DaemonError::Tool { .. }));
}
