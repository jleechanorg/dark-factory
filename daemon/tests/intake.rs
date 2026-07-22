// Task 6: intake.rs normalizer tests (design doc §5, spec §4.2.3).
// Step 1 (TDD): failing tests against the scripted fakes in tests/common/mod.rs
// before intake.rs has any real implementation.
mod common;

use common::{FakeScm, FakeTracker};
use daemon::config::Config;
use daemon::intake::{self, IntakeVerdict};
use daemon::tools::{Bead, Issue, LabeledPr, Permission};

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
        ao_project: None,
        base_branch: "main".into(),
        stage: 1,
        max_workers: 30,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 600,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
        reroll_head_stability_window_secs: 1,
        reroll_death_confirm_secs: 0,
        held_recheck_cooldown_secs: 900,
        repos: std::collections::HashMap::new(),
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
    }
}

fn issue(number: u64, author_login: &str) -> Issue {
    Issue {
        number,
        title: format!("issue {number}"),
        body: "body text".into(),
        author_login: author_login.into(),
        external_ref: format!("owner/repo#{number}"),
    }
}

fn labeled_pr(number: u64, author_login: &str, head_ref_name: &str) -> LabeledPr {
    LabeledPr {
        number,
        title: format!("pr {number}"),
        body: "pr body".into(),
        author_login: author_login.into(),
        external_ref: format!("owner/repo#{number}"),
        head_ref_name: head_ref_name.into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
    }
}

/// (a) new labeled issue from a write-tier collaborator -> `create_bead` called
/// once with `external_ref="owner/repo#N"`, returns 1 new bead id, INTAKE event.
#[test]
fn new_issue_from_write_tier_collaborator_creates_one_bead() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(42, "alice"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert_eq!(created, vec!["fake-bead-1".to_string()]);
    assert!(
        outcomes.is_empty(),
        "a clean single-candidate adopt should produce zero skip/error outcomes: {outcomes:?}"
    );

    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(
        create_calls.len(),
        1,
        "expected exactly one create_bead call"
    );
    assert!(
        create_calls[0].contains("owner/repo#42"),
        "create_bead call should carry external_ref owner/repo#42, got: {}",
        create_calls[0]
    );
}

/// (b) same issue submitted again (tracker already knows the external_ref via
/// fetch_candidates) -> no duplicate create_bead call.
#[test]
fn already_known_external_ref_is_not_duplicated() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(42, "alice"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "existing-bead".into(),
        title: "issue 42".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#42".into()),
    });

    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert!(
        created.is_empty(),
        "expected no new beads for an already-known external_ref, got: {created:?}"
    );
    // jleechan-eazj: the skip must still be reported so telemetry can prove
    // the daemon saw and evaluated the candidate.
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one verdict: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#42");
    assert_eq!(outcomes[0].verdict, IntakeVerdict::SkippedDuplicate);

    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert!(
        create_calls.is_empty(),
        "expected zero create_bead calls for duplicate issue, got: {create_calls:?}"
    );
}

/// (c) issue submitted by a read-tier user -> skipped, zero create_bead calls,
/// audit context present (verified via the returned Vec being empty; the actual
/// audit-logged event emission is asserted through normalize's Ok(()) contract
/// — normalize must not error out, it must silently-but-audibly skip).
#[test]
fn read_tier_creator_is_skipped_without_create_bead() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(7, "mallory"));
    scm.permissions.insert("mallory".into(), Permission::Read);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert!(
        created.is_empty(),
        "read-tier creator must not produce a new bead, got: {created:?}"
    );
    // jleechan-eazj: "ineligible" verdict names the actual precondition that
    // failed, not a generic message.
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one verdict: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#7");
    assert_eq!(
        outcomes[0].verdict,
        IntakeVerdict::SkippedIneligible {
            precondition: "author_permission_below_write_tier:Read".to_string()
        }
    );

    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert!(
        create_calls.is_empty(),
        "expected zero create_bead calls for read-tier creator, got: {create_calls:?}"
    );
}

