use std::hash::Hasher;
use std::time::{Duration, Instant};
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeStateStore, FakeTracker, FakeVcs};
use daemon::config::Config;
use daemon::constraints;
use daemon::errors::DaemonError;
use daemon::reroll::{self, RerollDeps, RerollOutcome};
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::tools::PrComment;
use daemon::tick::{run_tick, TickDeps};
use daemon::tools::{Issue, Llm, Permission, PrSnapshot};

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
        ao_project: None,
        base_branch: "main".into(),
        stage: 2, // stage 2 enabled!
        max_workers: 30,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 60,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: std::env::temp_dir()
            .join("afd_spec_dir_test")
            .to_string_lossy()
            .to_string(),
        // Small real-wall-clock windows so the fail-closed predicate's timing
        // is genuinely exercised (not mocked) while keeping tests fast.
        // Individual tests override these when they need a specific window.
        reroll_head_stability_window_secs: 1,
        reroll_death_confirm_secs: 0,
        held_recheck_cooldown_secs: 900,
        repos: std::collections::HashMap::new(),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
    }
}

fn adopted_overlay(bead_id: &str) -> BeadOverlay {
    BeadOverlay {
        bead_id: bead_id.into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 5,
        spend_usd: 0.0,
        pr_number: Some(777),
        branch: Some("alice/my-cool-feature".into()),
        session_id: None,
        is_adopted: true,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    }
}

#[test]
fn test_constraints_redact_and_extract() {
    let reply = r#"
        {"inhibitionSpecs":["no dynamic imports"],"positiveAssertions":["use type assertions"],"securityRedactionEncountered":false}
    "#.to_string();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(reply));

    // Input with holdout leak
    let feedback = "Feedback containing $DARK_FACTORY_HOLDOUTS/scenario_1.py and holdouts/test_api.py. Do not use dynamic imports. Use type assertions.";
    let extracted = constraints::extract(&llm, feedback).unwrap();

    assert_eq!(extracted.inhibition_specs, vec!["no dynamic imports"]);
    assert_eq!(extracted.positive_assertions, vec!["use type assertions"]);
    // Programmatic redaction must set this to true
    assert!(extracted.security_redaction_encountered);
}

#[test]
fn test_spec_mutation_atomicity() {
    let temp_dir = std::env::temp_dir().join("afd_spec_mutation_test");
    std::fs::create_dir_all(&temp_dir).unwrap();
    let spec_file = temp_dir.join("bead-xyz.toml");
    let _ = std::fs::remove_file(&spec_file);

    constraints::append_mutation(&spec_file, "initial = 1\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(&spec_file).unwrap(),
        "initial = 1\n"
    );

    constraints::append_mutation(&spec_file, "append = 2\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(&spec_file).unwrap(),
        "initial = 1\nappend = 2\n"
    );

    std::fs::remove_dir_all(&temp_dir).ok();
}

#[test]
fn test_circuit_breaker() {
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    let vcs = FakeVcs::new();
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let telemetry_log = std::env::temp_dir().join("afd_circuit_breaker_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "bead-breaker".into(),
        state: OverlayState::Attested,
        attempt: 2, // Second attempt
        reroll_count: 1,
        autonomy_secs: 10,
        spend_usd: 1.0,
        pr_number: Some(102),
        branch: Some("factory/bead-breaker-r2".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();

    // 1. Script a previous rejection for attempt 1 citing "coderabbit" and "fail reason" hash
    let feedback = "fail reason";
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::Hash;
    feedback.hash(&mut hasher);
    let feedback_hash = format!("{:016x}", hasher.finish());

    store
        .save_rejection("bead-breaker", 1, "coderabbit", &feedback_hash, feedback)
        .unwrap();

    // 2. Prepare dependencies for attempt 2, citing the exact same reviewer & feedback
    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "coderabbit".into(),
        review_text: feedback.into(),
    };

    // 3. Execute should trigger circuit breaker, returning RerollOutcome::Held
    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Held(reason) => {
            assert!(reason.contains("circuit-breaker"));
        }
        other => panic!("expected RerollOutcome::Held, got {:?}", other),
    }

    // Verify overlay state is HUMAN_HELD
    let final_state = store.load("bead-breaker").unwrap().unwrap();
    assert_eq!(final_state.state, OverlayState::HumanHeld);

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_reroll_success() {
    let scm = FakeScm::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = true;
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), "base-sha-123".into());
    vcs.heads
        .insert("factory/bead-success-r1".into(), "head-sha-123".into());
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    // mock LLM reply for constraint extraction
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into()
    ));

    let mut cfg = test_cfg();
    cfg.spec_dir = std::env::temp_dir()
        .join("afd_spec_dir_success_test")
        .to_string_lossy()
        .to_string();
    // Clean up spec directory
    let spec_dir = std::path::Path::new(&cfg.spec_dir);
    let _ = std::fs::remove_dir_all(spec_dir);
    std::fs::create_dir_all(spec_dir).unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_reroll_success_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "bead-success".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 20,
        spend_usd: 0.5,
        pr_number: Some(201),
        branch: Some("factory/bead-success-r1".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "skeptic".into(),
        review_text: "Don't print to stdout, log errors.".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Rerolled { new_branch } => {
            assert_eq!(new_branch, "factory/bead-success-r2");
        }
        other => panic!("expected RerollOutcome::Rerolled, got {:?}", other),
    }

    // Verify overlay update
    let updated = store.load("bead-success").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::Recovery);
    assert_eq!(updated.attempt, 2);
    assert_eq!(updated.reroll_count, 1);
    assert_eq!(updated.branch, Some("factory/bead-success-r2".into()));
    assert_eq!(updated.pr_number, None); // Old PR number cleared

    // Verify branch registration
    assert_eq!(
        store.branches.borrow().as_slice(),
        &["factory/bead-success-r2"]
    );

    // Verify SCM PR close call. jleechan-v6ud / issue #340: reroll now
    // dispatches through `close_pr_for_repo(<bead.repo(cfg)>, ...)` —
    // `bead.target_repo` is None here, so it falls back to
    // `cfg.target_repo` ("owner/repo") via `BeadOverlay::repo`. The
    // regression test for cross-repo beads (8jxr / 9rkz class) is added
    // below.
    let scm_calls = scm.calls.borrow();
    assert!(
        scm_calls
            .iter()
            .any(|c| c.contains("close_pr_for_repo(owner/repo,201")),
        "expected close_pr_for_repo call to bead's resolved repo (owner/repo); got: {scm_calls:?}"
    );

    // bead jleechan-tfs1 regression guard: a factory-fabricated bead
    // (is_adopted=false) must still use today's create-branch-at path and
    // must NEVER go through the adopted-branch append-only push path.
    // jleechan-wuts / issue #349: the reroll now routes through
    // `create_branch_at_for_repo(<bead.repo(cfg)>, ...)` instead of the
    // legacy CWD-bound `create_branch_at(...)`. The assertion below
    // pins both that the routed-repo entry point was used AND that the
    // legacy local-cwd method was NOT used.
    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls.iter().any(|c| c.contains(
            "create_branch_at_for_repo(owner/repo,factory/bead-success-r2"
        )),
        "factory-fabricated reroll must fabricate a new branch via the routed-repo entry point: {vcs_calls:?}"
    );
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")),
        "factory-fabricated reroll must NEVER use the legacy CWD-bound create_branch_at (issue #349): {vcs_calls:?}"
    );
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("push_fix_commit(")),
        "factory-fabricated reroll must never take the adopted append-only path: {vcs_calls:?}"
    );

    // Verify spec file mutation
    let spec_path = spec_dir.join("bead-success.toml");
    assert!(spec_path.exists());
    let spec_content = std::fs::read_to_string(&spec_path).unwrap();
    assert!(spec_content.contains("reviewer = \"skeptic\""));
    assert!(spec_content.contains("inhibition_specs = ["));
    assert!(spec_content.contains("\"no print\""));

    std::fs::remove_dir_all(spec_dir).ok();
    let _ = std::fs::remove_file(&telemetry_log);
}

