// Shared test-only scripted fakes for the five tool-boundary traits (design doc
// §4, TDD plan Task 5 Step 4). Each fake holds scripted responses plus a
// `RefCell<Vec<String>>` call log and performs NO subprocess use — downstream
// tasks (intake, router, dispatch, verifier) unit-test against these instead of
// real CLIs. Included by integration test files via `#[path = "common/mod.rs"]
// mod common;` (the idiomatic way to share a module across multiple files under
// `tests/`, since each file in `tests/` is its own separate crate).
#![allow(dead_code)]

use daemon::errors::DaemonError;
use daemon::state::{
    is_permanent_human_hold_reason, set_human_hold_reason, BeadOverlay, HumanHoldReason,
    OverlayState, StateStore,
};
use daemon::tools::{
    Bead, Issue, LabeledPr, Llm, Permission, PrHeadBranch, PrSnapshot, Scm, SessionActivity,
    SessionId, Sessions, SpawnSpec, Tracker, Vcs,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Scripted `Tracker` fake: pre-seeded candidates + a call log of every method
/// invocation (method name + key args), so tests can assert both output and
/// call shape (e.g. "create_bead called exactly once"). `candidates` is a
/// `RefCell` so `create_bead` can append the newly-created bead back onto the
/// list, mirroring real `br`'s durable-store idempotency: a bead `br create`
/// just wrote shows up on the very next `br list` (`fetch_candidates`) call.
/// Without this, a caller that re-runs `intake::normalize` on a later tick
/// (e.g. the Task 10 tick loop) would see the same external_ref as "unknown"
/// forever and create a duplicate bead every tick.
#[derive(Default)]
pub struct FakeTracker {
    pub candidates: RefCell<Vec<Bead>>,
    pub create_bead_result: RefCell<Option<Result<String, String>>>,
    /// Scripts `create_bead` to fail with the exact `br create` duplicate
    /// error shape (`DaemonError::duplicate_external_ref_bead_id` parses
    /// it), simulating jleechan-u4gb: a concurrent/stale `fetch_all_external_refs`
    /// snapshot missed a ref that `br create`'s own uniqueness check
    /// correctly rejects. Value is the existing bead id to report.
    pub create_bead_duplicate_of: RefCell<Option<String>>,
    /// jleechan-eazj: scripts a NON-duplicate `create_bead` failure for one
    /// specific `external_ref` only (consumed once), so a multi-candidate
    /// batch can exercise "candidate A's create_bead errors, candidate B's
    /// does not" — the exact shape needed to prove one candidate's error no
    /// longer starves the rest of the batch of telemetry.
    pub create_bead_fail_for_ref: RefCell<Option<(String, String)>>,
    pub fail_next_fetch_candidates: RefCell<Option<String>>,
    pub fail_next_comment: RefCell<Option<String>>,
    pub calls: RefCell<Vec<String>>,
}

impl FakeTracker {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Tracker for FakeTracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError> {
        self.calls.borrow_mut().push("fetch_candidates".into());
        if let Some(stderr) = self.fail_next_fetch_candidates.borrow_mut().take() {
            return Err(DaemonError::Tool {
                tool: "br".into(),
                rc: 1,
                stderr,
            });
        }
        Ok(self.candidates.borrow().clone())
    }

    fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError> {
        self.calls
            .borrow_mut()
            .push("fetch_all_external_refs".into());
        let refs = self
            .candidates
            .borrow()
            .iter()
            .filter_map(|bead| bead.external_ref.clone())
            .collect();
        Ok(refs)
    }

    fn create_bead(
        &self,
        title: &str,
        body: &str,
        external_ref: &str,
    ) -> Result<String, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("create_bead({title},{body},{external_ref})"));
        if let Some(existing_bead_id) = self.create_bead_duplicate_of.borrow_mut().take() {
            return Err(DaemonError::Tool {
                tool: "br".into(),
                rc: 7,
                stderr: format!(
                    "Error: Configuration error: External reference '{external_ref}' already exists on issue {existing_bead_id}\n"
                ),
            });
        }
        // NB: clone into an owned local first — using the `.borrow()` call
        // directly as the `if let` scrutinee extends the `Ref` guard's
        // lifetime across the whole block (temporary lifetime extension),
        // which panics on the `.borrow_mut()` below with "already borrowed".
        let fail_for_ref = self.create_bead_fail_for_ref.borrow().clone();
        if let Some((fail_ref, msg)) = fail_for_ref {
            if fail_ref == external_ref {
                self.create_bead_fail_for_ref.borrow_mut().take();
                // `DaemonError::Tool` matches the real shape a generic
                // (non-duplicate) `br create` subprocess failure takes —
                // e.g. a malformed body/title `br` rejects for reasons
                // unrelated to the uniqueness constraint above.
                return Err(DaemonError::Tool {
                    tool: "br".into(),
                    rc: 1,
                    stderr: msg,
                });
            }
        }
        let result = match self.create_bead_result.borrow().as_ref() {
            Some(Ok(id)) => Ok(id.clone()),
            Some(Err(e)) => Err(DaemonError::Tool {
                tool: "br".into(),
                rc: 1,
                stderr: e.clone(),
            }),
            None => Ok("fake-bead-1".into()),
        };
        if let Ok(id) = &result {
            self.candidates.borrow_mut().push(Bead {
                id: id.clone(),
                title: title.to_string(),
                description: body.to_string(),
                file_tree_summary: String::new(),
                external_ref: Some(external_ref.to_string()),
            });
        }
        result
    }

    fn comment_external(&self, external_ref: &str, body: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("comment_external({external_ref},{body})"));
        if let Some(stderr) = self.fail_next_comment.borrow_mut().take() {
            return Err(DaemonError::Tool {
                tool: "br".into(),
                rc: 1,
                stderr,
            });
        }
        Ok(())
    }
}

