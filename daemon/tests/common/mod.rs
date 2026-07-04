// Shared test-only scripted fakes for the five tool-boundary traits (design doc
// §4, TDD plan Task 5 Step 4). Each fake holds scripted responses plus a
// `RefCell<Vec<String>>` call log and performs NO subprocess use — downstream
// tasks (intake, router, dispatch, verifier) unit-test against these instead of
// real CLIs. Included by integration test files via `#[path = "common/mod.rs"]
// mod common;` (the idiomatic way to share a module across multiple files under
// `tests/`, since each file in `tests/` is its own separate crate).
#![allow(dead_code)]

use daemon::errors::DaemonError;
use daemon::tools::{
    Bead, Issue, Llm, Permission, PrSnapshot, Scm, SessionId, Sessions, SpawnSpec, Tracker, Vcs,
};
use std::cell::RefCell;
use std::collections::HashMap;

/// Scripted `Tracker` fake: pre-seeded candidates + a call log of every method
/// invocation (method name + key args), so tests can assert both output and
/// call shape (e.g. "create_bead called exactly once").
#[derive(Default)]
pub struct FakeTracker {
    pub candidates: Vec<Bead>,
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
        Ok(self.candidates.clone())
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
        match self.create_bead_result.borrow().as_ref() {
            Some(Ok(id)) => Ok(id.clone()),
            Some(Err(e)) => Err(DaemonError::Tool {
                tool: "br".into(),
                rc: 1,
                stderr: e.clone(),
            }),
            None => Ok("fake-bead-1".into()),
        }
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
