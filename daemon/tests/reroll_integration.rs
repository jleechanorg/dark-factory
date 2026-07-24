use std::hash::Hasher;
use std::time::{Duration, Instant};
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeStateStore, FakeTracker, FakeVcs};
use daemon::config::Config;
use daemon::constraints;
use daemon::errors::DaemonError;
use daemon::reroll::{self, RerollDeps, RerollOutcome};
use daemon::state::{BeadOverlay, OverlayState, StateStore};
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
        repos: std::collections::HashMap::new(),
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

    // Verify SCM PR close call
    let scm_calls = scm.calls.borrow();
    assert!(scm_calls.iter().any(|c| c.contains("close_pr(201")));

    // bead jleechan-tfs1 regression guard: a factory-fabricated bead
    // (is_adopted=false) must still use today's create-branch-at path and
    // must NEVER go through the adopted-branch append-only push path.
    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls
            .iter()
            .any(|c| c.contains("create_branch_at(factory/bead-success-r2")),
        "factory-fabricated reroll must fabricate a new branch: {vcs_calls:?}"
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
    };

    // --- Tick 1: Intake -> Route -> Dispatch ---
    run_tick(&deps, 0, 0).unwrap();
    let overlay1 = store.load("fake-bead-1").unwrap().unwrap();
    assert_eq!(overlay1.state, OverlayState::Dispatched);

    // --- Prepare PR opening ---
    let mut overlay = overlay1;
    overlay.pr_number = Some(15);
    store.save(&overlay).unwrap();

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
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "red".to_string(),
            coderabbit_status: "green".to_string(),
            bugbot_status: "green".to_string(),
            ci_pending: false,
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
    vcs.heads.insert("alice/my-cool-feature".into(), "pre-session-sha-abc123".into());
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
        scm_calls.iter().all(|c| !c.starts_with("close_pr(")),
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
    vcs.heads.insert("alice/my-cool-feature".into(), "pre-session-sha-abc123".into());
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
        scm_calls.iter().all(|c| !c.starts_with("close_pr(")),
        "a failed adopted remediation must never close the contributor's PR: {scm_calls:?}"
    );

    let _ = std::fs::remove_file(&telemetry_log);
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
            assert_eq!(new_branch, "alice/my-cool-feature");
        }
        other => panic!("expected RerollOutcome::Rerolled, got {:?}", other),
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
    assert_eq!(updated.state, OverlayState::ReRoll);
    assert_eq!(updated.branch.as_deref(), Some("alice/my-cool-feature"));
    assert_eq!(updated.pr_number, Some(777));

    let _ = std::fs::remove_file(&telemetry_log);
}

