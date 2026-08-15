// Task 6: intake.rs normalizer tests (design doc §5, spec §4.2.3).
// Step 1 (TDD): failing tests against the scripted fakes in tests/common/mod.rs
// before intake.rs has any real implementation.
mod common;

use common::{FakeScm, FakeTracker};
use daemon::config::Config;
use daemon::errors::DaemonError;
use daemon::intake::{self, IntakeVerdict};
use daemon::tools::{Bead, Issue, LabeledPr, Permission, PrSnapshot, Scm, Tracker};

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
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
    }
}

/// PR #629 follow-up fix: `normalize_labeled_prs_outcome` now takes a
/// `telemetry_log` path so per-repo sweep-failure isolation points can emit
/// a structured `INTAKE_REPO_SWEEP_FAILED` event. Every call site in this
/// file needs a real (unique, per-call) path — writes are best-effort and
/// never asserted on here, but the path must be writable so
/// `emit_intake_repo_sweep_failed`'s `telemetry::emit` doesn't error.
fn test_telemetry_log() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "afd_intake_test_telemetry_{}_{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn adoption_probe_cache_persists_in_runtime_state_not_target_beads_dir() {
    let root = std::env::temp_dir().join(format!(
        "afd_adoption_cache_runtime_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cache_path = root.join("adoption_probe_cache.json");
    let key = ProbeCacheKey {
        external_ref: "owner/repo#1".into(),
        head_sha: Some("head".into()),
        updated_at_epoch: Some(1),
    };
    let mut cache = AdoptionProbeCache::load_or_default_at(&cache_path);
    cache.insert(key.clone(), CachedDecisionKind::AuthorPermission(Permission::Write), 1);
    cache.persist().unwrap();

    assert!(cache_path.is_file());
    assert!(!root.join(".beads/adoption_probe_cache.json").exists());
    let restored = AdoptionProbeCache::load_or_default_at(&cache_path);
    assert!(restored.contains(&key));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn adoption_probe_cache_concurrent_persists_leave_no_shared_temp_file() {
    let root = std::env::temp_dir().join(format!(
        "afd_adoption_cache_race_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("adoption_probe_cache.json");
    let expected_keys = (0..8)
        .map(|index| ProbeCacheKey {
            external_ref: format!("owner/repo#{index}"),
            head_sha: Some(format!("head-{index}")),
            updated_at_epoch: Some(index),
        })
        .collect::<Vec<_>>();
    let mut workers = Vec::new();
    for key in expected_keys.clone() {
        let path = path.clone();
        workers.push(std::thread::spawn(move || {
            let mut cache = AdoptionProbeCache::load_or_default_at(path);
            cache.insert(
                key,
                CachedDecisionKind::AuthorPermission(Permission::Write),
                1,
            );
            cache.persist().unwrap();
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    let restored = AdoptionProbeCache::load_or_default_at(&path);
    for key in &expected_keys {
        assert!(restored.contains(key), "concurrent key was lost: {key:?}");
    }
    let leftovers = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().contains(".json.tmp"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "temporary files leaked: {leftovers:?}");
    std::fs::remove_dir_all(root).unwrap();
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
        // Default cache-key fields: distinct head SHA per PR so the
        // adoption-probe cache treats each PR independently. Tests that
        // exercise cache hit/invalidation behavior use `labeled_pr_with_cache_key`
        // to populate these explicitly.
        head_sha: Some(format!("sha-{number}")),
        updated_at_epoch: Some(1_700_000_000 + number),
    }
}

/// jtg8-r4: variant of `labeled_pr` that lets the caller script the cache
/// key fields explicitly so cache hit / miss / invalidation tests can drive
/// distinct `head_sha`/`updated_at_epoch` shapes per PR per tick.
fn labeled_pr_with_cache_key(
    number: u64,
    author_login: &str,
    head_ref_name: &str,
    head_sha: &str,
    updated_at_epoch: u64,
) -> LabeledPr {
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
        head_sha: Some(head_sha.into()),
        updated_at_epoch: Some(updated_at_epoch),
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

/// jleechan-uinw (acceptance criterion #1+#2): a NON-duplicate
/// `create_bead` failure on ONE candidate must NOT abort the rest of the
/// batch — every remaining candidate in the same `scm.labeled_issues`
/// fetch must still be evaluated, recorded as a verdict, and have its
/// `create_bead` attempted. The fix that enforces this contract
/// (jleechan-eazj, already on main) wraps `tracker.create_bead` in
/// `match ... continue` inside the per-issue loop body and converts any
/// non-duplicate error into an `IntakeVerdict::Errored { reason }` outcome
/// for that single candidate only. Before the fix, the same shape
/// short-circuited via `?` and made `normalize` return Err, which
/// `run_slow_tier` then propagated to `main` -> `std::process::exit(1)`,
/// killing the daemon and silently starving every later candidate of any
/// telemetry at all (the live jleechan-eazj incident: 0 outcomes logged
/// on a slow tick after the first bad candidate).
///
/// The regression vector uses `FakeTracker.create_bead_fail_for_ref` (the
/// hook the eazj fix added for exactly this test): the fake returns a
/// generic `DaemonError::Tool` (NOT a `duplicate_external_ref_bead_id`)
/// for the scripted external_ref, and the default success path for
/// everything else. Two write-tier issues are presented; the FIRST issue's
/// `create_bead` errors, the SECOND issue is genuinely new. The test
/// fails if either (a) the second issue's bead doesn't get created, or
/// (b) the first issue's error reason is not surfaced as an
/// `IntakeVerdict::Errored` verdict.
#[test]
fn create_bead_non_duplicate_error_does_not_abort_remaining_batch() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue(11, "alice")); // first in iteration order — create_bead fails
    scm.issues.push(issue(12, "alice")); // second — must still be processed
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    *tracker.create_bead_fail_for_ref.borrow_mut() = Some((
        "owner/repo#11".to_string(),
        "beads store rejected #11: simulated transient infrastructure failure".to_string(),
    ));

    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

    // jleechan-uinw acceptance criterion: the second, legitimately new
    // issue MUST still get a bead created in the same `normalize` call.
    assert!(
        created.iter().any(|id| id == "fake-bead-1"),
        "second issue (#12) must still be created when the first issue's create_bead fails: {created:?}"
    );
    assert_eq!(
        created.len(),
        1,
        "expected exactly one new bead (the second issue), got: {created:?}"
    );

    // jleechan-eazj: every non-adopted candidate still gets a verdict —
    // the failing first issue MUST be reported as `Errored { reason }`,
    // not silently dropped, and the reason MUST carry the real stderr
    // (not a generic "something failed" placeholder) so operators can
    // triage from telemetry alone.
    assert_eq!(
        outcomes.len(),
        1,
        "expected one verdict for the first issue's failure: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#11");
    match &outcomes[0].verdict {
        IntakeVerdict::Errored { reason } => assert!(
            reason.contains("simulated transient infrastructure failure"),
            "errored verdict must surface the real failure reason, got: {reason:?}"
        ),
        other => {
            panic!("expected IntakeVerdict::Errored for the failing first issue, got: {other:?}")
        }
    }

    // jleechan-uinw acceptance criterion (call-shape proof): the fake
    // MUST see TWO create_bead attempts (one for each candidate) — the
    // first failing, the second succeeding. A regression that aborts the
    // loop on the first error would only show one create_bead call.
    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(
        create_calls.len(),
        2,
        "both candidates must reach create_bead — one failing, one succeeding: {create_calls:?}"
    );
    assert!(
        create_calls.iter().any(|c| c.contains("owner/repo#11")),
        "first candidate create_bead call missing: {create_calls:?}"
    );
    assert!(
        create_calls
            .iter()
            .any(|c| c.contains("owner/repo#12")),
        "second candidate create_bead call missing (regression — per-candidate abort leaked): {create_calls:?}"
    );
}

/// jleechan-uinw (acceptance criterion #3): same contract as
/// `create_bead_non_duplicate_error_does_not_abort_remaining_batch`, but
/// on the PR-adoption sister loop (`normalize_labeled_prs`,
/// daemon/src/intake.rs line 1347). One PR's `create_bead` errors
/// non-duplicately; the rest of the PR batch must still be processed and
/// adopted. Mirrors the jleechan-eazj fix in the labeled-prs loop body
/// (lines 1462-1501 of `intake.rs`).
///
/// Companion to the issue-intake test above — together they cover the
/// two consumer loops in `intake.rs` that the eazj fix hardened. The
/// issue test covers the `scm.labeled_issues` fetch path; this one
/// covers `scm.labeled_prs`. Both must continue to hold across any
/// future refactor that touches the per-candidate loop bodies.
#[test]
fn pr_create_bead_non_duplicate_error_does_not_abort_remaining_batch() {
    let mut scm = FakeScm::new();
    scm.prs
        .push(labeled_pr(21, "alice", "feature/first-pr-21")); // first — create_bead fails
    scm.prs
        .push(labeled_pr(22, "alice", "feature/second-pr-22")); // second — must still be adopted
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    *tracker.create_bead_fail_for_ref.borrow_mut() = Some((
        "owner/repo#21".to_string(),
        "beads store rejected #21: simulated non-duplicate transient failure".to_string(),
    ));

    let cfg = test_cfg();

    let (adopted, outcomes) = intake::normalize_labeled_prs(&scm, &tracker, &cfg).unwrap();

    // jleechan-uinw acceptance criterion: second PR MUST still be adopted
    // in the same sweep as the first PR's create_bead failure.
    assert_eq!(
        adopted.len(),
        1,
        "second PR (#22) must still be adopted when the first PR's create_bead fails: {adopted:?}"
    );
    assert_eq!(adopted[0].pr_number, 22);
    assert_eq!(adopted[0].external_ref, "owner/repo#22");
    assert!(
        adopted[0].newly_created,
        "the surviving adoption must be a fresh bead creation: {adopted:?}"
    );

    // jleechan-eazj: the failing first PR still surfaces a verdict.
    assert_eq!(
        outcomes.len(),
        1,
        "expected one Errored verdict for the first PR: {outcomes:?}"
    );
    assert_eq!(outcomes[0].external_ref, "owner/repo#21");
    match &outcomes[0].verdict {
        IntakeVerdict::Errored { reason } => assert!(
            reason.contains("simulated non-duplicate transient failure"),
            "errored verdict must surface the real failure reason, got: {reason:?}"
        ),
        other => panic!(
            "expected IntakeVerdict::Errored for the failing first PR, got: {other:?}"
        ),
    }

    let calls = tracker.calls.borrow();
    let create_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(
        create_calls.len(),
        2,
        "both PRs must reach create_bead — one failing, one succeeding: {create_calls:?}"
    );
    assert!(
        create_calls.iter().any(|c| c.contains("owner/repo#21")),
        "first PR create_bead call missing: {create_calls:?}"
    );
    assert!(
        create_calls.iter().any(|c| c.contains("owner/repo#22")),
        "second PR create_bead call missing (regression — per-candidate abort leaked): {create_calls:?}"
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

// jtg8-r4 TDD red tests ======================================================
//
// These tests exercise the r4 fix for bead jleechan-jtg8:
// 1. The slow-tier intake sweep must NOT abort the rest of run_slow_tier when
//    a `gh` rate-limit is detected on the PR-intake phase — only the PR-intake
//    pass itself degrades; routing + dispatch continue (the r3 fix returned
//    `Ok(())` early, which starved every subsequent phase).
// 2. The `IntakeProbeMetrics.gh_call_count` field must count REAL subprocess
//    invocations on the Scm boundary (so the slow tier can warn when the
//    count drifts back toward pre-fix behavior).
// 3. The adoption-probe cache's AuthorPermission entry must re-probe when the
//    contributor's collaborator tier changes between ticks (the r3 cache used
//    only (external_ref, head_sha, updated_at_epoch), which never changes on
//    a tier promotion/demotion).

use daemon::intake::{AdoptionProbeCache, CachedDecisionKind, ProbeCacheKey};

/// r4 red test #1: rate-limit on `labeled_prs` must NOT short-circuit the
/// slow-tier's routing + dispatch work. The slow tier MUST log a skip,
/// persist whatever cache state it has so far, and continue into the
/// `intake::normalize` issue path + routing + dispatch phases. The expected
/// observable contract is:
///   - normalize_labeled_prs_outcome returns `Ok(_)` with `rate_limited = true`
///   - the slow tier continues to call `tracker.fetch_candidates()` for routing
///   - `dispatch_ready` runs against any QUEUED beads from prior ticks
#[test]
fn intake_rate_limit_during_labeled_prs_does_not_abort_dispatch() {
    let scm = FakeScm::new();
    // Scripted rate-limit on the very next `labeled_prs` call.
    *scm.rate_limit_next_labeled_prs.borrow_mut() = true;

    let tracker = FakeTracker::new();
    let cfg = test_cfg();
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();
    assert!(
        outcome.rate_limited,
        "rate-limited intake sweep must report rate_limited=true so the slow tier can skip without erroring"
    );
    assert!(
        outcome.adopted.is_empty() && outcome.outcomes.is_empty(),
        "rate-limited sweep must produce zero adopted/outcomes (no telemetry = no false negatives)"
    );
    // The list query itself counts as 1 gh call before the rate-limit skip.
    assert_eq!(outcome.metrics.gh_call_count, 1);
    assert_eq!(outcome.metrics.rate_limited_skips, 1);

    // The PR-intake contract: the rate-limit skip fires before the per-PR
    // loop, so NO collaborator_permission calls should have been made.
    // We assert this on the Scm's call log because FakeScm records every
    // tool invocation. The downstream contract — that the slow tier
    // continues to routing + dispatch phases after a rate-limited
    // intake — is covered separately by the run_slow_tier restructure
    // (the r4 fix changes `return Ok(())` to "log skip, continue").
    let scm_calls = scm.calls.borrow();
    assert_eq!(
        scm_calls
            .iter()
            .filter(|c| c.starts_with("collaborator_permission("))
            .count(),
        0,
        "rate-limited sweep must NOT probe any PR; got calls: {:?}",
        scm_calls.iter().collect::<Vec<_>>()
    );
}

/// r4 red test #2: zero per-PR probes on the second tick over an unchanged
/// PR set. The cache MUST serve every PR's adoption/duplicate decision from
/// disk (zero `collaborator_permission` invocations), and the only allowed
/// `gh` call this tick is the single `labeled_prs` list query.
///
/// This is the headline acceptance criterion from the bead:
/// "two consecutive ticks over an unchanged PR set — second tick makes
/// zero per-PR probe calls".
#[test]
fn second_tick_over_unchanged_prs_makes_zero_per_pr_probes() {
    let mut scm = FakeScm::new();
    scm.prs.push(labeled_pr_with_cache_key(
        501,
        "alice",
        "feature/pr-501",
        "sha-501-aaaa",
        1_700_000_000,
    ));
    scm.prs.push(labeled_pr_with_cache_key(
        502,
        "alice",
        "feature/pr-502",
        "sha-502-bbbb",
        1_700_000_010,
    ));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();
    let mut cache = AdoptionProbeCache::new();

    // Tick 1 — populates the probe cache for PRs 501/502.
    let outcome1 = intake::normalize_labeled_prs_outcome(&scm, &tracker, &cfg, &mut cache, 1_700_000_000, &test_telemetry_log()).unwrap();
    assert_eq!(outcome1.adopted.len(), 2, "tick 1 must adopt both fresh PRs");
    assert!(outcome1.outcomes.is_empty());
    assert_eq!(outcome1.metrics.probe_cache_misses, 2);
    assert_eq!(outcome1.metrics.probe_cache_hits, 0);

    let tick1_perm_calls = scm
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .count();
    assert_eq!(
        tick1_perm_calls, 2,
        "tick 1 must probe each PR's collaborator permission exactly once"
    );

    // Reset call log so we can prove tick 2's zero-per-PR behavior.
    scm.calls.borrow_mut().clear();

    // Tick 2 — same PR set, same head_sha, same updated_at. The probe cache
    // MUST serve all per-PR adoption/duplicate decisions from disk, so the
    // only allowed gh call this tick is the single `labeled_prs` list query
    // (used to discover PRs and read their cache keys).
    let outcome2 = intake::normalize_labeled_prs_outcome(&scm, &tracker, &cfg, &mut cache, 1_700_000_000, &test_telemetry_log()).unwrap();
    assert_eq!(outcome2.adopted.len(), 2, "tick 2 must re-adopt both PRs");
    assert!(outcome2.outcomes.is_empty());
    assert_eq!(outcome2.metrics.probe_cache_hits, 2);
    assert_eq!(outcome2.metrics.probe_cache_misses, 0);

    let tick2_calls = scm.calls.borrow();
    let tick2_labeled_prs_calls = tick2_calls
        .iter()
        .filter(|c| c.starts_with("labeled_prs("))
        .count();
    let tick2_perm_calls = tick2_calls
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .count();
    assert_eq!(
        tick2_labeled_prs_calls, 1,
        "tick 2 must do exactly one list query, got: {:?}",
        tick2_calls.iter().collect::<Vec<_>>()
    );
    assert_eq!(
        tick2_perm_calls, 0,
        "tick 2 must NOT re-probe collaborator_permission for cached PRs, got: {:?}",
        tick2_calls.iter().collect::<Vec<_>>()
    );
}

/// r4 red test #3: cache invalidation on `head_sha` / `updated_at_epoch`
/// change. When a contributor pushes new commits, only the changed PR
/// must re-probe; unchanged PRs continue to be served from cache. This is
/// the central correctness invariant of the cache: cached == stable key,
/// never stale reads.
#[test]
fn probe_cache_invalidates_on_changed_head_sha_but_serves_unchanged_prs() {
    let mut scm = FakeScm::new();
    scm.prs.push(labeled_pr_with_cache_key(
        601,
        "alice",
        "feature/pr-601",
        "sha-601-original",
        1_700_001_000,
    ));
    scm.prs.push(labeled_pr_with_cache_key(
        602,
        "alice",
        "feature/pr-602",
        "sha-602-original",
        1_700_001_010,
    ));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();
    let mut cache = AdoptionProbeCache::new();

    // Tick 1: both PRs probed.
    let outcome1 = intake::normalize_labeled_prs_outcome(&scm, &tracker, &cfg, &mut cache, 1_700_000_000, &test_telemetry_log()).unwrap();
    assert_eq!(outcome1.adopted.len(), 2);
    scm.calls.borrow_mut().clear();

    // Contributor pushes a new commit to PR 601 only; PR 602 is unchanged.
    scm.prs[0].head_sha = Some("sha-601-NEWHEAD".into());
    scm.prs[0].updated_at_epoch = Some(1_700_002_000);

    // Tick 2: PR 601 re-probed, PR 602 served from cache.
    let outcome2 = intake::normalize_labeled_prs_outcome(&scm, &tracker, &cfg, &mut cache, 1_700_000_000, &test_telemetry_log()).unwrap();
    assert_eq!(outcome2.adopted.len(), 2);
    assert_eq!(outcome2.metrics.probe_cache_misses, 1, "only PR 601 missed");
    assert_eq!(outcome2.metrics.probe_cache_hits, 1, "only PR 602 hit");

    let tick2_calls = scm.calls.borrow();
    let tick2_perm_calls = tick2_calls
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .count();
    // We expect exactly 1 collaborator_permission call (PR 601 only).
    assert_eq!(
        tick2_perm_calls, 1,
        "tick 2 must probe ONLY the PR whose head_sha changed (PR 601), got: {:?}",
        tick2_calls.iter().collect::<Vec<_>>()
    );
}

/// r4 red test #4: cache invalidation on collaborator tier change. The r3
/// cache used only `(external_ref, head_sha, updated_at_epoch)` as the key,
/// which means a contributor promoted from `Read` → `Write` between ticks
/// would keep replaying the stale `SkippedIneligible` decision forever
/// (because PR commits/updated_at don't change on a tier promotion). r4
/// MUST re-probe when the upstream collaborator tier for the same
/// `external_ref`/`head_sha`/`updated_at` triple changes — either via a
/// TTL on cached `AuthorPermission` entries, or by detecting that the
/// latest probe tier differs from the cached one and treating it as a
/// miss.
///
/// We assert the minimal contract: when the contributor's tier changes
/// from Read to Write between tick 1 and tick 2 (with NO change to
/// `head_sha` or `updated_at_epoch`), tick 2 must re-probe the PR and
/// correctly adopt it instead of replaying the stale Read-tier skip.
#[test]
fn probe_cache_revalidates_when_collaborator_tier_changes() {
    let mut scm = FakeScm::new();
    scm.prs.push(labeled_pr_with_cache_key(
        701,
        "alice",
        "feature/pr-701",
        "sha-701",
        1_700_003_000,
    ));
    // Initial tier: Read — tick 1 should skip alice as ineligible.
    scm.permissions.insert("alice".into(), Permission::Read);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();
    let mut cache = AdoptionProbeCache::new();

    // Tick 1: alice is Read tier, PR 701 is ineligible.
    let outcome1 = intake::normalize_labeled_prs_outcome(&scm, &tracker, &cfg, &mut cache, 1_700_000_000, &test_telemetry_log()).unwrap();
    assert!(outcome1.adopted.is_empty());
    assert_eq!(outcome1.outcomes.len(), 1);
    assert!(matches!(
        outcome1.outcomes[0].verdict,
        IntakeVerdict::SkippedIneligible { .. }
    ));

    // Alice is promoted to Write — PR head_sha + updated_at unchanged.
    scm.permissions.insert("alice".into(), Permission::Write);
    scm.calls.borrow_mut().clear();

    // Tick 2: must detect the tier change and re-probe (no stale skip).
    // Advance now_epoch past MAX_CACHED_PERMISSION_AGE_SECS so the cached
    // AuthorPermission(Read) entry expires and the cache lookup misses,
    // forcing a fresh `collaborator_permission` probe that observes the
    // new Write tier.
    let outcome2 = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000 + intake::MAX_CACHED_PERMISSION_AGE_SECS + 1,
        &test_telemetry_log(),
    )
    .unwrap();
    assert_eq!(
        outcome2.adopted.len(),
        1,
        "tick 2 must adopt PR 701 after alice's promotion to Write; \
         got adopted={:?}, outcomes={:?}",
        outcome2.adopted, outcome2.outcomes
    );
    let tick2_perm_calls = scm
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .count();
    assert_eq!(
        tick2_perm_calls, 1,
        "tick 2 must re-probe the PR's permission tier after promotion, got: {:?}",
        scm.calls.borrow().iter().collect::<Vec<_>>()
    );
}

/// r4 unit test: the AdoptionProbeCache keying distinguishes between
/// (1) two PRs sharing an external_ref prefix but different head SHAs and
/// (2) the same PR across two ticks with different updated_at.
#[test]
fn probe_cache_keys_on_external_ref_head_sha_and_updated_at() {
    let mut cache = AdoptionProbeCache::new();

    let p1 = labeled_pr_with_cache_key(1, "alice", "feature/pr-1", "sha-a", 100);
    let p1_dup = labeled_pr_with_cache_key(1, "alice", "feature/pr-1", "sha-a", 100);
    let p1_new_sha = labeled_pr_with_cache_key(1, "alice", "feature/pr-1", "sha-b", 100);
    let p1_new_time = labeled_pr_with_cache_key(1, "alice", "feature/pr-1", "sha-a", 101);

    let k1 = ProbeCacheKey::from_pr(&p1);
    let k1_dup = ProbeCacheKey::from_pr(&p1_dup);
    let k1_new_sha = ProbeCacheKey::from_pr(&p1_new_sha);
    let k1_new_time = ProbeCacheKey::from_pr(&p1_new_time);

    // Identical keys collide (cache hit semantics).
    cache.insert(k1.clone(), CachedDecisionKind::AuthorPermission(Permission::Write), 1_700_000_000);
    assert!(cache.contains(&k1_dup), "identical keys must collide");

    // A different head_sha invalidates the cache entry.
    assert!(
        !cache.contains(&k1_new_sha),
        "head_sha change must produce a new key (cache miss)"
    );

    // A different updated_at invalidates the cache entry.
    assert!(
        !cache.contains(&k1_new_time),
        "updated_at change must produce a new key (cache miss)"
    );
}

// jtg8-r5 TDD red tests =====================================================
//
// r5 closes three remaining codex P2 review items left open on PR #455 (r4):
//   1. REST fallback (`labeled_prs_via_rest`) must populate `head_sha` from
//      `head.sha` (the real GitHub REST `/pulls/{n}` shape), not from a
//      non-existent top-level `head_sha` field. Test lives in
//      `adapters_integration.rs` because it's pure deserialization logic.
//   2. AdoptionProbeCache entries with INCOMPLETE keys (None for any of
//      `head_sha` / `updated_at_epoch`) must NOT be served as cache hits.
//      The r4 cache stores entries whose key has `None` fields, and
//      `contains_fresh` returns true on subsequent ticks; the r3 P2 review
//      "Avoid caching PRs without complete cache keys" called this out
//      because it silently turns an "uncacheable PR" into a stale-skip
//      forever. Fix: `cache.insert` must refuse incomplete keys; tests
//      below pin both halves (the API contract AND the integration path
//      through `normalize_labeled_prs_outcome`).
//   3. `IntakeProbeMetrics.gh_call_count` must count REAL `gh` subprocess
//      invocations including the REST fallback's per-PR `pulls/{n}` calls.
//      r4 only counted the list query (1); codex's P2 review "Count REST
//      fallback subprocesses in gh metrics" flagged this because the
//      slow-tier `gh_call_count >= INTAKE_GH_CALL_WARN_THRESHOLD` warning
//      could never fire under the REST path. Fix: plumb `&mut gh_calls`
//      through `Scm::labeled_prs` so each impl records its own invocations.

/// r5 red test #2a: `AdoptionProbeCache::insert` MUST refuse keys with
/// `None` fields. PR rows whose upstream `gh pr list` (or REST fallback)
/// payload lacks `head_sha` / `updated_at_epoch` are "uncacheable" per
/// the r4 docs — caching them would silently serve stale decisions on
/// the next tick even though we have no signal to invalidate them.
#[test]
fn adoption_probe_cache_refuses_incomplete_keys() {
    let mut cache = AdoptionProbeCache::new();
    let now = 1_700_000_000;

    // Complete key (Some, Some) — insert succeeds and is served back.
    let pr_complete = labeled_pr_with_cache_key(101, "alice", "feature/pr-101", "sha-complete", 1_700_000_000);
    let key_complete = ProbeCacheKey::from_pr(&pr_complete);
    cache.insert(
        key_complete.clone(),
        CachedDecisionKind::AuthorPermission(Permission::Write),
        now,
    );
    assert!(
        cache.contains_fresh(&key_complete, now),
        "complete-key insert must be a cache hit"
    );

    // Incomplete key: head_sha = None. Insert MUST be a no-op (the cache
    // entry stays empty) so the daemon falls through to a fresh probe
    // every tick instead of replaying a stale decision.
    let mut pr_no_sha = pr_complete.clone();
    pr_no_sha.head_sha = None;
    let key_no_sha = ProbeCacheKey::from_pr(&pr_no_sha);
    cache.insert(
        key_no_sha.clone(),
        CachedDecisionKind::AuthorPermission(Permission::Write),
        now,
    );
    assert!(
        !cache.contains(&key_no_sha),
        "head_sha=None cache insert must be a no-op (uncacheable PR)"
    );

    // Incomplete key: updated_at_epoch = None. Same gate.
    let mut pr_no_time = pr_complete.clone();
    pr_no_time.updated_at_epoch = None;
    let key_no_time = ProbeCacheKey::from_pr(&pr_no_time);
    cache.insert(
        key_no_time.clone(),
        CachedDecisionKind::AuthorPermission(Permission::Write),
        now,
    );
    assert!(
        !cache.contains(&key_no_time),
        "updated_at_epoch=None cache insert must be a no-op (uncacheable PR)"
    );

    // The complete-key entry is still served — refused inserts must NOT
    // evict valid entries.
    assert!(
        cache.contains_fresh(&key_complete, now),
        "refused inserts must not evict pre-existing complete-key entries"
    );
}

/// r5 red test #2b: integration path — a PR whose REST fallback populates
/// `head_sha = None` must NOT be served from cache on the second tick.
/// Without the r5 fix, the r4 code stores the decision under
/// `(external_ref, None, _)` and `contains_fresh` returns true, so tick 2
/// skips the probe even though we have no signal to invalidate the
/// decision.
#[test]
fn incomplete_key_pr_is_reprobed_every_tick() {
    let mut scm = FakeScm::new();
    // Script a PR with NO head_sha — simulating the r4 REST-fallback
    // bug where `RestPullExt` looks for a non-existent top-level field.
    let mut pr = labeled_pr_with_cache_key(901, "alice", "feature/pr-901", "sha-901", 1_700_000_000);
    pr.head_sha = None;
    pr.updated_at_epoch = Some(1_700_000_005);
    scm.prs.push(pr);
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();
    let mut cache = AdoptionProbeCache::new();

    // Tick 1: probes alice (cache miss).
    let _ = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();

    let tick1_perm_calls = scm
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .count();
    assert_eq!(
        tick1_perm_calls, 1,
        "tick 1 must probe alice's permission exactly once (incomplete key, no cache hit possible)"
    );

    // Tick 2: same PR list, same incomplete key. r4 cached the decision
    // under (external_ref, None, _), so a probe-skip would silently
    // replay the stale Read tier (or whatever was first cached). The r5
    // fix: refuse to cache incomplete keys, so tick 2 MUST re-probe.
    scm.calls.borrow_mut().clear();
    let _ = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();

    let tick2_perm_calls = scm
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("collaborator_permission("))
        .count();
    assert_eq!(
        tick2_perm_calls, 1,
        "tick 2 must re-probe alice's permission because the cache key is incomplete (head_sha=None)"
    );
}

/// r5 red test #3: `IntakeProbeMetrics.gh_call_count` must reflect REAL
/// `gh` subprocess invocations including the per-PR REST fallback calls.
/// The r4 design only counts the list query (1), so the
/// `INTAKE_GH_CALL_WARN_THRESHOLD` warning never fires when `labeled_prs`
/// falls back to `labeled_prs_via_rest` and burns N+1 core API calls per
/// tick. This test pins the contract via the `Scm` trait plumbing — the
/// `&mut gh_calls` parameter is incremented by every `run_tool` (or
/// equivalent fake) inside `labeled_prs`, so the intake-side metric
/// matches what actually crossed the daemon's tool boundary.
///
/// `FakeScm` doesn't shell out, so its `labeled_prs` increments the
/// counter exactly once. The test simulates the REST fallback by having
/// `FakeScm::labeled_prs` route through a scripted multi-call path that
/// records (list query + 2 per-PR calls) into the counter — proving the
/// metric reflects real subprocess invocations, not just a constant `1`.
#[test]
fn intake_metrics_gh_call_count_counts_real_subprocesses() {
    use std::cell::Cell;

    /// Scm impl that mimics the REST-fallback path: one `labeled_prs`
    /// call results in multiple "subprocess" calls (the list query +
    /// one `pulls/{n}` per PR). The count is reported via `&mut gh_calls`.
    struct RestFallbackScm {
        #[allow(dead_code)]
        gh_calls: Cell<u32>,
        recorded_calls: std::cell::RefCell<Vec<String>>,
    }
    impl RestFallbackScm {
        fn new() -> Self {
            Self {
                gh_calls: Cell::new(0),
                recorded_calls: std::cell::RefCell::new(Vec::new()),
            }
        }
    }
    impl Scm for RestFallbackScm {
        fn labeled_issues(&self, _label: &str) -> Result<Vec<Issue>, DaemonError> {
            Ok(Vec::new())
        }
        fn labeled_prs(
            &self,
            label: &str,
            gh_calls: &mut u32,
        ) -> Result<Vec<LabeledPr>, DaemonError> {
            // Mirrors the real REST fallback: 1 list query + 2 per-PR pulls.
            *gh_calls += 1;
            self.recorded_calls
                .borrow_mut()
                .push(format!("labeled_prs({label})"));
            *gh_calls += 1;
            self.recorded_calls.borrow_mut().push("pulls/1".into());
            *gh_calls += 1;
            self.recorded_calls.borrow_mut().push("pulls/2".into());
            Ok(Vec::new())
        }
        fn collaborator_permission(
            &self,
            _login: &str,
        ) -> Result<Permission, DaemonError> {
            Ok(Permission::Write)
        }
        fn pr_snapshot(&self, _pr: u64) -> Result<PrSnapshot, DaemonError> {
            unimplemented!()
        }
        fn close_pr(&self, _pr: u64, _comment: &str) -> Result<(), DaemonError> {
            unimplemented!()
        }
        fn remote_branch_last_commit(&self, _branch: &str) -> Result<Option<u64>, DaemonError> {
            unimplemented!()
        }
    }

    let scm = RestFallbackScm::new();
    let tracker = FakeTracker::new();
    let cfg = test_cfg();
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();

    // The slow-tier metric MUST reflect the 3 real subprocess invocations
    // (1 list + 2 per-PR pulls), not the r4 behavior of reporting `1`.
    assert_eq!(
        outcome.metrics.gh_call_count, 3,
        "gh_call_count must equal the number of real subprocess invocations, \
         including REST fallback per-PR pulls; got {}",
        outcome.metrics.gh_call_count
    );
}

// =============================================================================
// Multi-repo intake RED regression tests (bead dark-factory-9x69)
// =============================================================================

#[test]
fn two_repositories_sharing_a_pr_number() {
    let mut scm = FakeScm::new();
    let mut pr_a = labeled_pr_with_cache_key(100, "alice", "feature/pr-100-a", "sha-100-a", 1_700_000_000);
    pr_a.external_ref = "jleechanorg/dark-factory#100".into();
    pr_a.head_repo_full_name = Some("jleechanorg/dark-factory".into());
    pr_a.head_repo_owner_login = Some("jleechanorg".into());

    let mut pr_b = labeled_pr_with_cache_key(100, "alice", "feature/pr-100-b", "sha-100-b", 1_700_000_000);
    pr_b.external_ref = "jleechanorg/worldarchitect.ai#100".into();
    pr_b.head_repo_full_name = Some("jleechanorg/worldarchitect.ai".into());
    pr_b.head_repo_owner_login = Some("jleechanorg".into());

    scm.prs.push(pr_a);
    scm.prs.push(pr_b);
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "worldai".into(),
            local_checkout: None,
        },
    );

    let mut cache = AdoptionProbeCache::new();
    let outcome = intake::normalize_labeled_prs_outcome(&scm, &tracker, &cfg, &mut cache, 1_700_000_000, &test_telemetry_log()).unwrap();

    assert_eq!(outcome.adopted.len(), 2, "must adopt PR 100 from both repos without colliding: {:?}", outcome.adopted);
    let refs: Vec<_> = outcome.adopted.iter().map(|a| a.external_ref.as_str()).collect();
    assert!(refs.contains(&"jleechanorg/dark-factory#100"));
    assert!(refs.contains(&"jleechanorg/worldarchitect.ai#100"));
}

#[test]
fn one_repository_failing_while_another_succeeds() {
    struct FailingRepoScm {
        inner: FakeScm,
    }
    impl Scm for FailingRepoScm {
        fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError> {
            self.inner.labeled_issues(label)
        }
        fn labeled_prs(&self, label: &str, gh_calls: &mut u32) -> Result<Vec<LabeledPr>, DaemonError> {
            self.inner.labeled_prs(label, gh_calls)
        }
        fn labeled_prs_for_repo(
            &self,
            repo: &str,
            label: &str,
            gh_calls: &mut u32,
        ) -> Result<Vec<LabeledPr>, DaemonError> {
            if repo == "jleechanorg/failing-repo" {
                *gh_calls += 1;
                return Err(DaemonError::Tool {
                    tool: "gh".into(),
                    rc: 1,
                    stderr: "gh: API rate limit exceeded".into(),
                });
            }
            self.inner.labeled_prs_for_repo(repo, label, gh_calls)
        }
        fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError> {
            self.inner.collaborator_permission(login)
        }
        fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
            self.inner.pr_snapshot(pr)
        }
        fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError> {
            self.inner.close_pr(pr, comment)
        }
        fn remote_branch_last_commit(&self, branch: &str) -> Result<Option<u64>, DaemonError> {
            self.inner.remote_branch_last_commit(branch)
        }
    }

    let mut inner = FakeScm::new();
    let mut pr = labeled_pr_with_cache_key(50, "alice", "feature/pr-50", "sha-50", 1_700_000_000);
    pr.external_ref = "jleechanorg/dark-factory#50".into();
    pr.head_repo_full_name = Some("jleechanorg/dark-factory".into());
    inner.prs.push(pr);
    inner.permissions.insert("alice".into(), Permission::Write);

    let scm = FailingRepoScm { inner };
    let tracker = FakeTracker::new();
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/failing-repo".into(),
        daemon::config::RepoConfig {
            ao_project: "failing".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );

    let mut cache = AdoptionProbeCache::new();
    let outcome = intake::normalize_labeled_prs_outcome(&scm, &tracker, &cfg, &mut cache, 1_700_000_000, &test_telemetry_log()).unwrap();

    assert_eq!(outcome.adopted.len(), 1, "must preserve successful repo dark-factory results despite failing-repo error");
    assert_eq!(outcome.adopted[0].external_ref, "jleechanorg/dark-factory#50");
}

/// PR #629 follow-up fix (finding 2): pre-fix, `normalize_labeled_prs_with_cache`
/// called `tracker.fetch_candidates()?`/`tracker.fetch_all_external_refs()?`
/// INSIDE the per-repo loop — once per repo — even though the tracker is one
/// global beads store whose snapshot is identical every time within a tick.
/// This double reproduces that redundant-refetch shape: `fetch_candidates`
/// succeeds on its first invocation (repo 1's processing) and fails on its
/// second (what pre-fix code treated as repo 2's re-fetch). Pre-fix, that
/// second failure propagated via `?` through the ENTIRE multi-repo sweep,
/// discarding repo 1's already-accumulated adoption — directly contradicting
/// this function's own fail-soft, per-repo-isolation contract (the same
/// contract `one_repository_failing_while_another_succeeds` above already
/// proves for a raw SCM error). Post-fix, the tracker snapshot is fetched
/// exactly ONCE, before the loop starts, so this double's second invocation
/// is never reached and both repos' results survive.
#[test]
fn tracker_fetch_failure_isolated_to_second_repo_preserves_first_repos_adoption() {
    struct FailingSecondFetchTracker {
        inner: FakeTracker,
        fetch_candidates_calls: std::cell::Cell<u32>,
    }
    impl Tracker for FailingSecondFetchTracker {
        fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
            let call_number = self.fetch_candidates_calls.get() + 1;
            self.fetch_candidates_calls.set(call_number);
            if call_number >= 2 {
                return Err(DaemonError::Tool {
                    tool: "br".into(),
                    rc: 1,
                    stderr: "br: list beads: connection refused".into(),
                });
            }
            self.inner.fetch_candidates()
        }
        fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError> {
            self.inner.fetch_all_external_refs()
        }
        fn create_bead(
            &self,
            title: &str,
            body: &str,
            external_ref: &str,
        ) -> Result<String, DaemonError> {
            self.inner.create_bead(title, body, external_ref)
        }
        fn comment_external(&self, external_ref: &str, body: &str) -> Result<(), DaemonError> {
            self.inner.comment_external(external_ref, body)
        }
    }

    let mut scm = FakeScm::new();
    let mut pr_a = labeled_pr_with_cache_key(60, "alice", "feature/pr-60", "sha-60", 1_700_000_000);
    pr_a.external_ref = "jleechanorg/dark-factory#60".into();
    pr_a.head_repo_full_name = Some("jleechanorg/dark-factory".into());

    let mut pr_b = labeled_pr_with_cache_key(61, "alice", "feature/pr-61", "sha-61", 1_700_000_000);
    pr_b.external_ref = "jleechanorg/worldarchitect.ai#61".into();
    pr_b.head_repo_full_name = Some("jleechanorg/worldarchitect.ai".into());

    scm.prs.push(pr_a);
    scm.prs.push(pr_b);
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FailingSecondFetchTracker {
        inner: FakeTracker::new(),
        fetch_candidates_calls: std::cell::Cell::new(0),
    };
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "worldai".into(),
            local_checkout: None,
        },
    );

    let mut cache = AdoptionProbeCache::new();
    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    );

    let outcome = outcome.expect(
        "a tracker error isolated to what pre-fix code treated as one repo's \
         redundant re-fetch must not abort the whole multi-repo sweep and \
         discard the prior repo's already-accumulated adoption",
    );
    let refs: Vec<_> = outcome
        .adopted
        .iter()
        .map(|a| a.external_ref.as_str())
        .collect();
    assert!(
        refs.contains(&"jleechanorg/dark-factory#60"),
        "repo 1's adoption must be retained even though the tracker snapshot \
         fetch failed on what pre-fix code treated as repo 2's re-fetch; got \
         adopted={:?}",
        outcome.adopted
    );
}

