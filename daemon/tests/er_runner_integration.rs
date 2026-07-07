// Integration test for the `/er` runner (bead jleechan-qqq). Verifies the
// full pipeline: when an ATTESTED bead's PR has no `/er` verdict in its
// comments, the daemon's fast-tier fires the runner; the runner spawns a
// reviewer (mock `Llm` here), posts the verbatim reply as a PR comment,
// and the same tick's re-fetched snapshot carries the new comment so
// `parse_er_verdict` returns the verdict instead of `Absent`.
//
// Self-contained fakes (separate from `tests/common/mod.rs`) so the
// shared fakes don't gain a "track newly-posted comments" behavior that
// might surprise other tests. The single behavioral difference vs the
// shared fakes: `TrackerMock::comment_external` here ALSO pushes the
// comment into the `ScmMock`'s scripted snapshot, emulating `gh pr view
// --json comments` seeing a freshly-posted comment on the next fetch.

use daemon::config::Config;
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::tick::{run_tick, TickDeps};
use daemon::tools::{
    Bead, Issue, Llm, Permission, PrComment, PrSnapshot, Scm, SessionId, SpawnSpec, Tracker, Vcs,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Default)]
struct LlmMock {
    response: RefCell<Option<Result<String, String>>>,
    real: bool,
    calls: RefCell<Vec<String>>,
}

impl Llm for LlmMock {
    fn judge(&self, prompt: &str) -> Result<String, daemon::errors::DaemonError> {
        self.calls.borrow_mut().push(format!("judge({prompt})"));
        match self.response.borrow().as_ref() {
            Some(Ok(t)) => Ok(t.clone()),
            Some(Err(e)) => Err(daemon::errors::DaemonError::Parse(e.clone())),
            None => Ok(String::new()),
        }
    }
    fn is_real(&self) -> bool {
        self.real
    }
}

#[derive(Default)]
struct ScmMock {
    snapshots: RefCell<HashMap<u64, PrSnapshot>>,
    issues: Vec<Issue>,
    perms: HashMap<String, Permission>,
    calls: RefCell<Vec<String>>,
}

impl ScmMock {
    fn inject_comment(&self, pr: u64, comment: PrComment) {
        if let Some(snap) = self.snapshots.borrow_mut().get_mut(&pr) {
            snap.comments.push(comment);
        }
    }
}

impl Scm for ScmMock {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, daemon::errors::DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("labeled_issues({label})"));
        Ok(self.issues.clone())
    }
    fn collaborator_permission(
        &self,
        login: &str,
    ) -> Result<Permission, daemon::errors::DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("collaborator_permission({login})"));
        Ok(self.perms.get(&login.to_string()).copied().unwrap_or(Permission::None))
    }
    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, daemon::errors::DaemonError> {
        self.calls.borrow_mut().push(format!("pr_snapshot({pr})"));
        self.snapshots
            .borrow()
            .get(&pr)
            .cloned()
            .ok_or_else(|| daemon::errors::DaemonError::Tool {
                tool: "gh".into(),
                rc: 1,
                stderr: format!("no scripted snapshot for pr {pr}"),
            })
    }
    fn close_pr(&self, pr: u64, _c: &str) -> Result<(), daemon::errors::DaemonError> {
        self.calls.borrow_mut().push(format!("close_pr({pr})"));
        Ok(())
    }
    fn remote_branch_last_commit(&self, _b: &str) -> Result<Option<u64>, daemon::errors::DaemonError> {
        Ok(None)
    }
}

/// Custom Tracker that, in addition to logging `comment_external` calls,
/// ALSO calls back into the ScmMock so the next `pr_snapshot` fetch
/// reflects the posted comment (mimics gh CLI behavior).
struct TrackerMock {
    posted: RefCell<HashMap<u64, Vec<PrComment>>>,
    calls: RefCell<Vec<String>>,
    scm: Rc<RefCell<ScmMock>>,
}