/// Stage-2 prerequisite #3 (quiescence timeout validated): four adversarial
/// race-condition tests against `reroll::execute`'s quiescence-confirmation
/// loop (reroll.rs, "Stop AO session and wait for quiescence" section), run
/// with REAL wall-clock timing against the actual production
/// `Duration::from_secs(60)` timeout and `500ms` poll interval hardcoded in
/// `reroll::execute` — not an injected/fake clock — so a wrong constant,
/// unit, or off-by-one in that wiring would actually fail these tests, not
/// just a logic-level unit test with a mocked clock. Each test takes real
/// wall-clock seconds to run by design; see individual doc comments for
/// expected duration.
mod quiescence_timeout_races {
    use super::*;

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
        }
    }

    /// Case A — genuine mid-push race that NEVER settles: the fake AO
    /// session reports "not terminal" for the entire 60s window (models a
    /// worker that is actively pushing / retrying throughout). Expect: the
    /// hard 60s timeout fires, `RerollOutcome::Held("quiescence timeout
    /// exceeded (60s)")`, bead parks HUMAN_HELD, and — critically — the
    /// daemon must NOT proceed to fabricate a fresh branch or close the old
    /// PR on top of an unconfirmed base. Real wall-clock duration: ~60s.
    #[test]
    fn case_a_never_settles_timeout_fires_and_blocks_branch_creation() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-a".into());
        vcs.heads
            .insert("factory/bead-race-a-r1".into(), "head-sha-a".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_case_a_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-a", "factory/bead-race-a-r1");
        store.save(&bead).unwrap();

        // Never terminal within any realistic test window.
        sessions.set_terminal_at(Instant::now() + Duration::from_secs(3600));

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "mid-push race case A".into(),
        };

        let start = Instant::now();
        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Held(reason) => {
                assert!(
                    reason.contains("quiescence timeout exceeded"),
                    "expected timeout Held reason, got: {reason}"
                );
            }
            other => panic!("expected Held(timeout), got {:?}", other),
        }

        // Proves the REAL 60s constant is wired (not e.g. 60ms or 6000ms):
        // an aborted attempt must take roughly 60 real seconds, not near-zero.
        assert!(
            elapsed >= Duration::from_secs(58) && elapsed <= Duration::from_secs(65),
            "expected ~60s real elapsed time for the hard timeout, got {elapsed:?}"
        );

        let updated = store.load("bead-race-a").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::HumanHeld);
        // Never proceeded past quiescence: no fresh branch, old PR untouched.
        let vcs_calls = vcs.calls.borrow();
        assert!(
            vcs_calls
                .iter()
                .all(|c| !c.starts_with("create_branch_at(")),
            "must not fabricate a branch when quiescence never confirmed: {vcs_calls:?}"
        );
        let scm_calls = scm.calls.borrow();
        assert!(
            scm_calls.iter().all(|c| !c.starts_with("close_pr(")),
            "must not close the old PR when quiescence never confirmed: {scm_calls:?}"
        );

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Case B — session settles well under the timeout (~50s in). Expect a
    /// normal successful re-roll, not a false-positive abort. Real
    /// wall-clock duration: ~50-51s.
    #[test]
    fn case_b_settles_under_timeout_succeeds() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-b".into());
        vcs.heads
            .insert("factory/bead-race-b-r1".into(), "head-sha-b".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        *llm.response.borrow_mut() = Some(Ok(
            r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into(),
        ));
        let mut cfg = test_cfg();
        cfg.spec_dir = std::env::temp_dir()
            .join("afd_quiescence_case_b_spec")
            .to_string_lossy()
            .to_string();
        let spec_dir = std::path::Path::new(&cfg.spec_dir);
        let _ = std::fs::remove_dir_all(spec_dir);
        std::fs::create_dir_all(spec_dir).unwrap();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_case_b_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-b", "factory/bead-race-b-r1");
        store.save(&bead).unwrap();

        // Terminal at t=50s — squarely in the "45-55s" boundary band, still
        // well inside the 60s window. HEAD SHA is static (no schedule), so
        // once terminal, the very next two polls read the same value and
        // confirm.
        sessions.set_terminal_at(Instant::now() + Duration::from_secs(50));

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "settles under timeout case B".into(),
        };

        let start = Instant::now();
        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-b-r2");
            }
            other => panic!(
                "expected Rerolled (not a false-positive abort), got {:?}",
                other
            ),
        }
        assert!(
            elapsed >= Duration::from_secs(50) && elapsed < Duration::from_secs(58),
            "expected confirmation shortly after the t=50s settle point and well under the \
             60s timeout, got {elapsed:?}"
        );

        let updated = store.load("bead-race-b").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);

        std::fs::remove_dir_all(spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Case C — session settles just OVER the timeout (~61s in, i.e. AFTER
    /// the 60s deadline has already elapsed). Expect the abort — boundary
    /// correctness, not just "eventually true". Real wall-clock duration:
    /// ~60s (the loop exits at the 60s deadline, never observing the
    /// terminal state that only arrives at 61s).
    #[test]
    fn case_c_settles_just_over_timeout_aborts() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-c".into());
        vcs.heads
            .insert("factory/bead-race-c-r1".into(), "head-sha-c".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_case_c_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-c", "factory/bead-race-c-r1");
        store.save(&bead).unwrap();

        // Terminal only at t=61s — one second past the hard deadline.
        sessions.set_terminal_at(Instant::now() + Duration::from_secs(61));

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "settles just over timeout case C".into(),
        };

        let start = Instant::now();
        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Held(reason) => {
                assert!(
                    reason.contains("quiescence timeout exceeded"),
                    "expected timeout Held reason, got: {reason}"
                );
            }
            other => panic!(
                "expected Held(timeout) for a settle-just-past-deadline session, got {:?}",
                other
            ),
        }
        assert!(
            elapsed >= Duration::from_secs(58) && elapsed < Duration::from_secs(61),
            "the loop must exit at the ~60s deadline itself, before ever observing the t=61s \
             terminal state, got {elapsed:?}"
        );

        let updated = store.load("bead-race-c").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::HumanHeld);

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Case D — the actual race from the spec's own rationale: the AO
    /// process is ALREADY terminal from t=0 (it exited quickly), but the
    /// worker's `git push` for its final commit is still landing — the
    /// branch's HEAD SHA changes partway through the confirmation window
    /// (at ~250ms, between the 1st poll at ~0ms and the 2nd poll at
    /// ~500ms). Expect: the daemon must NOT declare success off the stale
    /// pre-push SHA; it must detect the SHA change as non-stability, keep
    /// polling, and only confirm once the POST-push SHA has itself been
    /// observed unchanged across two consecutive polls. This is exactly the
    /// class of bug the missing HEAD-SHA check (fixed alongside these
    /// tests) would produce a false positive on. Real wall-clock duration:
    /// ~1-1.5s — the race resolves quickly; only the boundary cases A-C
    /// need the full 60s window.
    #[test]
    fn case_d_push_lands_mid_poll_is_detected_not_falsely_confirmed() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-d".into());
        let branch = "factory/bead-race-d-r1";
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        *llm.response.borrow_mut() = Some(Ok(
            r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into(),
        ));
        let mut cfg = test_cfg();
        cfg.spec_dir = std::env::temp_dir()
            .join("afd_quiescence_case_d_spec")
            .to_string_lossy()
            .to_string();
        let spec_dir = std::path::Path::new(&cfg.spec_dir);
        let _ = std::fs::remove_dir_all(spec_dir);
        std::fs::create_dir_all(spec_dir).unwrap();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_case_d_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-d", branch);
        store.save(&bead).unwrap();

        // AO process already exited — terminal immediately.
        sessions.set_terminal_at(Instant::now() - Duration::from_millis(1));

        // But the branch HEAD SHA is still "sha-mid-push" until t=250ms
        // (between the 1st poll at ~0ms and the 2nd poll at ~500ms), then
        // becomes the final "sha-final" for good — models the worker's push
        // landing exactly inside the confirmation window.
        let t0 = Instant::now();
        vcs.schedule_head_sha(branch, t0, "sha-mid-push");
        vcs.schedule_head_sha(branch, t0 + Duration::from_millis(250), "sha-final");

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "push lands mid-poll case D".into(),
        };

        let outcome = reroll::execute(&deps, &mut bead).unwrap();

        match outcome {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-d-r2");
            }
            other => panic!(
                "expected eventual Rerolled success once the post-push SHA settled, got {:?}",
                other
            ),
        }

        // The crux of the proof: the quiescence loop must have actually
        // polled `head_sha(branch)` more than once (>= 3: t=0 reads
        // "sha-mid-push", t=0.5s reads "sha-final" — a MISMATCH resetting
        // the streak, t=1.0s reads "sha-final" again — confirms). A
        // process-only check (the pre-fix behavior) would never call
        // `head_sha` in this loop at all, and would have confirmed instantly
        // at t=0 on the stale pre-push value.
        let head_sha_polls = vcs
            .calls
            .borrow()
            .iter()
            .filter(|c| c.as_str() == format!("head_sha({branch})"))
            .count();
        assert!(
            head_sha_polls >= 3,
            "expected the mismatch at t=0.5s to force at least a 3rd poll before confirming \
             (saw a stale value, a changed value, then a re-confirmed value), got \
             {head_sha_polls} head_sha({branch}) calls: {:?}",
            vcs.calls.borrow()
        );

        let updated = store.load("bead-race-d").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);

        std::fs::remove_dir_all(spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }
}

/// jleechan-cq8r: a malformed/unparseable reply from the circuit-breaker's
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
    };
    store.save(&bead).unwrap();
    store
        .save_rejection("cq8r-bead", 1, "verifier", "deadbeefdeadbeef", "prior rejection text")
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
