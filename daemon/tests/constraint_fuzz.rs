// Stage-2 prerequisite #2 evidence harness (docs/auto-factory-daemon-spec.md
// §4.2.9): "Constraint-extraction fuzz run: >= 50 synthetic rejection
// comments processed without circuit-breaker false positives or holdout-leak
// false negatives."
//
// Two independent corpora, run through the REAL production code paths:
//
//   1. `circuit_breaker_fuzz_corpus` — every case calls the real
//      `reroll::execute()` (src/reroll.rs) with scripted-but-real
//      `StateStore`/`Sessions`/`Vcs`/`Scm` fakes from `tests/common`, so the
//      circuit-breaker hash comparison at reroll.rs:101-140 runs unmodified.
//      No LLM call is needed for this half — the breaker hashes raw
//      `review_text` *before* `constraints::extract` is ever invoked.
//
//   2. `holdout_leak_fuzz_real_llm` — calls the real `constraints::extract`
//      (src/constraints.rs:74) with the real `ChainLlm` adapter (subprocess
//      `codex exec --yolo`), because the interesting failure surface — a
//      leak that doesn't contain the literal substring "holdout" — is
//      entirely dependent on the LLM's judgment; the programmatic
//      `redact_holdouts` keyword screen only catches leaks that literally
//      say "holdout". This half is `#[ignore]`d by default (network + slow +
//      token cost) — run explicitly:
//        cargo test --test constraint_fuzz -- --ignored --nocapture
//
// This file is intentionally NOT a pass/fail CI gate (no panicking asserts
// on the interesting axis) — it's an evidence-generation script. Verdict
// interpretation happens in the accompanying session report, per the
// "gate self-certification anti-pattern" guardrail (a check whose expected
// value comes from its own template can't fail).

mod common;

use common::{FakeLlm, FakeScm, FakeSessions, FakeStateStore, FakeVcs};
use daemon::adapters::ChainLlm;
use daemon::config::Config;
use daemon::constraints;
use daemon::reroll::{self, RerollDeps, RerollOutcome};
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::tools::Llm;
use std::hash::{Hash, Hasher};

fn test_cfg(spec_dir: &std::path::Path) -> Config {
    Config {
        target_repo: "owner/repo".into(),
        ao_project: None,
        base_branch: "main".into(),
        stage: 2,
        max_workers: 30,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 60,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: spec_dir.to_string_lossy().to_string(),
    }
}