impl TrackerMock {
    fn new(scm: Rc<RefCell<ScmMock>>) -> Self {
        Self {
            posted: RefCell::new(HashMap::new()),
            calls: RefCell::new(Vec::new()),
            scm,
        }
    }
}

fn parse_pr_from_external_ref(ext_ref: &str) -> Option<u64> {
    ext_ref.split('#').nth(1).and_then(|n| n.parse().ok())
}

impl Tracker for TrackerMock {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, daemon::errors::DaemonError> {
        self.calls.borrow_mut().push("fetch_candidates".into());
        Ok(Vec::new())
    }
    fn fetch_all_external_refs(&self) -> Result<HashSet<String>, daemon::errors::DaemonError> {
        self.calls.borrow_mut().push("fetch_all_external_refs".into());
        Ok(HashSet::new())
    }
    fn create_bead(
        &self,
        _t: &str,
        _b: &str,
        _e: &str,
    ) -> Result<String, daemon::errors::DaemonError> {
        Ok("fake-bead-1".into())
    }
    fn comment_external(&self, ext_ref: &str, body: &str) -> Result<(), daemon::errors::DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("comment_external({ext_ref},{body})"));
        if let Some(pr) = parse_pr_from_external_ref(ext_ref) {
            let comment = PrComment {
                author: "dark-factory-er".into(),
                body: body.into(),
            };
            self.posted
                .borrow_mut()
                .entry(pr)
                .or_default()
                .push(comment.clone());
            self.scm.borrow().inject_comment(pr, comment);
        }
        Ok(())
    }
}

#[derive(Default)]
struct SessionsMock;
impl daemon::tools::Sessions for SessionsMock {
    fn active_count(&self) -> Result<usize, daemon::errors::DaemonError> {
        Ok(0)
    }
    fn spawn(&self, _s: &SpawnSpec) -> Result<SessionId, daemon::errors::DaemonError> {
        Ok(SessionId("fake".into()))
    }
    fn attach(&self, _b: &str, _i: &str) -> Result<SessionId, daemon::errors::DaemonError> {
        Ok(SessionId("fake".into()))
    }
    fn stop(&self, _i: &SessionId) -> Result<(), daemon::errors::DaemonError> {
        Ok(())
    }
    fn is_quiescent(&self, _i: &SessionId) -> Result<bool, daemon::errors::DaemonError> {
        Ok(true)
    }
}

#[derive(Default)]
struct VcsMock;
impl Vcs for VcsMock {
    fn base_head(&self, _b: &str) -> Result<String, daemon::errors::DaemonError> {
        Ok("deadbeef".into())
    }
    fn create_branch_at(&self, _n: &str, _s: &str) -> Result<(), daemon::errors::DaemonError> {
        Ok(())
    }
    fn head_sha(&self, _b: &str) -> Result<String, daemon::errors::DaemonError> {
        Ok("deadbeef".into())
    }
}

#[derive(Default)]
struct StoreMock {
    overlays: RefCell<HashMap<String, BeadOverlay>>,
    er_counts: RefCell<HashMap<String, (u32, Option<u64>)>>,
    branch_beads: RefCell<HashMap<String, String>>,
}

