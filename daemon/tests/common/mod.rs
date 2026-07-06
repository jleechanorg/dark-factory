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
    Bead, Issue, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs,
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
        Ok(self.candidates.borrow().clone())
    }

    fn fetch_all_external_refs(&self) -> Result<std::collections::HashSet<String>, DaemonError> {
        self.calls.borrow_mut().push("fetch_all_external_refs".into());
        let refs = self.candidates
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
        Ok(())
    }
}

/// Scripted `Scm` fake: pre-seeded issues/permissions/snapshots keyed by input.
#[derive(Default)]
pub struct FakeScm {
    pub issues: Vec<Issue>,
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
    pub calls: RefCell<Vec<String>>,
}

impl Default for FakeSessions {
    fn default() -> Self {
        Self {
            active_count: 0,
            next_session_id: "fake-session-1".into(),
            quiescent: true,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl FakeSessions {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Sessions for FakeSessions {
    fn active_count(&self) -> Result<usize, DaemonError> {
        self.calls.borrow_mut().push("active_count".into());
        Ok(self.active_count)
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError> {
        self.calls
            .borrow_mut()
            .push(format!("spawn({})", spec.bead_id));
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
        Ok(self.quiescent)
    }
}

/// Scripted `Vcs` fake: pre-seeded SHAs keyed by branch name.
#[derive(Default)]
pub struct FakeVcs {
    pub heads: HashMap<String, String>,
    pub calls: RefCell<Vec<String>>,
}

impl FakeVcs {
    pub fn new() -> Self {
        Self::default()
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
        self.heads
            .get(branch)
            .cloned()
            .ok_or_else(|| DaemonError::Tool {
                tool: "git".into(),
                rc: 1,
                stderr: format!("no scripted head for {branch}"),
            })
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
    pub calls: RefCell<Vec<String>>,
}

impl FakeStateStore {
    pub fn new() -> Self {
        Self::default()
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

    fn increment_active_autonomy(&self, elapsed_secs: u64) -> Result<Vec<BeadOverlay>, DaemonError> {
        self.calls.borrow_mut().push(format!("increment_active_autonomy({elapsed_secs})"));
        let mut updated = Vec::new();
        let mut overlays = self.overlays.borrow_mut();
        for overlay in overlays.values_mut() {
            if overlay.state == OverlayState::Dispatched || overlay.state == OverlayState::Attested {
                overlay.autonomy_secs += elapsed_secs;
                updated.push(overlay.clone());
            }
        }
        Ok(updated)
    }

    fn save_rejection(&self, bead_id: &str, attempt: u32, reviewer: &str, feedback_hash: &str, _feedback_text: &str) -> Result<(), DaemonError> {
        self.calls.borrow_mut().push(format!("save_rejection({bead_id},{attempt},{reviewer},{feedback_hash})"));
        self.rejections.borrow_mut().insert((bead_id.to_string(), attempt), (reviewer.to_string(), feedback_hash.to_string()));
        Ok(())
    }

    fn load_rejection(&self, bead_id: &str, attempt: u32) -> Result<Option<(String, String)>, DaemonError> {
        self.calls.borrow_mut().push(format!("load_rejection({bead_id},{attempt})"));
        Ok(self.rejections.borrow().get(&(bead_id.to_string(), attempt)).cloned())
    }
}