/// Scripted `Scm` fake: pre-seeded issues/permissions/snapshots keyed by input.
#[derive(Default)]
pub struct FakeScm {
    pub issues: Vec<Issue>,
    pub prs: Vec<LabeledPr>,
    pub permissions: HashMap<String, Permission>,
    pub pr_snapshots: HashMap<u64, PrSnapshot>,
    pub remote_branches: HashMap<String, Option<u64>>,
    /// jleechan-drive-pr-branch-binding-pcpr: scripted open-PR lookups,
    /// keyed by `(repo, pr_number)`. Absence of a key (the `Default` case)
    /// means `PrHeadBranch::NotFound`, matching the real `CliScm` fail-safe
    /// default — script `PrHeadBranch::SameRepo(head_ref)` for a confirmed
    /// same-repo open PR, or `PrHeadBranch::Fork` for a confirmed open PR
    /// whose head lives on a fork (the fail-closed guard).
    pub open_pr_head_refs: HashMap<(String, u64), PrHeadBranch>,
    pub calls: RefCell<Vec<String>>,
}

impl FakeScm {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Scm for FakeScm {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("labeled_issues({label})"));
        Ok(self.issues.clone())
    }

    fn labeled_prs(&self, label: &str) -> Result<Vec<LabeledPr>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("labeled_prs({label})"));
        Ok(self.prs.clone())
    }

    fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("collaborator_permission({login})"));
        Ok(self
            .permissions
            .get(login)
            .copied()
            .unwrap_or(Permission::None))
    }

    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError> {
        self.calls.borrow_mut().push(format!("pr_snapshot({pr})"));
        self.pr_snapshots
            .get(&pr)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "gh".into(),
                rc: 1,
                stderr: format!("no scripted snapshot for pr {pr}"),
            })
    }

    /// jleechan-9xrs Stage D regression coverage: records the `repo`
    /// argument distinctly from the plain `pr_snapshot({pr})` call log entry
    /// so integration tests can assert the full fast-tier verification loop
    /// (skeptic gate, /er runner, gate assessment) fetched the bead's OWN
    /// resolved repo, not `cfg.target_repo`.
    fn pr_snapshot_for_repo(&self, repo: &str, pr: u64) -> Result<PrSnapshot, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("pr_snapshot_for_repo({repo},{pr})"));
        self.pr_snapshots
            .get(&pr)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "gh".into(),
                rc: 1,
                stderr: format!("no scripted snapshot for pr {pr}"),
            })
    }

    fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("close_pr({pr},{comment})"));
        Ok(())
    }

    /// jleechan-v6ud / issue #340 regression coverage: records the
    /// repo-scoped close with the bead's resolved `target_repo` argument
    /// distinctly from the plain `close_pr({pr},{comment})` call log
    /// entry, so the regression test can prove reroll closes the bead's
    /// OWN PR (in its resolved repo) rather than `cfg.target_repo`'s
    /// same-numbered PR.
    fn close_pr_for_repo(&self, repo: &str, pr: u64, comment: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("close_pr_for_repo({repo},{pr},{comment})"));
        Ok(())
    }

    fn remote_branch_last_commit(&self, branch: &str) -> Result<Option<u64>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("remote_branch_last_commit({branch})"));
        if let Some(&res) = self.remote_branches.get(branch) {
            Ok(res)
        } else {
            Ok(None)
        }
    }

    fn open_pr_head_ref_for_repo(&self, repo: &str, pr: u64) -> Result<PrHeadBranch, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("open_pr_head_ref_for_repo({repo},{pr})"));
        Ok(self
            .open_pr_head_refs
            .get(&(repo.to_string(), pr))
            .cloned()
            .unwrap_or(PrHeadBranch::NotFound))
    }
}