impl StateStore for StoreMock {
    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, daemon::errors::DaemonError> {
        Ok(self.overlays.borrow().get(bead_id).cloned())
    }
    fn save(&self, overlay: &BeadOverlay) -> Result<(), daemon::errors::DaemonError> {
        self.overlays
            .borrow_mut()
            .insert(overlay.bead_id.clone(), overlay.clone());
        Ok(())
    }
    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), daemon::errors::DaemonError> {
        self.branch_beads
            .borrow_mut()
            .insert(branch.to_string(), bead_id.to_string());
        Ok(())
    }
    fn owned_branches(&self) -> Result<Vec<String>, daemon::errors::DaemonError> {
        let mut v: Vec<String> = self.branch_beads.borrow().keys().cloned().collect();
        v.sort();
        Ok(v)
    }
    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, daemon::errors::DaemonError> {
        Ok(self.branch_beads.borrow().get(branch).cloned())
    }
    fn increment_active_autonomy(&self, _e: u64) -> Result<Vec<BeadOverlay>, daemon::errors::DaemonError> {
        Ok(Vec::new())
    }
    fn save_rejection(
        &self,
        _b: &str,
        _a: u32,
        _r: &str,
        _h: &str,
        _t: &str,
    ) -> Result<(), daemon::errors::DaemonError> {
        Ok(())
    }
    fn load_rejection(
        &self,
        _b: &str,
        _a: u32,
    ) -> Result<Option<(String, String)>, daemon::errors::DaemonError> {
        Ok(None)
    }
    fn er_runner_attempt(
        &self,
        bead_id: &str,
    ) -> Result<(u32, Option<u64>), daemon::errors::DaemonError> {
        Ok(self
            .er_counts
            .borrow()
            .get(bead_id)
            .copied()
            .unwrap_or((0, None)))
    }
    fn incr_er_runner_attempt(
        &self,
        bead_id: &str,
        now_epoch: u64,
    ) -> Result<u32, daemon::errors::DaemonError> {
        let mut counts = self.er_counts.borrow_mut();
        let entry = counts.entry(bead_id.to_string()).or_insert((0, None));
        entry.0 += 1;
        entry.1 = Some(now_epoch);
        Ok(entry.0)
    }
    fn reconcile_dispatching(&self) -> Result<(), daemon::errors::DaemonError> {
        Ok(())
    }
}

fn test_cfg() -> Config {
    Config {
        target_repo: "owner/repo".into(),
        base_branch: "main".into(),
        stage: 1,
        max_workers: 30,
        max_batch: 15,
        fast_tick_secs: 60,
        slow_tick_secs: 60,
        autonomy_timebox_secs: 10_800,
        budget_warn_usd: 20.0,
        spec_dir: ".factory/specs/".into(),
    }
}

fn all_green_snapshot(pr: u64) -> PrSnapshot {
    PrSnapshot {
        pr_number: pr,
        ci_success: true,
        mergeable: true,
        coderabbit_approved: true,
        bugbot_error_count: 0,
        unresolved_thread_count: 0,
        head_sha: "deadbeef".into(),
        body: String::new(),
        comments: Vec::new(),
        files: Vec::new(),
        updated_at_epoch: 0,
        ci_status: "green".into(),
        coderabbit_status: "green".into(),
        ci_pending: false,
    }
}

fn attested_overlay(bead_id: &str, pr: u64) -> BeadOverlay {
    BeadOverlay {
        bead_id: bead_id.into(),
        state: OverlayState::Attested,
        attempt: 1,
        reroll_count: 0,
        autonomy_secs: 0,
        spend_usd: 0.0,
        pr_number: Some(pr),
        branch: Some(format!("factory/{bead_id}-r1")),
        session_id: Some("s1".into()),
    }
}

fn drive_one_tick(
    scm_rc: &Rc<RefCell<ScmMock>>,
    tracker: &TrackerMock,
    llm: &LlmMock,
    store: &StoreMock,
    cfg: &Config,
    telemetry_log: &std::path::Path,
    reviewer_reply: &str,
) -> Result<daemon::tick::TickSummary, Box<dyn std::error::Error>> {
    *llm.response.borrow_mut() = Some(Ok(reviewer_reply.to_string()));
    let summary = run_tick(
        &TickDeps {
            scm: &*scm_rc.borrow(),
            tracker,
            sessions: &SessionsMock,
            llm,
            store,
            vcs: &VcsMock,
            cfg,
            telemetry_log,
        },
        0,
        0,
    )?;
    Ok(summary)
}