/// Tracker double for the round-3 lazy/memoized snapshot tests: counts
/// `fetch_candidates` invocations and, when `should_fail` is set, always
/// fails (simulating a malformed/unavailable closed-bead listing) so the
/// tests can assert BOTH "never called" and "called exactly once".
struct CountingTracker {
    inner: FakeTracker,
    fetch_candidates_calls: std::cell::Cell<u32>,
    should_fail: bool,
}
impl CountingTracker {
    fn new(should_fail: bool) -> Self {
        Self {
            inner: FakeTracker::new(),
            fetch_candidates_calls: std::cell::Cell::new(0),
            should_fail,
        }
    }
}
impl Tracker for CountingTracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
        self.fetch_candidates_calls.set(self.fetch_candidates_calls.get() + 1);
        if self.should_fail {
            return Err(DaemonError::Tool {
                tool: "br".into(),
                rc: 1,
                stderr: "br: list beads: malformed closed-bead listing".into(),
            });
        }
        self.inner.fetch_candidates()
    }
    fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError> {
        if self.should_fail {
            return Err(DaemonError::Tool {
                tool: "br".into(),
                rc: 1,
                stderr: "br: list beads: malformed closed-bead listing".into(),
            });
        }
        self.inner.fetch_all_external_refs()
    }
    fn create_bead(&self, title: &str, body: &str, external_ref: &str) -> Result<String, DaemonError> {
        self.inner.create_bead(title, body, external_ref)
    }
    fn comment_external(&self, external_ref: &str, body: &str) -> Result<(), DaemonError> {
        self.inner.comment_external(external_ref, body)
    }
}

