//! Resolve and provision the checkout used by execution-time gates.
//!
//! The daemon binary may be installed from an immutable uv/release location,
//! so gate code must never infer a repository from its own current directory.
//! This module owns the small amount of git plumbing needed to create the
//! configured isolated checkout when it has not been created yet, plus the
//! preflight self-heal that lets the verifier gate recover from stale
//! index locks, orphaned `.git/` state, dirty untracked caches, or missing
//! branches without parking the bead `HUMAN_HELD` (bead jleechan-y189).

use crate::errors::DaemonError;
use crate::tools::{remote_url_matches_repo, run_tool, run_tool_in_dir};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The lock inode is deliberately persistent.  Kernel-owned `flock` state is
/// released when the process exits, while removing/recreating the pathname
/// during a clone would let a waiter acquire a different inode and race the
/// publisher.
struct TargetWorktreeLock {
    file: File,
}

impl TargetWorktreeLock {
    fn acquire(target: &Path) -> Result<Self, DaemonError> {
        let lock_path = target.with_extension("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                DaemonError::Config(format!(
                    "open target worktree lock {}: {error}",
                    lock_path.display()
                ))
            })?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            const LOCK_EX: i32 = 2;
            if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
                return Err(DaemonError::Config(format!(
                    "acquire target worktree lock {}: {}",
                    lock_path.display(),
                    std::io::Error::last_os_error()
                )));
            }
        }
        Ok(Self { file })
    }
}

impl Drop for TargetWorktreeLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            const LOCK_UN: i32 = 8;
            let _ = unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
        }
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

/// Reuse an existing target checkout or provision one by cloning the named
/// GitHub repository into `requested`.
///
/// A missing checkout is intentionally created outside the daemon release
/// tree. `head_sha` is optional because callers that only need a source root
/// can provision the checkout first and resolve a revision later. Existing
/// directories are accepted only after their `origin` URL matches `repo`; a
/// supplied `head_sha` must also match the checked-out HEAD exactly. Mismatch
/// is a hard error rather than an invitation to reset an operator checkout.
pub fn ensure_target_worktree(
    repo: &str,
    requested: &Path,
    head_sha: Option<&str>,
) -> Result<PathBuf, DaemonError> {
    ensure_target_worktree_inner(repo, requested, head_sha, false)
}

/// Reuse a daemon-owned target checkout, refreshing a stale clean checkout to
/// `head_sha` when necessary. This is deliberately separate from
/// [`ensure_target_worktree`]: configured operator checkouts must remain
/// fail-closed rather than being moved by the daemon.
pub fn ensure_managed_target_worktree(
    repo: &str,
    requested: &Path,
    head_sha: Option<&str>,
) -> Result<PathBuf, DaemonError> {
    ensure_target_worktree_inner(repo, requested, head_sha, true)
}

/// Like [`ensure_target_worktree`] but runs the [`self_heal_target_worktree`]
/// preflight first. The heal pass is given an `is_live` predicate supplied by
/// the caller (typically an AO session lookup for the bead's branch). When
/// the predicate reports a live in-flight session, the helper surfaces a
/// structured `WorktreeHealOutcome::RefusedLive` report and the caller
/// decides whether to escalate; for every other broken-but-recoverable
/// state, the heal pass clears the failure mode before the existing
/// ensure pipeline runs, so the verifier gate never parks a bead
/// `HUMAN_HELD` just because a stale `index.lock` survived a crash
/// (bead jleechan-y189).
pub fn ensure_target_worktree_with_heal<F>(
    repo: &str,
    requested: &Path,
    head_sha: Option<&str>,
    expected_branch: &str,
    is_live: F,
) -> Result<(PathBuf, WorktreeHealReport), DaemonError>
where
    F: Fn(&Path) -> Result<bool, DaemonError>,
{
    let report = self_heal_target_worktree(repo, requested, expected_branch, false, is_live)?;
    if matches!(report.outcome, WorktreeHealOutcome::RefusedLive(_)) {
        return Ok((requested.to_path_buf(), report));
    }
    let path = ensure_target_worktree_inner(repo, requested, head_sha, false)?;
    Ok((path, report))
}

/// Managed-checkout counterpart to
/// [`ensure_target_worktree_with_heal`]. Same contract, but destructive
/// recovery (e.g. `git reset --hard` on dirty tracked state) is enabled
/// because the daemon owns the directory.
pub fn ensure_managed_target_worktree_with_heal<F>(
    repo: &str,
    requested: &Path,
    head_sha: Option<&str>,
    expected_branch: &str,
    is_live: F,
) -> Result<(PathBuf, WorktreeHealReport), DaemonError>
where
    F: Fn(&Path) -> Result<bool, DaemonError>,
{
    let report = self_heal_target_worktree(repo, requested, expected_branch, true, is_live)?;
    if matches!(report.outcome, WorktreeHealOutcome::RefusedLive(_)) {
        return Ok((requested.to_path_buf(), report));
    }
    let path = ensure_target_worktree_inner(repo, requested, head_sha, true)?;
    Ok((path, report))
}