/// Regression test for issue #349 / bead jleechan-wuts — the
/// factory-fabricated reroll path in `reroll.rs::execute` (steps 4 & 5:
/// `deps.vcs.base_head(...)` and `deps.vcs.create_branch_at(...)`) used to
/// shell out to the LOCAL `git` binary in the daemon process's CWD (its
/// systemd `WorkingDirectory`, the daemon's own source-repo checkout) --
/// structurally incapable of succeeding for any bead whose
/// `overlay.target_repo` names a DIFFERENT repo from `cfg.target_repo`.
///
/// This test seeds the bead with `target_repo = Some("jleechanorg/other-repo")`
/// (NOT the `cfg.target_repo` of "owner/repo") and asserts that the reroll
/// VCS calls route through the per-repo variant with that routed repo as
/// the first arg, AND that the legacy CWD-bound `base_head(...)` /
/// `create_branch_at(...)` calls are NOT issued. The fake's
/// `base_head_for_repo` is the only call site that returns a valid base SHA
/// for the cross-repo bead -- the legacy `base_head(...)` deliberately
/// returns Err for `main` in this test, so a regression to the old
/// code path fails the reroll outright.
#[test]
fn test_reroll_routes_vcs_ops_through_bead_repo_for_cross_repo_bead() {
    let scm = FakeScm::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = true;
    let mut vcs = FakeVcs::new();
    // Pre-seed only the cross-repo base branch. The legacy `base_head`
    // path is deliberately NOT seeded, so a regression to the
    // CWD-bound `base_head(main)` call fails the reroll with
    // `DaemonError::Tool` and the bead never reaches Recovery state.
    vcs.heads.insert(
        "jleechanorg/other-repo@main".into(),
        "cross-repo-base-sha".into(),
    );
    // Seed the prior attempt's head in the bare `heads` map: reroll's
    // pre-quiescence `head_sha_within` (a separate code path, unchanged
    // by this fix) reads `heads[branch]` directly. Without this entry
    // the reroll defers before reaching the per-repo base lookup.
    vcs.heads
        .insert("factory/bead-cross-repo-r1".into(), "head-sha-cross".into());
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into()
    ));

    let mut cfg = test_cfg();
    cfg.spec_dir = std::env::temp_dir()
        .join("afd_spec_dir_cross_repo_test")
        .to_string_lossy()
        .to_string();
    let spec_dir = std::path::Path::new(&cfg.spec_dir);
    let _ = std::fs::remove_dir_all(spec_dir);
    std::fs::create_dir_all(spec_dir).unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_reroll_cross_repo_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "bead-cross-repo".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 20,
        spend_usd: 0.5,
        pr_number: Some(4242),
        branch: Some("factory/bead-cross-repo-r1".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        // Cross-repo: bead's resolved repo DIFFERS from `cfg.target_repo`.
        // This is the exact shape of the live failure (8jxr / 9rkz class):
        // daemon's default repo is `jleechanorg/worldarchitect.ai`, the
        // bead's reroll target repo is something else.
        target_repo: Some("jleechanorg/other-repo".into()),
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "skeptic".into(),
        review_text: "Don't print to stdout, log errors.".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Rerolled { new_branch } => {
            assert_eq!(new_branch, "factory/bead-cross-repo-r2");
        }
        other => panic!("expected RerollOutcome::Rerolled, got {:?}", other),
    }

    // Verify overlay update.
    let updated = store.load("bead-cross-repo").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::Recovery);
    assert_eq!(updated.attempt, 2);
    assert_eq!(updated.reroll_count, 1);
    assert_eq!(updated.branch, Some("factory/bead-cross-repo-r2".into()));
    assert_eq!(updated.pr_number, None);

    // The whole point: VCS ops routed through the bead's resolved repo,
    // NOT through the CWD-bound legacy methods. The repo qualifier
    // "jleechanorg/other-repo" is what the bead's `overlay.repo(cfg)`
    // returns; `cfg.target_repo` ("owner/repo") would silently target
    // the daemon's default repo's same-named branches (the original bug).
    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls.iter().any(|c| c.contains(
            "base_head_for_repo(jleechanorg/other-repo,main)"
        )),
        "reroll must resolve base SHA via base_head_for_repo with the bead's repo; got: {vcs_calls:?}"
    );
    assert!(
        vcs_calls.iter().any(|c| c.contains(
            "create_branch_at_for_repo(jleechanorg/other-repo,factory/bead-cross-repo-r2"
        )),
        "reroll must create the new branch via create_branch_at_for_repo with the bead's repo; got: {vcs_calls:?}"
    );
    // No call to the legacy CWD-bound methods for cross-repo beads --
    // those would have computed the baseline / created the branch in the
    // daemon's own cwd (the daemon's source-repo checkout) instead of the
    // routed target repo.
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("base_head(")),
        "reroll must never call legacy CWD-bound base_head for a cross-repo bead; got: {vcs_calls:?}"
    );
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("create_branch_at(")),
        "reroll must never call legacy CWD-bound create_branch_at for a cross-repo bead; got: {vcs_calls:?}"
    );

    // PR close mirrors the bead's repo (PR #342 v6ud fix already covered
    // the gh-side; this test pins the git-side sibling).
    let scm_calls = scm.calls.borrow();
    assert!(
        scm_calls.iter().any(|c| c.contains(
            "close_pr_for_repo(jleechanorg/other-repo,4242"
        )),
        "reroll must close the old PR via close_pr_for_repo with the bead's repo; got: {scm_calls:?}"
    );

    std::fs::remove_dir_all(spec_dir).ok();
    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn test_tick_stage2_integration() {
    let mut scm = FakeScm::new();
    scm.issues.push(Issue {
        number: 5,
        title: "Feature X".into(),
        body: "Add feature X".into(),
        author_login: "dev".into(),
        external_ref: "owner/repo#5".into(),
    });
    scm.permissions.insert("dev".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let sessions = FakeSessions::new();
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), "base-sha-abc".into());
    vcs.heads
        .insert("factory/fake-bead-1-r1".into(), "head-sha-abc".into());

    let llm = FakeLlm::new();
    // Mock router response
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"x"}"#.into()
    ));

    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.spec_dir = std::env::temp_dir()
        .join("afd_spec_dir_tick_test")
        .to_string_lossy()
        .to_string();
    let spec_dir = std::path::Path::new(&cfg.spec_dir);
    let _ = std::fs::remove_dir_all(spec_dir);
    std::fs::create_dir_all(spec_dir).unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_tick_stage2_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let deps = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    // --- Tick 1: Intake -> Route -> Dispatch ---
    run_tick(&deps, 0, 0).unwrap();
    let overlay1 = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay1.state, OverlayState::Dispatched);

    // --- Prepare PR opening ---
    let mut overlay = overlay1;
    overlay.pr_number = Some(15);
    store.save(&overlay).unwrap();

    // jleechan-t40t r6: the slow-tier branch→PR re-resolution now
    // fail-closed-clears stale pr_number when the branch has no live PR.
    // Script the fake branch→PR lookup so the gate-assessment path
    // proceeds against the live PR 15.
    scm.pr_numbers_for_branch.insert(
        ("owner/repo".into(), "factory/fake-bead-1-r1".into()),
        Some(15),
    );

    scm.pr_snapshots.insert(
        15,
        PrSnapshot {
            pr_number: 15,
            ci_success: false, // CI fails! triggers re-roll path
            mergeable: true,
            coderabbit_approved: true,
            bugbot_error_count: 0,
            unresolved_thread_count: Some(0),
            head_sha: "head-sha-abc".into(),
            body: "".into(),
            // bead jleechan-8mlh: wire an /er PASS comment so the
            // EvidenceFloor gate is decisively Green (Pass on production
            // evidence). Without this, the evidence-floor gate reads
            // `ErVerdict::Absent` -> `Unknown`, which is now a transient
            // block that defers the reroll (auto-factory SKILL.md step 7:
            // "`unknown` defers to the next tick rather than racing to
            // READY"). The test exercises the spec-creation path on a real
            // red reroll, so the only non-green gates must be Red.
            comments: vec![PrComment {
                author: "dark-factory-bot".into(),
                body: "/er PASS".into(),
                created_at_epoch: 0,
            }],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "red".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
        bugbot_pending: false,
            head_committed_epoch: 0,
        },
    );

    // Mock Skeptic response for assessment, and then Mock Constraint Extractor response
    // First, Skeptic returns fail
    *llm.response.borrow_mut() = Some(Ok("fail build broke".into()));

    // Wait, the next judge call will be for constraint extraction!
    // Since FakeLlm in this integration test gets called sequentially:
    // 1. Skeptic: returns "fail build broke"
    // 2. Constraint Extractor: returns JSON
    // We can change the FakeLlm mock behavior dynamically or use a smart FakeLlm.
    // In our common::FakeLlm, it returns `response` regardless of prompt.
    // Let's modify llm.response inside a custom Llm implementation for this test!
    struct SmartLlm {
        state: std::cell::RefCell<usize>,
    }
    impl Llm for SmartLlm {
        fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
            let mut s = self.state.borrow_mut();
            *s += 1;
            if prompt.contains("Constraint Extractor") {
                Ok(r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log error"],"securityRedactionEncountered":false}"#.into())
            } else if prompt.contains("Skeptic") {
                Ok("fail skeptic failed".into())
            } else {
                Ok(r#"{"routingVerdict":"SMALL_PATH","justification":"x"}"#.into())
            }
        }
    }
    let smart_llm = SmartLlm {
        state: std::cell::RefCell::new(0),
    };

    let deps_smart = TickDeps {
        scm: &scm,
        tracker: &tracker,
        sessions: &sessions,
        llm: &smart_llm,
        store: &store,
        vcs: &vcs,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        vendor_health: None,
    };

    // --- Tick 2: assess gates (fails) -> execute re-roll -> spec mutation -> recovery -> redispatched ---
    let summary = run_tick(&deps_smart, 1, 0).unwrap();
    assert_eq!(summary.gates_assessed, 1);

    // Verify that the bead was re-rolled, mutated, validated, and transitioned to REDISPATCHED!
    let final_overlay = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(final_overlay.state, OverlayState::Redispatched);
    assert_eq!(final_overlay.attempt, 2);
    assert_eq!(final_overlay.reroll_count, 1);

    // Verify spec file was created and is valid TOML
    let spec_path = spec_dir.join("fake-bead-1.toml");
    assert!(spec_path.exists());
    let spec_content = std::fs::read_to_string(&spec_path).unwrap();
    assert!(spec_content.contains("reviewer = \"skeptic\""));

    std::fs::remove_dir_all(spec_dir).ok();
    let _ = std::fs::remove_file(&telemetry_log);
}

