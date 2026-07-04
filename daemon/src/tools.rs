// Task 5: the only traits in the system (design doc §4). Each trait wraps exactly
// one external tool; production impls (Cli*) are thin `Command` wrappers sharing
// `run_tool`. Test fakes live in `daemon/tests/common/mod.rs` (scripted responses,
// call log, no subprocess use).
use crate::errors::DaemonError;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A `br` bead candidate (design doc §4, spec §4.2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub external_ref: Option<String>, // "<owner>/<repo>#<issue_number>", None = manual bead
}

/// A labeled GitHub issue as seen by the pre-poll normalizer (spec §4.2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author_login: String,
    pub external_ref: String, // "<owner>/<repo>#<issue_number>"
}

/// Collaborator permission tier, coarsened to the write-tier gate (spec §4.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    None,
    Read,
    Triage,
    Write,
    Admin,
}

impl Permission {
    /// Only `Write` or `Admin` may trigger dispatch (spec §4.2.3 write-tier minimum).
    pub fn is_write_tier(&self) -> bool {
        matches!(self, Permission::Write | Permission::Admin)
    }
}

/// One gate's read from the SCM, gathered for the 7/8-green verifier (spec §4.2.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSnapshot {
    pub pr_number: u64,
    pub ci_success: bool,
    pub mergeable: bool,
    pub coderabbit_approved: bool,
    pub bugbot_error_count: u32,
    pub unresolved_thread_count: u32,
    pub head_sha: String,
}

/// Parameters for spawning a new AO/`aow` session (design doc §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub bead_id: String,
    pub branch: String,
    pub prompt: String,
}

/// Opaque handle to an AO/`aow` session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

/// `br` CLI. `fetch_candidates` == `br list --status open --label factory --json`.
pub trait Tracker {
    fn fetch_candidates(&self) -> Result<Vec<Bead>, DaemonError>;
    fn create_bead(
        &self,
        title: &str,
        body: &str,
        external_ref: &str,
    ) -> Result<String, DaemonError>;
    fn comment_external(&self, external_ref: &str, body: &str) -> Result<(), DaemonError>;
}

/// `gh` CLI (REST + GraphQL). Every fetch goes through the ETag cache (spec §4.2.5).
pub trait Scm {
    fn labeled_issues(&self, label: &str) -> Result<Vec<Issue>, DaemonError>;
    fn collaborator_permission(&self, login: &str) -> Result<Permission, DaemonError>;
    fn pr_snapshot(&self, pr: u64) -> Result<PrSnapshot, DaemonError>;
    fn close_pr(&self, pr: u64, comment: &str) -> Result<(), DaemonError>;
}

/// `ao` / `aow` CLIs.
pub trait Sessions {
    fn active_count(&self) -> Result<usize, DaemonError>;
    fn spawn(&self, spec: &SpawnSpec) -> Result<SessionId, DaemonError>;
    fn attach(&self, branch: &str, bead_id: &str) -> Result<SessionId, DaemonError>;
    fn stop(&self, id: &SessionId) -> Result<(), DaemonError>;
    fn is_quiescent(&self, id: &SessionId) -> Result<bool, DaemonError>;
}

/// `git` CLI, always `git -C <workdir>`.
pub trait Vcs {
    fn base_head(&self, base_branch: &str) -> Result<String, DaemonError>;
    fn create_branch_at(&self, name: &str, sha: &str) -> Result<(), DaemonError>;
    fn head_sha(&self, branch: &str) -> Result<String, DaemonError>;
}

/// LLM judgment calls (router, in-place-vs-reroll verdict, constraint extraction).
/// ZFC: ALL judgment goes through here — no keyword/heuristic routing in callers.
pub trait Llm {
    fn judge(&self, prompt: &str) -> Result<String, DaemonError>;
}

/// Shared subprocess helper for every `Cli*` impl: spawn `cmd args...`, poll
/// `try_wait` every 100ms, kill the child and return `DaemonError::Timeout` if the
/// deadline elapses first; non-zero exit -> `DaemonError::Tool`; otherwise stdout
/// as a `String`.
pub fn run_tool(cmd: &str, args: &[&str], timeout_secs: u64) -> Result<String, DaemonError> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DaemonError::Tool {
            tool: cmd.to_string(),
            rc: -1,
            stderr: format!("spawn failed: {e}"),
        })?;

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let poll_interval = Duration::from_millis(100);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().map_err(|e| DaemonError::Tool {
                    tool: cmd.to_string(),
                    rc: -1,
                    stderr: format!("collect output failed: {e}"),
                })?;
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                if status.success() {
                    return Ok(stdout);
                }
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                return Err(DaemonError::Tool {
                    tool: cmd.to_string(),
                    rc: status.code().unwrap_or(-1),
                    stderr,
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(DaemonError::Timeout(format!(
                        "{cmd} exceeded {timeout_secs}s timeout"
                    )));
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                return Err(DaemonError::Tool {
                    tool: cmd.to_string(),
                    rc: -1,
                    stderr: format!("try_wait failed: {e}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn run_tool_success_captures_stdout() {
        let out = run_tool("true", &[], 5).unwrap();
        assert_eq!(out, "");
    }

    #[test]
    #[cfg(unix)]
    fn run_tool_nonzero_exit_is_tool_error() {
        let err = run_tool("false", &[], 5).unwrap_err();
        match err {
            DaemonError::Tool { rc, .. } => assert_eq!(rc, 1),
            other => panic!("expected Tool error, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn run_tool_timeout_kills_child() {
        let err = run_tool("sleep", &["2"], 1).unwrap_err();
        assert!(
            matches!(err, DaemonError::Timeout(_)),
            "expected Timeout, got {err:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn run_tool_echo_captures_output() {
        let out = run_tool("echo", &["hello"], 5).unwrap();
        assert_eq!(out.trim(), "hello");
    }
}