/// PR #629 follow-up fix (round 3, codex P1): pre-fix, the tracker
/// snapshot (`fetch_candidates` + `fetch_all_external_refs`) was fetched
/// UNCONDITIONALLY, before the per-repo loop even started -- regardless
/// of whether any repo actually had a PR batch to process. This test
/// scripts every target repo to return an EMPTY PR list (the SCM side is
/// healthy, there is simply nothing to intake this tick) with a tracker
/// double that would error if ever called, and asserts the tracker is
/// NEVER touched. Run against pre-fix (unconditional-fetch) code, this
/// fails: `fetch_candidates_calls` is 1 even though no repo needed it, and
/// (worse) the whole sweep would have returned `Err` and aborted
/// `run_slow_tier` before issue intake / dispatch ever ran.
#[test]
fn tracker_never_touched_when_no_repo_has_a_pr_batch() {
    let scm = FakeScm::new(); // no scm.prs pushed -> every repo's labeled_prs_for_repo returns empty.
    let tracker = CountingTracker::new(/* should_fail = */ true);
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "worldai".into(),
            local_checkout: None,
        },
    );
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    );

    assert!(
        outcome.is_ok(),
        "a sweep where every repo returns zero PRs must succeed even when the \
         tracker is completely broken -- the tracker is never needed this tick: {outcome:?}"
    );
    assert_eq!(
        tracker.fetch_candidates_calls.get(),
        0,
        "the tracker snapshot must be LAZY: it must never be fetched when no \
         repo's PR batch actually needs it"
    );
}