/// Scripted `Sessions` fake: active-count + spawn/attach return a caller-set
/// `SessionId`, `is_quiescent` returns the scripted bool.
pub struct FakeSessions {
    pub active_count: usize,
    pub next_session_id: String,
    pub quiescent: bool,
    pub fail_spawn_for: RefCell<Vec<String>>,
    pub panic_after_spawn_for: RefCell<Vec<String>>,
    pub fail_spawn_cleanup_for: RefCell<Vec<String>>,
    pub fail_stop_for: RefCell<Vec<String>>,
    // jleechan-w28n: scripted `DaemonError::Deferred` spawn outcome — AO's
    // own admission-control queue at the target project's session cap, NOT a
    // failure. Distinct from `fail_spawn_for` (`DaemonError::Tool`) so
    // end-to-end tests can exercise the two paths independently and prove
    // they never share `overlay.spawn_failure_count`.
    pub fail_spawn_deferred_for: RefCell<Vec<String>>,
    pub spawn_prompts: RefCell<Vec<(String, String)>>,
    pub calls: RefCell<Vec<String>>,
    /// jleechan-5ia2: scripted `session_branch` override, keyed by session
    /// id. Empty by default (matches the trait's `Ok(None)` default —
    /// "cannot verify") so pre-existing tests are unaffected; tests that
    /// exercise the dispatch-integrity sweep populate this to simulate AO
    /// reporting a session whose live branch does NOT match what the
    /// daemon believes it dispatched (the `wa-3004` contamination
    /// scenario).
    pub branch_for: RefCell<HashMap<String, String>>,
    /// Real-wall-clock quiescence scripting for the reroll quiescence-timeout
    /// race tests (jleechan quiescence-timeout validation): when set,
    /// `is_quiescent` ignores the static `quiescent` flag and instead returns
    /// `Ok(Instant::now() >= terminal_at)` — i.e. the fake AO process reports
    /// "not terminal" before this real instant and "terminal" from then on.
    /// `None` preserves the original static-flag behavior so every
    /// pre-existing test is unaffected.
    pub terminal_at: RefCell<Option<std::time::Instant>>,
    /// Optional error to return from `is_quiescent` (models the "quiescence
    /// check failed" error path independent of the timeout path).
    pub quiescence_check_error: RefCell<Option<String>>,
    /// Bead jleechan-zeij / issue #322 r2. Branches for which `attach` returns
    /// `SessionNotFound` (the "already reaped" fast path). Empty by default.
    pub attach_not_found_for: RefCell<Vec<String>>,
    /// Branches for which `attach` returns a PERMANENT (`DaemonError::Parse`)
    /// error — models an ambiguous/malformed `ao status` that must PROPAGATE
    /// (Codex r3 P2) rather than defer/park. Empty by default.
    pub fail_attach_permanent_for: RefCell<Vec<String>>,
    /// Branches for which `attach` returns a TRANSIENT (`DaemonError::Tool`)
    /// error — models a momentary `ao status` failure that must DEFER. Empty
    /// by default.
    pub fail_attach_transient_for: RefCell<Vec<String>>,
    /// Bead jleechan-zeij / issue #322 r3 (positive-death modeling): once
    /// `true`, a successful `stop()` does NOT terminate the session — it
    /// survives as a live orphan (`ao session kill` swallowed the tmux
    /// destruction), so post-stop `attach` still returns it. Default `false`:
    /// a successful `stop()` genuinely terminates the session, so post-stop
    /// `attach` returns `SessionNotFound` (positive death).
    pub orphan_after_stop: Cell<bool>,
    /// Set once a `stop()` call has SUCCEEDED. Combined with
    /// `orphan_after_stop`, this drives the post-stop `attach` result:
    /// terminated (SessionNotFound) vs surviving orphan.
    pub stop_succeeded: Cell<bool>,
    /// Session ids for which `stop()` returns a PERMANENT (`DaemonError::Parse`)
    /// error — models a non-transient kill failure that must PROPAGATE. Empty
    /// by default.
    pub fail_stop_permanent_for: RefCell<Vec<String>>,
    /// Static `session_activity` override (idle vs running vs terminal). When
    /// `None`, `session_activity` derives from `terminal_at`/`quiescent` to
    /// match the trait default. Set it to `Idle` to reproduce the #322 live
    /// signature, or `Running` to model a worker actively pushing.
    pub activity: RefCell<Option<SessionActivity>>,
    /// Optional PERMANENT (`DaemonError::Parse`) error to return from
    /// `session_activity` on every call — models a non-transient `ao status`
    /// parse failure that must PROPAGATE, not be swallowed as a defer.
    pub activity_permanent_error: RefCell<Option<String>>,
    /// Bead jleechan-zeij / issue #322 r4 P1: a per-call scripted
    /// `session_activity` SEQUENCE (consumed front-to-back), used to model a
    /// FLAPPING session (e.g. Terminal, Terminal, NotFound, Running…). Once
    /// exhausted, `session_activity` falls back to the static `activity`
    /// override / derived value. Empty by default.
    pub activity_sequence: RefCell<Vec<SessionActivity>>,
    pub worktree_remote_override: RefCell<Option<String>>,
    /// jleechan-coder-silent-false-parks-h92r: scripted
    /// `worktree_transcript_last_activity_epoch` override, keyed by
    /// `"ao_project,branch"`. Empty by default (matches the trait's
    /// `Ok(None)` default — "no evidence") so pre-existing tests are
    /// unaffected; tests that exercise the transcript-liveness grace path
    /// populate this to simulate a coder whose transcript is still updating
    /// even though the remote branch has been silent.
    pub transcript_activity_for: RefCell<HashMap<String, u64>>,
}

impl Default for FakeSessions {
    fn default() -> Self {
        Self {
            active_count: 0,
            next_session_id: "fake-session-1".into(),
            quiescent: true,
            fail_spawn_for: RefCell::new(Vec::new()),
            panic_after_spawn_for: RefCell::new(Vec::new()),
            fail_spawn_cleanup_for: RefCell::new(Vec::new()),
            fail_stop_for: RefCell::new(Vec::new()),
            fail_spawn_deferred_for: RefCell::new(Vec::new()),
            spawn_prompts: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
            branch_for: RefCell::new(HashMap::new()),
            terminal_at: RefCell::new(None),
            quiescence_check_error: RefCell::new(None),
            attach_not_found_for: RefCell::new(Vec::new()),
            fail_attach_permanent_for: RefCell::new(Vec::new()),
            fail_attach_transient_for: RefCell::new(Vec::new()),
            orphan_after_stop: Cell::new(false),
            stop_succeeded: Cell::new(false),
            fail_stop_permanent_for: RefCell::new(Vec::new()),
            activity: RefCell::new(None),
            activity_permanent_error: RefCell::new(None),
            activity_sequence: RefCell::new(Vec::new()),
            worktree_remote_override: RefCell::new(None),
            transcript_activity_for: RefCell::new(HashMap::new()),
        }
    }
}