/// Ensure a daemon-owned checkout exposes the configured push-remote name.
///
/// Managed checkouts are cloned with Git's conventional `origin` remote, but
/// a repository routing entry may deliberately use another remote name. AO
/// creates its worker worktrees from this checkout, so the alias must exist
/// before spawn. Existing aliases are never rewritten: they must already
/// point at the verified target repository. This helper is intentionally not
/// used for operator-owned checkouts.
pub fn ensure_managed_push_remote(
    repo: &str,
    path: &Path,
    remote_name: &str,
) -> Result<(), DaemonError> {
    validate_repo(repo)?;
    if remote_name.is_empty() || remote_name.starts_with('-') {
        return Err(DaemonError::Config(format!(
            "configured push remote name is invalid: {remote_name:?}"
        )));
    }
    verify_origin(repo, path)?;
    if remote_name == "origin" {
        return Ok(());
    }

    // `dispatch_ready` may process several beads for the same repository in
    // parallel. Serialize the inspect-or-add sequence with target checkout
    // provisioning so two spawns cannot both observe a missing alias and
    // race to add it.
    let _lock = TargetWorktreeLock::acquire(path)?;

    let path_str = path.to_string_lossy();
    let remotes = run_tool_in_dir("git", &["remote"], &path_str, 30)?;
    if remotes.lines().any(|name| name == remote_name) {
        let url = run_tool_in_dir(
            "git",
            &["remote", "get-url", "--push", remote_name],
            &path_str,
            30,
        )?;
        if remote_url_matches_repo(&url, repo) != Some(true) {
            return Err(DaemonError::Config(format!(
                "managed target worktree {} has push remote {remote_name:?} that does not match {repo}",
                path.display()
            )));
        }
        return Ok(());
    }

    let origin_push_url = run_tool_in_dir(
        "git",
        &["remote", "get-url", "--push", "origin"],
        &path_str,
        30,
    )?;
    if remote_url_matches_repo(&origin_push_url, repo) != Some(true) {
        return Err(DaemonError::Config(format!(
            "managed target worktree {} has origin push URL that does not match {repo}",
            path.display()
        )));
    }
    run_tool_in_dir(
        "git",
        &["remote", "add", remote_name, origin_push_url.trim()],
        &path_str,
        30,
    )?;
    Ok(())
}

fn ensure_target_worktree_inner(
    repo: &str,
    requested: &Path,
    head_sha: Option<&str>,
    refresh_stale: bool,
) -> Result<PathBuf, DaemonError> {
    validate_repo(repo)?;
    if !requested.is_absolute() {
        return Err(DaemonError::Config(format!(
            "target worktree path must be absolute: {}",
            requested.display()
        )));
    }
    if let Some(parent) = requested.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            DaemonError::Config(format!(
                "create target worktree parent {}: {e}",
                parent.display()
            ))
        })?;
    }
    let _lock = TargetWorktreeLock::acquire(requested)?;

    if requested.is_dir() {
        verify_origin(repo, requested)?;
        if refresh_stale {
            refresh_existing_if_stale(requested, head_sha)?;
        } else {
            verify_head(requested, head_sha)?;
        }
        return Ok(requested.to_path_buf());
    }
    if requested.exists() {
        return Err(DaemonError::Config(format!(
            "target worktree path exists but is not a directory: {}",
            requested.display()
        )));
    }

    let staging = staging_path(requested);
    let staging_str = staging.to_str().ok_or_else(|| {
        DaemonError::Config(format!(
            "target worktree staging path is not valid UTF-8: {}",
            staging.display()
        ))
    })?;
    let remote = format!("https://github.com/{repo}.git");
    let clone_result = if head_sha.is_some() {
        let args = [
            "clone",
            "--no-checkout",
            "--filter=blob:none",
            remote.as_str(),
            staging_str,
        ];
        run_tool("git", &args, 600)
    } else {
        let args = ["clone", "--filter=blob:none", remote.as_str(), staging_str];
        run_tool("git", &args, 600)
    };
    if let Err(err) = clone_result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }

    if let Some(sha) = head_sha.filter(|sha| !sha.trim().is_empty()) {
        let fetch_args = ["fetch", "--depth=1", "origin", sha];
        if let Err(err) = run_tool_in_dir("git", &fetch_args, staging_str, 600) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        let checkout_args = ["checkout", "--detach", sha];
        if let Err(err) = run_tool_in_dir("git", &checkout_args, staging_str, 60) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
    }
    if let Err(error) = std::fs::rename(&staging, requested) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(DaemonError::Config(format!(
            "atomically publish target worktree {}: {error}",
            requested.display()
        )));
    }
    Ok(requested.to_path_buf())
}