fn feedback_hash(text: &str) -> String {
    // Identical algorithm to reroll.rs:102-106 — replicated here ONLY to
    // seed the "prior attempt" rejection record via StateStore::save_rejection
    // directly (skipping a full prior `execute()` call for speed). The
    // circuit-breaker COMPARISON itself is never reimplemented — that part
    // always runs through the real `reroll::execute`.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// One circuit-breaker corpus case: attempt N-1 was rejected by
/// `reviewer_a` citing `text_a`; attempt N is rejected by `reviewer_b`
/// citing `text_b`. `expect_impl_fire` is what the CURRENT byte-exact-hash
/// implementation will do; `expect_spec_fire` is what spec §4.2.6's
/// "same semantic rejection reason" language implies SHOULD happen. When
/// they diverge, that divergence IS the finding.
struct CbCase {
    id: &'static str,
    group: &'static str,
    reviewer_a: &'static str,
    text_a: &'static str,
    reviewer_b: &'static str,
    text_b: &'static str,
    expect_impl_fire: bool,
    expect_spec_fire: bool,
}

fn cb_corpus() -> Vec<CbCase> {
    vec![
        // ---- Group A: cross-category, same reviewer, genuinely different
        // reason. Breaker must NOT fire. (10 pairs / 20 comments)
        CbCase { id: "A1", group: "cross-category", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "Please run rustfmt on this file — the indentation in the match arms is inconsistent and trailing whitespace was left on lines 42 and 58.",
            reviewer_b: "coderabbit",
            text_b: "No test exercises the timeout branch — please add a case that scripts is_quiescent to stay false the whole 60s window and confirms the HUMAN_HELD transition." },
        CbCase { id: "A2", group: "cross-category", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "This loop terminates one iteration early — `i < len - 1` should be `i <= len - 1` given the off-by-one in how `len` is computed above, so the last element in the batch is silently dropped.",
            reviewer_b: "coderabbit",
            text_b: "This shells out with the raw PR title interpolated into the command string — a title containing `; rm -rf` would execute arbitrary shell. Use argv-array invocation, not string interpolation." },
        CbCase { id: "A3", group: "cross-category", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "This re-implements what BeadOverlay::as_str() already does — please call the existing method instead of duplicating the match arms here.",
            reviewer_b: "skeptic",
            text_b: "Between the `is_quiescent` check and `stop()`, another tick could dispatch a second session for the same bead — there's no lock around the read-then-act sequence here." },
        CbCase { id: "A4", group: "cross-category", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "This clones the entire `overlays` HashMap on every tick just to compute a count; a `.len()` on a reference would avoid the O(n) copy every 60 seconds.",
            reviewer_b: "skeptic",
            text_b: "This `.ok()`s the result of `comment_external` and moves on — if posting the escalation comment fails, the operator never finds out and the bead is stuck silently. Propagate or at least log the error." },
        CbCase { id: "A5", group: "cross-category", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "No test exercises the timeout branch — please add a case that scripts is_quiescent to stay false the whole 60s window and confirms the HUMAN_HELD transition.",
            reviewer_b: "verifier",
            text_b: "Minor nit: this uses snake_case for one field and camelCase for the adjacent one; pick one convention within the struct." },
        CbCase { id: "A6", group: "cross-category", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "The retry counter is incremented before the operation is attempted, not after, so a single real failure is being counted as two toward the max-retries budget.",
            reviewer_b: "verifier",
            text_b: "This new error variant has zero test coverage; at minimum add a unit test asserting the Display impl matches the expected message format." },
        CbCase { id: "A7", group: "cross-category", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "Polling the file system every 500ms for quiescence is going to thrash under load; consider a listener/callback from the Sessions trait instead of a busy-wait loop.",
            reviewer_b: "coderabbit",
            text_b: "The webhook secret comparison uses `==` on a `String`, which isn't constant-time and is vulnerable to a timing side-channel; switch to a constant-time compare." },
        CbCase { id: "A8", group: "cross-category", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "The circuit-breaker hash recomputation happens on every single reroll call even when bead.attempt <= 1, which is unreachable but still allocates a hasher — trivial, but move it inside the if guard.",
            reviewer_b: "skeptic",
            text_b: "The `unwrap_or_default()` here masks a genuine parse failure as an empty vec; a malformed constraint block would silently produce zero specs instead of surfacing the error." },
        CbCase { id: "A9", group: "cross-category", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "Two ticks racing on save_rejection for the same (bead_id, attempt) key could interleave; this needs a transaction or a unique constraint, not just a plain INSERT.",
            reviewer_b: "verifier",
            text_b: "Please run rustfmt on this file — the indentation in the match arms is inconsistent and trailing whitespace was left on lines 42 and 58." },
        CbCase { id: "A10", group: "cross-category", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "This shells out with the raw PR title interpolated into the command string — a title containing `; rm -rf` would execute arbitrary shell. Use argv-array invocation, not string interpolation.",
            reviewer_b: "coderabbit",
            text_b: "This clones the entire `overlays` HashMap on every tick just to compute a count; a `.len()` on a reference would avoid the O(n) copy every 60 seconds." },

        // ---- Group B: exact-duplicate text, same reviewer. Breaker MUST
        // fire (sanity: the flip-side named case from the task brief — "a
        // genuinely-repeating case DOES trigger"). (6 pairs / 12 comments)
        CbCase { id: "B1", group: "exact-duplicate", reviewer_a: "coderabbit", expect_impl_fire: true, expect_spec_fire: true,
            text_a: "CI is red: `cargo test --test reroll_integration` fails on `test_circuit_breaker` — the HUMAN_HELD transition never happens.",
            reviewer_b: "coderabbit",
            text_b: "CI is red: `cargo test --test reroll_integration` fails on `test_circuit_breaker` — the HUMAN_HELD transition never happens." },
        CbCase { id: "B2", group: "exact-duplicate", reviewer_a: "skeptic", expect_impl_fire: true, expect_spec_fire: true,
            text_a: "The quiescence wait loop busy-polls at 500ms with no upper bound on CPU wakeups; please back off exponentially.",
            reviewer_b: "skeptic",
            text_b: "The quiescence wait loop busy-polls at 500ms with no upper bound on CPU wakeups; please back off exponentially." },
        CbCase { id: "B3", group: "exact-duplicate", reviewer_a: "verifier", expect_impl_fire: true, expect_spec_fire: true,
            text_a: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review.",
            reviewer_b: "verifier",
            text_b: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review." },
        CbCase { id: "B4", group: "exact-duplicate", reviewer_a: "coderabbit", expect_impl_fire: true, expect_spec_fire: true,
            text_a: "The new `spawn_failure_count` field is never reset on a successful dispatch, so one transient blip permanently inflates it across the bead's whole lifetime.",
            reviewer_b: "coderabbit",
            text_b: "The new `spawn_failure_count` field is never reset on a successful dispatch, so one transient blip permanently inflates it across the bead's whole lifetime." },
        CbCase { id: "B5", group: "exact-duplicate", reviewer_a: "skeptic", expect_impl_fire: true, expect_spec_fire: true,
            text_a: "Test `test_reroll_success` doesn't assert on the branch registry call log — a regression here would go undetected.",
            reviewer_b: "skeptic",
            text_b: "Test `test_reroll_success` doesn't assert on the branch registry call log — a regression here would go undetected." },
        CbCase { id: "B6", group: "exact-duplicate", reviewer_a: "verifier", expect_impl_fire: true, expect_spec_fire: true,
            text_a: "bugbot: this PR still leaves `Cargo.lock` out of sync with `Cargo.toml` — commit the regenerated lockfile.",
            reviewer_b: "verifier",
            text_b: "bugbot: this PR still leaves `Cargo.lock` out of sync with `Cargo.toml` — commit the regenerated lockfile." },

        // ---- Group C: near-miss — same reviewer, SAME underlying reason,
        // materially different wording (the harder true-positive case per
        // the task brief). Spec's "semantic rejection reason" language
        // implies these SHOULD fire; the byte-exact hash implementation
        // will NOT. (6 pairs / 12 comments)
        CbCase { id: "C1", group: "near-miss-paraphrase", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "CI is red: `cargo test --test reroll_integration` fails on `test_circuit_breaker` — the HUMAN_HELD transition never happens.",
            reviewer_b: "coderabbit",
            text_b: "The reroll_integration suite is still failing on the circuit-breaker test — the bead isn't reaching HUMAN_HELD like it should. Please re-check this before the next review pass." },
        CbCase { id: "C2", group: "near-miss-paraphrase", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "The quiescence wait loop busy-polls at 500ms with no upper bound on CPU wakeups; please back off exponentially.",
            reviewer_b: "skeptic",
            text_b: "Same concern as before — the polling interval in the quiescence wait is fixed at 500ms and never backs off, so it'll burn CPU under sustained load. Needs exponential backoff." },
        CbCase { id: "C3", group: "near-miss-paraphrase", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review.",
            reviewer_b: "verifier",
            text_b: "This branch still hasn't been rebased onto main; state.rs conflicts. Can't proceed with review until that's resolved." },
        CbCase { id: "C4", group: "near-miss-paraphrase", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "The new `spawn_failure_count` field is never reset on a successful dispatch, so one transient blip permanently inflates it across the bead's whole lifetime.",
            reviewer_b: "coderabbit",
            text_b: "spawn_failure_count still isn't cleared when a dispatch eventually succeeds — a single flaky spawn keeps counting against the bead forever. This is the same gap flagged last round." },
        CbCase { id: "C5", group: "near-miss-paraphrase", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "Test `test_reroll_success` doesn't assert on the branch registry call log — a regression here would go undetected.",
            reviewer_b: "skeptic",
            text_b: "test_reroll_success is missing an assertion on the branch registry calls; without it a future regression in register_branch slips through silently." },
        CbCase { id: "C6", group: "near-miss-paraphrase", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "bugbot: this PR still leaves `Cargo.lock` out of sync with `Cargo.toml` — commit the regenerated lockfile.",
            reviewer_b: "verifier",
            text_b: "Cargo.lock is out of date relative to Cargo.toml on this branch again — please regenerate and commit it." },

        // ---- Group D: byte-identical text, DIFFERENT reviewer. Breaker
        // must NOT fire (reviewer scoping). (4 pairs / 8 comments)
        CbCase { id: "D1", group: "different-reviewer-same-text", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "CI is red: `cargo test --test reroll_integration` fails on `test_circuit_breaker` — the HUMAN_HELD transition never happens.",
            reviewer_b: "skeptic",
            text_b: "CI is red: `cargo test --test reroll_integration` fails on `test_circuit_breaker` — the HUMAN_HELD transition never happens." },
        CbCase { id: "D2", group: "different-reviewer-same-text", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review.",
            reviewer_b: "verifier",
            text_b: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review." },
        CbCase { id: "D3", group: "different-reviewer-same-text", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "bugbot: this PR still leaves `Cargo.lock` out of sync with `Cargo.toml` — commit the regenerated lockfile.",
            reviewer_b: "coderabbit",
            text_b: "bugbot: this PR still leaves `Cargo.lock` out of sync with `Cargo.toml` — commit the regenerated lockfile." },
        CbCase { id: "D4", group: "different-reviewer-same-text", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: false,
            text_a: "The quiescence wait loop busy-polls at 500ms with no upper bound on CPU wakeups; please back off exponentially.",
            reviewer_b: "verifier",
            text_b: "The quiescence wait loop busy-polls at 500ms with no upper bound on CPU wakeups; please back off exponentially." },

        // ---- Group E: whitespace-only diff, same reviewer/reason. Second
        // adversarial pass — trying to break the corpus with the smallest
        // possible textual delta that's still "different bytes". (2 pairs)
        CbCase { id: "E1", group: "whitespace-only-diff", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "CI is red: `cargo test --test reroll_integration` fails on `test_circuit_breaker` — the HUMAN_HELD transition never happens.",
            reviewer_b: "coderabbit",
            text_b: "CI is red: `cargo test --test reroll_integration` fails on `test_circuit_breaker` —  the HUMAN_HELD transition never happens." },
        CbCase { id: "E2", group: "whitespace-only-diff", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review.",
            reviewer_b: "skeptic",
            text_b: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review. " },

        // ---- Group F: case-only diff, same reviewer/reason. (2 pairs)
        CbCase { id: "F1", group: "case-only-diff", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "bugbot: this PR still leaves `Cargo.lock` out of sync with `Cargo.toml` — commit the regenerated lockfile.",
            reviewer_b: "verifier",
            text_b: "Bugbot: This PR still leaves `Cargo.lock` out of sync with `Cargo.toml` — commit the regenerated lockfile." },
        CbCase { id: "F2", group: "case-only-diff", reviewer_a: "coderabbit", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "The new `spawn_failure_count` field is never reset on a successful dispatch, so one transient blip permanently inflates it across the bead's whole lifetime.",
            reviewer_b: "coderabbit",
            text_b: "The new `Spawn_Failure_Count` field is never reset on a successful dispatch, so one transient blip permanently inflates it across the bead's whole lifetime." },

        // ---- Group G: trailing "please fix" / punctuation-only append,
        // same reviewer/reason. (2 pairs)
        CbCase { id: "G1", group: "trailing-append-diff", reviewer_a: "skeptic", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "Test `test_reroll_success` doesn't assert on the branch registry call log — a regression here would go undetected.",
            reviewer_b: "skeptic",
            text_b: "Test `test_reroll_success` doesn't assert on the branch registry call log — a regression here would go undetected. Please fix before next review." },
        CbCase { id: "G2", group: "trailing-append-diff", reviewer_a: "verifier", expect_impl_fire: false, expect_spec_fire: true,
            text_a: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review.",
            reviewer_b: "verifier",
            text_b: "Merge conflict against main in daemon/src/state.rs — please rebase before requesting another review!" },
    ]
}

/// Runs one circuit-breaker corpus case through the REAL `reroll::execute`.
/// Returns (actually_fired, extraction_ok).
fn run_cb_case(case: &CbCase, spec_dir: &std::path::Path) -> bool {
    let scm = FakeScm::new();
    let mut sessions = FakeSessions::new();
    sessions.quiescent = true;
    let mut vcs = FakeVcs::new();
    vcs.heads.insert("main".into(), format!("base-sha-{}", case.id));
    let store = FakeStateStore::new();
    let llm = FakeLlm::new();
    *llm.response.borrow_mut() = Some(Ok(
        r#"{"inhibitionSpecs":["placeholder"],"positiveAssertions":["placeholder"],"securityRedactionEncountered":false}"#.into(),
    ));
    let cfg = test_cfg(spec_dir);
    let telemetry_log = std::env::temp_dir().join(format!("afd_cbfuzz_{}.jsonl", case.id));
    let _ = std::fs::remove_file(&telemetry_log);

    let bead_id = format!("bead-cb-{}", case.id);
    let branch = format!("factory/{}-r2", bead_id);

    let mut bead = BeadOverlay {
        bead_id: bead_id.clone(),
        state: OverlayState::Attested,
        attempt: 2,
        reroll_count: 1,
        autonomy_secs: 10,
        spend_usd: 1.0,
        pr_number: Some(900),
        branch: Some(branch),
        session_id: None,
        is_adopted: false,
        spawn_failure_count: 0,
            pre_session_head_sha: None,
    };
    store.save(&bead).unwrap();

    // Seed the "prior attempt" (attempt N-1 = 1) rejection exactly as a real
    // attempt-1 `reroll::execute` call would have via `save_rejection`.
    store
        .save_rejection(&bead_id, 1, case.reviewer_a, &feedback_hash(case.text_a), case.text_a)
        .unwrap();

    let deps = RerollDeps {
        scm: &scm,
        sessions: &sessions,
        vcs: &vcs,
        store: &store,
        llm: &llm,
        cfg: &cfg,
        telemetry_log: &telemetry_log,
        reviewer: case.reviewer_b.to_string(),
        review_text: case.text_b.to_string(),
    };

    let outcome = reroll::execute(&deps, &mut bead).unwrap();
    let _ = std::fs::remove_file(&telemetry_log);

    matches!(outcome, RerollOutcome::Held(ref r) if r.contains("circuit-breaker"))
}

#[test]
fn circuit_breaker_fuzz_corpus() {
    let spec_dir = std::env::temp_dir().join("afd_cbfuzz_spec_dir");
    let _ = std::fs::remove_dir_all(&spec_dir);
    std::fs::create_dir_all(&spec_dir).unwrap();

    let corpus = cb_corpus();
    let mut false_positives = Vec::new(); // impl fired but expect_spec_fire says it shouldn't have (different reason)
    let mut impl_false_negatives_vs_spec = Vec::new(); // impl didn't fire but spec-intent says it should have
    let mut sanity_failures = Vec::new(); // impl disagreed with the deterministic exact-hash prediction itself

    println!("\n=== Circuit-breaker fuzz corpus: {} cases ===", corpus.len());
    println!("{:<5} {:<28} {:<6} {:<6} {:<6}", "id", "group", "impl", "exp_i", "exp_s");

    for case in &corpus {
        let fired = run_cb_case(case, &spec_dir);
        println!(
            "{:<5} {:<28} {:<6} {:<6} {:<6}",
            case.id, case.group, fired, case.expect_impl_fire, case.expect_spec_fire
        );

        if fired != case.expect_impl_fire {
            sanity_failures.push(case.id);
        }
        // "Circuit-breaker false positive" per the task brief: breaker fired
        // for a pair that is a GENUINELY different reason (expect_spec_fire
        // == false) — this is the zero-tolerance axis the spec names
        // explicitly.
        if fired && !case.expect_spec_fire {
            false_positives.push(case.id);
        }
        // Flip side named in the task brief: a genuinely-same-reason pair
        // should trigger. Track where the CURRENT implementation doesn't,
        // even though spec-intent says it should (near-miss/whitespace/case
        // groups) — this is the headline finding, not a literal spec
        // violation of the explicitly-named "false positive" axis.
        if !fired && case.expect_spec_fire {
            impl_false_negatives_vs_spec.push(case.id);
        }
    }

    std::fs::remove_dir_all(&spec_dir).ok();

    println!("\n--- Circuit-breaker corpus summary ---");
    println!("total cases: {}", corpus.len());
    println!(
        "sanity mismatches (impl behavior != predicted exact-hash behavior): {:?}",
        sanity_failures
    );
    println!(
        "circuit-breaker FALSE POSITIVES (fired on a genuinely different reason): {:?} (count={})",
        false_positives,
        false_positives.len()
    );
    println!(
        "circuit-breaker false negatives vs. spec's 'semantic reason' intent (same reason, different wording, did NOT fire): {:?} (count={})",
        impl_false_negatives_vs_spec,
        impl_false_negatives_vs_spec.len()
    );

    // Not a panicking assert by design (see file header) — this is an
    // evidence-generation script, not a self-certifying gate. The numbers
    // above are the deliverable.
}

/// One holdout-leak corpus case: a single rejection review text, run
/// through the real `constraints::extract`. `expect_leak` is the ground
/// truth this fuzz corpus author independently assigned BEFORE seeing any
/// extractor output (adversarial verification requires this ordering).
struct HoldoutCase {
    id: &'static str,
    group: &'static str,
    text: &'static str,
    expect_leak: bool,
}

fn holdout_corpus() -> Vec<HoldoutCase> {
    vec![
        // ---- HL1: obvious leaks containing the literal word "holdout" —
        // caught deterministically by `redact_holdouts` regardless of LLM
        // judgment (already covered by src/constraints.rs unit tests, kept
        // here for corpus completeness / count). (4)
        HoldoutCase { id: "HL1-1", group: "literal-holdout-word", expect_leak: true,
            text: "This breaks the holdout test scenario_concurrent_writes_v3 — I can see the assertion in the holdout evaluator expects lock_acquired_ms < 50." },
        HoldoutCase { id: "HL1-2", group: "literal-holdout-word", expect_leak: true,
            text: "Ran this against $DARK_FACTORY_HOLDOUTS/evaluator/run.py locally and it fails the role-attack case." },
        HoldoutCase { id: "HL1-3", group: "literal-holdout-word", expect_leak: true,
            text: "Check holdouts/scenario_role_attack_v2.py — your fix doesn't handle the payload it scripts." },
        HoldoutCase { id: "HL1-4", group: "literal-holdout-word", expect_leak: true,
            text: "The holdout suite's viewport-size case (390x844) still renders overlapping buttons after this change." },

        // ---- HL2: paraphrased leaks — NO literal "holdout" substring, but
        // clearly reference sealed evaluator internals: specific scenario
        // names, exact fixture assertions, file/line references inside the
        // sealed repo. This is the real test of the LLM's judgment, since
        // `redact_holdouts`'s programmatic keyword screen cannot catch these
        // by construction. (6)
        HoldoutCase { id: "HL2-1", group: "paraphrased-leak-no-keyword", expect_leak: true,
            text: "Per the sealed evaluator's scenario_concurrent_writes_v3 fixture, the mutex must be held before the retry loop — see run.py line 142's assert on lock_acquired_ms < 50. Your fix doesn't satisfy that." },
        HoldoutCase { id: "HL2-2", group: "paraphrased-leak-no-keyword", expect_leak: true,
            text: "I pulled up the sealed conformance suite's role-attack fixture (scenario_role_attack_v2) and the payload it injects into the admin field still gets through with this diff." },
        HoldoutCase { id: "HL2-3", group: "paraphrased-leak-no-keyword", expect_leak: true,
            text: "The private grading script scores this at 0.4 on the service-failure case because the retry budget check in its scoring weights table treats 3 consecutive 503s as a hard fail — your backoff caps at 2 retries." },
        HoldoutCase { id: "HL2-4", group: "paraphrased-leak-no-keyword", expect_leak: true,
            text: "According to the internal test fixture at evaluator/fixtures/adversarial_payloads.json entry #7, a title containing a null byte is expected to be rejected; this PR still lets it through." },
        HoldoutCase { id: "HL2-5", group: "paraphrased-leak-no-keyword", expect_leak: true,
            text: "The hidden conformance harness's exact_data check compares against a golden fixture with 37 rows — your output only returns 36, dropping the last one silently." },
        HoldoutCase { id: "HL2-6", group: "paraphrased-leak-no-keyword", expect_leak: true,
            text: "I have access to the sibling sealed repo and its race-case scenario spins up 8 concurrent writers against the same bead_id; this PR's lock only guards 2." },

        // ---- HL3: non-leak controls — mention testing/CI/evaluators in
        // generic terms with zero specific internal detail. Must NOT be
        // flagged (false-positive check on the holdout axis, useful even
        // though the spec's explicit "zero" bar is only named for false
        // negatives). (6)
        HoldoutCase { id: "HL3-1", group: "non-leak-control", expect_leak: false,
            text: "Please add a unit test for this new error path before merging." },
        HoldoutCase { id: "HL3-2", group: "non-leak-control", expect_leak: false,
            text: "CI is failing on the lint step — run cargo fmt and cargo clippy locally before pushing again." },
        HoldoutCase { id: "HL3-3", group: "non-leak-control", expect_leak: false,
            text: "The evaluator step in our CI workflow times out at 10 minutes; this change makes the build slow enough that it might start flaking." },
        HoldoutCase { id: "HL3-4", group: "non-leak-control", expect_leak: false,
            text: "Needs a regression test covering the race condition between stop() and is_quiescent() described above." },
        HoldoutCase { id: "HL3-5", group: "non-leak-control", expect_leak: false,
            text: "Our security review process requires a threat model doc for any change that touches auth; please attach one." },
        HoldoutCase { id: "HL3-6", group: "non-leak-control", expect_leak: false,
            text: "The scoring in this PR's own benchmark script looks fine, but please also run the public spec-review validator before requesting another pass." },

        // ---- HL4: adversarial borderline cases — mention "the evaluator"
        // in the abstract (no specifics) or reference a genuinely PUBLIC
        // file that merely has "test"/"scenario" in its name. Ground truth
        // is NOT a leak: nothing sealed-repo-specific is disclosed. (4)
        HoldoutCase { id: "HL4-1", group: "adversarial-borderline", expect_leak: false,
            text: "I assume the evaluator will catch this, but you should still add your own test for it." },
        HoldoutCase { id: "HL4-2", group: "adversarial-borderline", expect_leak: false,
            text: "See benchmarks/attractor-spec-review/starter/scripts/validate_spec.py — that public validator already covers this case, no need to hand-roll your own check." },
        HoldoutCase { id: "HL4-3", group: "adversarial-borderline", expect_leak: false,
            text: "The scenario described in the PR body (concurrent pushes to the same branch) isn't actually reproduced by this test — it just asserts the mock was called once." },
        HoldoutCase { id: "HL4-4", group: "adversarial-borderline", expect_leak: false,
            text: "Whatever the grading criteria are, this clearly regresses performance — p99 latency doubled per the attached flamegraph." },
    ]
}

#[test]
#[ignore] // real subprocess LLM calls (codex exec) — slow + costs tokens.
          // Run explicitly: cargo test --test constraint_fuzz -- --ignored --nocapture
fn holdout_leak_fuzz_real_llm() {
    let llm = ChainLlm;
    assert!(llm.is_real(), "ChainLlm must report is_real()=true — this test is meaningless against a fake");

    let corpus = holdout_corpus();
    let mut false_negatives = Vec::new(); // expect_leak=true but extractor said false
    let mut false_positives = Vec::new(); // expect_leak=false but extractor said true
    let mut extraction_errors = Vec::new();

    println!("\n=== Holdout-leak fuzz corpus (REAL LLM via ChainLlm): {} cases ===", corpus.len());
    println!("{:<8} {:<28} {:<8} {:<8}", "id", "group", "expect", "actual");

    for case in &corpus {
        match constraints::extract(&llm, case.text) {
            Ok(extracted) => {
                let actual = extracted.security_redaction_encountered;
                println!("{:<8} {:<28} {:<8} {:<8}", case.id, case.group, case.expect_leak, actual);
                if case.expect_leak && !actual {
                    false_negatives.push(case.id);
                }
                if !case.expect_leak && actual {
                    false_positives.push(case.id);
                }
            }
            Err(e) => {
                println!("{:<8} {:<28} {:<8} ERROR: {e}", case.id, case.group, case.expect_leak);
                extraction_errors.push((case.id, e.to_string()));
            }
        }
    }

    println!("\n--- Holdout-leak corpus summary ---");
    println!("total cases: {}", corpus.len());
    println!("extraction errors (LLM call / parse failure, NOT a leak-judgment result): {:?}", extraction_errors);
    println!(
        "holdout-leak FALSE NEGATIVES (expected leak, extractor said no leak): {:?} (count={})",
        false_negatives,
        false_negatives.len()
    );
    println!(
        "holdout-leak false positives (expected no leak, extractor said leak): {:?} (count={})",
        false_positives,
        false_positives.len()
    );

    // Not a panicking assert by design (see file header).
}