impl FakeSessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_spawn_for(&self, bead_id: &str) {
        self.fail_spawn_for.borrow_mut().push(bead_id.to_string());
    }

    pub fn panic_after_spawn_for(&self, bead_id: &str) {
        self.panic_after_spawn_for
            .borrow_mut()
            .push(bead_id.to_string());
    }

    pub fn fail_spawn_cleanup_for(&self, bead_id: &str) {
        self.fail_spawn_cleanup_for
            .borrow_mut()
            .push(bead_id.to_string());
    }

    pub fn fail_stop_for(&self, session_id: &str) {
        self.fail_stop_for
            .borrow_mut()
            .push(session_id.to_string());
    }

    pub fn fail_spawn_deferred_for(&self, bead_id: &str) {
        self.fail_spawn_deferred_for
            .borrow_mut()
            .push(bead_id.to_string());
    }

    /// Script `session_branch(session_id)` to report `branch`.
    pub fn set_session_branch(&self, session_id: &str, branch: &str) {
        self.branch_for
            .borrow_mut()
            .insert(session_id.to_string(), branch.to_string());
    }

    /// Script the fake AO process to report "not terminal" (`is_quiescent`
    /// returns `Ok(false)`) until real wall-clock instant `at`, and
    /// "terminal" (`Ok(true)`) from `at` onward. Passing an instant in the
    /// past means "terminal immediately". Passing an instant far in the
    /// future (beyond the quiescence timeout under test) simulates a session
    /// that never settles — the genuine mid-push-race case.
    pub fn set_terminal_at(&self, at: std::time::Instant) {
        *self.terminal_at.borrow_mut() = Some(at);
    }

    /// Script `is_quiescent` to return this error on every call, modeling an
    /// AO status-query failure independent of the timeout path.
    pub fn fail_quiescence_check(&self, message: &str) {
        *self.quiescence_check_error.borrow_mut() = Some(message.to_string());
    }

    /// Bead jleechan-zeij / issue #322 r2: script `attach(branch, _)` to
    /// return `SessionNotFound` — the "worker already fully reaped" fast path.
    pub fn attach_not_found_for(&self, branch: &str) {
        self.attach_not_found_for
            .borrow_mut()
            .push(branch.to_string());
    }

    /// Script `attach(branch, _)` to return a PERMANENT `DaemonError::Parse`
    /// — models an ambiguous/malformed `ao status` that must PROPAGATE.
    pub fn fail_attach_permanent_for(&self, branch: &str) {
        self.fail_attach_permanent_for
            .borrow_mut()
            .push(branch.to_string());
    }

    /// Script `attach(branch, _)` to return a TRANSIENT `DaemonError::Tool` —
    /// models a momentary `ao status` failure that must DEFER.
    pub fn fail_attach_transient_for(&self, branch: &str) {
        self.fail_attach_transient_for
            .borrow_mut()
            .push(branch.to_string());
    }

    /// Bead jleechan-zeij / issue #322 r3: model `ao session kill` swallowing
    /// tmux destruction — a successful `stop()` leaves the session alive as an
    /// orphan, so post-stop `attach` keeps returning it (no positive death).
    pub fn set_orphan_after_stop(&self) {
        self.orphan_after_stop.set(true);
    }

    /// Script `stop(session_id)` to return a PERMANENT `DaemonError::Parse` —
    /// models a non-transient kill failure that must PROPAGATE.
    pub fn fail_stop_permanent_for(&self, session_id: &str) {
        self.fail_stop_permanent_for
            .borrow_mut()
            .push(session_id.to_string());
    }

    /// Script the static `session_activity` classification (idle vs running
    /// vs terminal), overriding the `terminal_at`/`quiescent`-derived default.
    pub fn set_activity(&self, activity: SessionActivity) {
        *self.activity.borrow_mut() = Some(activity);
    }

    /// Script `session_activity` to return a PERMANENT `DaemonError::Parse` on
    /// every call — models a non-transient failure that must propagate.
    pub fn fail_activity_permanent(&self, message: &str) {
        *self.activity_permanent_error.borrow_mut() = Some(message.to_string());
    }

    /// Script a per-call `session_activity` sequence (consumed front-to-back)
    /// to model a flapping session; falls back to the static override once
    /// exhausted.
    pub fn set_activity_sequence(&self, seq: Vec<SessionActivity>) {
        *self.activity_sequence.borrow_mut() = seq;
    }

    pub fn set_worktree_remote(&self, remote: &str) {
        *self.worktree_remote_override.borrow_mut() = Some(remote.to_string());
    }

    /// Script `worktree_transcript_last_activity_epoch(ao_project, branch)`
    /// to report `epoch` — simulates a coder transcript that was modified at
    /// unix time `epoch`, independent of any remote branch commit activity.
    pub fn set_transcript_activity(&self, ao_project: &str, branch: &str, epoch: u64) {
        self.transcript_activity_for
            .borrow_mut()
            .insert(format!("{ao_project},{branch}"), epoch);
    }
}

impl Sessions for FakeSessions {
    fn active_count(&self) -> Result<usize, DaemonError> {
        self.calls.borrow_mut().push("active_count".into());
        Ok(self.active_count)
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        self.spawn_prompts
            .borrow_mut()
            .push((spec.bead_id.clone(), spec.prompt.clone()));
        self.calls
            .borrow_mut()
            .push(format!("spawn({})", spec.bead_id));
        if self.panic_after_spawn_for.borrow().contains(&spec.bead_id) {
            panic!("scripted process death after external spawn for {}", spec.bead_id);
        }
        if self.fail_spawn_for.borrow().contains(&spec.bead_id) {
            return Err(DaemonError::Tool {
                tool: "ao".into(),
                rc: 1,
                stderr: format!("scripted spawn failure for {}", spec.bead_id),
            });
        }
        if self
            .fail_spawn_cleanup_for
            .borrow()
            .contains(&spec.bead_id)
        {
            return Err(DaemonError::SpawnCleanupFailed {
                session: format!("leaked-{}", spec.bead_id),
                spawn_error: Box::new(DaemonError::Parse(
                    "scripted invalid spawn metadata".into(),
                )),
                cleanup_error: Box::new(DaemonError::Tool {
                    tool: "ao".into(),
                    rc: 1,
                    stderr: "scripted cleanup failure".into(),
                }),
            });
        }
        if self
            .fail_spawn_deferred_for
            .borrow()
            .contains(&spec.bead_id)
        {
            return Err(DaemonError::Deferred(format!(
                "REQUEST=sq-scripted-{}",
                spec.bead_id
            )));
        }
        Ok(SessionId(self.next_session_id.clone()))
    }

