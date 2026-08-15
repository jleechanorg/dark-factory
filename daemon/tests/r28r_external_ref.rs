// Regression coverage for bead jleechan-r28r: duplicate beads with malformed
// `external_ref` causing high-volume ESCALATION_NOTIFICATION_FAILED thrashing.
//
// Three concerns, all anchored on the same root cause (URL-form vs canonical
// `owner/repo#N` divergence in stored `external_ref` data):
//
//   Fix A — URL-form `external_ref` MUST be normalized to the canonical
//            `owner/repo#N` short form AT INTAKE so `br`'s string-equal
//            uniqueness constraint catches every later duplicate that
//            points at the same PR/issue.
//
//   Fix B — once intake normalizes, the `known_refs` bulk pre-check catches
//            a repeat labeled issue/PR for the same canonical ref and
//            emits `SkippedDuplicate` without re-calling `create_bead`.
//
//   Fix C — the write-time race (a concurrent/stale `known_refs` snapshot
//            misses an existing ref) is still recovered by treating
//            `br create`'s own "already exists" failure as `SkippedDuplicate`
//            rather than a tick-killing transient error.
//
// RED proof (pre-fix, against current main): tests below fail because
// `intake::normalize`/`normalize_labeled_prs` pass URL-form refs straight
// through to `tracker.create_bead`. The intake caller stores them with
// whatever raw string `gh pr/issue list` produced — which is the
// full GitHub URL when `gh` emits URLs, or short form when it doesn't.
// Without canonicalization, `br`'s uniqueness check sees the two shapes
// as distinct strings and lets the duplicate through.
//
// GREEN proof (post-fix): the intake path normalizes before the
// `known_refs.contains` check AND before `tracker.create_bead`, so the
// dedup contract holds for both URL and short forms.

#[path = "common/mod.rs"]
mod common;

use common::{FakeScm, FakeTracker};
use daemon::config::Config;
use daemon::intake::{self, IntakeVerdict};
use daemon::tools::{Issue, LabeledPr, Permission, Scm};

/// `FakeScm` (in `daemon/tests/common`) uses the trait default impl for
/// `labeled_prs_for_repo`, which filters via `parse_external_ref_repo` —
/// a STRICT `owner/repo#N` parser in `tools.rs` that rejects URL-form refs.
/// For r28r we MUST be able to feed URL-form refs through the intake sweep
/// (that's exactly the bug class being fixed), so this thin wrapper just
/// returns every PR unfiltered when `labeled_prs_for_repo` is called. Tests
/// that don't exercise URL-form normalization should keep using `FakeScm`
/// directly.
struct UnfilteredFakeScm {
    inner: FakeScm,
}

impl UnfilteredFakeScm {
    fn new() -> Self {
        Self {
            inner: FakeScm::new(),
        }
    }
}

impl Scm for UnfilteredFakeScm {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, daemon::errors::DaemonError> {
        self.inner.labeled_issues(label)
    }

    fn labeled_prs(
        &self,
        label: &str,
        gh_calls: &mut u32,
    ) -> Result<Vec<LabeledPr>, daemon::errors::DaemonError> {
        self.inner.labeled_prs(label, gh_calls)
    }

    /// Override the trait default — return every PR (including URL-form
    /// refs that the strict `parse_external_ref_repo` would otherwise drop).
    fn labeled_prs_for_repo(
        &self,
        _repo: &str,
        label: &str,
        gh_calls: &mut u32,
    ) -> Result<Vec<LabeledPr>, daemon::errors::DaemonError> {
        self.inner.labeled_prs(label, gh_calls)
    }

    fn collaborator_permission(
        &self,
        login: &str,
    ) -> Result<Permission, daemon::errors::DaemonError> {
        self.inner.collaborator_permission(login)
    }

    fn collaborator_permission_for_repo(
        &self,
        repo: &str,
        login: &str,
    ) -> Result<Permission, daemon::errors::DaemonError> {
        self.inner.collaborator_permission_for_repo(repo, login)
    }

    fn pr_snapshot(
        &self,
        pr: u64,
    ) -> Result<daemon::tools::PrSnapshot, daemon::errors::DaemonError> {
        self.inner.pr_snapshot(pr)
    }

