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
        repos: std::collections::HashMap::new(),
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

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

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

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

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

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

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

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

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

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

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

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();

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

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();

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

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();

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

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();

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

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();

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

    let (created_issues, _issue_outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();
    let (adopted_prs, _pr_outcomes) =
        intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();

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