    fn attach(&self, branch: &str, bead_id: &str) -> Result<SessionId, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("attach({branch},{bead_id})"));
        if self
            .attach_not_found_for
            .borrow()
            .iter()
            .any(|b| b == branch)
        {
            return Err(DaemonError::SessionNotFound {
                branch: branch.to_string(),
                bead_id: bead_id.to_string(),
            });
        }
        if self
            .fail_attach_permanent_for
            .borrow()
            .iter()
            .any(|b| b == branch)
        {
            return Err(DaemonError::Parse(format!(
                "scripted permanent attach failure for {branch}"
            )));
        }
        if self
            .fail_attach_transient_for
            .borrow()
            .iter()
            .any(|b| b == branch)
        {
            return Err(DaemonError::Tool {
                tool: "ao".into(),
                rc: 1,
                stderr: format!("scripted transient attach failure for {branch}"),
            });
        }
        // Bead jleechan-zeij / issue #322 r3: after a successful `stop()` that
        // genuinely terminated the session (the default, not an orphan), a
        // re-attach reports the session gone — the positive-death signal.
        if self.stop_succeeded.get() && !self.orphan_after_stop.get() {
            return Err(DaemonError::SessionNotFound {
                branch: branch.to_string(),
                bead_id: bead_id.to_string(),
            });
        }
        Ok(SessionId(self.next_session_id.clone()))
    }

    fn stop(&self, id: &SessionId) -> Result<(), DaemonError> {
        self.calls.borrow_mut().push(format!("stop({})", id.0));
        if self.fail_stop_permanent_for.borrow().contains(&id.0) {
            return Err(DaemonError::Parse(format!(
                "scripted permanent stop failure for {}",
                id.0
            )));
        }
        if self.fail_stop_for.borrow().contains(&id.0) {
            return Err(DaemonError::Tool {
                tool: "ao".into(),
                rc: 1,
                stderr: format!("scripted stop failure for {}", id.0),
            });
        }
        // A successful kill: record it so the post-stop re-attach models the
        // session as terminated (positive death) unless flagged as an orphan.
        self.stop_succeeded.set(true);
        Ok(())
    }

    fn is_quiescent(&self, id: &SessionId) -> Result<bool, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("is_quiescent({})", id.0));
        if let Some(msg) = self.quiescence_check_error.borrow().as_ref() {
            return Err(DaemonError::Tool {
                tool: "ao".into(),
                rc: 1,
                stderr: msg.clone(),
            });
        }
        if let Some(at) = *self.terminal_at.borrow() {
            return Ok(std::time::Instant::now() >= at);
        }
        Ok(self.quiescent)
    }

    fn session_activity(&self, id: &SessionId) -> Result<SessionActivity, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("session_activity({})", id.0));
        // Permanent error takes precedence — must PROPAGATE, not defer.
        if let Some(msg) = self.activity_permanent_error.borrow().as_ref() {
            return Err(DaemonError::Parse(msg.clone()));
        }
        // A scripted transient quiescence-check error also breaks the probe.
        if let Some(msg) = self.quiescence_check_error.borrow().as_ref() {
            return Err(DaemonError::Tool {
                tool: "ao".into(),
                rc: 1,
                stderr: msg.clone(),
            });
        }
        // Scripted per-call sequence (flapping session) takes precedence.
        {
            let mut seq = self.activity_sequence.borrow_mut();
            if !seq.is_empty() {
                return Ok(seq.remove(0));
            }
        }
        // Explicit static override (idle / running / terminal / not-found).
        if let Some(activity) = *self.activity.borrow() {
            return Ok(activity);
        }
        // Otherwise derive from the terminal_at/quiescent schedule so the
        // classification matches this fake's `is_quiescent` (and the trait
        // default): terminal once quiescent, running until then. This fake
        // never reports Idle unless `set_activity` is used.
        let terminal = if let Some(at) = *self.terminal_at.borrow() {
            std::time::Instant::now() >= at
        } else {
            self.quiescent
        };
        Ok(if terminal {
            SessionActivity::Terminal
        } else {
            SessionActivity::Running
        })
    }

    fn session_branch(&self, id: &SessionId) -> Result<Option<String>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("session_branch({})", id.0));
        Ok(self.branch_for.borrow().get(&id.0).cloned())
    }

    fn worktree_remote_url(
        &self,
        ao_project: &str,
        branch: &str,
        remote_name: &str,
    ) -> Result<Option<String>, DaemonError> {
        self.calls.borrow_mut().push(format!(
            "worktree_remote_url({ao_project},{branch},{remote_name})"
        ));
        if let Some(remote) = self.worktree_remote_override.borrow().clone() {
            return Ok(Some(remote));
        }
        let repo = if ao_project == "worldarchitect" {
            "jleechanorg/worldarchitect.ai"
        } else {
            "owner/repo"
        };
        Ok(Some(format!("https://github.com/{repo}.git")))
    }

    fn worktree_transcript_last_activity_epoch(
        &self,
        ao_project: &str,
        branch: &str,
    ) -> Result<Option<u64>, DaemonError> {
        self.calls.borrow_mut().push(format!(
            "worktree_transcript_last_activity_epoch({ao_project},{branch})"
        ));
        Ok(self
            .transcript_activity_for
            .borrow()
            .get(&format!("{ao_project},{branch}"))
            .copied())
    }
}