/// Mixed batch: one write-tier + one read-tier + one already-known -> exactly
/// one create_bead call, for the write-tier newcomer only.
#[test]
fn mixed_batch_only_creates_bead_for_new_write_tier_issue() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(1, "alice")); // write-tier, new
    scm.issues.push(issue(2, "mallory")); // read-tier, skipped
    scm.issues.push(issue(3, "alice")); // write-tier, but already known
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.permissions.insert("mallory".into(), Permission::Read);

    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "existing-bead-3".into(),
        title: "issue 3".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#3".into()),
    });

    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert_eq!(created, vec!["fake-bead-1".to_string()]);
    // jleechan-eazj: the two non-adopted candidates in this batch (mallory's
    // read-tier issue and alice's already-known issue 3) must each still
    // produce exactly one verdict — a mixed batch is exactly the shape
    // where a per-candidate abort used to silently swallow candidates after
    // the first hiccup.
    assert_eq!(
        outcomes.len(),
        2,
        "expected 2 non-adopted verdicts: {outcomes:?}"
    );
    let by_ref = |r: &str| outcomes.iter().find(|o| o.external_ref == r);
    assert_eq!(
        by_ref("owner/repo#2").map(|o| &o.verdict),
        Some(&IntakeVerdict::SkippedIneligible {
            precondition: "author_permission_below_write_tier:Read".to_string()
        })
    );
    assert_eq!(
        by_ref("owner/repo#3").map(|o| &o.verdict),
        Some(&IntakeVerdict::SkippedDuplicate)
    );

    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(create_calls.len(), 1);
    assert!(create_calls[0].contains("owner/repo#1"));
}

/// jleechan-u4gb: the `known_refs` pre-check is a bulk snapshot read that can
/// race with reality (pagination skew, staleness, a duplicate entry within
/// the same batch, etc.) and miss a ref that is actually already tracked.
/// When that happens, `br create`'s own uniqueness constraint is the
/// authoritative signal — `create_bead` fails with the exact "already
/// exists on issue <id>" shape. `normalize` must treat that as an
/// already-tracked skip (matching the known_refs.contains path), NOT
/// propagate a tick-killing error that would retry forever against a ref
/// that will always already exist.
#[test]
fn create_bead_duplicate_error_is_recovered_as_already_known() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(8227, "alice"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    // known_refs (derived from `candidates`) does NOT contain owner/repo#8227
    // -- simulates the race: the pre-check snapshot missed it.
    *tracker.create_bead_duplicate_of.borrow_mut() = Some("jleechan-vj89".into());

    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert!(
        created.is_empty(),
        "duplicate create_bead race must not be reported as newly created: {created:?}"
    );
    // jleechan-eazj: a write-time-race recovery is still a SkippedDuplicate
    // verdict, not silence.
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one verdict: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#8227");
    assert_eq!(outcomes[0].verdict, IntakeVerdict::SkippedDuplicate);

    let calls = tracker.calls.borrow();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("create_bead("))
            .count(),
        1,
        "expected exactly one create_bead attempt (which raced and was recovered)"
    );
    assert!(
        calls.iter().all(|c| !c.starts_with("comment_external(")),
        "must not post the 'picked up this task' comment for a ref that was already tracked: {calls:?}"
    );
}

/// Same race, but for the existing-PR-adoption path (`normalize_labeled_prs`).
#[test]
fn pr_create_bead_duplicate_error_is_recovered_as_already_adopted() {
    let mut scm = FakeScm::new();
    scm.prs
        .push(labeled_pr(8227, "alice", "feature/existing-pr-8227"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    *tracker.create_bead_duplicate_of.borrow_mut() = Some("jleechan-vj89".into());

    let cfg = test_cfg();

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert!(
        adopted.is_empty(),
        "duplicate create_bead race must not produce a fresh adoption: {adopted:?}"
    );
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one verdict: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#8227");
    assert_eq!(outcomes[0].verdict, IntakeVerdict::SkippedDuplicate);
    let calls = tracker.calls.borrow();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("create_bead("))
            .count(),
        1
    );
    assert!(
        calls.iter().all(|c| !c.starts_with("comment_external(")),
        "must not post the adoption comment for a ref that was already tracked: {calls:?}"
    );
}

