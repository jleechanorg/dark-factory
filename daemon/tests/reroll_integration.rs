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
                assert!(matches!(result, Err(DaemonError::SpawnCleanupFailed { .. })));
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
                assert!(matches!(result, Err(DaemonError::SpawnCleanupFailed { .. })));
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
    assert!(result.is_err(), "the fake must simulate process death after spawn");

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

/// Bead jleechan-zeij / issue #322 (r2): adversarial race tests against
/// `reroll::execute`'s FAIL-CLOSED proceed predicate (reroll.rs, "Stop the AO
/// session and evaluate the fail-closed proceed predicate" section), run with
/// REAL wall-clock timing against the actual production `500ms` poll interval
/// and `3s` best-effort ceiling — not an injected/fake clock — so a wrong
/// constant, unit, or off-by-one in that wiring would actually fail these
/// tests.
///
/// The r2 contract: supersede the previous worker (fabricate a fresh branch,
/// close the old PR) ONLY on a positive proceed signal — attach() ->
/// SessionNotFound, session terminal + stable HEAD, or session idle + stable
/// HEAD. On any doubt (a running worker, a moving HEAD, or a failed stop())
/// the engine DEFERS (leaves the bead ATTESTED, preserves session_id, retries
/// next tick) up to a bounded cap, then parks HUMAN_HELD — it NEVER proceeds
/// on doubt and never parks on a single unconfirmed poll. Each test's doc
/// comment states its expected real wall-clock duration.
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
        }
    }

    /// Live worker (r2 core case): the AO session reports `activity=running`
    /// and its branch HEAD is actively moving (a `git push` schedule) for the
    /// entire best-effort window. This is the exact "do not supersede a
    /// worker that is still pushing" hazard the Codex P1 review flagged
    /// against r1's fail-OPEN behavior. The fail-closed predicate must NEVER
    /// confirm here: no fresh branch is fabricated, the old PR is NOT closed,
    /// the outcome is `Deferred`, the bead stays `ATTESTED`, and its
    /// `session_id` is PRESERVED (the worker may still be live). Real
    /// wall-clock duration: ~3s (the ceiling elapses without a confirm).
    #[test]
    fn live_worker_running_moving_head_defers_without_superseding() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let branch = "factory/bead-race-live-r1";
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-live".into());
        vcs.heads.insert(branch.into(), "head-live-0".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_live_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-live", branch);
        // A live worker holds a durable session handle; the defer path must
        // preserve it (clearing is reserved for a confirmed proceed).
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();

        // Worker actively running AND pushing: `session_activity` is Running,
        // and the branch HEAD advances mid-window. Either alone denies the
        // predicate; together they are the unambiguous "still live" signal.
        sessions.set_activity(SessionActivity::Running);
        let t0 = Instant::now();
        vcs.schedule_head_sha(branch, t0, "head-live-0");
        vcs.schedule_head_sha(branch, t0 + Duration::from_millis(800), "head-live-1");
        vcs.schedule_head_sha(branch, t0 + Duration::from_millis(1600), "head-live-2");

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

        let start = Instant::now();
        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Deferred(reason) => {
                assert_eq!(reason, "unconfirmed_live_or_moving_head");
            }
            other => panic!(
                "expected Deferred on a live+pushing worker (must not supersede; issue #322 r2), got {:?}",
                other
            ),
        }
        // Bounded by the ~3s ceiling, never the old 60s park.
        assert!(
            elapsed < Duration::from_secs(15),
            "expected the best-effort wait to give up at the ~3s ceiling, got {elapsed:?}"
        );

        let updated = store.load("bead-race-live").unwrap().unwrap();
        // Left re-eligible for the fast tier next tick, NOT parked, NOT advanced.
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(
            updated.session_id.as_deref(),
            Some("fake-session-1"),
            "session_id must be preserved on defer — clearing is reserved for a confirmed proceed"
        );
        assert_eq!(store.reroll_deferral_count("bead-race-live").unwrap(), 1);

        let vcs_calls = vcs.calls.borrow();
        assert!(
            vcs_calls.iter().all(|c| !c.starts_with("create_branch_at(")),
            "must NOT fabricate a branch while the worker is still live: {vcs_calls:?}"
        );
        let scm_calls = scm.calls.borrow();
        assert!(
            scm_calls.iter().all(|c| !c.starts_with("close_pr(")),
            "must NOT close the old PR while the worker is still live: {scm_calls:?}"
        );

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Terminal settles under the ceiling (predicate (b)): the AO session
    /// becomes terminal at ~t=1.5s with a static HEAD, so the next two polls
    /// read the same value and confirm well inside the ~3s ceiling. Expect a
    /// normal successful re-roll and the durable `session_id` CLEARED (a
    /// confirmed proceed is the only path that clears it). One shared `start`
    /// Instant anchors both `set_terminal_at` and the elapsed assertion so
    /// the "t=1.5s" boundary is precise (CodeRabbit minor). Real wall-clock
    /// duration: ~2s.
    #[test]
    fn terminal_settles_under_ceiling_proceeds() {
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
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();

        // One shared reference instant anchors both the scripted terminal
        // boundary and the elapsed measurement below, so "terminal at t=1.5s"
        // is measured against the same clock the assertion uses.
        let start = Instant::now();
        // Terminal at t=1.5s — under the ~3s ceiling. HEAD SHA is static, so
        // once terminal, two consecutive polls read the same value and confirm.
        sessions.set_terminal_at(start + Duration::from_millis(1500));

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "terminal settles under ceiling".into(),
        };

        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-b-r2");
            }
            other => panic!(
                "expected Rerolled (terminal + stable HEAD is a confirmed proceed), got {:?}",
                other
            ),
        }
        assert!(
            elapsed >= Duration::from_millis(1500) && elapsed < Duration::from_secs(15),
            "expected confirmation shortly after the t=1.5s settle point and well under the \
             ~3s ceiling, got {elapsed:?}"
        );

        let updated = store.load("bead-race-b").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);
        assert_eq!(
            updated.session_id, None,
            "a confirmed proceed must clear the durable session handle"
        );

        std::fs::remove_dir_all(spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Settle-after-ceiling (r2 boundary): the AO session is still `Running`
    /// throughout the ~3s best-effort window and only becomes terminal at
    /// t=10s — well past the ceiling. Because the predicate can only observe
    /// a running worker within the window, it must NOT proceed: it DEFERS
    /// (fail-closed), giving up at the ceiling rather than blocking for the
    /// full 10s settle. One shared `start` Instant anchors both the terminal
    /// boundary and the elapsed assertion. Real wall-clock duration: ~3s.
    #[test]
    fn settles_after_short_ceiling_defers() {
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
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();

        // One shared reference instant for both the scripted terminal boundary
        // and the elapsed measurement.
        let start = Instant::now();
        // Terminal only at t=10s — well past the ~3s ceiling. Until then the
        // derived activity is `Running`, so the predicate never confirms.
        sessions.set_terminal_at(start + Duration::from_secs(10));

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "settles after short ceiling".into(),
        };

        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Deferred(reason) => {
                assert_eq!(reason, "unconfirmed_live_or_moving_head");
            }
            other => panic!(
                "expected Deferred on a session still running at the ceiling, got {:?}",
                other
            ),
        }
        // Gave up at the ~3s ceiling, did NOT block for the t=10s settle.
        assert!(
            elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(9),
            "expected the best-effort wait to give up at the ~3s ceiling, not block on the t=10s \
             settle, got {elapsed:?}"
        );

        let updated = store.load("bead-race-c").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Case D — the mid-push race: the AO process is ALREADY terminal from
    /// t=0 (it exited quickly), but the worker's `git push` for its final
    /// commit is still landing — the branch's HEAD SHA changes partway
    /// through the confirmation window (at ~250ms, between the 1st poll at
    /// ~0ms and the 2nd poll at ~500ms). Expect: the predicate must NOT
    /// declare success off the stale pre-push SHA (req 3 — head_sha is
    /// sampled every poll); it must detect the SHA change as non-stability,
    /// keep polling, and only confirm once the POST-push SHA has itself been
    /// observed unchanged across two consecutive polls. Real wall-clock
    /// duration: ~1-1.5s — the race resolves inside the ~3s ceiling.
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

    /// Case E (the #322 pin) — the AO session is idle+spawning: the worker
    /// finished its task and went back to idle without an explicit kill
    /// (`status=spawning, activity=idle`), so `is_quiescent` returns
    /// `Ok(false)` forever. This is the EXACT live state observed in the
    /// 04:30:43Z U4 telemetry (df-179 / df-180 on the `…-9lvs-r1` / `…-mh9o-r1`
    /// branches). r0 parked HUMAN_HELD on it (defeating unattended
    /// end-to-end); r2 recognizes it as predicate (c): idle activity + a
    /// stable HEAD is a positive, FAST proceed. The elapsed LOWER bound pins
    /// that the two-read stability streak actually runs (it is not an
    /// instant/unchecked confirm), and the upper bound pins that it never
    /// regresses to the old 60s park. Real wall-clock duration: ~0.5-1s.
    #[test]
    fn case_e_idle_spawning_session_proceeds_fast() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-e".into());
        let branch = "factory/bead-race-e-r1";
        vcs.heads.insert(branch.into(), "head-sha-e".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        *llm.response.borrow_mut() = Some(Ok(
            r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into(),
        ));
        let mut cfg = test_cfg();
        cfg.spec_dir = std::env::temp_dir()
            .join("afd_quiescence_case_e_spec")
            .to_string_lossy()
            .to_string();
        let spec_dir = std::path::Path::new(&cfg.spec_dir);
        let _ = std::fs::remove_dir_all(spec_dir);
        std::fs::create_dir_all(spec_dir).unwrap();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_case_e_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-e", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();

        // Live bug signature: AO reports the session idle (alive but not
        // terminal). HEAD is static, so predicate (c) confirms on the second
        // consecutive idle+matching-HEAD poll.
        sessions.set_activity(SessionActivity::Idle);

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "idle+spawning AO session; stable HEAD; reroll must proceed".into(),
        };

        let start = Instant::now();
        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        let elapsed = start.elapsed();

        match outcome {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-e-r2");
            }
            other => panic!(
                "expected Rerolled on an idle+stable-HEAD session (issue #322 predicate c), got {:?}",
                other
            ),
        }
        // Lower bound: the two-read stability streak spans at least one poll
        // interval (~500ms), proving the confirm is not an unchecked instant
        // pass. Upper bound: nowhere near the old 60s park.
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(15),
            "expected an idle+stable-HEAD confirm after ~one poll interval and well under 60s, got {elapsed:?}"
        );

        let updated = store.load("bead-race-e").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);
        assert_eq!(
            updated.session_id, None,
            "a confirmed proceed must clear the durable session handle"
        );

        // A fresh attempt branch was fabricated, NOT the bead parked HUMAN_HELD.
        let vcs_calls = vcs.calls.borrow();
        assert!(
            vcs_calls
                .iter()
                .any(|c| c.starts_with("create_branch_at(factory/bead-race-e-r2")),
            "expected fresh attempt branch to be fabricated on idle+stable-HEAD reroll, got {vcs_calls:?}"
        );

        std::fs::remove_dir_all(spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// SessionNotFound fast path (predicate (a)): the previous worker has
    /// been fully reaped, so `attach` returns `SessionNotFound`. There is
    /// nothing to stop and nothing to wait for — reroll proceeds essentially
    /// instantly with reason `no_live_session`, clears the durable handle,
    /// and never calls `stop`/`is_quiescent`/`session_activity`. Real
    /// wall-clock duration: near-zero.
    #[test]
    fn session_not_found_fast_path_proceeds() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-nf".into());
        let branch = "factory/bead-race-nf-r1";
        vcs.heads.insert(branch.into(), "head-sha-nf".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        *llm.response.borrow_mut() = Some(Ok(
            r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into(),
        ));
        let mut cfg = test_cfg();
        cfg.spec_dir = std::env::temp_dir()
            .join("afd_quiescence_nf_spec")
            .to_string_lossy()
            .to_string();
        let spec_dir = std::path::Path::new(&cfg.spec_dir);
        let _ = std::fs::remove_dir_all(spec_dir);
        std::fs::create_dir_all(spec_dir).unwrap();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_nf_telemetry.jsonl");
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
            review_text: "session already reaped".into(),
        };

        let outcome = reroll::execute(&deps, &mut bead).unwrap();
        match outcome {
            RerollOutcome::Rerolled { new_branch } => {
                assert_eq!(new_branch, "factory/bead-race-nf-r2");
            }
            other => panic!("expected Rerolled on the no-live-session fast path, got {:?}", other),
        }

        let updated = store.load("bead-race-nf").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Recovery);
        assert_eq!(
            updated.session_id, None,
            "the no-live-session fast path clears the durable handle before proceeding"
        );

        let session_calls = sessions.calls.borrow();
        assert!(
            session_calls.iter().all(|c| !c.starts_with("stop(")
                && !c.starts_with("is_quiescent(")
                && !c.starts_with("session_activity(")),
            "SessionNotFound must skip stop/liveness polling entirely, got {session_calls:?}"
        );

        std::fs::remove_dir_all(spec_dir).ok();
        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// Hard (permanent) attach error still parks: a non-`SessionNotFound`,
    /// non-transient `attach` failure (an ambiguous/malformed `ao status`)
    /// means the daemon cannot even identify the session — it fails closed to
    /// HUMAN_HELD rather than deferring or proceeding. Real wall-clock
    /// duration: near-zero.
    #[test]
    fn hard_attach_error_parks_human_held() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-ha".into());
        let branch = "factory/bead-race-ha-r1";
        vcs.heads.insert(branch.into(), "head-sha-ha".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_ha_telemetry.jsonl");
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

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Held(reason) => {
                assert!(
                    reason.contains("failed to attach to session"),
                    "expected an attach-failure Held reason, got: {reason}"
                );
            }
            other => panic!("expected Held on a permanent attach failure, got {:?}", other),
        }

        let updated = store.load("bead-race-ha").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::HumanHeld);
        assert_eq!(
            updated.park_reason.as_deref(),
            Some("reroll_session_attach_failed")
        );
        // A permanent attach error is NOT counted as a deferral.
        assert_eq!(store.reroll_deferral_count("bead-race-ha").unwrap(), 0);

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// A failed `stop()` defers (ordering req 4): the kill did not succeed,
    /// so the worker is (or may be) alive — the predicate must not evaluate
    /// past it. Expect `Deferred`, the bead left ATTESTED, `session_id`
    /// preserved, no branch fabricated, no PR closed. Fails fast (no poll
    /// wait). Real wall-clock duration: near-zero.
    #[test]
    fn stop_failure_defers() {
        let scm = FakeScm::new();
        let sessions = FakeSessions::new();
        let mut vcs = FakeVcs::new();
        vcs.heads.insert("main".into(), "base-sha-sf".into());
        let branch = "factory/bead-race-sf-r1";
        vcs.heads.insert(branch.into(), "head-sha-sf".into());
        let store = FakeStateStore::new();
        let llm = FakeLlm::new();
        let cfg = test_cfg();
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_sf_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-sf", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();

        // attach() returns the default session id "fake-session-1"; scripting
        // stop() to fail for it forces the fail-closed defer.
        sessions.fail_stop_for("fake-session-1");

        let deps = RerollDeps {
            scm: &scm,
            sessions: &sessions,
            vcs: &vcs,
            store: &store,
            llm: &llm,
            cfg: &cfg,
            telemetry_log: &telemetry_log,
            reviewer: "skeptic".into(),
            review_text: "stop failed".into(),
        };

        match reroll::execute(&deps, &mut bead).unwrap() {
            RerollOutcome::Deferred(reason) => assert_eq!(reason, "stop_failed"),
            other => panic!("expected Deferred on a failed stop(), got {:?}", other),
        }

        let updated = store.load("bead-race-sf").unwrap().unwrap();
        assert_eq!(updated.state, OverlayState::Attested);
        assert_eq!(updated.session_id.as_deref(), Some("fake-session-1"));
        assert_eq!(store.reroll_deferral_count("bead-race-sf").unwrap(), 1);

        let vcs_calls = vcs.calls.borrow();
        assert!(
            vcs_calls.iter().all(|c| !c.starts_with("create_branch_at(")),
            "a failed stop() must not fabricate a branch: {vcs_calls:?}"
        );
        let scm_calls = scm.calls.borrow();
        assert!(
            scm_calls.iter().all(|c| !c.starts_with("close_pr(")),
            "a failed stop() must not close the PR: {scm_calls:?}"
        );

        let _ = std::fs::remove_file(&telemetry_log);
    }

    /// The bounded deferral counter escalates to HUMAN_HELD at the cap: a
    /// worker that defers on every tick (here via a persistently-failing
    /// stop(), so each call fails fast) is retried `MAX_REROLL_DEFERRALS`
    /// times, and only the cap-th call parks HUMAN_HELD with the
    /// deferral-cap reason. The first `cap-1` calls all return `Deferred`.
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
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_cap_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-cap", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();
        // Every tick's stop() fails → every reroll defers, driving the counter.
        sessions.fail_stop_for("fake-session-1");

        // MAX_REROLL_DEFERRALS is 5; drive up to and including the cap.
        const CAP: u32 = 5;
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
                other => panic!("tick {tick}: expected Deferred below the cap, got {:?}", other),
            }
            assert_eq!(store.reroll_deferral_count("bead-race-cap").unwrap(), tick);
            assert_eq!(
                store.load("bead-race-cap").unwrap().unwrap().state,
                OverlayState::Attested
            );
        }

        // The cap-th deferral parks HUMAN_HELD.
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

    /// A PERMANENT (non-transient) liveness-probe error PROPAGATES (req 5):
    /// only `is_transient()` errors are swallowed as a defer. A
    /// `DaemonError::Parse` from `session_activity` (a malformed `ao status`)
    /// must surface as `Err`, not be silently absorbed. Real wall-clock
    /// duration: near-zero.
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
        let telemetry_log = std::env::temp_dir().join("afd_quiescence_pe_telemetry.jsonl");
        let _ = std::fs::remove_file(&telemetry_log);

        let mut bead = race_test_bead("bead-race-pe", branch);
        bead.session_id = Some("fake-session-1".into());
        store.save(&bead).unwrap();

        // attach() + stop() succeed; the poll's activity probe hits a
        // permanent parse failure.
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