/// Scripted `Vcs` fake: pre-seeded SHAs keyed by branch name.
#[derive(Default)]
pub struct FakeVcs {
    pub heads: HashMap<String, String>,
    /// Per-(branch, remote_sha) script for `is_remote_ahead`. When absent the
    /// default is `false` so tests that don't exercise the stall-bypass guard
    /// don't have to set it up. Tests that DO exercise the guard
    /// (`test_wedge_detection_attested_session_not_stalled_if_remote_ahead` and
    /// its companion `*_local_ahead_or_diverged_still_parks`) insert the
    /// (branch, remote_sha) key with the desired answer.
    pub remote_ahead: HashMap<(String, String), bool>,
    /// Scripts `push_fix_commit` to fail for a given branch, simulating a
    /// non-fast-forward rejection (remote diverged / genuine conflict) —
    /// exercises the adopted-branch "needs a human" path (bead jleechan-tfs1)
    /// without ever touching a real `git` subprocess.
    pub fail_push_fix_commit_for: RefCell<Vec<String>>,
    pub calls: RefCell<Vec<String>>,
    /// Real-wall-clock HEAD SHA scripting for the reroll quiescence-timeout
    /// race tests: `(branch, [(instant, sha), ...])` sorted ascending by
    /// instant. `head_sha(branch)` returns the SHA of the last entry whose
    /// instant is `<= Instant::now()`; if no entry has elapsed yet, or the
    /// branch has no schedule, falls back to the static `heads` map. This
    /// lets a test simulate "the worker pushes a final commit at t=Xs",
    /// changing what `head_sha` reports mid-quiescence-poll without any
    /// fake/injected clock in the code under test — `reroll::execute` reads
    /// real `Instant::now()` throughout.
    pub head_sha_schedule: RefCell<HashMap<String, Vec<(std::time::Instant, String)>>>,
    /// Optional error to return from `head_sha` on every call for a given
    /// branch, modeling a `git rev-parse` failure mid-quiescence-check.
    pub fail_head_sha_for: RefCell<HashMap<String, String>>,
    /// Scripts `remote_head_sha(branch)`. Reuses the same `heads` map as
    /// `head_sha` (the fake doesn't model the local-vs-remote-tracking-ref
    /// distinction) — set `heads.insert(branch, sha)` to script both.
    ///
    /// Scripts `is_ancestor(ancestor_sha, descendant_sha)`: `(ancestor_sha,
    /// descendant_sha) -> bool`. Missing entries default to `true` (i.e.
    /// "no rewrite detected") so tests that don't exercise the force-push
    /// detector don't have to script it.
    pub ancestor_pairs: HashMap<(String, String), bool>,
}

impl FakeVcs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_push_fix_commit_for(&self, branch: &str) {
        self.fail_push_fix_commit_for
            .borrow_mut()
            .push(branch.to_string());
    }

    /// Script `head_sha(branch)` to return `sha_before` until real instant
    /// `at`, then `sha_after` from `at` onward — i.e. a single simulated
    /// push landing at wall-clock instant `at`. Call multiple times with
    /// ascending instants to script more than one push.
    pub fn schedule_head_sha(&self, branch: &str, at: std::time::Instant, sha: &str) {
        self.head_sha_schedule
            .borrow_mut()
            .entry(branch.to_string())
            .or_default()
            .push((at, sha.to_string()));
    }

    pub fn fail_head_sha_for(&self, branch: &str, message: &str) {
        self.fail_head_sha_for
            .borrow_mut()
            .insert(branch.to_string(), message.to_string());
    }
}

