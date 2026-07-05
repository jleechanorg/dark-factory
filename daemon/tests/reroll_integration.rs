use std::hash::Hasher;
mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeStateStore, FakeTracker, FakeVcs};
use daemon::config::Config;
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::reroll::{self, RerollDeps, RerollOutcome};
use daemon::constraints;
use daemon::tick::{run_tick, TickDeps};
use daemon::errors::DaemonError;
use daemon::tools::{Issue, Permission, PrSnapshot, Bead, Llm};

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
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
    run_tick(&deps, 0).unwrap();
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
    let summary = run_tick(&deps_smart, 1).unwrap();
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