/// PR #629 follow-up fix (round 3, codex P1): the tracker snapshot must
/// be MEMOIZED -- fetched at most once per sweep even when multiple repos
/// each have a non-empty PR batch that needs it. Two repos, both with a
/// real PR, tracker healthy: `fetch_candidates` must be called exactly
/// once (repo 1's batch triggers the fetch; repo 2's batch reuses the
/// memoized result). Run against pre-fix code, `fetch_candidates` is
/// still called exactly once here too (pre-fix already fetched upfront,
/// unconditionally) -- this test's job is to PROTECT that "at most once"
/// property across the lazy-fetch refactor, not to prove a regression by
/// itself.
#[test]
fn tracker_snapshot_fetched_at_most_once_across_two_repos_with_prs() {
    let mut scm = FakeScm::new();
    let mut pr_a = labeled_pr_with_cache_key(70, "alice", "feature/pr-70", "sha-70", 1_700_000_000);
    pr_a.external_ref = "jleechanorg/dark-factory#70".into();
    pr_a.head_repo_full_name = Some("jleechanorg/dark-factory".into());
    let mut pr_b = labeled_pr_with_cache_key(71, "alice", "feature/pr-71", "sha-71", 1_700_000_000);
    pr_b.external_ref = "jleechanorg/worldarchitect.ai#71".into();
    pr_b.head_repo_full_name = Some("jleechanorg/worldarchitect.ai".into());
    scm.prs.push(pr_a);
    scm.prs.push(pr_b);
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = CountingTracker::new(/* should_fail = */ false);
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "worldai".into(),
            local_checkout: None,
        },
    );
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();

    assert_eq!(
        tracker.fetch_candidates_calls.get(),
        1,
        "the tracker snapshot must be fetched exactly ONCE across a sweep, no \
         matter how many repos' PR batches need it"
    );
    assert_eq!(
        outcome.adopted.len(),
        2,
        "both repos' PRs must still be adopted using the shared, memoized \
         snapshot: {:?}",
        outcome.adopted
    );
}