impl Vcs for FakeVcs {
    fn base_head(&self, base_branch: &str) -> Result<String, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("base_head({base_branch})"));
        self.heads
            .get(base_branch)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: format!("no scripted head for {base_branch}"),
            })
    }

    fn create_branch_at(&self, name: &str, sha: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("create_branch_at({name},{sha})"));
        Ok(())
    }

    /// jleechan-wuts / issue #349: per-repo variant of `base_head`.
    /// Default trait impl would delegate to `base_head`, which is keyed
    /// on branch name only — that collides across repos (a `main` in
    /// repo A and a `main` in repo B both resolve to the same key).
    /// The fake looks up `"<repo>@<branch>"` first (the form
    /// cross-repo tests seed) and falls back to the bare `<branch>`
    /// key (the form single-repo tests seed) — preserves existing
    /// test scripts without forcing a sweeping rewrite, while letting
    /// cross-repo tests opt into distinct per-repo fixtures.
    fn base_head_for_repo(&self, repo: &str, base_branch: &str) -> Result<String, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("base_head_for_repo({repo},{base_branch})"));
        let scoped_key = format!("{repo}@{base_branch}");
        if let Some(sha) = self.heads.get(&scoped_key) {
            return Ok(sha.clone());
        }
        self.heads
            .get(base_branch)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: format!("no scripted head for {scoped_key}"),
            })
    }

    /// jleechan-wuts / issue #349: per-repo variant of `create_branch_at`.
    /// Default trait impl would delegate to `create_branch_at` (which
    /// shells out to the daemon's local git), masking the cross-repo bug.
    /// The fake simply records the call so tests can assert that reroll
    /// routed through the per-repo entry point with the bead's repo.
    fn create_branch_at_for_repo(
        &self,
        repo: &str,
        name: &str,
        sha: &str,
    ) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("create_branch_at_for_repo({repo},{name},{sha})"));
        Ok(())
    }

    fn head_sha(&self, branch: &str) -> Result<String, DaemonError> {
        self.calls.borrow_mut().push(format!("head_sha({branch})"));
        if let Some(msg) = self.fail_head_sha_for.borrow().get(branch) {
            return Err(DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: msg.clone(),
            });
        }
        if let Some(schedule) = self.head_sha_schedule.borrow().get(branch) {
            let now = std::time::Instant::now();
            if let Some((_, sha)) = schedule.iter().rfind(|(at, _)| *at <= now) {
                return Ok(sha.clone());
            }
            // Schedule exists but nothing has elapsed yet — fall through to
            // the static map only if present, otherwise this is a genuine
            // "no head yet" scripting error (test forgot a baseline entry).
        }
        self.heads
            .get(branch)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: format!("no scripted head for {branch}"),
            })
    }

    fn is_remote_ahead(&self, branch: &str, remote_sha: &str) -> Result<bool, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("is_remote_ahead({branch},{remote_sha})"));
        // Empty / equal SHAs are never "ahead" — same contract as the real
        // CliVcs impl so tests don't have to script a false positive.
        let local = self
            .heads
            .get(branch)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: format!("no scripted head for {branch}"),
            })?;
        if local.is_empty() || remote_sha.is_empty() || local == remote_sha {
            return Ok(false);
        }
        Ok(self
            .remote_ahead
            .get(&(branch.to_string(), remote_sha.to_string()))
            .copied()
            .unwrap_or(false))
    }

    fn push_fix_commit(&self, branch: &str, message: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("push_fix_commit({branch},{message})"));
        if self
            .fail_push_fix_commit_for
            .borrow()
            .contains(&branch.to_string())
        {
            return Err(DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: format!(
                    "! [rejected] {branch} -> {branch} (non-fast-forward): scripted append-only push failure"
                ),
            });
        }
        Ok(())
    }

    fn remote_head_sha(&self, branch: &str) -> Result<String, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("remote_head_sha({branch})"));
        self.heads
            .get(branch)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: format!("no scripted remote head for {branch}"),
            })
    }

    fn is_ancestor(&self, ancestor_sha: &str, descendant_sha: &str) -> Result<bool, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("is_ancestor({ancestor_sha},{descendant_sha})"));
        if ancestor_sha == descendant_sha {
            return Ok(true);
        }
        Ok(self
            .ancestor_pairs
            .get(&(ancestor_sha.to_string(), descendant_sha.to_string()))
            .copied()
            .unwrap_or(true))
    }
}

/// Scripted `Llm` fake: returns the scripted response regardless of prompt
/// content (ZFC — the fake never inspects prompt text to branch behavior).
#[derive(Default)]
pub struct FakeLlm {
    pub response: RefCell<Option<Result<String, String>>>,
    pub calls: RefCell<Vec<String>>,
}

impl FakeLlm {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Llm for FakeLlm {
    fn judge(&self, prompt: &str) -> Result<String, DaemonError> {
        self.calls.borrow_mut().push(format!("judge({prompt})"));
        match self.response.borrow().as_ref() {
            Some(Ok(text)) => Ok(text.clone()),
            Some(Err(e)) => Err(DaemonError::Parse(e.clone())),
            None => Ok(String::new()),
        }
    }
}

type RejectionRecord = (String, String, String);

/// Scripted `StateStore` fake: in-memory overlay map + branch registry, plus a
/// call log. No SQLite involved — downstream tasks (dispatch, verifier) unit-test
/// against this instead of `SqliteStateStore` (design doc §3).
#[derive(Default)]
pub struct FakeStateStore {
    pub overlays: RefCell<HashMap<String, BeadOverlay>>,
    pub branches: RefCell<Vec<String>>,
    pub branch_beads: RefCell<HashMap<String, String>>,
    pub rejections: RefCell<HashMap<(String, u32), RejectionRecord>>,
    pub fail_save_for_state: RefCell<Vec<(String, OverlayState)>>,
    /// Bead jleechan-zeij / issue #322 r2: consecutive re-roll deferral count
    /// per bead. Persisted independently of `BeadOverlay` (mirrors the real
    /// `reroll_deferral_count` SQLite column), so the fail-closed defer/cap
    /// path can be driven across repeated `reroll::execute` calls in a test.
    pub reroll_deferrals: RefCell<HashMap<String, u32>>,
    pub calls: RefCell<Vec<String>>,
}

impl FakeStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_save_for(&self, bead_id: &str, state: OverlayState) {
        self.fail_save_for_state
            .borrow_mut()
            .push((bead_id.to_string(), state));
    }
}

impl StateStore for FakeStateStore {
    fn reconcile_dispatching(&self) -> Result<(), DaemonError> {
        for overlay in self.overlays.borrow_mut().values_mut() {
            if overlay.state == OverlayState::Dispatching {
                overlay.state = OverlayState::HumanHeld;
                set_human_hold_reason(overlay, HumanHoldReason::AmbiguousDispatchingRecovery);
            }
        }
        Ok(())
    }

    fn load(&self, bead_id: &str) -> Result<Option<BeadOverlay>, DaemonError> {
        self.calls.borrow_mut().push(format!("load({bead_id})"));
        Ok(self.overlays.borrow().get(bead_id).cloned())
    }