/// bead jleechan-tfs1, requirement (a) + (c): a red-gate reroll on an
/// ADOPTED bead must spawn a real remediation coder session on the EXISTING
/// contributor branch, briefed with the actual gate feedback. It must leave
/// the original PR OPEN, must never fabricate a replacement branch, must
/// never fabricate an empty fix commit in the daemon, and must never
/// force-push/rebase.
#[test]
fn test_reroll_adopted_success_spawns_remediation_session_leaves_pr_open() {
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    let mut vcs = FakeVcs::new();
    vcs.heads.insert(
        "alice/my-cool-feature".into(),
        "pre-session-sha-abc123".into(),
    );
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let telemetry_log = std::env::temp_dir().join("afd_reroll_adopted_success_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    // Adopted bead: branch is the contributor's OWN head_ref_name (does not
    // match the `factory/<bead>-r<n>` pattern at all — proving detection is
    // NOT based on branch-name shape), pr_number is the contributor's PR.
    let mut bead = BeadOverlay {
        bead_id: "bead-adopted".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 5,
        spend_usd: 0.0,
        pr_number: Some(777),
        branch: Some("alice/my-cool-feature".into()),
        session_id: None,
        is_adopted: true,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();
    store
        .register_branch("bead-adopted", "alice/my-cool-feature")
        .unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "verifier".into(),
        review_text: "CI check-run(s) not all success".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Rerolled { new_branch } => {
            assert_eq!(
                new_branch, "alice/my-cool-feature",
                "adopted remediation must reuse the EXISTING contributor branch"
            );
        }
        other => panic!("expected RerollOutcome::Rerolled, got {:?}", other),
    }

    let spawn_prompts = sessions.spawn_prompts.borrow();
    assert_eq!(
        spawn_prompts.len(),
        1,
        "adopted remediation must spawn exactly one coder session with real feedback: {spawn_prompts:?}"
    );
    let (spawned_bead_id, prompt) = &spawn_prompts[0];
    assert_eq!(spawned_bead_id, "bead-adopted");
    assert!(
        prompt.contains(&deps.review_text),
        "spawn prompt must include the literal gate feedback text: {prompt}"
    );
    assert!(
        prompt.contains("alice/my-cool-feature"),
        "spawn prompt must target the existing adopted branch: {prompt}"
    );

    // Overlay: attempt bumped, branch/pr_number UNCHANGED, now DISPATCHED
    // with the remediation session tracked until it quiesces.
    let updated = store.load("bead-adopted").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::Dispatched);
    assert_eq!(updated.attempt, 2);
    assert_eq!(updated.reroll_count, 1);
    assert_eq!(updated.branch.as_deref(), Some("alice/my-cool-feature"));
    assert_eq!(
        updated.pr_number,
        Some(777),
        "adopted remediation must leave the original PR number in place (PR stays open)"
    );

    // Branch ownership/registry unchanged: still exactly the one branch that
    // was already registered, no new `factory/...-r2`-shaped branch appeared.
    assert_eq!(
        store.branches.borrow().as_slice(),
        &["alice/my-cool-feature"],
        "adopted remediation must not register a fabricated replacement branch"
    );

    assert!(
        updated.session_id.is_some(),
        "adopted remediation must track the spawned coder session"
    );
    assert_eq!(
        updated.pre_session_head_sha.as_deref(),
        Some("pre-session-sha-abc123"),
        "adopted remediation must capture the pre-session HEAD SHA for later force-push detection"
    );

    // (c) Never force-pushes/rewrites history, never fabricates a branch or
    // daemon-side placeholder commit:
    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("push_fix_commit(")),
        "adopted remediation must never fabricate a daemon-side fix commit: {vcs_calls:?}"
    );
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")),
        "adopted remediation must never fabricate a replacement branch: {vcs_calls:?}"
    );
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.contains("force") && !c.contains("rebase")),
        "adopted remediation must never force-push or rebase: {vcs_calls:?}"
    );

    // (c) Never closes the original PR:
    let scm_calls = scm.calls.borrow();
    assert!(
        scm_calls
            .iter()
            .all(|c| !c.starts_with("close_pr_for_repo(") && !c.starts_with("close_pr(")),
        "adopted remediation must never close the contributor's PR: {scm_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// bead jleechan-tfs1, requirement (d): when the remediation coder session
/// cannot be spawned, `reroll::execute` must park the bead `HUMAN_HELD`
/// rather than fabricating a placeholder commit. This is the direct
/// `reroll::execute`-level proof; `tick_integration.rs` carries the
/// full-pipeline proof that the escalation comment is actually posted on the
/// PR.
#[test]
fn test_reroll_adopted_spawn_failure_parks_human_held() {
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    sessions.fail_spawn_for("bead-adopted-conflict");
    let mut vcs = FakeVcs::new();
    vcs.heads.insert(
        "alice/my-cool-feature".into(),
        "pre-session-sha-abc123".into(),
    );
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let telemetry_log = std::env::temp_dir().join("afd_reroll_adopted_failure_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "bead-adopted-conflict".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 5,
        spend_usd: 0.0,
        pr_number: Some(778),
        branch: Some("alice/my-cool-feature".into()),
        session_id: None,
        is_adopted: true,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();
    store
        .register_branch("bead-adopted-conflict", "alice/my-cool-feature")
        .unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "verifier".into(),
        review_text: "CI check-run(s) not all success".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Held(reason) => {
            assert!(
                reason.contains("failed to spawn a remediation coder session"),
                "Held reason should explain the session spawn failure: {reason}"
            );
        }
        other => panic!("expected RerollOutcome::Held, got {:?}", other),
    }

    let updated = store.load("bead-adopted-conflict").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::HumanHeld);
    // PR stays open and branch unchanged even on the failure path — a
    // failed remediation attempt must not touch either.
    assert_eq!(updated.pr_number, Some(778));
    assert_eq!(updated.branch.as_deref(), Some("alice/my-cool-feature"));

    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")),
        "a failed adopted remediation must never fall back to fabricating a branch: {vcs_calls:?}"
    );
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("push_fix_commit(")),
        "a failed adopted remediation must never fabricate a daemon-side fix commit: {vcs_calls:?}"
    );
    assert!(
        vcs_calls
            .iter()
            .all(|c| !c.contains("force") && !c.contains("rebase")),
        "a failed adopted remediation must never force-push or rebase as a fallback: {vcs_calls:?}"
    );
    let scm_calls = scm.calls.borrow();
    assert!(
        scm_calls
            .iter()
            .all(|c| !c.starts_with("close_pr_for_repo(") && !c.starts_with("close_pr(")),
        "a failed adopted remediation must never close the contributor's PR: {scm_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn adopted_spawn_failures_never_leave_an_untracked_or_recoverable_live_worker() {
    for case in ["typed-cleanup", "save-stop-ok", "save-stop-fails"] {
        let bead_id = format!("adopted-{case}");
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        if case == "typed-cleanup" {
            sessions.fail_spawn_cleanup_for(&bead_id);
        } else {
            sessions.fail_stop_for("fake-session-1");
            if case == "save-stop-ok" {
                sessions.fail_stop_for.borrow_mut().clear();
            }
        }
        let mut vcs = FakeVcs::new();
        vcs.heads.insert(
            "alice/my-cool-feature".into(),
            "pre-session-sha-abc123".into(),
        );
        let store = FakeStateStore::new();
        if case != "typed-cleanup" {
            store.fail_save_for(&bead_id, OverlayState::Dispatched);
        }
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log =
            std::env::temp_dir().join(format!("afd_reroll_{case}_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&telemetry_log);
        let mut bead = adopted_overlay(&bead_id);
        store.save(&bead).unwrap();

        let result = reroll::execute(
            &RerollDeps {
                scm: &scm,
                sessions: &sessions,
                vcs: &vcs,
                store: &store,
                llm: &llm,
                cfg: &cfg,
                telemetry_log: &telemetry_log,
                reviewer: "verifier".into(),
                review_text: "red gate".into(),
            },
            &mut bead,
        );
        assert!(result.is_err(), "{case} must surface a non-success result");

        let held = store.load(&bead_id).unwrap().unwrap();
        assert_eq!(held.state, OverlayState::HumanHeld, "case {case}");
        match case {
            "typed-cleanup" => {
                assert_eq!(
                    held.session_id.as_deref(),
                    Some(format!("leaked-{bead_id}").as_str())
                );
                assert!(matches!(
                    result,
                    Err(DaemonError::SpawnCleanupFailed { .. })
                ));
            }
            "save-stop-ok" => {
                assert_eq!(held.session_id, None);
                assert!(sessions
                    .calls
                    .borrow()
                    .iter()
                    .any(|call| call == "stop(fake-session-1)"));
            }
            "save-stop-fails" => {
                assert_eq!(held.session_id.as_deref(), Some("fake-session-1"));
                assert!(matches!(
                    result,
                    Err(DaemonError::SpawnCleanupFailed { .. })
                ));
            }
            _ => unreachable!(),
        }
        assert!(matches!(
            held.park_reason.as_deref(),
            Some("spawn_cleanup_failed" | "adopted_spawn_failed")
        ));

        let calls_before_recovery = sessions.calls.borrow().len();
        assert!(store.recover_human_held(10).unwrap().is_empty());
        let after_recovery = store.load(&bead_id).unwrap().unwrap();
        assert_eq!(after_recovery.state, held.state);
        assert_eq!(after_recovery.attempt, held.attempt);
        assert_eq!(after_recovery.session_id, held.session_id);
        assert_eq!(after_recovery.park_reason, held.park_reason);
        assert_eq!(calls_before_recovery, sessions.calls.borrow().len());
        let _ = std::fs::remove_file(&telemetry_log);
    }
}

#[test]
fn test_reroll_adopted_skips_duplicate_spawn_when_session_already_active() {
    let scm = FakeScm::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = false;
    let vcs = FakeVcs::new();
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let telemetry_log = std::env::temp_dir().join("afd_reroll_adopted_duplicate_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "bead-adopted".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 5,
        spend_usd: 0.0,
        pr_number: Some(777),
        branch: Some("alice/my-cool-feature".into()),
        session_id: None,
        is_adopted: true,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();
    store
        .register_branch("bead-adopted", "alice/my-cool-feature")
        .unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "verifier".into(),
        review_text: "CI check-run(s) not all success".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Held(reason) => assert!(reason.contains("already active")),
        other => panic!("expected fail-closed RerollOutcome::Held, got {other:?}"),
    }

    let session_calls = sessions.calls.borrow();
    assert!(
        session_calls
            .iter()
            .any(|c| c == "attach(alice/my-cool-feature,bead-adopted)"),
        "duplicate-spawn guard must attach to the branch before deciding: {session_calls:?}"
    );
    assert!(
        session_calls.iter().all(|c| !c.starts_with("spawn(")),
        "duplicate-spawn guard must not spawn another session: {session_calls:?}"
    );

    let updated = store.load("bead-adopted").unwrap().unwrap();
    assert_eq!(updated.attempt, 1);
    assert_eq!(updated.reroll_count, 0);
    assert_eq!(updated.state, OverlayState::HumanHeld);
    assert_eq!(updated.branch.as_deref(), Some("alice/my-cool-feature"));
    assert_eq!(updated.pr_number, Some(777));
    assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));
    assert_eq!(
        updated.park_reason.as_deref(),
        Some("adopted_session_already_active")
    );
    assert!(store.recover_human_held(10).unwrap().is_empty());

    let _ = std::fs::remove_file(&telemetry_log);
}