#[test]
fn er_runner_already_posted_short_circuits_no_llm_call() {
    let _ = std::fs::remove_file("/tmp/afd_er_runner_int_test_already.jsonl");

    let scm_rc = Rc::new(RefCell::new(ScmMock::default()));
    let tracker = TrackerMock::new(scm_rc.clone());
    let llm = LlmMock::default();
    let store = StoreMock::default();
    let cfg = test_cfg();

    // Pre-populate Attested overlay + PrSnapshot with a `/er PASS` comment
    // so the runner should short-circuit (Outcome::AlreadyPosted).
    let mut snap = all_green_snapshot(101);
    snap.comments.push(PrComment {
        author: "some-human".into(),
        body: "/er PASS — human verifier ran this manually".into(),
    });
    scm_rc.borrow_mut().snapshots.borrow_mut().insert(101, snap);
    store
        .overlays
        .borrow_mut()
        .insert("b1".into(), attested_overlay("b1", 101));
    store.register_branch("b1", "factory/b1-r1").unwrap();

    let _summary = drive_one_tick(
        &scm_rc,
        &tracker,
        &llm,
        &store,
        &cfg,
        std::path::Path::new("/tmp/afd_er_runner_int_test_already.jsonl"),
        "/er PASS",
    )
    .unwrap();

    // /er runner must NOT have spawned when /er verdict already in comments.
    let llm_calls = llm.calls.borrow();
    let er_calls: Vec<&String> = llm_calls
        .iter()
        .filter(|c| c.contains("You are the /er"))
        .collect();
    assert!(
        er_calls.is_empty(),
        "/er runner must NOT spawn when /er verdict already in comments; got calls: {er_calls:?}"
    );
    // /er runner must NOT have posted any PR comment.
    let comment_calls = tracker.calls.borrow();
    let er_comments: Vec<&String> = comment_calls
        .iter()
        .filter(|c| c.contains("dark-factory /er"))
        .collect();
    assert!(
        er_comments.is_empty(),
        "/er runner must NOT post when verdict already present; got: {er_comments:?}"
    );
}

#[test]
fn er_runner_spawns_reviewer_and_posted_comment_flips_gate_to_pass() {
    let _ = std::fs::remove_file("/tmp/afd_er_runner_int_test_posted.jsonl");

    let scm_rc = Rc::new(RefCell::new(ScmMock::default()));
    let tracker = TrackerMock::new(scm_rc.clone());
    let llm = LlmMock::default();
    let store = StoreMock::default();
    let cfg = test_cfg();

    scm_rc
        .borrow_mut()
        .snapshots
        .borrow_mut()
        .insert(102, all_green_snapshot(102));
    store
        .overlays
        .borrow_mut()
        .insert("b1".into(), attested_overlay("b1", 102));
    store.register_branch("b1", "factory/b1-r1").unwrap();

    let _summary = drive_one_tick(
        &scm_rc,
        &tracker,
        &llm,
        &store,
        &cfg,
        std::path::Path::new("/tmp/afd_er_runner_int_test_posted.jsonl"),
        "/er PASS — saw integration test output",
    )
    .unwrap();

    // /er runner DID spawn the reviewer (one Llm::judge call with the /er prompt).
    let llm_calls = llm.calls.borrow();
    let er_calls: Vec<&String> = llm_calls
        .iter()
        .filter(|c| c.contains("You are the /er"))
        .collect();
    assert_eq!(er_calls.len(), 1, "exactly one /er runner spawn expected");

    // /er runner DID post a comment with the verbatim verdict.
    let comment_calls = tracker.calls.borrow();
    let er_comments: Vec<&String> = comment_calls
        .iter()
        .filter(|c| c.contains("dark-factory /er"))
        .collect();
    assert_eq!(
        er_comments.len(),
        1,
        "exactly one /er verdict PR comment expected: {comment_calls:?}"
    );
    assert!(
        er_comments[0].contains("/er PASS"),
        "verbatim /er PASS must be in the posted comment: {er_comments:?}"
    );

    // The attempt counter was incremented to 1.
    let (count, _last) = store.er_runner_attempt("b1").unwrap();
    assert_eq!(count, 1);

    // The /er verdict comment now lives in the ScmMock snapshot, so
    // parse_er_verdict sees Pass.
    let snap = scm_rc
        .borrow()
        .snapshots
        .borrow()
        .get(&102)
        .unwrap()
        .clone();
    assert_eq!(
        daemon::verifier::parse_er_verdict(&snap.comments),
        daemon::verifier::ErVerdict::Pass,
        "after the runner posts, parse_er_verdict must see Pass"
    );

    let _ = std::fs::remove_file("/tmp/afd_er_runner_int_test_posted.jsonl");
}

