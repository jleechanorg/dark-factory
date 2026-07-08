// Shared test-only scripted fakes for the five tool-boundary traits (design doc
// §4, TDD plan Task 5 Step 4). Each fake holds scripted responses plus a
// `RefCell<Vec<String>>` call log and performs NO subprocess use — downstream
// tasks (intake, router, dispatch, verifier) unit-test against these instead of
// real CLIs. Included by integration test files via `#[path = "common/mod.rs"]
// mod common;` (the idiomatic way to share a module across multiple files under
// `tests/`, since each file in `tests/` is its own separate crate).
#![allow(dead_code)]

use daemon::errors::DaemonError;
use daemon::state::{BeadOverlay, OverlayState, StateStore};
use daemon::tools::{
    Bead, Issue, LabeledPr, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec,
    Tracker, Vcs,
};
use std::cell::RefCell;
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

    fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("close_pr({pr},{comment})"));
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
}

/// Scripted `Sessions` fake: active-count + spawn/attach return a caller-set
/// `SessionId`, `is_quiescent` returns the scripted bool.
pub struct FakeSessions {
    pub active_count: usize,
    pub next_session_id: String,
    pub quiescent: bool,
    pub fail_spawn_for: RefCell<Vec<String>>,
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
}

impl Default for FakeSessions {
    fn default() -> Self {
        Self {
            active_count: 0,
            next_session_id: "fake-session-1".into(),
            quiescent: true,
            fail_spawn_for: RefCell::new(Vec::new()),
            fail_spawn_deferred_for: RefCell::new(Vec::new()),
            spawn_prompts: RefCell::new(Vec::new()),
            calls: RefCell::new(Vec::new()),
            branch_for: RefCell::new(HashMap::new()),
            terminal_at: RefCell::new(None),
            quiescence_check_error: RefCell::new(None),
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
        if self.fail_spawn_for.borrow().contains(&spec.bead_id) {
            return Err(DaemonError::Tool {
                tool: "ao".into(),
                rc: 1,
                stderr: format!("scripted spawn failure for {}", spec.bead_id),
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
        Ok(SessionId(self.next_session_id.clone()))
    }

    fn stop(&self, id: &SessionId) -> Result<(), DaemonError> {
        self.calls.borrow_mut().push(format!("stop({})", id.0));
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

    fn session_branch(&self, id: &SessionId) -> Result<Option<String>, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("session_branch({})", id.0));
        Ok(self.branch_for.borrow().get(&id.0).cloned())
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

/// Scripted `StateStore` fake: in-memory overlay map + branch registry, plus a
/// call log. No SQLite involved — downstream tasks (dispatch, verifier) unit-test
/// against this instead of `SqliteStateStore` (design doc §3).
#[derive(Default)]
pub struct FakeStateStore {
    pub overlays: RefCell<HashMap<String, BeadOverlay>>,
    pub branches: RefCell<Vec<String>>,
    pub branch_beads: RefCell<HashMap<String, String>>,
    pub rejections: RefCell<HashMap<(String, u32), (String, String)>>,
    pub fail_save_for_state: RefCell<Vec<(String, OverlayState)>>,
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
            if overlay.state == OverlayState::HumanHeld && overlay.attempt < max_attempt {
                overlay.state = OverlayState::Queued;
                overlay.attempt += 1;
                overlay.autonomy_secs = 0;
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

    fn save_rejection(
        &self,
        bead_id: &str,
        attempt: u32,
        reviewer: &str,
        feedback_hash: &str,
        _feedback_text: &str,
    ) -> Result<(), DaemonError> {
        self.calls.borrow_mut().push(format!(
            "save_rejection({bead_id},{attempt},{reviewer},{feedback_hash})"
        ));
        self.rejections.borrow_mut().insert(
            (bead_id.to_string(), attempt),
            (reviewer.to_string(), feedback_hash.to_string()),
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
            .cloned())
    }
}
