use std::hash::Hasher;
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeStateStore, FakeTracker, FakeVcs};
use daemon::config::Config;
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::reroll::{self, RerollDeps, RerollOutcome};
use daemon::constraints;
use daemon::tick::{run_tick, TickDeps};
use daemon::errors::DaemonError;
use daemon::tools::{Issue, Permission, PrSnapshot, Llm};

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
        spec_dir: std::env::temp_dir().join("afd_spec_dir_test").to_string_lossy().to_string(),
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
    assert_eq!(std::fs::read_to_string(&spec_file).unwrap(), "initial = 1\n");

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
    };
    store.save(&bead).unwrap();

    // 1. Script a previous rejection for attempt 1 citing "coderabbit" and "fail reason" hash
    let feedback = "fail reason";
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::Hash;
    feedback.hash(&mut hasher);
    let feedback_hash = format!("{:016x}", hasher.finish());

    store.save_rejection("bead-breaker", 1, "coderabbit", &feedback_hash, feedback).unwrap();

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
    vcs.heads.insert("factory/bead-success-r1".into(), "head-sha-123".into());
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    // mock LLM reply for constraint extraction
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"inhibitionSpecs":["no print"],"positiveAssertions":["log errors"],"securityRedactionEncountered":false}"#.into()
    ));

    let mut cfg = test_cfg();
    cfg.spec_dir = std::env::temp_dir().join("afd_spec_dir_success_test").to_string_lossy().to_string();
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
    assert_eq!(store.branches.borrow().as_slice(), &["factory/bead-success-r2"]);

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
    vcs.heads.insert("factory/fake-bead-1-r1".into(), "head-sha-abc".into());
    
    let llm = FakeLlm::new();
    // Mock router response
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"routingVerdict":"SMALL_PATH","justification":"x"}"#.into()
    ));

    let store = FakeStateStore::new();
    let mut cfg = test_cfg();
    cfg.spec_dir = std::env::temp_dir().join("afd_spec_dir_tick_test").to_string_lossy().to_string();
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
            unresolved_thread_count: 0,
            head_sha: "head-sha-abc".into(),
            body: "".into(),
            comments: vec![],
            files: vec![],
            updated_at_epoch: 0,
            ci_status: "red".to_string(),
            coderabbit_status: "green".to_string(),
            ci_pending: false,
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
    let smart_llm = SmartLlm { state: std::cell::RefCell::new(0) };

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
/// ADOPTED bead must push an append-only fix commit to the EXISTING
/// contributor branch, must leave the original PR OPEN (no `close_pr`
/// call), must never fabricate a replacement branch (no `create_branch_at`
/// call, branch registry untouched), and must never force-push/rebase
/// (asserted directly against the `FakeVcs` call log — the only method that
/// can mutate the remote branch in this path is `push_fix_commit`, whose
/// call arguments carry no `--force`/rebase semantics; `create_branch_at`
/// and `close_pr` are asserted absent entirely).
#[test]
fn test_reroll_adopted_success_pushes_fix_commit_leaves_pr_open() {
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    let vcs = FakeVcs::new();
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
    };
    store.save(&bead).unwrap();
    store.register_branch("bead-adopted", "alice/my-cool-feature").unwrap();

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

    // Overlay: attempt bumped, branch/pr_number UNCHANGED, back to ATTESTED
    // (no factory session exists to redispatch to).
    let updated = store.load("bead-adopted").unwrap().unwrap();
    assert_eq!(updated.state, OverlayState::Attested);
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

    // (c) Never force-pushes/rewrites history, never fabricates a branch:
    let vcs_calls = vcs.calls.borrow();
    assert!(
        vcs_calls
            .iter()
            .any(|c| c.starts_with("push_fix_commit(alice/my-cool-feature,")),
        "adopted remediation must call push_fix_commit on the existing branch: {vcs_calls:?}"
    );
    assert!(
        vcs_calls.iter().all(|c| !c.starts_with("create_branch_at(")),
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

/// bead jleechan-tfs1, requirement (d): when the append-only push genuinely
/// can't land (e.g. the remote diverged, or a real conflict with base needs
/// a rebase) `reroll::execute` must park the bead `HUMAN_HELD` rather than
/// silently failing or falling back to a force-push. This is the direct
/// `reroll::execute`-level proof; `tick_integration.rs` carries the
/// full-pipeline proof that the escalation comment is actually posted on
/// the PR (posting happens one layer up, in `tick::run_fast_tier`, which is
/// the only layer with access to the `Tracker`/comment-posting path).
#[test]
fn test_reroll_adopted_push_failure_parks_human_held() {
    let scm = FakeScm::new();
    let sessions = FakeSessions::new();
    let vcs = FakeVcs::new();
    vcs.fail_push_fix_commit_for("alice/my-cool-feature");
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
    };
    store.save(&bead).unwrap();
    store.register_branch("bead-adopted-conflict", "alice/my-cool-feature").unwrap();

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
                reason.contains("append-only") || reason.contains("human"),
                "Held reason should explain the append-only/needs-human situation: {reason}"
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
        vcs_calls.iter().all(|c| !c.starts_with("create_branch_at(")),
        "a failed adopted remediation must never fall back to fabricating a branch: {vcs_calls:?}"
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