    fn save(&self, overlay: &BeadOverlay) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("save({})", overlay.bead_id));
        if self
            .fail_save_for_state
            .borrow()
            .iter()
            .any(|(bead_id, state)| bead_id == &overlay.bead_id && *state == overlay.state)
        {
            return Err(DaemonError::Tool {
                tool: "sqlite".into(),
                rc: -1,
                stderr: format!("scripted save failure for {}", overlay.state.as_str()),
            });
        }
        self.overlays
            .borrow_mut()
            .insert(overlay.bead_id.clone(), overlay.clone());
        Ok(())
    }

    fn register_branch(&self, bead_id: &str, branch: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("register_branch({bead_id},{branch})"));
        self.branches.borrow_mut().push(branch.to_string());
        self.branch_beads
            .borrow_mut()
            .insert(branch.to_string(), bead_id.to_string());
        Ok(())
    }

    fn bead_id_for_branch(&self, branch: &str) -> Result<Option<String>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("bead_id_for_branch({branch})"));
        Ok(self.branch_beads.borrow().get(branch).cloned())
    }

    fn owned_branches(&self) -> Result<Vec<String>, DaemonError> {
        self.calls.borrow_mut().push("owned_branches".into());
        Ok(self.branches.borrow().clone())
    }

    fn increment_active_autonomy(
        &self,
        elapsed_secs: u64,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("increment_active_autonomy({elapsed_secs})"));
        // Convenience override: mirror the new trait-level behavior of
        // `list_active_overlays` + per-row `bump_autonomy_secs`. Tests that
        // need the ci_pending pause (jleechan-54ky) should call
        // `list_active_overlays` directly and `bump_autonomy_secs` for the
        // rows they want to advance.
        let updated = self.list_active_overlays()?;
        if elapsed_secs > 0 {
            for overlay in &updated {
                self.bump_autonomy_secs(&overlay.bead_id, elapsed_secs)?;
            }
        }
        // Re-read so callers see the bumped values, matching the original
        // "increment then return" contract the tick loop's budget-warning
        // crossing check depends on.
        self.list_active_overlays()
    }

    fn list_active_overlays(&self) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.calls.borrow_mut().push("list_active_overlays".into());
        let mut out = Vec::new();
        for overlay in self.overlays.borrow().values() {
            if overlay.state == OverlayState::Dispatched || overlay.state == OverlayState::Attested
            {
                out.push(overlay.clone());
            }
        }
        Ok(out)
    }

    fn bump_autonomy_secs(&self, bead_id: &str, delta_secs: u64) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("bump_autonomy_secs({bead_id},{delta_secs})"));
        if delta_secs == 0 {
            return Ok(());
        }
        if let Some(overlay) = self.overlays.borrow_mut().get_mut(bead_id) {
            overlay.autonomy_secs += delta_secs;
        }
        Ok(())
    }

    fn recover_human_held(&self, max_attempt: u32) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("recover_human_held({max_attempt})"));
        let mut recovered = Vec::new();
        for overlay in self.overlays.borrow_mut().values_mut() {
            // Mirror the production allow-list and its durable no-session
            // proof so integration fakes cannot hide duplicate-spawn bugs.
            let is_permanent =
                is_permanent_human_hold_reason(overlay.park_reason.as_deref());
            if overlay.state == OverlayState::HumanHeld
                && overlay.attempt < max_attempt
                && !is_permanent
                && overlay.session_id.is_none()
            {
                overlay.state = OverlayState::Queued;
                overlay.attempt += 1;
                overlay.autonomy_secs = 0;
                overlay.park_reason = None;
                recovered.push(overlay.clone());
            }
        }
        Ok(recovered)
    }

    fn human_held_at_or_above_attempt(
        &self,
        max_attempt: u32,
    ) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("human_held_at_or_above_attempt({max_attempt})"));
        Ok(self
            .overlays
            .borrow()
            .values()
            .filter(|overlay| {
                overlay.state == OverlayState::HumanHeld && overlay.attempt >= max_attempt
            })
            .cloned()
            .collect())
    }

    fn reroll_deferral_count(&self, bead_id: &str) -> Result<u32, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("reroll_deferral_count({bead_id})"));
        Ok(self
            .reroll_deferrals
            .borrow()
            .get(bead_id)
            .copied()
            .unwrap_or(0))
    }

    fn incr_reroll_deferral(&self, bead_id: &str) -> Result<u32, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("incr_reroll_deferral({bead_id})"));
        let mut map = self.reroll_deferrals.borrow_mut();
        let count = map.entry(bead_id.to_string()).or_insert(0);
        *count += 1;
        Ok(*count)
    }

    fn reset_reroll_deferral(&self, bead_id: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("reset_reroll_deferral({bead_id})"));
        self.reroll_deferrals.borrow_mut().insert(bead_id.to_string(), 0);
        Ok(())
    }

    fn save_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
        reviewer: &str,
        feedback_hash: &str,
        feedback_text: &str,
    ) -> Result<(), DaemonError> {
        self.calls.borrow_mut().push(format!(
            "save_rejection({bead_id},{attempt},{reviewer},{feedback_hash})"
        ));
        self.rejections.borrow_mut().insert(
            (bead_id.to_string(), attempt),
            (
                reviewer.to_string(),
                feedback_hash.to_string(),
                feedback_text.to_string(),
            ),
        );
        Ok(())
    }

    fn load_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<Option<(String, String)>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("load_rejection({bead_id},{attempt})"));
        Ok(self
            .rejections
            .borrow()
            .get(&(bead_id.to_string(), attempt))
            .map(|(reviewer, feedback_hash, _feedback_text)| {
                (reviewer.clone(), feedback_hash.clone())
            }))
    }

    fn load_rejection_text(
        &self,
        bead_id: &str,
        attempt: u32,
    ) -> Result<Option<String>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("load_rejection_text({bead_id},{attempt})"));
        Ok(self
            .rejections
            .borrow()
            .get(&(bead_id.to_string(), attempt))
            .map(|(_, _, feedback_text)| feedback_text.clone()))
    }
}