#[test]
fn new_factory_pr_from_write_tier_collaborator_creates_existing_pr_intake() {
    let mut scm = FakeScm::new();
    scm.prs
        .push(labeled_pr(51, "alice", "feature/existing-pr-51"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert_eq!(adopted.len(), 1);
    assert_eq!(adopted[0].bead_id, "fake-bead-1");
    assert_eq!(adopted[0].pr_number, 51);
    assert_eq!(adopted[0].head_ref_name, "feature/existing-pr-51");
    assert_eq!(adopted[0].external_ref, "owner/repo#51");
    assert!(adopted[0].newly_created);
    assert!(
        outcomes.is_empty(),
        "a clean adoption should produce zero skip/error outcomes: {outcomes:?}"
    );

    let calls = tracker.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|call| call.contains("create_bead(pr 51 (owner/repo),pr body,owner/repo#51)")),
        "expected PR bead creation call, got: {calls:?}"
    );
}

#[test]
fn factory_pr_with_existing_external_ref_reuses_bead() {
    let mut scm = FakeScm::new();
    scm.prs
        .push(labeled_pr(52, "alice", "feature/existing-pr-52"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "existing-pr-bead".into(),
        title: "pr 52".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#52".into()),
    });
    let cfg = test_cfg();

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert_eq!(adopted.len(), 1);
    assert_eq!(adopted[0].bead_id, "existing-pr-bead");
    assert!(!adopted[0].newly_created);
    assert!(
        outcomes.is_empty(),
        "reusing an existing bead should produce zero skip/error outcomes: {outcomes:?}"
    );
    let create_calls: Vec<_> = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.starts_with("create_bead("))
        .cloned()
        .collect();
    assert!(
        create_calls.is_empty(),
        "got duplicate creates: {create_calls:?}"
    );
}

#[test]
fn factory_pr_from_read_tier_creator_is_skipped() {
    let mut scm = FakeScm::new();
    scm.prs
        .push(labeled_pr(53, "mallory", "feature/existing-pr-53"));
    scm.permissions.insert("mallory".into(), Permission::Read);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert!(adopted.is_empty());
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one verdict: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#53");
    assert_eq!(
        outcomes[0].verdict,
        IntakeVerdict::SkippedIneligible {
            precondition: "author_permission_below_write_tier:Read".to_string()
        }
    );
    let create_calls: Vec<_> = tracker
        .calls
        .borrow()
        .iter()
        .filter(|call| call.starts_with("create_bead("))
        .cloned()
        .collect();
    assert!(
        create_calls.is_empty(),
        "read-tier PR created bead: {create_calls:?}"
    );
}

#[test]
fn fork_factory_pr_is_skipped_with_escalation_comment() {
    let mut scm = FakeScm::new();
    let mut pr = labeled_pr(54, "alice", "factory/existing-bead-r1");
    pr.is_cross_repository = true;
    pr.head_repo_full_name = Some("fork/repo".into());
    pr.head_repo_owner_login = Some("fork".into());
    scm.prs.push(pr);
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert!(adopted.is_empty());
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one verdict: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#54");
    assert_eq!(outcomes[0].verdict, IntakeVerdict::SkippedFork);
    let calls = tracker.calls.borrow();
    assert!(
        calls.iter().all(|call| !call.starts_with("create_bead(")),
        "fork PR must not create an adopted bead: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| {
            call.contains("comment_external(owner/repo#54")
                && call.contains("fork/cross-repository PR adoption is not supported")
                && call.contains("jleechan-tfs1")
        }),
        "fork PR must receive an escalation comment: {calls:?}"
    );
}