#[test]
fn adopted_spawn_crash_is_reconciled_without_duplicate_redispatch() {
    let bead_id = "adopted-crash-after-spawn";
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    sessions.panic_after_spawn_for(bead_id);
    let mut vcs = FakeVcs::new();
    vcs.heads.insert(
        "alice/my-cool-feature".into(),
        "pre-session-sha-abc123".into(),
    );
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    let cfg = test_cfg();
    let telemetry_log = std::env::temp_dir().join(format!(
        "afd_reroll_adopted_crash_{}.jsonl",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&telemetry_log);
    let mut bead = adopted_overlay(bead_id);
    store.save(&bead).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = reroll::execute(
            &RerollDeps {
                scm: &scm,
                sessions: &sessions,
                vcs: &vcs,
                store: &store,
                llm: &llm,
                cfg: &cfg,
                telemetry_log: &telemetry_log,
                reviewer: "verifier".into(),
                review_text: "red gate".into(),
            },
            &mut bead,
        );
    }));
    assert!(
        result.is_err(),
        "the fake must simulate process death after spawn"
    );

    let durable_intent = store.load(bead_id).unwrap().unwrap();
    assert_eq!(durable_intent.state, OverlayState::Dispatching);
    assert_eq!(durable_intent.session_id, None);
    assert_eq!(
        durable_intent.pre_session_head_sha.as_deref(),
        Some("pre-session-sha-abc123")
    );

    store.reconcile_dispatching().unwrap();
    let held = store.load(bead_id).unwrap().unwrap();
    assert_eq!(held.state, OverlayState::HumanHeld);
    assert_eq!(held.session_id, None);
    assert_eq!(
        held.park_reason.as_deref(),
        Some("ambiguous_dispatching_recovery")
    );
    assert!(store.recover_human_held(10).unwrap().is_empty());
    assert_eq!(
        sessions
            .calls
            .borrow()
            .iter()
            .filter(|call| call.starts_with("spawn("))
            .count(),
        1,
        "startup recovery must never create a second worker"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Bead jleechan-zeij / issue #322 (r3, adversarial Codex review of r2):
/// real-wall-clock tests against `reroll::execute`'s FAIL-CLOSED proceed
/// predicate (reroll.rs step 3). r3 hardened the confirmation: a post-stop
/// `Idle`/`NotFound` + two ~500ms HEAD reads does not prove process death, so
/// the predicate now supersedes ONLY on (a) attach()->SessionNotFound at
/// entry, (b) POSITIVE DEATH — a re-attach probe observing continuous
/// SessionNotFound for `reroll_death_confirm_secs` after stop() — or (c) a
/// WIDENED STABILITY WINDOW — a still-present non-running session (Terminal,
/// or Idle with a transcript quiet for the window) whose branch HEAD holds
/// stable for `reroll_head_stability_window_secs`. Everything else DEFERS.
///
/// These tests configure small windows (`test_cfg` sets window=1s,
/// death=0s; some override) so the timing is genuinely exercised against the
/// real 500ms poll loop while staying fast. Each test's doc comment states
/// its expected duration.
mod quiescence_timeout_races {
    use super::*;
    use daemon::tools::SessionActivity;

    fn race_test_bead(bead_id: &str, branch: &str) -> BeadOverlay {
        BeadOverlay {
            bead_id: bead_id.into(),
            state: OverlayState::Attested,
            attempt: 1,
            reroll_count: 0,
            autonomy_secs: 5,
            spend_usd: 0.0,
            pr_number: Some(900),
            branch: Some(branch.into()),
            session_id: None,
            is_adopted: false,
            spawn_failure_count: 0,
            pre_session_head_sha: None,
            park_reason: None,
            target_repo: None,
            attempt_started_at: None,
        }
    }

    fn now_epoch() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// A proceed test needs the spec-mutation step (constraint extraction) to
    /// succeed, so wire an LLM response + a real spec dir. Returns the cfg with
    /// the given windows and a spec dir handle the caller cleans up.
    fn proceed_cfg(tag: &str, window_secs: u64, death_secs: u64) -> (Config, std::path::PathBuf) {
        let mut cfg = test_cfg();
        cfg.reroll_head_stability_window_secs = window_secs;
        cfg.reroll_death_confirm_secs = death_secs;
        let spec_dir = std::env::temp_dir().join(format!("afd_r3_{tag}_spec"));
        cfg.spec_dir = spec_dir.to_string_lossy().to_string();
        let _ = std::fs::remove_dir_all(&spec_dir);
        std::fs::create_dir_all(&spec_dir).unwrap();
        (cfg, spec_dir)
    }

    fn proceed_llm() -> FakeLlm {
        let llm = FakeLlm::new();
        *llm.response.borrow_mut() = Some(Ok(
            r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into(),
        ));
        llm
    }

    /// Positive death (predicate b) — the strengthened #322 case E. `stop()`
    /// genuinely terminates the session, so the post-stop re-attach reports it
    /// gone; with `death_confirm_secs=0` the continuous-SessionNotFound streak
    /// confirms on the first re-attach and reroll proceeds FAST. Assert the
    /// session was actually stopped (positive termination) and the durable
    /// handle is cleared. Real wall-clock duration: near-zero.
    #[test]
    fn positive_death_proceeds_fast() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-pd".into());
        let branch = "factory/bead-race-pd-r1";
        vcs.heads.insert(branch.into(), "head-sha-pd".into());
        let store = FakeStateStore::new();
        let llm = proceed_llm();
        let (cfg, spec_dir) = proceed_cfg("pd", 30, 0); // window irrelevant (death path)
        let telemetry_log = std::env::temp_dir().join("afd_r3_pd_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-pd", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        // NOT an orphan: stop() actually terminates, so re-attach -> SessionNotFound.

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "worker done; stop terminates it".into(),
        };

        let start = Instant::now();
        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-pd-r2");
            }
            other => panic!("expected Rerolled via positive death, got {:?}", other),
        }
        assert!(
            elapsed < Duration::from_secs(10),
            "positive death (death_confirm_secs=0) must confirm fast, got {elapsed:?}"
        );

        let updated = store.load("bead-race-pd").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);
        assert_eq!(
            updated.session_id, None,
            "a confirmed proceed must clear the durable session handle"
        );

        // Positive termination: stop() was actually called, and a re-attach
        // probe ran after it (proving the death check, not a one-instant read).
        let calls = sessions.calls.borrow();
        assert!(
            calls.iter().any(|c| c.starts_with("stop(")),
            "stop() must be called before superseding, got {calls:?}"
        );
        let attach_count = calls.iter().filter(|c| c.starts_with("attach(")).count();
        assert!(
            attach_count >= 2,
            "expected an entry attach + at least one post-stop death-probe re-attach, got {calls:?}"
        );

        std::fs::remove_dir_all(&spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// No-live-session fast path (predicate a): the previous worker was already
    /// reaped before reroll, so the ENTRY attach returns SessionNotFound.
    /// Reroll proceeds immediately, never calling stop()/session_activity, and
    /// clears the durable handle. Real wall-clock duration: near-zero.
    #[test]
    fn no_live_session_fast_path_proceeds() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-nf".into());
        let branch = "factory/bead-race-nf-r1";
        vcs.heads.insert(branch.into(), "head-sha-nf".into());
        let store = FakeStateStore::new();
        let llm = proceed_llm();
        let (cfg, spec_dir) = proceed_cfg("nf", 1, 0);
        let telemetry_log = std::env::temp_dir().join("afd_r3_nf_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-nf", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.attach_not_found_for(branch);

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "already reaped".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-nf-r2");
            }
            other => panic!(
                "expected Rerolled on the no-live-session fast path, got {:?}",
                other
            ),
        }

        let updated = store.load("bead-race-nf").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);
        assert_eq!(updated.session_id, None);

        let calls = sessions.calls.borrow();
        assert!(
            calls
                .iter()
                .all(|c| !c.starts_with("stop(") && !c.starts_with("session_activity(")),
            "the entry-SessionNotFound fast path must skip stop/liveness polling, got {calls:?}"
        );

        std::fs::remove_dir_all(&spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Terminal + stable HEAD over the widened window (predicate c-terminal):
    /// the session survives stop() as an orphan (`ao session kill` swallowed
    /// termination) but reports Terminal activity with a static HEAD. It must
    /// proceed only AFTER the HEAD holds stable for the full window (here 1s),
    /// not after a ~500ms two-read check. Real wall-clock duration: ~1s.
    #[test]
    fn terminal_stable_window_proceeds() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-tw".into());
        let branch = "factory/bead-race-tw-r1";
        vcs.heads.insert(branch.into(), "head-sha-tw".into());
        let store = FakeStateStore::new();
        let llm = proceed_llm();
        let (cfg, spec_dir) = proceed_cfg("tw", 1, 0);
        let telemetry_log = std::env::temp_dir().join("afd_r3_tw_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-tw", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        // Orphan: survives stop() and is probed as Terminal.
        sessions.set_orphan_after_stop();
        sessions.set_activity(SessionActivity::Terminal);

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "terminal orphan, static head".into(),
        };

        let start = Instant::now();
        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-tw-r2");
            }
            other => panic!(
                "expected Rerolled via stable-window terminal, got {:?}",
                other
            ),
        }
        let elapsed = start.elapsed();
        // Must span the full 1s window (proving it is not a ~500ms check), but
        // not the death window (session never goes SessionNotFound).
        assert!(
            elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(10),
            "expected confirmation after the full 1s stability window, got {elapsed:?}"
        );

        let updated = store.load("bead-race-tw").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);
        assert_eq!(updated.session_id, None);

        std::fs::remove_dir_all(&spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Idle + quiet transcript + stable HEAD over the window (predicate
    /// c-idle): an orphan session reports `activity=idle`, but the idle
    /// classification only counts as non-running because the coder's transcript
    /// last-activity timestamp is old (quiet) for the whole window. Proceeds
    /// via stable_window_idle. Real wall-clock duration: ~1s.
    #[test]
    fn idle_quiet_transcript_proceeds() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-iq".into());
        let branch = "factory/bead-race-iq-r1";
        vcs.heads.insert(branch.into(), "head-sha-iq".into());
        let store = FakeStateStore::new();
        let llm = proceed_llm();
        let (cfg, spec_dir) = proceed_cfg("iq", 1, 0);
        let telemetry_log = std::env::temp_dir().join("afd_r3_iq_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-iq", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.set_orphan_after_stop();
        sessions.set_activity(SessionActivity::Idle);
        // ao_project resolves to "repo" (last path segment of owner/repo);
        // transcript quiet: last activity 1000s ago, far beyond the 1s window.
        sessions.set_transcript_activity("repo", branch, now_epoch().saturating_sub(1000));

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "idle orphan, quiet transcript".into(),
        };

        let start = Instant::now();
        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-iq-r2");
            }
            other => panic!("expected Rerolled via stable-window idle, got {:?}", other),
        }
        assert!(start.elapsed() >= Duration::from_secs(1));

        let updated = store.load("bead-race-iq").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);
        assert_eq!(updated.session_id, None);

        std::fs::remove_dir_all(&spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Idle-but-mid-tool-call must DEFER (Codex P1b counter-case): the orphan
    /// session reports `activity=idle` with a static HEAD, but the coder's
    /// transcript shows RECENT activity — the worker is blocked in a long tool
    /// call, not done. The idle classification is therefore NOT quiet, so the
    /// predicate never confirms and DEFERS. session_id is preserved, no branch
    /// fabricated, no PR closed. Real wall-clock duration: ~1s.
    #[test]
    fn idle_but_recently_active_transcript_defers() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-ir".into());
        let branch = "factory/bead-race-ir-r1";
        vcs.heads.insert(branch.into(), "head-sha-ir".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let mut cfg = test_cfg();
        cfg.reroll_head_stability_window_secs = 1;
        cfg.reroll_death_confirm_secs = 0;
        let telemetry_log = std::env::temp_dir().join("afd_r3_ir_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-ir", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.set_orphan_after_stop();
        sessions.set_activity(SessionActivity::Idle);
        // Transcript updated just now — a live mid-tool-call worker.
        sessions.set_transcript_activity("repo", branch, now_epoch());

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "idle but mid-tool-call".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Deferred(reason) => {
                assert_eq!(reason, "unconfirmed_live_or_moving_head");
            }
            other => panic!(
                "expected Deferred on an idle-but-recently-active worker (P1b), got {:?}",
                other
            ),
        }

        let updated = store.load("bead-race-ir").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));
        let vcs_calls = vcs.calls.borrow();
        assert!(vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")));
        let scm_calls = scm.calls.borrow();
        assert!(scm_calls
            .iter()
            .all(|c| !c.starts_with("close_pr_for_repo(") && !c.starts_with("close_pr(")));

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Live worker still running + pushing must DEFER: an orphan session
    /// reports `activity=running` with a moving HEAD for the whole window. The
    /// predicate never confirms (running resets the stability streak every
    /// poll), so no branch is fabricated, the PR is not closed, and session_id
    /// is preserved. Real wall-clock duration: ~1s.
    #[test]
    fn live_worker_running_moving_head_defers() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let branch = "factory/bead-race-live-r1";
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-live".into());
        vcs.heads.insert(branch.into(), "head-live-0".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let mut cfg = test_cfg();
        cfg.reroll_head_stability_window_secs = 1;
        cfg.reroll_death_confirm_secs = 0;
        let telemetry_log = std::env::temp_dir().join("afd_r3_live_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-live", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.set_orphan_after_stop();
        sessions.set_activity(SessionActivity::Running);
        let t0 = Instant::now();
        vcs.schedule_head_sha(branch, t0, "head-live-0");
        vcs.schedule_head_sha(branch, t0 + Duration::from_millis(400), "head-live-1");
        vcs.schedule_head_sha(branch, t0 + Duration::from_millis(800), "head-live-2");

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "live worker still pushing".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Deferred(reason) => {
                assert_eq!(reason, "unconfirmed_live_or_moving_head");
            }
            other => panic!(
                "expected Deferred on a live+pushing worker, got {:?}",
                other
            ),
        }

        let updated = store.load("bead-race-live").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));
        assert_eq!(store.reroll_deferral_count("bead-race-live").unwrap(), 1);
        let vcs_calls = vcs.calls.borrow();
        assert!(vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")));
        let scm_calls = scm.calls.borrow();
        assert!(scm_calls
            .iter()
            .all(|c| !c.starts_with("close_pr_for_repo(") && !c.starts_with("close_pr(")));

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Mid-window push is detected and DEFERS (Codex P3 + req 3): an orphan
    /// Terminal session's HEAD moves partway through the window. Because
    /// head_sha is sampled on EVERY poll (before any activity break), the push
    /// resets the stability streak; the streak cannot re-accumulate the full
    /// window before the deadline, so the predicate DEFERS this tick (it would
    /// proceed on a later tick once the HEAD is stable from the start). Proves
    /// head_sha is polled multiple times. Real wall-clock duration: ~1s.
    #[test]
    fn mid_window_push_resets_streak_and_defers() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let branch = "factory/bead-race-mp-r1";
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-mp".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let mut cfg = test_cfg();
        cfg.reroll_head_stability_window_secs = 1;
        cfg.reroll_death_confirm_secs = 0;
        let telemetry_log = std::env::temp_dir().join("afd_r3_mp_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-mp", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.set_orphan_after_stop();
        sessions.set_activity(SessionActivity::Terminal);
        // HEAD is "sha-a" until ~700ms, then pushes to "sha-b" — mid-window,
        // resetting the 1s stability streak so it can't complete before the
        // 1s deadline.
        let t0 = Instant::now();
        vcs.schedule_head_sha(branch, t0, "sha-a");
        vcs.schedule_head_sha(branch, t0 + Duration::from_millis(700), "sha-b");

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "push lands mid-window".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Deferred(_) => {}
            other => panic!(
                "expected Deferred when a push lands mid-window, got {:?}",
                other
            ),
        }

        // Proves head_sha was sampled every poll (not once): the moving HEAD
        // was observed, which is only possible with repeated sampling.
        let head_polls = vcs
            .calls
            .borrow()
            .iter()
            .filter(|c| c.as_str() == format!("head_sha({branch})"))
            .count();
        assert!(
            head_polls >= 2,
            "head_sha must be sampled on every poll (Codex P3), saw {head_polls} calls"
        );

        let updated = store.load("bead-race-mp").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// A TRANSIENT stop() failure DEFERS (req 4 / Codex P2): the kill did not
    /// succeed, so the worker may be alive — the predicate must not evaluate
    /// past it. Fails fast (no poll wait). session_id preserved, no
    /// branch/PR. Real wall-clock duration: near-zero.
    #[test]
    fn transient_stop_failure_defers() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-sf".into());
        let branch = "factory/bead-race-sf-r1";
        vcs.heads.insert(branch.into(), "head-sha-sf".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_r3_sf_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-sf", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.fail_stop_for("fake-session-1"); // transient (DaemonError::Tool)

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "transient stop failure".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Deferred(reason) => assert_eq!(reason, "stop_failed"),
            other => panic!(
                "expected Deferred on a transient stop() failure, got {:?}",
                other
            ),
        }

        let updated = store.load("bead-race-sf").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));
        assert_eq!(store.reroll_deferral_count("bead-race-sf").unwrap(), 1);
        let vcs_calls = vcs.calls.borrow();
        assert!(vcs_calls
            .iter()
            .all(|c| !c.starts_with("create_branch_at(")));

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// A PERMANENT stop() failure PROPAGATES (Codex P2): only transient errors
    /// enter the deferral path; a non-transient kill failure surfaces as an
    /// error outcome, never a silent defer/park. Real wall-clock duration:
    /// near-zero.
    #[test]
    fn permanent_stop_error_propagates() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-ps".into());
        let branch = "factory/bead-race-ps-r1";
        vcs.heads.insert(branch.into(), "head-sha-ps".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_r3_ps_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-ps", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.fail_stop_permanent_for("fake-session-1"); // permanent (DaemonError::Parse)

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "permanent stop failure".into(),
        };

        let result = reroll::execute(&deps, &mut bead);
        assert!(
            matches!(result, Err(DaemonError::Parse(_))),
            "a permanent stop() error must propagate, got {:?}",
            result
        );

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// A PERMANENT entry-attach error PROPAGATES (Codex P2 — changed from r2's
    /// HUMAN_HELD park): an ambiguous/malformed `ao status` cannot identify the
    /// session, and only transient errors enter deferral, so it surfaces as an
    /// error outcome. Real wall-clock duration: near-zero.
    #[test]
    fn permanent_attach_error_propagates() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-ha".into());
        let branch = "factory/bead-race-ha-r1";
        vcs.heads.insert(branch.into(), "head-sha-ha".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_r3_ha_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-ha", branch);
        store.save(&bead).unwrap();
        sessions.fail_attach_permanent_for(branch);

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "ambiguous ao status".into(),
        };

        let result = reroll::execute(&deps, &mut bead);
        assert!(
            matches!(result, Err(DaemonError::Parse(_))),
            "a permanent attach error must propagate (not park), got {:?}",
            result
        );
        // Not counted as a deferral.
        assert_eq!(store.reroll_deferral_count("bead-race-ha").unwrap(), 0);

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// A TRANSIENT entry-attach error DEFERS: a momentary `ao status` failure
    /// means the session can't be identified this tick — defer and retry.
    /// Real wall-clock duration: near-zero.
    #[test]
    fn transient_attach_error_defers() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-at".into());
        let branch = "factory/bead-race-at-r1";
        vcs.heads.insert(branch.into(), "head-sha-at".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_r3_at_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-at", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.fail_attach_transient_for(branch);

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "transient attach failure".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Deferred(reason) => assert_eq!(reason, "attach_transient"),
            other => panic!(
                "expected Deferred on a transient attach error, got {:?}",
                other
            ),
        }
        let updated = store.load("bead-race-at").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// A PERMANENT liveness-probe error PROPAGATES (req 5): during the poll
    /// loop, `session_activity` on a present (orphan) session hits a
    /// non-transient parse failure — it must surface as an error, not be
    /// swallowed as a defer. Real wall-clock duration: near-zero.
    #[test]
    fn permanent_liveness_error_propagates() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-pe".into());
        let branch = "factory/bead-race-pe-r1";
        vcs.heads.insert(branch.into(), "head-sha-pe".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_r3_pe_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-pe", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        // Orphan so the session is present in the poll loop, then its activity
        // probe fails permanently.
        sessions.set_orphan_after_stop();
        sessions.fail_activity_permanent("ao status JSON must be an array");

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "malformed ao status".into(),
        };

        let result = reroll::execute(&deps, &mut bead);
        assert!(
            matches!(result, Err(DaemonError::Parse(_))),
            "a permanent liveness-probe error must propagate, got {:?}",
            result
        );

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// The bounded deferral counter escalates to HUMAN_HELD at the cap: a
    /// worker that defers on every tick (here via a persistently-failing
    /// transient stop(), so each call fails fast) is retried
    /// `MAX_REROLL_DEFERRALS` times, and only the cap-th call parks HUMAN_HELD.
    /// Real wall-clock duration: near-zero (no poll waits).
    #[test]
    fn deferral_cap_parks_human_held() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-cap".into());
        let branch = "factory/bead-race-cap-r1";
        vcs.heads.insert(branch.into(), "head-sha-cap".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_r3_cap_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-cap", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        sessions.fail_stop_for("fake-session-1");

        const CAP: u32 = 5; // MAX_REROLL_DEFERRALS
        for tick in 1..CAP {
            let deps = RerollDeps {
                scm: &scm,
                sessions: &sessions,
                vcs: &vcs,
                store: &store,
                llm: &llm,
                cfg: &cfg,
                telemetry_log: &telemetry_log,
                reviewer: "skeptic".into(),
                review_text: "stop keeps failing".into(),
            };
            let mut b = store.load("bead-race-cap").unwrap().unwrap();
            match reroll::execute(&deps, &mut b).unwrap() {
                RerollOutcome::Deferred(_) => {}
                other => panic!(
                    "tick {tick}: expected Deferred below the cap, got {:?}",
                    other
                ),
            }
            assert_eq!(store.reroll_deferral_count("bead-race-cap").unwrap(), tick);
            assert_eq!(
                store.load("bead-race-cap").unwrap().unwrap().state,
                OverlayState::Attested
            );
        }

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "stop keeps failing".into(),
        };
        let mut b = store.load("bead-race-cap").unwrap().unwrap();
        match reroll::execute(&deps, &mut b).unwrap() {
            RerollOutcome::Held(reason) => {
                assert!(
                    reason.contains("deferred") && reason.contains("cap"),
                    "expected a deferral-cap Held reason, got: {reason}"
                );
            }
            other => panic!("expected Held at the deferral cap, got {:?}", other),
        }
        let updated = store.load("bead-race-cap").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::HumanHeld);
        assert_eq!(
            updated.park_reason.as_deref(),
            Some("reroll_quiescence_deferral_cap_exceeded")
        );

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Codex r4 P1 — a `NotFound` observation inside the poll must NOT
    /// shortcut the stable-HEAD (window) lane; it routes ONLY through
    /// positive-death, and any successful re-attach after it resets that
    /// streak. A FLAPPING session — Terminal (building a stable-HEAD streak),
    /// then a single NotFound poll after the window has elapsed, then Running
    /// — must DEFER: r2 would have superseded on that NotFound poll (treating
    /// it like Terminal with a stable HEAD), r4 does not. HEAD is static
    /// throughout; window=1s, death=2s so continuous-NotFound death cannot
    /// confirm before the flap breaks it. Real wall-clock duration: ~2s.
    #[test]
    fn notfound_activity_does_not_shortcut_stable_window_lane() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let branch = "factory/bead-race-nfl-r1";
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-nfl".into());
        vcs.heads.insert(branch.into(), "head-sha-nfl".into()); // static HEAD
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let mut cfg = test_cfg();
        cfg.reroll_head_stability_window_secs = 1;
        cfg.reroll_death_confirm_secs = 2;
        let telemetry_log = std::env::temp_dir().join("afd_r4_nfl_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-nfl", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        // Present (orphan) so `session_activity` is what's probed; the flap:
        // Terminal, Terminal, NotFound (past the 1s window), then Running.
        sessions.set_orphan_after_stop();
        sessions.set_activity_sequence(vec![
            SessionActivity::Terminal,
            SessionActivity::Terminal,
            SessionActivity::NotFound,
            SessionActivity::Running,
        ]);
        // After the sequence, stay Running (a live worker) so nothing later
        // grants a stable-window proceed.
        sessions.set_activity(SessionActivity::Running);

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "flapping NotFound must not shortcut".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Deferred(reason) => {
                assert_eq!(reason, "unconfirmed_live_or_moving_head");
            }
            other => panic!(
                "expected Deferred — a NotFound poll must not supersede via the stable-HEAD lane (r4 P1), got {:?}",
                other
            ),
        }

        let updated = store.load("bead-race-nfl").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));
        let vcs_calls = vcs.calls.borrow();
        assert!(
            vcs_calls
                .iter()
                .all(|c| !c.starts_with("create_branch_at(")),
            "a NotFound flap must not fabricate a branch: {vcs_calls:?}"
        );

        let _ = std::fs::remove_file(&telemetry_log);
    }
}
/// semantic comparator LLM call must NOT crash the daemon. Before this fix,
/// `same_underlying_issue` (reroll.rs) constructed `DaemonError::Parse` for
/// this case, which `is_transient()` does not cover -- the exact
/// jleechan-5ia2 crash-loop pattern (PR #197), reintroduced via this
/// brand-new subprocess-LLM call site. This test drives the REAL
/// `reroll::execute` through a second-attempt circuit-breaker comparison
/// (attempt=2, same reviewer as the stored attempt-1 rejection) with a
/// scripted LLM reply that contains no JSON object at all, and asserts the
/// resulting error is transient.
#[test]
fn same_underlying_issue_malformed_reply_is_transient_not_fatal() {
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    let vcs = FakeVcs::new();
    let store = FakeStateStore::new();
    let cfg = test_cfg();
    let llm = FakeLlm::new();
    // No JSON object anywhere in this reply -- `reply.rfind('}')` returns
    // None, exercising exactly the failure mode jleechan-cq8r found.
    *llm.response.borrow_mut() = Some(Ok(
        "the model babbled without any JSON object at all".to_string()
    ));

    let telemetry_log = std::env::temp_dir().join("afd_cq8r_malformed_comparator.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "cq8r-bead".into(),
        state: OverlayState::Attested,
        attempt: 2,
        reroll_count: 1,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(900),
        branch: Some("factory/cq8r-bead-r2".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();
    store
        .save_rejection(
            "cq8r-bead",
            1,
            "verifier",
            "deadbeefdeadbeef",
            "prior rejection text",
        )
        .unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "verifier".to_string(),
        review_text: "new rejection text, different from prior".to_string(),
    };

    let err = reroll::execute(&deps, &mut bead)
        .expect_err("malformed comparator reply must surface as an Err, not silently succeed");

    assert!(
        matches!(err, DaemonError::ComparatorUnparseable(_)),
        "expected DaemonError::ComparatorUnparseable, got {err:?}"
    );
    assert!(
        err.is_transient(),
        "a malformed circuit-breaker comparator reply must be classified transient (jleechan-cq8r / jleechan-5ia2 pattern), got non-transient: {err:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
}

/// jleechan-v6ud / issue #340 — RECURSIVE INSTANCE of the jleechan-8jxr /
/// jleechan-drive-pr-branch-binding-pcpr #306 bug class.
///
/// Live failure: at 2026-07-18T21:46:53Z the reroll for `jleechan-8jxr`
/// (real PR: jleechanorg/dark-factory#315, OPEN) attempted to close its
/// superseded r1 PR after a clean `REROLL_BRANCH_CREATED`. The close call
/// resolved PR #315 against `jleechanorg/worldarchitect.ai` instead of
/// `jleechanorg/dark-factory` — a different, already-merged PR that
/// happened to share the same number — and `gh pr close` errored with
/// "can't be closed because it was already merged", wedging the bead on
/// a transient tool error. The identical failure mode hit `jleechan-9rkz`
/// (#314) twice on the same day. Repro construction below:
///   * `cfg.target_repo = "jleechanorg/worldarchitect.ai"` (the daemon
///     default — the OLD wrong target for the close call).
///   * `bead.target_repo = Some("jleechanorg/dark-factory")` (Stage A
///     intake resolved repo — the BEAD's real target).
///   * The bead has `pr_number = Some(315)`. In production there are TWO
///     same-numbered PRs: the bead's real open PR in
///     `jleechanorg/dark-factory#315`, AND a different merged PR
///     `jleechanorg/worldarchitect.ai#315`. Reroll's old
///     `close_pr(pr_number, comment)` was bound at `main.rs` construction
///     time to `cfg.target_repo`, so it would `gh pr close 315 --repo
///     jleechanorg/worldarchitect.ai` against the merged one and error
///     out. Reroll's new `close_pr_for_repo(bead.repo(cfg), ...)` MUST
///     close the bead's real repo's PR (`jleechanorg/dark-factory`) and
///     MUST NOT touch the default repo's PR.
///
/// The test asserts:
///   (a) The recorded SCM call uses `close_pr_for_repo(<bead's repo>, 315, ...)`
///       — not the legacy `close_pr(315, ...)` form.
///   (b) The recorded repo string is `jleechanorg/dark-factory`, NOT
///       `cfg.target_repo` (`jleechanorg/worldarchitect.ai`). This is the
///       exact assertion that would have caught the jleechan-8jxr /
///       jleechan-9rkz regression pre-deploy.
///   (c) The reroll completes (`RerollOutcome::Rerolled`), proving the
///       bead's resolved repo is now usable end-to-end.
#[test]
fn test_reroll_close_pr_uses_bead_resolved_repo_not_cfg_target_repo() {
    let scm = FakeScm::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = true;
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), "base-sha-v6ud".into());
    vcs.heads
        .insert("factory/jleechan-8jxr-r1".into(), "head-sha-v6ud".into());
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"inhibitionSpecs":["address 315"],"positiveAssertions":["close bead's PR"],"securityRedactionEncountered":false}"#.into()
    ));

    // `cfg.target_repo` deliberately names the OLD wrong default
    // (`worldarchitect.ai`) — the same daemon-wide default that the live
    // failure had. This is the repo reroll.rs MUST NOT use for the close
    // call when the bead has a different resolved `target_repo`.
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/worldarchitect.ai".into();
    cfg.spec_dir = std::env::temp_dir()
        .join("afd_spec_dir_v6ud_test")
        .to_string_lossy()
        .to_string();
    let spec_dir = std::path::Path::new(&cfg.spec_dir);
    let _ = std::fs::remove_dir_all(spec_dir);
    std::fs::create_dir_all(spec_dir).unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_reroll_v6ud_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    // The bead has its OWN resolved `target_repo` (Stage A intake) —
    // this is what reroll MUST use for the PR-close call.
    let mut bead = BeadOverlay {
        bead_id: "jleechan-8jxr".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 30,
        spend_usd: 0.5,
        pr_number: Some(315), // same number as the merged default-repo PR
        branch: Some("factory/jleechan-8jxr-r1".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: Some("jleechanorg/dark-factory".into()),
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "skeptic".into(),
        review_text: "Address the open comments on 8jxr.".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Rerolled { new_branch } => {
            assert_eq!(new_branch, "factory/jleechan-8jxr-r2");
        }
        other => panic!("expected RerollOutcome::Rerolled, got {:?}", other),
    }

    // (a) + (b): the close call MUST be repo-scoped to the bead's
    // resolved repo (`jleechanorg/dark-factory`), NOT `cfg.target_repo`
    // (`jleechanorg/worldarchitect.ai`).
    let scm_calls = scm.calls.borrow();
    let close_calls: Vec<&String> = scm_calls
        .iter()
        .filter(|c| c.starts_with("close_pr_for_repo(") || c.starts_with("close_pr("))
        .collect();
    assert_eq!(
        close_calls.len(),
        1,
        "reroll must make exactly one PR-close call; got: {scm_calls:?}"
    );
    assert!(
        close_calls[0].starts_with("close_pr_for_repo(jleechanorg/dark-factory,315,"),
        "reroll must close the bead's resolved repo's PR (jleechanorg/dark-factory#315); \
         the live failure for 8jxr/9rkz was that it targeted cfg.target_repo's \
         same-numbered PR (jleechanorg/worldarchitect.ai#315, already merged). \
         Actual close call: {}",
        close_calls[0]
    );
    // Belt-and-suspenders: explicit anti-assertion that the wrong repo
    // was NOT targeted. If the regression recurs, this string would
    // appear in the recorded call and the test fails loudly with a
    // bead ID + repo pair (not just "close_pr" was missing).
    assert!(
        !close_calls[0].contains("worldarchitect.ai"),
        "reroll closed against cfg.target_repo (worldarchitect.ai), which is the exact \
         jleechan-8jxr/9rkz regression: got {}",
        close_calls[0]
    );

    let updated = store.load("jleechan-8jxr").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::Recovery);
    assert_eq!(updated.attempt, 2);
    assert_eq!(updated.pr_number, None);

    std::fs::remove_dir_all(spec_dir).ok();
    let _ = std::fs::remove_file(&telemetry_log);
}