/// PR #629 follow-up fix (round 3, codex P1, the headline fix): when the
/// tracker snapshot fetch itself FAILS for a repo that has a real PR
/// batch, `normalize_labeled_prs_outcome` must DEGRADE (skip that repo's
/// PR intake, emit structured `INTAKE_REPO_SWEEP_FAILED`
/// `error_class=tracker_snapshot` telemetry) rather than propagate the
/// error and abort the whole call. The caller, `run_slow_tier` (tick.rs),
/// uses `?` on this function's return value -- an `Err` here would abort
/// issue intake AND dispatch for the entire tick on a failure that has
/// nothing to do with either. Run against pre-fix (round-2) code, this
/// test's `outcome.is_ok()` assertion fails: the unconditional upfront
/// fetch propagates the tracker error via `?` before the per-repo loop
/// even starts.
#[test]
fn tracker_snapshot_failure_degrades_without_aborting_the_sweep() {
    let mut scm = FakeScm::new();
    let mut pr = labeled_pr_with_cache_key(80, "alice", "feature/pr-80", "sha-80", 1_700_000_000);
    pr.external_ref = "jleechanorg/dark-factory#80".into();
    pr.head_repo_full_name = Some("jleechanorg/dark-factory".into());
    scm.prs.push(pr);
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = CountingTracker::new(/* should_fail = */ true);
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    let mut cache = AdoptionProbeCache::new();
    let telemetry_log = test_telemetry_log();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &telemetry_log,
    );

    let outcome = outcome.unwrap_or_else(|e| {
        panic!(
            "a tracker snapshot fetch failure must DEGRADE this sweep, not abort \
             it -- run_slow_tier (tick.rs) uses `?` on this return value, so an \
             Err here would starve issue intake + dispatch on a failure \
             unrelated to either: {e:?}"
        )
    });
    assert!(
        outcome.adopted.is_empty(),
        "the PR whose repo needed the (failed) tracker snapshot must not be \
         adopted this tick: {:?}",
        outcome.adopted
    );

    let telemetry_body = std::fs::read_to_string(&telemetry_log)
        .expect("emit_intake_repo_sweep_failed must have written the telemetry log");
    assert!(
        telemetry_body.contains("INTAKE_REPO_SWEEP_FAILED") && telemetry_body.contains("tracker_snapshot"),
        "a structured INTAKE_REPO_SWEEP_FAILED (error_class=tracker_snapshot) \
         event must be emitted, not just an eprintln!; got: {telemetry_body}"
    );
}