    fn pr_snapshot_for_repo(
        &self,
        repo: &str,
        pr: u64,
    ) -> Result<daemon::tools::PrSnapshot, daemon::errors::DaemonError> {
        self.inner.pr_snapshot_for_repo(repo, pr)
    }

    fn close_pr(
        &self,
        pr: u64,
        comment: &str,
    ) -> Result<(), daemon::errors::DaemonError> {
        self.inner.close_pr(pr, comment)
    }

    fn close_pr_for_repo(
        &self,
        repo: &str,
        pr: u64,
        comment: &str,
    ) -> Result<(), daemon::errors::DaemonError> {
        self.inner.close_pr_for_repo(repo, pr, comment)
    }

    fn remote_branch_last_commit(
        &self,
        branch: &str,
    ) -> Result<Option<u64>, daemon::errors::DaemonError> {
        self.inner.remote_branch_last_commit(branch)
    }

    fn open_pr_head_ref_for_repo(
        &self,
        repo: &str,
        pr: u64,
    ) -> Result<daemon::tools::PrHeadBranch, daemon::errors::DaemonError> {
        self.inner.open_pr_head_ref_for_repo(repo, pr)
    }

    fn pr_number_for_branch(
        &self,
        repo: &str,
        branch: &str,
    ) -> Result<Option<u64>, daemon::errors::DaemonError> {
        self.inner.pr_number_for_branch(repo, branch)
    }

    fn gist_nonempty(
        &self,
        gist_id: &str,
    ) -> Result<Option<bool>, daemon::errors::DaemonError> {
        self.inner.gist_nonempty(gist_id)
    }
}

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
        agent_worktree_root: None,
        worktree_ttl_secs: 14 * 24 * 60 * 60,
        worktree_max_count: 200,
        pre_gate_validation_enabled: false,
        escalation_refire_secs: 3600,
    }
}

fn test_telemetry_log() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "afd_r28r_test_telemetry_{}_{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// Issue helper that lets the caller choose the external_ref shape (URL vs
/// short form) so we can prove the intake path normalizes either to the
/// canonical `owner/repo#N` form before any downstream call.
fn issue_with_ref(number: u64, author_login: &str, external_ref: &str) -> Issue {
    Issue {
        number,
        title: format!("issue {number}"),
        body: "body text".into(),
        author_login: author_login.into(),
        external_ref: external_ref.into(),
    }
}

fn labeled_pr_with_ref(
    number: u64,
    author_login: &str,
    head_ref_name: &str,
    external_ref: &str,
) -> LabeledPr {
    LabeledPr {
        number,
        title: format!("pr {number}"),
        body: "pr body".into(),
        author_login: author_login.into(),
        external_ref: external_ref.into(),
        head_ref_name: head_ref_name.into(),
        is_cross_repository: false,
        head_repo_full_name: Some("owner/repo".into()),
        head_repo_owner_login: Some("owner".into()),
        head_sha: Some(format!("sha-{number}")),
        updated_at_epoch: Some(1_700_000_000 + number),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fix A — URL-form external_ref normalized to canonical owner/repo#N on intake
// ─────────────────────────────────────────────────────────────────────────────

/// Issue path: an issue arrives with the URL form (`https://github.com/owner/repo/issues/N`).
/// The intake function MUST pass the canonical short form (`owner/repo#N`) to
/// `tracker.create_bead` so the stored bead_overlay row has the canonical
/// ref. Pre-fix this fails because `intake::normalize` passes the URL through
/// verbatim.
#[test]
fn normalize_canonicalizes_url_form_issue_external_ref_for_create_bead() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue_with_ref(
        8058,
        "alice",
        "https://github.com/owner/repo/issues/8058",
    ));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

    assert_eq!(
        created.len(),
        1,
        "URL-form ref must normalize to short form and create a bead: {created:?}"
    );
    // Successful creates have no entry in `outcomes` (only skipped/errored
    // candidates get a verdict entry), so the dedup contract is purely about
    // the `create_bead` call shape.
    assert!(
        outcomes.is_empty(),
        "URL-form ref must NOT be reported as SkippedDuplicate (it was new, just URL-shaped): {outcomes:?}"
    );

    let calls = tracker.calls.borrow();
    let create_calls: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(
        create_calls.len(),
        1,
        "expected exactly one create_bead attempt"
    );
    assert!(
        create_calls[0].contains("owner/repo#8058"),
        "create_bead must be called with the canonical short form, not the URL form: {create_calls:?}"
    );
    assert!(
        !create_calls[0].contains("https://github.com/"),
        "create_bead must not be called with the URL form (would create a duplicate-shape ref): {create_calls:?}"
    );
}