/// Regression test for issue #341 / bead jleechan-znmh — reroll branch
/// creation must be reuse-or-reset-idempotent. When a prior failed
/// reroll attempt left a stale `factory/<bead>-r<n>` ref behind in the
/// routed repo (the live failure for jleechan-9rkz, 2026-07-18), the
/// next retry's `create_branch_at_for_repo` POST hits
/// `Reference already exists (refs/heads/<name>)` from the GH Data API
/// (HTTP 422). The reroll must classify that stderr signature, delete
/// the stale ref via `delete_branch_at_for_repo`, and retry the create —
/// NOT wedge the bead on a transient tool error.
#[test]
fn test_reroll_recovers_from_stale_local_remote_branch_on_retry() {
    let scm = FakeScm::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = true;
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), "base-sha-stale".into());
    vcs.heads
        .insert("factory/bead-stale-r1".into(), "head-sha-stale".into());
    // Script: the routed repo already has a `factory/bead-stale-r2` ref
    // (left behind by a prior failed reroll). The fake's
    // `create_branch_at_for_repo` returns the canonical GH 422 stderr
    // shape on the first call; on the second call (after reroll deletes
    // the stale ref via the new `delete_branch_at_for_repo` entry point)
    // it succeeds — matching how the real `CliVcs` will behave once the
    // production code does the delete-then-retry dance.
    vcs.stale_branch_exists_at.borrow_mut().insert(
        ("owner/repo".to_string(), "factory/bead-stale-r2".to_string()),
        "gh: Reference already exists (refs/heads/factory/bead-stale-r2) \
         (HTTP 422)"
            .to_string(),
    );

    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into()
    ));

    let mut cfg = test_cfg();
    cfg.spec_dir = std::env::temp_dir()
        .join("afd_spec_dir_stale_branch_test")
        .to_string_lossy()
        .to_string();
    let spec_dir = std::path::Path::new(&cfg.spec_dir);
    let _ = std::fs::remove_dir_all(spec_dir);
    std::fs::create_dir_all(spec_dir).unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_reroll_stale_branch_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "bead-stale".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 20,
        spend_usd: 0.5,
        pr_number: Some(303),
        branch: Some("factory/bead-stale-r1".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "skeptic".into(),
        review_text: "Don't print to stdout, log errors.".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Rerolled { new_branch } => {
            assert_eq!(new_branch, "factory/bead-stale-r2");
        }
        other => panic!(
            "expected RerollOutcome::Rerolled (reroll must recover from stale local -rN branch \
             left behind by a prior failed attempt, NOT wedge on the 422); got {:?}",
            other
        ),
    }

    // Verify the reroll actually called delete on the stale ref before
    // retrying the create — without this assertion a regression that
    // retries the create without deleting would still pass the green
    // path above (since the fake's second create succeeds), so we must
    // pin the delete-then-retry order explicitly.
    let vcs_calls = vcs.calls.borrow();
    let create_indices: Vec<usize> = vcs_calls
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if c.contains("create_branch_at_for_repo(owner/repo,factory/bead-stale-r2,") {
                Some(i)
            } else {
                None
            }
        })
        .collect();
    let delete_indices: Vec<usize> = vcs_calls
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if c.contains("delete_branch_at_for_repo(owner/repo,factory/bead-stale-r2)") {
                Some(i)
            } else {
                None
            }
        })
        .collect();
    assert!(
        !create_indices.is_empty(),
        "reroll never called create_branch_at_for_repo for the new -r2 branch: {vcs_calls:?}"
    );
    assert!(
        !delete_indices.is_empty(),
        "reroll reached Recovery but did NOT call delete_branch_at_for_repo for the stale -r2 ref — \
         the very next real attempt would still wedge on the same 422: {vcs_calls:?}"
    );
    // Ordering: the delete MUST fall between the first create (which
    // failed with the 422) and the second create (which succeeded). A
    // regression that retries the create without deleting — or that
    // deletes AFTER the successful retry — would still produce a green
    // path above, so we pin the exact delete-then-retry sandwich here.
    let first_create = *create_indices.iter().min().unwrap();
    let last_create = *create_indices.iter().max().unwrap();
    let delete_in_between = delete_indices
        .iter()
        .any(|&d| d > first_create && d < last_create);
    assert!(
        delete_in_between,
        "delete-then-retry sandwich violated: first_create={first_create}, \
         last_create={last_create}, deletes at {delete_indices:?}; \
         a delete must land BETWEEN the failing create and the successful retry create"
    );

    let updated = store.load("bead-stale").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::Recovery);
    assert_eq!(updated.attempt, 2);
    assert_eq!(updated.branch, Some("factory/bead-stale-r2".into()));

    std::fs::remove_dir_all(spec_dir).ok();
    let _ = std::fs::remove_file(&telemetry_log);
}