#[test]
fn no_duplicate_replay_from_default_fake_adapter() {
    let mut scm = FakeScm::new();
    let mut pr = labeled_pr_with_cache_key(123, "alice", "feature/pr-123", "sha-123", 1_700_000_000);
    pr.external_ref = "jleechanorg/dark-factory#123".into();
    scm.prs.push(pr);

    let mut calls = 0;
    let prs = scm.labeled_prs_for_repo("jleechanorg/worldarchitect.ai", "factory", &mut calls).unwrap();
    assert!(prs.is_empty(), "labeled_prs_for_repo for worldarchitect.ai must not replay dark-factory PRs: {:?}", prs);
}

#[test]
fn deterministic_bounded_repository_order() {
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    for i in (0..15).rev() {
        cfg.repos.insert(
            format!("jleechanorg/repo-{:02}", i),
            daemon::config::RepoConfig {
                ao_project: format!("proj-{i}"),
                push_remote: "origin".into(),
                local_checkout: None,
            },
        );
    }

    // now_epoch=0 -> rotation offset (0 / slow_tick_secs) % secondary_len == 0,
    // i.e. "tick zero, no rotation has happened yet" -- deliberately chosen
    // so this test's exact-position assertions describe the un-rotated base
    // case; `secondary_repo_window_rotates_across_ticks_for_full_coverage`
    // below proves the rotation itself.
    let repos = intake::target_repositories_sweep_order(&cfg, 0);
    assert_eq!(repos[0], "jleechanorg/dark-factory", "target_repo must always be scanned first");
    assert_eq!(repos[1], "jleechanorg/repo-00");
    assert_eq!(repos[2], "jleechanorg/repo-01");
    assert_eq!(repos[3], "jleechanorg/repo-02");
    assert!(repos.len() <= intake::MAX_INTAKE_REPOS_PER_SWEEP, "sweep repo count must be bounded");
}

