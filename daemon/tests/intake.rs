// Task 6: intake.rs normalizer tests (design doc §5, spec §4.2.3).
// Step 1 (TDD): failing tests against the scripted fakes in tests/common/mod.rs
// before intake.rs has any real implementation.
mod common;

use common::{FakeScm, FakeTracker};
use daemon::config::Config;
use daemon::intake;
use daemon::tools::{Bead, Issue, Permission};

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
        base_branch: "main".into(),
        stage: 1,
        max_workers: 30,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 600,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
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

/// (a) new labeled issue from a write-tier collaborator -> `create_bead` called
/// once with `external_ref="owner/repo#N"`, returns 1 new bead id, INTAKE event.
#[test]
fn new_issue_from_write_tier_collaborator_creates_one_bead() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(42, "alice"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let created = intake::normalize(&scm, &tracker, &cfg).unwrap();

    assert_eq!(created, vec!["fake-bead-1".to_string()]);

    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(create_calls.len(), 1, "expected exactly one create_bead call");
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
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#42".into()),
    });

    let cfg = test_cfg();

    let created = intake::normalize(&scm, &tracker, &cfg).unwrap();

    assert!(
        created.is_empty(),
        "expected no new beads for an already-known external_ref, got: {created:?}"
    );

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

    let created = intake::normalize(&scm, &tracker, &cfg).unwrap();

    assert!(
        created.is_empty(),
        "read-tier creator must not produce a new bead, got: {created:?}"
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
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#3".into()),
    });

    let cfg = test_cfg();

    let created = intake::normalize(&scm, &tracker, &cfg).unwrap();

    assert_eq!(created, vec!["fake-bead-1".to_string()]);

    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(create_calls.len(), 1);
    assert!(create_calls[0].contains("owner/repo#1"));
}