/// jleechan-3wh0: external_ref lineage backfill regression coverage.
///
/// 15 of 18 pre-existing factory beads were discovered to carry no
/// `external_ref`, so there's no queryable link back to the GitHub issue/PR
/// that produced them. Root cause investigation (file:line-cited in the
/// jleechan-3wh0 PR description) found that *every* in-process bead-creation
/// call site — both `intake::normalize` (new-issue path) and
/// `intake::normalize_labeled_prs` (existing-PR-adoption path) — has always
/// required `external_ref: &str` as a mandatory, non-optional parameter on
/// the `Tracker::create_bead` trait (see `daemon/src/tools.rs`); no caller in
/// this codebase's history has ever been able to omit it. The orphans
/// instead trace to `tick.rs`'s *separate* "manual bead adoption" path
/// (tick.rs, `serde_json::json!({"manual": true})` marker), which does not
/// call `create_bead` at all — it only initializes local `BeadOverlay`
/// tracking state for a bead that was already created *outside* the daemon
/// (an operator/agent running `br create` directly). See
/// `tick_integration.rs::manual_bead_adoption_never_calls_create_bead_or_fabricates_external_ref`
/// for that path's coverage.
///
/// This test is the generic regression guard: run a batch through BOTH
/// creation paths simultaneously and assert every single `create_bead` call
/// recorded in the tracker's call log — regardless of which path produced
/// it — carries a non-empty `owner/repo#<number>`-shaped `external_ref`. A
/// future change that adds a new bead-creation path, or that weakens either
/// existing path to allow an empty/placeholder ref, will fail this test.
#[test]
fn every_create_bead_call_across_both_intake_paths_carries_nonempty_external_ref() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(101, "alice"));
    scm.issues.push(issue(102, "alice"));
    scm.prs.push(labeled_pr(201, "alice", "feature/pr-201"));
    scm.prs.push(labeled_pr(202, "alice", "feature/pr-202"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let (created_issues, _issue_outcomes) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();
    let (adopted_prs, _pr_outcomes) =
        intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    assert_eq!(created_issues.len(), 2, "expected both new issues to create beads");
    assert_eq!(adopted_prs.len(), 2, "expected both new PRs to be adopted");

    let calls = tracker.calls.borrow();
    let create_calls: Vec<&String> =
        calls.iter().filter(|c| c.starts_with("create_bead(")).collect();
    assert_eq!(
        create_calls.len(),
        4,
        "expected exactly 4 create_bead calls (2 issue + 2 PR), got: {create_calls:?}"
    );

    for call in &create_calls {
        // Call log shape is `create_bead(<title>,<body>,<external_ref>)`.
        let inner = call
            .strip_prefix("create_bead(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or_else(|| panic!("unexpected create_bead call shape: {call}"));
        let external_ref = inner
            .rsplit(',')
            .next()
            .unwrap_or_else(|| panic!("create_bead call missing external_ref segment: {call}"));
        assert!(
            !external_ref.trim().is_empty(),
            "create_bead call carried an empty external_ref — this is exactly the jleechan-3wh0 \
             orphan-bead defect shape: {call}"
        );
        assert!(
            external_ref.contains('#'),
            "external_ref should be shaped owner/repo#<number>, got '{external_ref}' from call: {call}"
        );
    }
}

// =============================================================================
// jleechan-jtg8: intake sweep gh-REST burn regression tests.
//
// The factory daemon slow-tier tick calls `normalize_labeled_prs` every
// ~60s. With ~30 factory-labeled PRs (across dark-factory +
// worldarchitect.ai) and `collaborator_permission` doing one REST call per
// author, the slow-tier intake was burning ~1800 gh REST calls per hour
// JUST for the permission probe, exhausting the shared 5000/hr core bucket
// on user 13840161 and starving operator/CI tooling.
//
// Acceptance criteria require: (a) per-PR probes cached across ticks; (b)
// steady-state tick = O(1) gh calls per repo, not O(N); (c) per-tick gh call
// count metric + warn threshold; (d) regression test asserting zero per-PR
// probe calls on the second tick of an unchanged PR set; (e) rate-limit 403
// during intake must not bump `consecutive_failures` into mass-backoff when
// dispatch work is independent of gh.
//
// These tests pin all five contracts at the level of the
// `intake::normalize*` entry points (the function the daemon actually calls
// every slow tick). They use the shared `FakeScm`/`FakeTracker` fakes to
// count exact gh call shape without any subprocess use.
// =============================================================================

#[test]
fn slow_tick_over_already_adopted_prs_makes_zero_per_pr_probe_calls() {
    // Pin the burn vector: when every factory-labeled PR is ALREADY in
    // the tracker's known_refs / candidates set (the steady state under
    // which the 2026-07-22 19:09-19:21 + 20:0x-20:2x rate-limit storms
    // were observed), `normalize_labeled_prs` must do ZERO
    // `collaborator_permission` calls and ZERO `create_bead` calls —
    // only the single `labeled_prs` list call.
    //
    // Today (RED) the function calls `collaborator_permission` BEFORE
    // the `tracker_candidates` / `known_refs` checks, so even already-
    // adopted PRs cost one REST call per tick. That is the bug.
    let mut scm = FakeScm::new();
    scm.prs.push(labeled_pr(9001, "alice", "feat/a"));
    scm.prs.push(labeled_pr(9002, "alice", "feat/b"));
    scm.prs.push(labeled_pr(9003, "bob", "feat/c"));
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.permissions.insert("bob".into(), Permission::Write);

    let tracker = FakeTracker::new();
    // Pre-seed the tracker as if every PR was already adopted in a prior
    // tick — this is the steady state the bug report describes.
    for n in [9001u64, 9002, 9003] {
        tracker.candidates.borrow_mut().push(Bead {
            id: format!("bead-{n}"),
            title: format!("pr {n}"),
            description: String::new(),
            notes: String::new(),
            file_tree_summary: String::new(),
            external_ref: Some(format!("owner/repo#{n}")),
        });
    }
    let cfg = test_cfg();

    let (_adopted, _outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();

    let calls = scm.calls.borrow();
    let per_pr_probe_calls = calls
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .count();
    assert_eq!(
        per_pr_probe_calls, 0,
        "already-adopted PRs MUST NOT trigger collaborator_permission REST calls \
         — that is the burn vector this test pins. got calls: {:?}",
        *calls
    );
    let create_bead_calls = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .count();
    assert_eq!(
        create_bead_calls, 0,
        "already-adopted PRs MUST NOT trigger create_bead (no new bead needed)"
    );
    let list_calls = calls
        .iter()
        .filter(|c| c.starts_with("labeled_prs("))
        .count();
    assert_eq!(list_calls, 1, "one labeled_prs list call per repo per slow tick");
}

#[test]
fn only_truly_new_pr_triggers_probe_existing_prs_skip_without_probe() {
    // Stricter version: a mix of one already-adopted PR and one new
    // candidate. The already-adopted PR must NOT trigger
    // collaborator_permission (the fix); the new PR must be probed
    // exactly once (its permission was not yet seen).
    let mut scm = FakeScm::new();
    scm.prs.push(labeled_pr(7001, "alice", "feat/already-adopted"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "bead-already".into(),
        title: "pr 7001".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#7001".into()),
    });

    // Now add a NEW PR that hasn't been adopted yet.
    scm.prs.push(labeled_pr(7002, "alice", "feat/newcomer"));
    let cfg = test_cfg();

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();
    assert_eq!(adopted.len(), 2, "both PRs are adopted (7001 reused, 7002 new)");
    assert_eq!(
        outcomes.len(),
        0,
        "no SKIPPED_* verdicts in this batch — both PRs adopted"
    );

    let adopted_newcomer = adopted.iter().any(|i| i.external_ref == "owner/repo#7002" && i.newly_created);
    let adopted_reused = adopted.iter().any(|i| i.external_ref == "owner/repo#7001" && !i.newly_created);
    assert!(adopted_newcomer, "7002 should be a freshly-created adoption");
    assert!(adopted_reused, "7001 should be reused from existing bead");

    let calls = scm.calls.borrow();
    let per_pr: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .collect();
    assert_eq!(
        per_pr.len(),
        1,
        "ONLY the new PR may be probed; already-adopted PRs MUST skip \
         the REST call. got calls: {:?}",
        *calls
    );
    assert!(
        per_pr[0].contains("alice"),
        "the probe should be for alice, got: {}",
        per_pr[0]
    );
}

#[test]
fn issue_path_same_tick_unaffected_and_also_short_circuits_on_second_pass() {
    // Mirror contract for the labeled-issue path. The bug report mentions
    // EXISTING_PR_ADOPTED + SKIPPED_DUPLICATE both burning, but the issue
    // path has the same shape and the same per-author probe. Lock both
    // down in one test to keep them in sync going forward.
    let mut scm = FakeScm::new();
    scm.issues.push(issue(8001, "alice"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    tracker.candidates.borrow_mut().push(Bead {
        id: "bead-issue-8001".into(),
        title: "issue 8001".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#8001".into()),
    });
    let cfg = test_cfg();

    // Tick 1: issue already known -> SKIPPED_DUPLICATE, no probe.
    let (created1, outcomes1) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();
    assert_eq!(created1.len(), 0);
    assert_eq!(outcomes1.len(), 1);
    {
        let t1 = scm.calls.borrow();
        assert_eq!(
            t1.iter()
                .filter(|c| c.starts_with("collaborator_permission("))
                .count(),
            0
        );
    }

    scm.calls.borrow_mut().clear();
    // Tick 2: identical -> zero per-PR probes.
    let (created2, outcomes2) = intake::normalize(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default()).unwrap();
    assert_eq!(created2.len(), 0);
    assert_eq!(outcomes2.len(), 1);
    {
        let t2 = scm.calls.borrow();
        assert_eq!(
            t2.iter()
                .filter(|c| c.starts_with("collaborator_permission("))
                .count(),
            0,
            "issue path second tick must not re-probe collaborator_permission"
        );
        assert_eq!(
            t2.iter()
                .filter(|c| c.starts_with("labeled_issues("))
                .count(),
            1
        );
    }
}

// jleechan-jtg8 (e): rate-limit 403 during intake must not bump
// `consecutive_failures` into mass-backoff when dispatch work is
// independent of gh. We pin the contract at the SlowTierResult level —
// the slow tier now returns a structured outcome that distinguishes
// intake failures (recoverable, no mass-backoff) from dispatch failures
// (escalating). When only intake fails, the dispatch side of the tick
// must still be reachable on subsequent ticks.
//
// This test calls `intake::normalize_labeled_prs` directly with a fake
// SCM that returns a 403-shaped `DaemonError::Tool` from `labeled_prs`
// (mimicking `gh api` rate-limit exhaustion). The intake function must
// surface this as `Err(..)` so the slow tier can branch, but the
// dispatch path (post-intake queue) must NOT see `consecutive_failures`
// incremented by this single failure — that's a property the slow tier
// wrapper will enforce after the fix. Today, `run_slow_tier` propagates
// any Err via `?` and `run_tick` then wraps it through the same
// classify_tick_result path as a dispatch failure. After the fix,
// `run_slow_tier` returns a struct that distinguishes the two, and the
// tick loop applies backoff only on dispatch failures.
//
// To make the contract testable at the unit level, we expose a
// `SlowTierOutcome` (added by this PR) and assert that `run_slow_tier`
// classifies an intake-only 403 as `IntakeOnly` — observable via the
// returned outcome struct. The DaemonState-level integration (no
// consecutive_failures bump) is covered by the `tick_integration.rs`
// extension added in this same PR; pinning it at the intake-only level
// here keeps the cache contract tests self-contained.
#[test]
fn intake_labeled_prs_403_returns_err_so_slow_tier_can_branch() {
    // Contract: when `scm.labeled_prs` returns Err (rate-limit 403), the
    // intake function propagates Err unchanged. The slow tier wrapper
    // (added by this PR) then classifies this as `IntakeOnly` and does
    // not propagate to the dispatch-level backoff path. We don't assert
    // the wrapper here — that's tested separately — but we lock down the
    // shape of the intake error so the wrapper has a stable signal to
    // pattern-match on.
    let scm = common::RateLimitedScm::new();
    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let result = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut intake::IntakeMetrics::default());
    assert!(
        result.is_err(),
        "an intake-only 403 must surface as Err so the slow tier can \
         branch on it (today: returns Err with full stack trace context)"
    );
    // Shape: must be a DaemonError::Tool so the slow tier can distinguish
    // it from a parse/IO error.
    match result.unwrap_err() {
        daemon::errors::DaemonError::Tool { tool, stderr, .. } => {
            assert_eq!(tool, "gh");
            assert!(
                stderr.contains("rate limit"),
                "stderr should carry the gh 403 marker, got: {stderr}"
            );
        }
        other => panic!("expected DaemonError::Tool from a gh 403, got {other:?}"),
    }
}

#[test]
fn intake_metrics_counts_gh_calls_accurately_for_steady_state() {
    // Lock the per-tick gh call count metric. A steady-state tick (no
    // new candidates) MUST report exactly 1 gh call (the single
    // `labeled_prs` list call), regardless of how many PRs are
    // factory-labeled. Anything more means the cache reorder regressed
    // and the burn vector is back.
    let mut scm = FakeScm::new();
    for n in 9001u64..9010 {
        scm.prs.push(labeled_pr(n, "alice", &format!("feat/{n}")));
    }
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    for n in 9001u64..9010 {
        tracker.candidates.borrow_mut().push(Bead {
            id: format!("bead-{n}"),
            title: format!("pr {n}"),
            description: String::new(),
            notes: String::new(),
            file_tree_summary: String::new(),
            external_ref: Some(format!("owner/repo#{n}")),
        });
    }
    let cfg = test_cfg();

    let mut metrics = intake::IntakeMetrics::default();
    let _ = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut metrics).unwrap();
    assert_eq!(
        metrics.gh_calls, 1,
        "steady-state tick over 9 already-adopted PRs must record exactly 1 gh call \
         (the labeled_prs list call), got {}",
        metrics.gh_calls
    );
}

#[test]
fn intake_metrics_counts_each_new_candidate_probe() {
    // A tick with N genuinely-new PRs MUST record 1 + N gh calls
    // (the labeled_prs list call + one collaborator_permission per new
    // candidate). This is the "new work" path — the burn is bounded
    // by the number of new candidates, which is what the warn
    // threshold guards against.
    let mut scm = FakeScm::new();
    scm.prs.push(labeled_pr(6001, "alice", "feat/a"));
    scm.prs.push(labeled_pr(6002, "alice", "feat/b"));
    scm.prs.push(labeled_pr(6003, "bob", "feat/c"));
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.permissions.insert("bob".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let mut metrics = intake::IntakeMetrics::default();
    let _ = intake::normalize_labeled_prs(&scm, &tracker, &cfg, &mut metrics).unwrap();
    assert_eq!(
        metrics.gh_calls, 4,
        "3 new PRs + 1 list call = 4 gh calls; got {}",
        metrics.gh_calls
    );
}