/// PR #629 follow-up fix (codex P1 "Rotate repositories instead of
/// permanently truncating them"): pre-fix, `target_repositories_sweep_order`
/// always kept `target_repo` plus the alphabetically-first
/// `MAX_INTAKE_REPOS_PER_SWEEP - 1` secondary repos and discarded the rest
/// FOREVER, on every tick, with no cursor or rotation state anywhere. This
/// test drives 15 secondary repos (5 more than the cap allows in one
/// sweep) across successive slow-tier ticks (`now_epoch` advancing by
/// `slow_tick_secs` each time, exactly as `run_slow_tier` does in
/// production) and asserts every configured secondary repo appears in the
/// UNION of scanned repos across those ticks — i.e. no repo is left behind
/// forever. Run against pre-rotation code, this fails: repos 09-14 never
/// appear in the union no matter how many ticks are simulated.
#[test]
fn secondary_repo_window_rotates_across_ticks_for_full_coverage() {
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.slow_tick_secs = 600;
    let expected_secondary: Vec<String> = (0..15).map(|i| format!("jleechanorg/repo-{i:02}")).collect();
    for (i, repo) in expected_secondary.iter().enumerate() {
        cfg.repos.insert(
            repo.clone(),
            daemon::config::RepoConfig {
                ao_project: format!("proj-{i}"),
                push_remote: "origin".into(),
                local_checkout: None,
            },
        );
    }

    let mut union: std::collections::HashSet<String> = std::collections::HashSet::new();
    // 15 secondary repos -> full coverage is guaranteed within 15 rotation
    // steps; simulate one extra tick as headroom.
    for tick in 0..16u64 {
        let now_epoch = tick * cfg.slow_tick_secs;
        let repos = intake::target_repositories_sweep_order(&cfg, now_epoch);
        assert_eq!(
            repos[0], "jleechanorg/dark-factory",
            "target_repo must scan every tick regardless of rotation (tick {tick})"
        );
        assert!(
            repos.len() <= intake::MAX_INTAKE_REPOS_PER_SWEEP,
            "sweep repo count must stay bounded every tick (tick {tick}): {repos:?}"
        );
        union.extend(repos.into_iter().skip(1));
    }

    for repo in &expected_secondary {
        assert!(
            union.contains(repo),
            "{repo} must be scanned at least once across successive rotating \
             sweeps -- a repo permanently missing from the union means \
             rotation isn't actually cycling through the full configured set: \
             union={union:?}"
        );
    }
}

/// PR #629 follow-up fix (codex P2 "Deduplicate repository names
/// case-insensitively" + CodeRabbit convergent finding): `cfg.repos`
/// holding the target repository under a different ASCII case must be
/// treated as the SAME repository, not scanned twice.
#[test]
fn target_repo_case_variant_in_repos_map_is_deduped() {
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/Dark-Factory".into();
    cfg.repos.insert(
        // Same repo, different ASCII case, as could happen from an
        // operator hand-editing config.yaml.
        "jleechanorg/dark-factory".into(),
        daemon::config::RepoConfig {
            ao_project: "dup".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "worldai".into(),
            local_checkout: None,
        },
    );

    let repos = intake::target_repositories_sweep_order(&cfg, 0);
    assert_eq!(
        repos.len(),
        2,
        "the case-variant of target_repo must be deduped, leaving exactly \
         target_repo + worldarchitect.ai: {repos:?}"
    );
    let lower: Vec<String> = repos.iter().map(|r| r.to_ascii_lowercase()).collect();
    assert_eq!(
        lower.iter().filter(|r| r.as_str() == "jleechanorg/dark-factory").count(),
        1,
        "dark-factory must appear exactly once regardless of casing: {repos:?}"
    );
}