/// Validate an AO-created worker workspace without provisioning or mutating it.
/// A worker workspace must already exist; an absent path is never cloned as a
/// side effect of accepting a session.
pub fn validate_existing_target_worktree(
    repo: &str,
    path: &Path,
    head_sha: Option<&str>,
) -> Result<PathBuf, DaemonError> {
    validate_repo(repo)?;
    if !path.is_absolute() {
        return Err(DaemonError::Config(format!(
            "worker workspace path must be absolute: {}",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(DaemonError::Config(format!(
            "worker workspace path is not a directory: {}",
            path.display()
        )));
    }
    verify_existing(repo, path, head_sha)?;
    Ok(path.to_path_buf())
}

fn staging_path(requested: &Path) -> PathBuf {
    let nonce = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = requested
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("target-worktree");
    requested.with_file_name(format!(".{name}.staging.{}.{}", std::process::id(), nonce))
}

fn verify_existing(repo: &str, path: &Path, head_sha: Option<&str>) -> Result<(), DaemonError> {
    verify_origin(repo, path)?;
    verify_head(path, head_sha)
}

fn verify_origin(repo: &str, path: &Path) -> Result<(), DaemonError> {
    let path_str = path.to_string_lossy();
    let remote = run_tool_in_dir("git", &["remote", "get-url", "origin"], &path_str, 30)?;
    if remote_url_matches_repo(&remote, repo) != Some(true) {
        return Err(DaemonError::Config(format!(
            "target worktree {} has origin {:?}, not {repo}",
            path.display(),
            remote.trim()
        )));
    }
    Ok(())
}

fn verify_head(path: &Path, head_sha: Option<&str>) -> Result<(), DaemonError> {
    let path_str = path.to_string_lossy();
    if let Some(expected) = head_sha.filter(|sha| !sha.trim().is_empty()) {
        let actual = run_tool_in_dir("git", &["rev-parse", "HEAD"], &path_str, 30)?;
        if actual.trim() != expected {
            return Err(DaemonError::Config(format!(
                "target worktree {} is at HEAD {}, expected snapshot {}",
                path.display(),
                actual.trim(),
                expected
            )));
        }
    }
    Ok(())
}

fn refresh_existing_if_stale(path: &Path, head_sha: Option<&str>) -> Result<(), DaemonError> {
    let Some(expected) = head_sha.filter(|sha| !sha.trim().is_empty()) else {
        return Ok(());
    };
    let path_str = path.to_string_lossy();
    let actual = run_tool_in_dir("git", &["rev-parse", "HEAD"], &path_str, 30)?;
    if actual.trim() == expected {
        return Ok(());
    }
    let status = run_tool_in_dir(
        "git",
        &["status", "--porcelain", "--untracked-files=all"],
        &path_str,
        30,
    )?;
    if !status.trim().is_empty() {
        return Err(DaemonError::Config(format!(
            "managed target worktree {} has uncommitted changes; refusing to refresh stale snapshot",
            path.display()
        )));
    }
    run_tool_in_dir(
        "git",
        &["fetch", "--depth=1", "origin", expected],
        &path_str,
        600,
    )?;
    run_tool_in_dir(
        "git",
        &["checkout", "--no-overwrite-ignore", "--detach", expected],
        &path_str,
        60,
    )?;
    verify_head(path, Some(expected))
}

/// Outcome of a worktree self-heal attempt.
///
/// `RefusedLive` is the ONLY refusal the daemon surfaces from this path —
///// it means the caller-supplied `is_live` predicate observed an active
/// process or AO session executing on the worktree and the daemon
/// therefore will not touch the directory. Anything else resolves to
/// either `Healed` (one or more recovery actions ran) or `AlreadyClean`
/// (no action was needed). Neither `Healed` nor `AlreadyClean` is an
/// invitation to escalate to `HUMAN_HELD`; that gate is reserved for the
/// genuine in-flight-live case (bead jleechan-y189).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeHealOutcome {
    /// One or more recovery actions were applied; the caller may proceed.
    Healed,
    /// The worktree was inspected and required no recovery.
    AlreadyClean,
    /// A live in-flight PID or AO session is executing on this worktree;
    /// the daemon will not touch it. The string carries the rejection
    /// reason for telemetry.
    RefusedLive(String),
}

/// One recovery action applied during a self-heal pass. Multiple actions
/// may apply in a single pass (e.g. clear stale lock AND clean caches).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeHealAction {
    ClearedStaleIndexLock,
    CleanedUntrackedCaches,
    ResetDirtyTree,
    /// A missing target branch was recreated from the current HEAD so the
    /// downstream `ensure_*` call can check it out. The string is the
    /// branch name that was provisioned.
    ProvisionedFallbackBranch(String),
}

/// What the self-heal pass did, exposed to callers (tick.rs, dispatch.rs)
/// so they can decide whether to continue or escalate. The action journal
/// is also useful for telemetry: it gives operators a trace of which
/// recovery steps actually fired.
#[derive(Debug, Clone)]
pub struct WorktreeHealReport {
    pub outcome: WorktreeHealOutcome,
    pub actions: Vec<WorktreeHealAction>,
}

/// Preflight a target worktree and recover from the kinds of broken state
/// the daemon has historically been parking `HUMAN_HELD` for (stale
/// `index.lock`, leftover untracked caches, dirty tracked edits, missing
/// branch, corrupt `.git/`).
///
/// `is_live` is the caller's hook into "is an active process or AO session
/// currently executing on this worktree?". The self-heal function will not
/// touch the directory when `is_live(path)?` returns `true` — that's the
/// single refusal reason allowed for this layer. A `false` return (or any
/// error) is treated as "not live, proceed with recovery" so an
/// unreliable probe (e.g. a transient AO CLI failure) cannot block the
/// daemon from healing broken checkouts.
///
/// `is_managed = true` allows destructive recovery (e.g. `git reset
/// --hard` on dirty tracked state). Operator-owned checkouts (`is_managed
/// = false`) keep the existing fail-closed behaviour: only the safe
/// recovery steps (lock/cache cleanup, missing-branch provisioning) apply.
pub fn self_heal_target_worktree<F>(
    repo: &str,
    requested: &Path,
    expected_branch: &str,
    is_managed: bool,
    is_live: F,
) -> Result<WorktreeHealReport, DaemonError>
where
    F: Fn(&Path) -> Result<bool, DaemonError>,
{
    validate_repo(repo)?;
    if !requested.is_absolute() {
        return Err(DaemonError::Config(format!(
            "target worktree path must be absolute: {}",
            requested.display()
        )));
    }
    if !requested.exists() {
        // Nothing to heal: ensure_* will provision from scratch.
        return Ok(WorktreeHealReport {
            outcome: WorktreeHealOutcome::AlreadyClean,
            actions: Vec::new(),
        });
    }
    if !requested.is_dir() {
        return Err(DaemonError::Config(format!(
            "target worktree path exists but is not a directory: {}",
            requested.display()
        )));
    }

    // Refusal is the only non-recoverable signal here. A probe error must
    // NOT block recovery: a flaky AO CLI would otherwise pin the bead in
    // HUMAN_HELD for a broken-but-recoverable checkout (the very failure
    // mode jleechan-y189 fixes).
    let live = match is_live(requested) {
        Ok(flag) => flag,
        Err(error) => {
            return Ok(WorktreeHealReport {
                outcome: WorktreeHealOutcome::RefusedLive(format!(
                    "live-session probe failed: {error}"
                )),
                actions: Vec::new(),
            });
        }
    };
    if live {
        return Ok(WorktreeHealReport {
            outcome: WorktreeHealOutcome::RefusedLive(format!(
                "active session or process is executing on {}",
                requested.display()
            )),
            actions: Vec::new(),
        });
    }

    let mut actions = Vec::new();

    // (1) Stale `.git/index.lock`. The only way this file exists after a
    // crash or unexpected exit; removing it is always safe because no
    // other live process holds the inode (we already checked is_live).
    let index_lock = requested.join(".git").join("index.lock");
    if index_lock.is_file() {
        std::fs::remove_file(&index_lock).map_err(|error| {
            DaemonError::Config(format!(
                "remove stale index lock {}: {error}",
                index_lock.display()
            ))
        })?;
        actions.push(WorktreeHealAction::ClearedStaleIndexLock);
    }

    // (2) Untracked caches that are safe to drop. These are directories
    // the daemon or a previous run may have left behind; they are not
    // source-controlled and not operator-owned. .git/objects/pack/*.idx
    // backups under a `.stale-pack` subfolder are also dropped.
    let cache_dirs: [(&str, &str); 4] = [
        (".cache", "leftover untracked .cache/"),
        (".pytest_cache", "leftover pytest bytecode cache"),
        ("target", "leftover cargo target/ build cache"),
        ("node_modules", "leftover node_modules/ install cache"),
    ];
    let mut removed_any_cache = false;
    for (name, label) in cache_dirs {
        let dir = requested.join(name);
        if dir.is_dir() {
            // SAFETY: never recurse into a directory whose top-level
            // looks like a git worktree subdir or a `.git` itself.
            if dir.join(".git").is_dir() {
                continue;
            }
            std::fs::remove_dir_all(&dir).map_err(|error| {
                DaemonError::Config(format!("remove {label} at {}: {error}", dir.display()))
            })?;
            removed_any_cache = true;
        }
    }
    if removed_any_cache {
        actions.push(WorktreeHealAction::CleanedUntrackedCaches);
    }

    // (3) Dirty tracked state — only managed checkouts. Operator-owned
    // checkouts may contain real uncommitted work; we MUST NOT clobber
    // them. If a managed checkout is dirty we reset --hard; otherwise
    // we leave it to the downstream `ensure_*` to fail-closed with the
    // existing "uncommitted changes" error (which itself parks only on
    // managed refresh, not on initial preflight).
    if is_managed {
        let path_str = requested.to_string_lossy();
        let status = run_tool_in_dir(
            "git",
            &["status", "--porcelain", "--untracked-files=normal"],
            &path_str,
            30,
        )?;
        let porcelain = status.trim();
        if !porcelain.is_empty() {
            // Confirm the change set is purely tracked-file modifications
            // before we reset; refuse to clobber an operator's actual
            // branch even on a managed checkout.
            let mut safe_to_reset = true;
            for line in porcelain.lines() {
                // `XY path` — only X in { ,M,A,D,R,C} (index) or Y in
                // { ,M,D} (worktree) is safe to discard. Anything else
                // (untracked ?, ignored !, conflict U) needs an explicit
                // safe-path decision we deliberately don't make here.
                let bytes = line.as_bytes();
                if bytes.len() < 4 {
                    safe_to_reset = false;
                    break;
                }
                let x = bytes[0];
                let y = bytes[1];
                let safe = |c: u8| matches!(c, b' ' | b'M' | b'A' | b'D' | b'R' | b'C');
                if !(safe(x) && safe(y)) {
                    safe_to_reset = false;
                    break;
                }
            }
            if safe_to_reset {
                run_tool_in_dir(
                    "git",
                    &[
                        "-c",
                        "user.email=jleechan2015@users.noreply.github.com",
                        "-c",
                        "user.name=dark-factory-self-heal",
                        "reset",
                        "--hard",
                        "HEAD",
                    ],
                    &path_str,
                    60,
                )?;
                // Re-run git clean -fdx to drop ignored + untracked that
                // remained after the cache-dir sweep.
                let _ = run_tool_in_dir("git", &["clean", "-fdx"], &path_str, 60);
                actions.push(WorktreeHealAction::ResetDirtyTree);
            }
        }
    }

    // (4) Missing target branch. Provision it from HEAD so the downstream
    // ensure_* can check it out without refusing. Applies to both managed
    // and operator-owned checkouts; creating a branch never destroys
    // existing work.
    if !expected_branch.trim().is_empty() {
        let path_str = requested.to_string_lossy();
        let listing = run_tool_in_dir(
            "git",
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            &path_str,
            30,
        )?;
        let has_branch = listing
            .lines()
            .any(|line| line.trim() == expected_branch.trim());
        if !has_branch {
            run_tool_in_dir(
                "git",
                &["checkout", "-B", expected_branch, "HEAD"],
                &path_str,
                60,
            )?;
            actions.push(WorktreeHealAction::ProvisionedFallbackBranch(
                expected_branch.to_string(),
            ));
        }
    }

    // (5) Corrupt `.git/` — detected by `git rev-parse HEAD` failing.
    // We can only run rev-parse after the lock is cleared.
    // (5) NOTE: a corrupt `.git/` (rev-parse HEAD fails) is intentionally
    // out of scope here. The bead spec asks the preflight to clear stale
    // locks, clean untracked caches, reset dirty state, and provision a
    // missing branch. A truly corrupt git directory is a separate failure
    // class that the downstream `ensure_*_target_worktree` family surfaces
    // as a hard error — self-deleting a checkout is too aggressive for
    // this layer and is reserved for an explicit operator action.

    let outcome = if actions.is_empty() {
        WorktreeHealOutcome::AlreadyClean
    } else {
        WorktreeHealOutcome::Healed
    };
    Ok(WorktreeHealReport { outcome, actions })
}

fn validate_repo(repo: &str) -> Result<(), DaemonError> {
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || [owner, name]
            .iter()
            .any(|part| *part == "." || *part == ".." || part.contains(['\\', ':']))
    {
        return Err(DaemonError::Config(format!(
            "target repository must be <owner>/<name>, got {repo:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_existing_directory_when_origin_and_head_match() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_existing_{}",
            std::process::id()
        ));
        let head = init_git_checkout(&root, "owner/repo");
        let resolved = ensure_target_worktree("owner/repo", &root, Some(&head)).unwrap();
        assert_eq!(resolved, root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_existing_checkout_with_wrong_origin() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_mismatch_{}",
            std::process::id()
        ));
        let head = init_git_checkout(&root, "other/repo");
        let err = ensure_managed_target_worktree("owner/repo", &root, Some(&head)).unwrap_err();
        assert!(err.to_string().contains("origin"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_existing_checkout_at_stale_snapshot_head() {
        let root =
            std::env::temp_dir().join(format!("afd_target_worktree_stale_{}", std::process::id()));
        let actual = init_git_checkout(&root, "owner/repo");
        let stale = "0000000000000000000000000000000000000000";
        assert_ne!(actual, stale);
        let err = ensure_target_worktree("owner/repo", &root, Some(stale)).unwrap_err();
        assert!(err.to_string().contains("expected snapshot"));
        let after =
            run_tool_in_dir("git", &["rev-parse", "HEAD"], &root.to_string_lossy(), 30).unwrap();
        assert_eq!(after.trim(), actual);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_checkout_adds_missing_configured_push_remote_as_origin_alias() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_remote_alias_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");

        ensure_managed_push_remote("owner/repo", &root, "deployment").unwrap();
        let url = run_tool_in_dir(
            "git",
            &["remote", "get-url", "--push", "deployment"],
            &root.to_string_lossy(),
            30,
        )
        .unwrap();
        assert_eq!(
            url.trim(),
            "https://github.com/owner/repo.git",
            "the worker source must expose the configured remote name"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_managed_remote_alias_provisioning_is_serialized() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_remote_concurrent_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let root = root.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                ensure_managed_push_remote("owner/repo", &root, "deployment")
            }));
        }
        for worker in workers {
            worker
                .join()
                .expect("alias worker must not panic")
                .expect("both racing callers must observe a valid alias");
        }
        let url = run_tool_in_dir(
            "git",
            &["remote", "get-url", "--push", "deployment"],
            &root.to_string_lossy(),
            30,
        )
        .unwrap();
        assert_eq!(url.trim(), "https://github.com/owner/repo.git");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_checkout_refuses_to_overwrite_mismatched_configured_push_remote() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_remote_mismatch_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");
        run_tool_in_dir(
            "git",
            &[
                "remote",
                "add",
                "deployment",
                "https://github.com/other/repo.git",
            ],
            &root.to_string_lossy(),
            30,
        )
        .unwrap();

        let error = ensure_managed_push_remote("owner/repo", &root, "deployment")
            .expect_err("a configured remote must never be rewritten");
        assert!(error.to_string().contains("does not match owner/repo"));
        let url = run_tool_in_dir(
            "git",
            &["remote", "get-url", "--push", "deployment"],
            &root.to_string_lossy(),
            30,
        )
        .unwrap();
        assert_eq!(url.trim(), "https://github.com/other/repo.git");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refreshes_clean_managed_checkout_to_stale_snapshot() {
        if std::env::var_os("AFD_TARGET_REFRESH_HELPER").is_some() {
            run_refresh_child(false, false);
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "target_worktree::tests::refreshes_clean_managed_checkout_to_stale_snapshot",
                "--nocapture",
            ])
            .env("AFD_TARGET_REFRESH_HELPER", "1")
            .env_remove("AFD_TARGET_REFRESH_DIRTY")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn refuses_to_refresh_dirty_managed_checkout() {
        if std::env::var_os("AFD_TARGET_REFRESH_HELPER").is_some() {
            run_refresh_child(true, false);
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "target_worktree::tests::refuses_to_refresh_dirty_managed_checkout",
                "--nocapture",
            ])
            .env("AFD_TARGET_REFRESH_HELPER", "1")
            .env("AFD_TARGET_REFRESH_DIRTY", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn refuses_refresh_when_checkout_would_overwrite_ignored_artifact() {
        if std::env::var_os("AFD_TARGET_REFRESH_HELPER").is_some() {
            run_refresh_child(false, true);
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "target_worktree::tests::refuses_refresh_when_checkout_would_overwrite_ignored_artifact",
                "--nocapture",
            ])
            .env("AFD_TARGET_REFRESH_HELPER", "1")
            .env_remove("AFD_TARGET_REFRESH_DIRTY")
            .status()
            .unwrap();
        assert!(status.success());
    }

    // -------------------------------------------------------------------
    // jleechan-y189: self-healing preflight (red-phase coverage).
    //
    // These tests exercise the new `self_heal_target_worktree` helper that
    // the verifier gate calls *before* `ensure_target_worktree` /
    // `ensure_managed_target_worktree`. The contract:
    //
    //   * Stale `.git/index.lock`, untracked caches, dirty state, and a
    //     missing target branch are all auto-recoverable.
    //   * The ONLY refusal reason is "an active live PID or AO session is
    //     currently executing on that worktree" — surfaced through the
    //     caller-supplied `is_live` predicate so the function is pure.
    //   * Stale, orphaned, collided, or broken worktrees must NEVER park
    //     HUMAN_HELD on their own; only an active live in-flight session
    //     keeps that escalation open.
    // -------------------------------------------------------------------

    fn not_live(_path: &Path) -> Result<bool, DaemonError> {
        Ok(false)
    }

    fn always_live(_path: &Path) -> Result<bool, DaemonError> {
        Ok(true)
    }

    fn make_untracked_cache(root: &Path) -> std::path::PathBuf {
        let cache = root.join(".cache").join("stale-build");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("leftover.o"), b"junk").unwrap();
        cache
    }

    fn touch_stale_index_lock(root: &Path) -> std::path::PathBuf {
        let lock = root.join(".git").join("index.lock");
        std::fs::write(&lock, b"stale lock from prior crashed git").unwrap();
        lock
    }

    fn dirty_tracked_file(root: &Path) -> std::path::PathBuf {
        let tracked = root.join("tracked.txt");
        std::fs::write(&tracked, b"original content\n").unwrap();
        // Commit it so subsequent edits register as tracked dirty edits,
        // not as untracked files (which the heal pass deliberately leaves
        // alone — untracked state may be real operator work).
        run_tool_in_dir(
            "git",
            &[
                "-c",
                "user.email=jleechan2015@users.noreply.github.com",
                "-c",
                "user.name=Test",
                "add",
                "tracked.txt",
            ],
            &root.to_string_lossy(),
            30,
        )
        .unwrap();
        run_tool_in_dir(
            "git",
            &[
                "-c",
                "user.email=jleechan2015@users.noreply.github.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "track file",
            ],
            &root.to_string_lossy(),
            30,
        )
        .unwrap();
        std::fs::write(&tracked, b"modified by failed run").unwrap();
        tracked
    }

    #[test]
    fn self_heal_removes_stale_index_lock_on_existing_checkout() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_stale_lock_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let head = init_git_checkout(&root, "owner/repo");
        let lock = touch_stale_index_lock(&root);
        assert!(lock.is_file(), "precondition: index.lock was created");

        let report =
            self_heal_target_worktree("owner/repo", &root, "main", true, not_live).unwrap();
        assert_eq!(report.outcome, WorktreeHealOutcome::Healed);
        assert!(report
            .actions
            .iter()
            .any(|a| matches!(a, WorktreeHealAction::ClearedStaleIndexLock)));
        assert!(!lock.exists(), "stale index.lock must be removed");
        // HEAD itself must remain intact (no spurious reset).
        let after =
            run_tool_in_dir("git", &["rev-parse", "HEAD"], &root.to_string_lossy(), 30).unwrap();
        assert_eq!(after.trim(), head);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_heal_cleans_untracked_caches_under_known_safe_names() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_cache_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");
        let cache = make_untracked_cache(&root);
        assert!(cache.is_file() || cache.join("leftover.o").is_file());

        let report =
            self_heal_target_worktree("owner/repo", &root, "main", true, not_live).unwrap();
        assert_eq!(report.outcome, WorktreeHealOutcome::Healed);
        assert!(report
            .actions
            .iter()
            .any(|a| matches!(a, WorktreeHealAction::CleanedUntrackedCaches)));
        assert!(!cache.join("leftover.o").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_heal_resets_dirty_managed_checkout_to_head() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_dirty_managed_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let _ = init_git_checkout(&root, "owner/repo");
        let tracked = dirty_tracked_file(&root);
        // Capture HEAD *after* the tracked-file commit: that is the
        // baseline `git reset --hard` must restore to.
        let head_after_track =
            run_tool_in_dir("git", &["rev-parse", "HEAD"], &root.to_string_lossy(), 30).unwrap();
        // Confirm the precondition: the file is tracked and modified.
        assert_ne!(
            std::fs::read_to_string(&tracked).unwrap(),
            "original content\n",
            "precondition: tracked file must be dirty before heal"
        );

        let report =
            self_heal_target_worktree("owner/repo", &root, "main", true, not_live).unwrap();
        assert_eq!(report.outcome, WorktreeHealOutcome::Healed);
        assert!(report
            .actions
            .iter()
            .any(|a| matches!(a, WorktreeHealAction::ResetDirtyTree)));
        // `git reset --hard HEAD` restores the file's *content* to HEAD;
        // the file itself still exists because it is tracked.
        assert!(
            tracked.exists(),
            "tracked file must still exist after reset"
        );
        assert_eq!(
            std::fs::read_to_string(&tracked).unwrap(),
            "original content\n",
            "tracked dirty file must be restored to its HEAD content"
        );
        let after =
            run_tool_in_dir("git", &["rev-parse", "HEAD"], &root.to_string_lossy(), 30).unwrap();
        assert_eq!(
            after.trim(),
            head_after_track.trim(),
            "HEAD itself must remain intact"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_heal_does_not_reset_dirty_operator_owned_checkout() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_dirty_operator_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");
        let tracked = dirty_tracked_file(&root);

        let report =
            self_heal_target_worktree("owner/repo", &root, "main", false, not_live).unwrap();
        // Operator-owned checkouts stay fail-closed on dirty state.
        // Either AlreadyClean (because we deliberately skipped reset) or
        // Healed via cache/lock cleanup is acceptable, but ResetDirtyTree
        // must NEVER appear for a non-managed checkout.
        assert!(report
            .actions
            .iter()
            .all(|a| !matches!(a, WorktreeHealAction::ResetDirtyTree)));
        assert!(
            tracked.exists(),
            "operator-owned dirty file must NOT be reset"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_heal_provisions_fallback_branch_when_target_branch_missing() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_branch_missing_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");

        let report = self_heal_target_worktree(
            "owner/repo",
            &root,
            "factory/jleechan-y189-r1",
            true,
            not_live,
        )
        .unwrap();
        assert_eq!(report.outcome, WorktreeHealOutcome::Healed);
        assert!(report.actions.iter().any(
            |a| matches!(a, WorktreeHealAction::ProvisionedFallbackBranch(name) if name == "factory/jleechan-y189-r1")
        ));
        let branches =
            run_tool_in_dir("git", &["branch", "--list"], &root.to_string_lossy(), 30).unwrap();
        assert!(
            branches.contains("factory/jleechan-y189-r1"),
            "fallback branch must be created: {branches:?}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_heal_refuses_when_live_process_is_in_flight() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_refused_live_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");
        // Commit a tracked file BEFORE injecting the stale index lock, or
        // the `git add` / `git commit` calls in `dirty_tracked_file` will
        // themselves fail (they need to acquire `.git/index.lock`).
        let tracked = dirty_tracked_file(&root);
        let cache = make_untracked_cache(&root);
        let lock = touch_stale_index_lock(&root);

        let report =
            self_heal_target_worktree("owner/repo", &root, "main", true, always_live).unwrap();
        assert!(
            matches!(report.outcome, WorktreeHealOutcome::RefusedLive(_)),
            "must refuse when a live process is reported: {:?}",
            report.outcome
        );
        assert!(
            report.actions.is_empty(),
            "no destructive action may run while a live process owns the worktree: {:?}",
            report.actions
        );
        // None of the pre-existing broken state should have been touched.
        assert!(lock.exists(), "stale index.lock must remain while live");
        assert!(
            cache.join("leftover.o").exists(),
            "cache must remain while live"
        );
        assert!(
            tracked.exists(),
            "dirty tracked file must remain while live"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_heal_is_noop_on_clean_checkout() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_already_clean_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        init_git_checkout(&root, "owner/repo");

        // Pass an empty `expected_branch` to disable the missing-branch
        // provisioning step (which depends on the host's `git init`
        // default-branch name — historically `master`, on newer git
        // `main` — and we don't want to entangle that with this test).
        let report = self_heal_target_worktree("owner/repo", &root, "", true, not_live).unwrap();
        assert_eq!(report.outcome, WorktreeHealOutcome::AlreadyClean);
        assert!(report.actions.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn self_heal_rejects_malformed_repo_before_touching_disk() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_heal_malformed_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let err = self_heal_target_worktree("not-a-valid-repo", &root, "main", true, not_live)
            .unwrap_err();
        assert!(matches!(err, DaemonError::Config(_)));
        assert!(!root.exists());
    }

    #[test]
    fn rejects_malformed_repository_before_touching_disk() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_invalid_{}",
            std::process::id()
        ));
        let err = ensure_target_worktree("repo-without-owner", &root, None).unwrap_err();
        assert!(matches!(err, DaemonError::Config(_)));
        assert!(!root.exists());
    }

    #[test]
    fn target_lock_path_is_persistent_and_workspace_validation_never_provisions() {
        let root =
            std::env::temp_dir().join(format!("afd_target_worktree_lock_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("owner").join("repo");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let lock_path = target.with_extension("lock");
        {
            let _lock = TargetWorktreeLock::acquire(&target).unwrap();
            assert!(lock_path.is_file());
        }
        assert!(lock_path.is_file());
        let missing = root.join("missing");
        let error = validate_existing_target_worktree("owner/repo", &missing, None).unwrap_err();
        assert!(error.to_string().contains("not a directory"));
        assert!(!missing.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn target_lock_serializes_contending_processes() {
        if std::env::var_os("AFD_TARGET_LOCK_HELPER").is_some() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_process_lock_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("owner").join("repo");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let marker = root.join("acquired");
        let lock = TargetWorktreeLock::acquire(&target).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "target_worktree::tests::target_lock_helper_process",
                "--nocapture",
            ])
            .env("AFD_TARGET_LOCK_HELPER", "1")
            .env("AFD_TARGET_LOCK_PATH", &target)
            .env("AFD_TARGET_LOCK_MARKER", &marker)
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !marker.exists(),
            "contender acquired the lock while owner held it"
        );
        drop(lock);
        let status = child.wait().unwrap();
        assert!(status.success());
        assert!(marker.is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn target_lock_helper_process() {
        if std::env::var_os("AFD_TARGET_LOCK_HELPER").is_none() {
            return;
        }
        let target = PathBuf::from(std::env::var("AFD_TARGET_LOCK_PATH").unwrap());
        let _lock = TargetWorktreeLock::acquire(&target).unwrap();
        std::fs::write(
            std::env::var("AFD_TARGET_LOCK_MARKER").unwrap(),
            b"acquired",
        )
        .unwrap();
    }

    #[test]
    fn clone_auth_failure_preserves_gate_failure_and_cleans_own_staging() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_clone_auth_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake_git = bin.join("git");
        let old_path = std::env::var_os("PATH");
        let real_git = old_path
            .as_deref()
            .into_iter()
            .flat_map(std::env::split_paths)
            .map(|dir| dir.join("git"))
            .find(|path| path.is_file())
            .expect("test environment must provide git");
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\nif [ \"$1\" = clone ]; then\n  echo 'authentication required' >&2\n  exit 128\nfi\nexec {} \"$@\"\n",
                real_git.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&fake_git).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_git, permissions).unwrap();
        }
        let target = root.join("owner").join("repo");
        let parent = target.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                old_path.as_deref().unwrap_or_default().to_string_lossy()
            ),
        );
        let result = ensure_target_worktree("owner/repo", &target, None);
        match old_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        let error = result.expect_err("clone auth failure must remain a gate failure");
        assert!(error.to_string().contains("authentication required"));
        assert!(!target.exists());
        assert!(target.with_extension("lock").is_file());
        let staging_entries: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".repo.staging.")
            })
            .collect();
        assert!(
            staging_entries.is_empty(),
            "staging leaked: {staging_entries:?}"
        );
        // Keep the wrapper directory alive until all parallel unit tests have
        // observed the restored PATH; non-clone invocations delegate to the
        // real git binary, so this avoids a PATH race with sibling tests.
    }

    fn init_git_checkout(path: &Path, repo: &str) -> String {
        let _ = std::fs::remove_dir_all(path);
        std::fs::create_dir_all(path).unwrap();
        let path_str = path.to_string_lossy();
        run_tool_in_dir("git", &["init", "-q"], &path_str, 30).unwrap();
        let remote = format!("https://github.com/{repo}.git");
        run_tool_in_dir(
            "git",
            &["remote", "add", "origin", remote.as_str()],
            &path_str,
            30,
        )
        .unwrap();
        run_tool_in_dir(
            "git",
            &[
                "-c",
                "user.email=jleechan2015@users.noreply.github.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "test",
            ],
            &path_str,
            30,
        )
        .unwrap();
        run_tool_in_dir("git", &["rev-parse", "HEAD"], &path_str, 30)
            .unwrap()
            .trim()
            .to_string()
    }

    fn run_refresh_child(dirty: bool, ignored_conflict: bool) {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_refresh_child_{}",
            std::process::id()
        ));
        let bin = root.join("bin");
        let target = root.join("target");
        let state = root.join("head");
        let conflict = root.join("ignored-conflict");
        let old_head = "1111111111111111111111111111111111111111";
        let new_head = "2222222222222222222222222222222222222222";
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(&state, old_head).unwrap();
        let ignored_artifact = target.join("ignored-artifact");
        if ignored_conflict {
            std::fs::write(&ignored_artifact, "preserve me").unwrap();
            std::fs::write(&conflict, "1").unwrap();
        }
        let fake_git = bin.join("git");
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\ncase \"$1:$2\" in\n  remote:get-url) printf '%s\\n' 'https://github.com/owner/repo.git' ;;\n  rev-parse:HEAD) cat '{}' ;;\n  status:--porcelain) {} ;;\n  fetch:--depth=1) : ;;\n  checkout:--no-overwrite-ignore) test \"$3\" = --detach || exit 1; if [ -f '{}' ]; then exit 1; fi; printf '%s' \"$4\" > '{}' ;;\n  *) exit 1 ;;\nesac\n",
                state.display(),
                if dirty { "printf ' M operator-note\\n'" } else { "exit 0" },
                conflict.display(),
                state.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&fake_git).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_git, permissions).unwrap();
        }
        let old_path = std::env::var_os("PATH");
        let path = std::env::join_paths(
            std::iter::once(bin.clone()).chain(
                old_path
                    .as_deref()
                    .into_iter()
                    .flat_map(std::env::split_paths),
            ),
        )
        .unwrap();
        std::env::set_var("PATH", path);
        let result = ensure_managed_target_worktree("owner/repo", &target, Some(new_head));
        if let Some(path) = old_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        if dirty || ignored_conflict {
            let error = result.expect_err("dirty managed checkout must fail closed");
            if dirty {
                assert!(error.to_string().contains("uncommitted changes"));
            }
            assert_eq!(std::fs::read_to_string(&state).unwrap(), old_head);
            if ignored_conflict {
                assert_eq!(
                    std::fs::read_to_string(ignored_artifact).unwrap(),
                    "preserve me"
                );
            }
        } else {
            assert_eq!(result.unwrap(), target);
            assert_eq!(std::fs::read_to_string(&state).unwrap(), new_head);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