/// Regression test for issue #341 / bead jleechan-znmh (acceptance
/// criterion #2): when step 7's `close_pr_for_repo` fails because the
/// PR is already merged or already closed (the live failure for
/// jleechan-8jxr, 2026-07-18 — a separate process merged the PR between
/// the reroll's snapshot and its close attempt), the reroll must
/// tolerate that as a successful supersede rather than wedge the bead
/// on a transient tool error. `gh` exits 1 with stderr matching
/// "already merged" / "already closed" / "is already in a closed state";
/// the reroll must classify that signature and continue to constraint
/// extraction with `pr_number` cleared.
#[test]
fn test_reroll_close_pr_already_merged_is_tolerated_as_successful_supersede() {
    let mut scm = FakeScm::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = true;
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), "base-sha-merged".into());
    vcs.heads
        .insert("factory/bead-merged-r1".into(), "head-sha-merged".into());
    // Script the bead's resolved repo's PR as already-merged. Exact
    // stderr shape matches `gh pr close --repo owner/repo <n>` for a
    // merged PR.
    scm.pr_already_terminal.insert(
        ("owner/repo".to_string(), 404u64),
        "cannot close: pull request #404 is already merged".to_string(),
    );

    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into()
    ));

    let mut cfg = test_cfg();
    cfg.spec_dir = std::env::temp_dir()
        .join("afd_spec_dir_pr_merged_test")
        .to_string_lossy()
        .to_string();
    let spec_dir = std::path::Path::new(&cfg.spec_dir);
    let _ = std::fs::remove_dir_all(spec_dir);
    std::fs::create_dir_all(spec_dir).unwrap();

    let telemetry_log = std::env::temp_dir().join("afd_reroll_pr_merged_telemetry.jsonl");
    let _ = std::fs::remove_file(&telemetry_log);

    let mut bead = BeadOverlay {
        bead_id: "bead-merged".into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 20,
        spend_usd: 0.5,
        pr_number: Some(404),
        branch: Some("factory/bead-merged-r1".into()),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
        pre_session_head_sha: None,
        park_reason: None,
        target_repo: None,
        attempt_started_at: None,
    };
    store.save(&bead).unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: "skeptic".into(),
        review_text: "Don't print to stdout, log errors.".into(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    match outcome {
        RerollOutcome::Rerolled { new_branch } => {
            assert_eq!(new_branch, "factory/bead-merged-r2");
        }
        other => panic!(
            "expected RerollOutcome::Rerolled (reroll must tolerate PR-already-merged as a \
             supersede, NOT wedge on the close failure); got {:?}",
            other
        ),
    }

    // pr_number must be cleared (the PR is gone — closing it again is
    // moot, the bead has successfully moved on to a new -rN branch).
    let updated = store.load("bead-merged").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::Recovery);
    assert_eq!(updated.attempt, 2);
    assert_eq!(
        updated.pr_number, None,
        "pr_number must be cleared after a tolerant already-merged supersede; \
         the bead should not carry a stale pr_number into Recovery"
    );

    // Telemetry: confirm the reroll emitted the REROLL_PR_ALREADY_MERGED
    // signal so operators can audit which rerolls took the tolerant
    // branch. Without this a regression that silently swallows the close
    // failure (rather than classifying it) would still pass the green
    // path above.
    let telemetry = std::fs::read_to_string(&telemetry_log)
        .expect("reroll must have written telemetry");
    assert!(
        telemetry.contains("REROLL_PR_ALREADY_MERGED"),
        "telemetry must record REROLL_PR_ALREADY_MERGED so operators can audit \
         tolerant supersedes; got: {telemetry}"
    );

    std::fs::remove_dir_all(spec_dir).ok();
    let _ = std::fs::remove_file(&telemetry_log);
}