/// PR #629 follow-up fix (codex P2 "Enforce the call cap within each
/// repository scan"): the sweep-wide `MAX_INTAKE_SWEEP_GH_CALLS` budget
/// was only checked BETWEEN repos in the outer sweep loop
/// (`normalize_labeled_prs_outcome`) -- a single repo with many labeled
/// PRs requiring fresh (cache-miss) permission probes could blow straight
/// through the cap before the outer loop ever got a chance to check
/// again. This test scripts 105 labeled PRs in ONE repo, each with a
/// distinct cache key (head_sha/updated_at) so every one is a genuine
/// cache miss requiring a real probe call, and asserts
/// `gh_call_count` never exceeds the cap. Run against pre-fix code (no
/// budget check inside the per-PR loop), `gh_call_count` overshoots to
/// 106 (1 list call + 105 probes) and this assertion fails.
#[test]
fn per_repo_probe_loop_stops_at_sweep_wide_call_cap() {
    let mut scm = FakeScm::new();
    let total_prs: u64 = 105;
    for number in 1..=total_prs {
        let mut pr = labeled_pr_with_cache_key(
            number,
            "alice",
            &format!("feature/pr-{number}"),
            &format!("sha-{number}"),
            1_700_000_000 + number,
        );
        pr.external_ref = format!("owner/repo#{number}");
        scm.prs.push(pr);
    }
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg(); // target_repo = "owner/repo", no secondary repos.
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();

    assert!(
        outcome.metrics.gh_call_count <= intake::MAX_INTAKE_SWEEP_GH_CALLS,
        "gh_call_count must never exceed the sweep-wide cap even mid-repo-scan; \
         got {} (cap={})",
        outcome.metrics.gh_call_count,
        intake::MAX_INTAKE_SWEEP_GH_CALLS
    );
    assert!(
        outcome.adopted.len() < total_prs as usize,
        "with a 105-PR single-repo batch and a cap of {}, some PRs must be left \
         unprocessed this tick (proving the loop actually stopped early, not \
         just that the final count happens to be under the cap): adopted={}",
        intake::MAX_INTAKE_SWEEP_GH_CALLS,
        outcome.adopted.len()
    );
}

#[test]
fn non_default_repository_labeled_pr_adoption_attribution() {
    let mut scm = FakeScm::new();
    let mut pr = labeled_pr_with_cache_key(
        8843,
        "jleechan2015",
        "fix/rewards-xp-anchor-followup",
        "9dc2c198a445450d8fe455e7d691a0492deefe2e",
        1_700_000_000,
    );
    pr.external_ref = "jleechanorg/worldarchitect.ai#8843".into();
    pr.head_repo_full_name = Some("jleechanorg/worldarchitect.ai".into());
    pr.head_repo_owner_login = Some("jleechanorg".into());
    scm.prs.push(pr);
    scm.permissions.insert("jleechan2015".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();

    assert_eq!(outcome.adopted.len(), 1);
    let adopted = &outcome.adopted[0];
    assert_eq!(adopted.repo, "jleechanorg/worldarchitect.ai");
    assert_eq!(adopted.pr_number, 8843);
    assert_eq!(adopted.head_ref_name, "fix/rewards-xp-anchor-followup");
    assert_eq!(
        adopted.head_sha.as_deref(),
        Some("9dc2c198a445450d8fe455e7d691a0492deefe2e")
    );
    assert_eq!(adopted.external_ref, "jleechanorg/worldarchitect.ai#8843");
    assert!(adopted.newly_created);
}

#[test]
fn non_default_repository_blocked_dispositions_attribution() {
    let mut scm = FakeScm::new();

    // 1. Fork PR
    let mut fork_pr = labeled_pr_with_cache_key(
        8001,
        "mallory",
        "feature/fork-branch",
        "sha-fork-8001",
        1_700_000_000,
    );
    fork_pr.external_ref = "jleechanorg/worldarchitect.ai#8001".into();
    fork_pr.is_cross_repository = true;
    fork_pr.head_repo_full_name = Some("mallory/worldarchitect.ai".into());
    scm.prs.push(fork_pr);
    scm.permissions.insert("mallory".into(), Permission::Write);

    // 2. Ineligible (Read-tier permission)
    let mut read_pr = labeled_pr_with_cache_key(
        8002,
        "bob-read",
        "feature/read-branch",
        "sha-read-8002",
        1_700_000_000,
    );
    read_pr.external_ref = "jleechanorg/worldarchitect.ai#8002".into();
    read_pr.head_repo_full_name = Some("jleechanorg/worldarchitect.ai".into());
    scm.prs.push(read_pr);
    scm.permissions.insert("bob-read".into(), Permission::Read);

    // 3. Ineligible (Empty head ref)
    let mut empty_ref_pr = labeled_pr_with_cache_key(
        8003,
        "alice",
        "   ",
        "sha-empty-8003",
        1_700_000_000,
    );
    empty_ref_pr.external_ref = "jleechanorg/worldarchitect.ai#8003".into();
    empty_ref_pr.head_repo_full_name = Some("jleechanorg/worldarchitect.ai".into());
    scm.prs.push(empty_ref_pr);
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let mut cfg = test_cfg();
    cfg.target_repo = "jleechanorg/dark-factory".into();
    cfg.repos.insert(
        "jleechanorg/worldarchitect.ai".into(),
        daemon::config::RepoConfig {
            ao_project: "worldarchitect".into(),
            push_remote: "origin".into(),
            local_checkout: None,
        },
    );
    let mut cache = AdoptionProbeCache::new();

    let outcome = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &test_telemetry_log(),
    )
    .unwrap();

    assert_eq!(outcome.adopted.len(), 0);
    assert_eq!(outcome.outcomes.len(), 3);

    let fork_outcome = outcome
        .outcomes
        .iter()
        .find(|o| o.external_ref == "jleechanorg/worldarchitect.ai#8001")
        .unwrap();
    assert_eq!(fork_outcome.verdict, IntakeVerdict::SkippedFork);
    assert_eq!(
        fork_outcome.repo.as_deref(),
        Some("jleechanorg/worldarchitect.ai")
    );
    assert_eq!(fork_outcome.pr_number, Some(8001));
    assert_eq!(fork_outcome.branch.as_deref(), Some("feature/fork-branch"));
    assert_eq!(fork_outcome.head_sha.as_deref(), Some("sha-fork-8001"));

    let read_outcome = outcome
        .outcomes
        .iter()
        .find(|o| o.external_ref == "jleechanorg/worldarchitect.ai#8002")
        .unwrap();
    assert!(matches!(
        read_outcome.verdict,
        IntakeVerdict::SkippedIneligible { .. }
    ));
    assert_eq!(
        read_outcome.repo.as_deref(),
        Some("jleechanorg/worldarchitect.ai")
    );
    assert_eq!(read_outcome.pr_number, Some(8002));
    assert_eq!(read_outcome.branch.as_deref(), Some("feature/read-branch"));
    assert_eq!(read_outcome.head_sha.as_deref(), Some("sha-read-8002"));

    let empty_outcome = outcome
        .outcomes
        .iter()
        .find(|o| o.external_ref == "jleechanorg/worldarchitect.ai#8003")
        .unwrap();
    assert_eq!(
        empty_outcome.verdict,
        IntakeVerdict::SkippedIneligible {
            precondition: "empty_head_ref_name".into()
        }
    );
    assert_eq!(
        empty_outcome.repo.as_deref(),
        Some("jleechanorg/worldarchitect.ai")
    );
    assert_eq!(empty_outcome.pr_number, Some(8003));
    assert_eq!(empty_outcome.head_sha.as_deref(), Some("sha-empty-8003"));
}