/// Existing-PR path: a labeled PR arrives with the URL form
/// (`https://github.com/owner/repo/pull/N`). The intake function MUST pass the
/// canonical short form (`owner/repo#N`) to `tracker.create_bead` so the
/// stored bead_overlay row has the canonical ref.
#[test]
fn normalize_labeled_prs_canonicalizes_url_form_external_ref_for_create_bead() {
    let mut scm = UnfilteredFakeScm::new();
    scm.inner.prs.push(labeled_pr_with_ref(
        8058,
        "alice",
        "feature/8058",
        "https://github.com/owner/repo/pull/8058",
    ));
    scm.inner.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    let telemetry_log = test_telemetry_log();
    let mut cache = intake::AdoptionProbeCache::default();

    let result = intake::normalize_labeled_prs_outcome(
        &scm,
        &tracker,
        &cfg,
        &mut cache,
        1_700_000_000,
        &telemetry_log,
    )
    .unwrap();

    // Successful adoptions produce an `adopted` entry (not an outcome entry).
    assert_eq!(
        result.adopted.len(),
        1,
        "URL-form PR must canonicalize and produce an ExistingPrIntake: {:?}",
        result.adopted
    );
    // Either outcome may be empty for a clean successful adoption; the
    // shape-correctness lives in the create_bead call below.
    let _ = result.outcomes;

    let calls = tracker.calls.borrow();
    let create_calls: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(
        create_calls.len(),
        1,
        "expected exactly one create_bead attempt"
    );
    assert!(
        create_calls[0].contains("owner/repo#8058"),
        "create_bead must be called with the canonical short form: {create_calls:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Fix B — once normalized, known_refs catches a repeat labeled candidate for
// the same canonical ref without re-calling create_bead.
// ─────────────────────────────────────────────────────────────────────────────

/// Simulates the live bead pair (e.g. jleechan-jpi vs jleechan-hslx for
/// PR #8058) where two intake events arrive in succession — the second one
/// after the first has been normalized and stored. After canonicalization,
/// the second event's URL-form ref MUST resolve to the same canonical key
/// the first event stored, and `known_refs.contains` MUST short-circuit to
/// `SkippedDuplicate` without calling `create_bead` a second time.
#[test]
fn normalize_dedups_url_then_short_form_for_same_pr() {
    let mut scm = UnfilteredFakeScm::new();
    scm.inner.issues.push(issue_with_ref(
        8058,
        "alice",
        "https://github.com/owner/repo/issues/8058",
    ));
    scm.inner.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    let cfg = test_cfg();

    // First intake event: URL form → normalize → create_bead → stored as
    // owner/repo#8058 (canonical).
    let (created_first, outcomes_first) = intake::normalize(&scm, &tracker, &cfg).unwrap();
    assert_eq!(created_first.len(), 1, "first event must create a bead");
    assert!(outcomes_first.is_empty(), "first event must not produce an outcome (it created): {outcomes_first:?}");

    // Second intake event: short form for the SAME canonical ref. The fake
    // tracker's `known_refs` derives from its candidates vec (post-create),
    // so once the first bead lands there, a second event with the canonical
    // ref MUST be a SkippedDuplicate without a second create_bead call.
    let mut scm2 = UnfilteredFakeScm::new();
    scm2.inner.issues.push(issue_with_ref(8058, "alice", "owner/repo#8058"));
    scm2.inner.permissions.insert("alice".into(), Permission::Write);

    let (created_second, outcomes_second) = intake::normalize(&scm2, &tracker, &cfg).unwrap();

    assert!(
        created_second.is_empty(),
        "second event must NOT create a duplicate bead (Fix B): {created_second:?}"
    );
    assert_eq!(
        outcomes_second.len(),
        1,
        "expected exactly one verdict: {outcomes_second:?}"
    );
    assert_eq!(
        outcomes_second[0].verdict,
        IntakeVerdict::SkippedDuplicate,
        "second event for the same canonical ref MUST be SkippedDuplicate: {outcomes_second:?}"
    );

    // Exactly one create_bead across both intake events.
    let calls = tracker.calls.borrow();
    let create_calls: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(
        create_calls.len(),
        1,
        "expected exactly one create_bead across both intake events (Fix B), got: {create_calls:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Fix C — write-time race recovery when known_refs missed an existing ref.
// Even with normalization, a concurrent writer can land between the
// `fetch_all_external_refs` snapshot and the `tracker.create_bead` call,
// so the `br create` "already exists" signature is still the authoritative
// dedup signal. intake.rs MUST treat it as SkippedDuplicate.
// ─────────────────────────────────────────────────────────────────────────────

/// Same race as `create_bead_duplicate_error_is_recovered_as_already_known`
/// but with URL-form external_ref on the incoming side — proves the race
/// recovery still fires after the URL→canonical normalization step.
#[test]
fn url_form_duplicate_create_bead_race_is_recovered_as_already_known() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue_with_ref(
        8058,
        "alice",
        "https://github.com/owner/repo/issues/8058",
    ));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    // Simulates the race: known_refs (derived from candidates) does NOT
    // contain owner/repo#8058 because the canonicalization step (post-fix)
    // is not yet applied or a concurrent writer hasn't propagated yet.
    *tracker.create_bead_duplicate_of.borrow_mut() = Some("jleechan-vj89".into());

    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

    assert!(
        created.is_empty(),
        "duplicate create_bead race must not be reported as newly created: {created:?}"
    );
    assert_eq!(
        outcomes.len(),
        1,
        "expected exactly one verdict: {outcomes:?}"
    );
    // The verdict MUST report the canonical short form (after normalization)
    // so downstream telemetry correlates with the stored ref shape.
    assert_eq!(
        outcomes[0].external_ref, "owner/repo#8058",
        "intake must report the canonical short form on the verdict even after a race recovery"
    );
    assert_eq!(
        outcomes[0].verdict,
        IntakeVerdict::SkippedDuplicate,
        "write-time race recovery MUST still classify as SkippedDuplicate (Fix C): {outcomes:?}"
    );

    let calls = tracker.calls.borrow();
    let create_calls: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bead("))
        .collect();
    assert_eq!(
        create_calls.len(),
        1,
        "expected exactly one create_bead attempt (which raced and was recovered)"
    );
    // The create_bead call MUST have used the canonical form, not the URL
    // form — otherwise the br-side uniqueness constraint would not have
    // fired in the first place (br would see a new ref).
    assert!(
        create_calls[0].contains("owner/repo#8058"),
        "create_bead must use canonical form so br's uniqueness check is the right one to fire: {create_calls:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Idempotent fast-path sanity: SkippedDuplicate (Fix B) is distinct from the
// write-time race recovery (Fix C) at the verdict level — both land on the
// same variant, but the call shape differs (Fix B never calls create_bead,
// Fix C calls it once and gets back the duplicate error).
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn normalize_skipped_duplicate_does_not_call_create_bead() {
    let mut scm = FakeScm::new();
    scm.issues.push(issue_with_ref(8058, "alice", "owner/repo#8058"));
    scm.permissions.insert("alice".into(), Permission::Write);

    let tracker = FakeTracker::new();
    // Pre-seed candidates with the canonical short form so known_refs catches
    // it on the bulk pre-check (Fix B fast path).
    tracker.candidates.borrow_mut().push(daemon::tools::Bead {
        id: "bead-already-tracked".into(),
        title: "existing bead".into(),
        description: String::new(),
        notes: String::new(),
        file_tree_summary: String::new(),
        external_ref: Some("owner/repo#8058".into()),
    });

    let cfg = test_cfg();

    let (created, outcomes) = intake::normalize(&scm, &tracker, &cfg).unwrap();

    assert!(
        created.is_empty(),
        "SkippedDuplicate must not create a bead: {created:?}"
    );
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].verdict,
        IntakeVerdict::SkippedDuplicate,
        "already-tracked ref MUST be SkippedDuplicate via Fix B fast path"
    );

    let calls = tracker.calls.borrow();
    assert!(
        calls.iter().all(|c| !c.starts_with("create_bead(")),
        "Fix B fast path must not call create_bead (gh-free); only the write-time-race recovery path may: {calls:?}"
    );
}