#[test]
fn er_runner_fail_comment_flips_gate_to_red() {
    let _ = std::fs::remove_file("/tmp/afd_er_runner_int_test_fail.jsonl");

    let scm_rc = Rc::new(RefCell::new(ScmMock::default()));
    let tracker = TrackerMock::new(scm_rc.clone());
    let llm = LlmMock::default();
    let store = StoreMock::default();
    let cfg = test_cfg();

    scm_rc
        .borrow_mut()
        .snapshots
        .borrow_mut()
        .insert(103, all_green_snapshot(103));
    store
        .overlays
        .borrow_mut()
        .insert("b1".into(), attested_overlay("b1", 103));
    store.register_branch("b1", "factory/b1-r1").unwrap();

    let _summary = drive_one_tick(
        &scm_rc,
        &tracker,
        &llm,
        &store,
        &cfg,
        std::path::Path::new("/tmp/afd_er_runner_int_test_fail.jsonl"),
        "/er FAIL — no integration tests",
    )
    .unwrap();

    // /er FAIL verdict -> parse_er_verdict sees it -> gate 6 is Red.
    let snap = scm_rc
        .borrow()
        .snapshots
        .borrow()
        .get(&103)
        .unwrap()
        .clone();
    assert_eq!(
        daemon::verifier::parse_er_verdict(&snap.comments),
        daemon::verifier::ErVerdict::Fail,
        "after the runner posts /er FAIL, parse_er_verdict must see Fail"
    );

    let _ = std::fs::remove_file("/tmp/afd_er_runner_int_test_fail.jsonl");
}

#[test]
fn er_runner_emits_posted_telemetry_event() {
    let _ = std::fs::remove_file("/tmp/afd_er_runner_int_test_telemetry.jsonl");

    let scm_rc = Rc::new(RefCell::new(ScmMock::default()));
    let tracker = TrackerMock::new(scm_rc.clone());
    let llm = LlmMock::default();
    let store = StoreMock::default();
    let cfg = test_cfg();

    scm_rc
        .borrow_mut()
        .snapshots
        .borrow_mut()
        .insert(104, all_green_snapshot(104));
    store
        .overlays
        .borrow_mut()
        .insert("b1".into(), attested_overlay("b1", 104));
    store.register_branch("b1", "factory/b1-r1").unwrap();

    let _ = drive_one_tick(
        &scm_rc,
        &tracker,
        &llm,
        &store,
        &cfg,
        std::path::Path::new("/tmp/afd_er_runner_int_test_telemetry.jsonl"),
        "/er PASS",
    )
    .unwrap();

    let body = std::fs::read_to_string("/tmp/afd_er_runner_int_test_telemetry.jsonl").unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let event_types: Vec<String> = events
        .iter()
        .map(|e| e["eventType"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        event_types.iter().any(|e| e == "ER_RUNNER_POSTED"),
        "expected an ER_RUNNER_POSTED telemetry event, got: {event_types:?}"
    );

    let _ = std::fs::remove_file("/tmp/afd_er_runner_int_test_telemetry.jsonl");
}