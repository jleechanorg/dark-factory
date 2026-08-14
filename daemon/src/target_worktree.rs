//! Resolve and provision the checkout used by execution-time gates.
//!
//! The daemon binary may be installed from an immutable uv/release location,
//! so gate code must never infer a repository from its own current directory.
//! This module owns the small amount of git plumbing needed to create the
//! configured isolated checkout when it has not been created yet.

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

/// Remove stale git lock files left behind by an interrupted prior operation
/// from `<path>` (a checkout). Real `git checkout`, `git pull`, `git fetch`
/// (with pack writes), and several other index-mutating commands refuse to
/// proceed while `<path>/.git/index.lock` exists; the linked-worktree lock
/// `<path>/.git/worktrees/<name>/locked` blocks every command that wants to
/// update that linked worktree's refs. Both are unlinking-only locks: when the
/// owning git process is no longer alive (the common case after a SIGKILL,
/// OOM kill, or daemon crash mid-operation), removing the file is the only
/// recovery Git itself performs.
///
/// Returns the number of lock files that were removed. Operationally safe by
/// design: refuses to recurse, never deletes anything outside `<path>/.git/`,
/// and treats a missing path or missing `.git/` directory as a no-op
/// (returns 0).
pub fn clean_stale_git_locks(path: &Path) -> usize {
    let git_dir = path.join(".git");
    if !git_dir.is_dir() {
        return 0;
    }
    let mut removed = 0;
    let index_lock = git_dir.join("index.lock");
    if index_lock.is_file() && std::fs::remove_file(&index_lock).is_ok() {
        removed += 1;
    }
    let worktrees_dir = git_dir.join("worktrees");
    if let Ok(entries) = std::fs::read_dir(&worktrees_dir) {
        for entry in entries.flatten() {
            let locked = entry.path().join("locked");
            if locked.is_file() && std::fs::remove_file(&locked).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

fn is_recoverable_worktree_failure(error: &DaemonError) -> bool {
    let message = error.to_string();
    [
        "index.lock",
        "Unable to create",
        "Another git process",
        "expected snapshot",
        "could not checkout",
        "cannot lock ref",
        "unable to update",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

/// Reuse or provision a daemon-owned target checkout with two recovery
/// safeguards layered onto [`ensure_managed_target_worktree`]:
///
/// 1. **Preflight lock cleanup.** Before any git operation runs against
///    `requested`, remove any stale `<path>/.git/index.lock` and
///    `<path>/.git/worktrees/<name>/locked` files left behind by an
///    interrupted prior checkout. Real git refuses to operate on a worktree
///    while these exist; the owning process is no longer alive in the
///    recovery scenario (daemon restart, SIGKILL, OOM kill mid-checkout).
///
/// 2. **Fresh-clone fallback.** If the inner
///    [`ensure_managed_target_worktree`] still fails with a recoverable
///    signature (stale `index.lock`, "expected snapshot" HEAD mismatch, or
///    `git checkout` failure), remove `requested` outright and provision a
///    fresh checkout from `origin/{default branch}` (passes `head_sha =
///    None` so the new clone reflects upstream HEAD, not the requested
///    snapshot). The fresh checkout is what the worker actually uses; the
///    `factory/<bead>-r<N>` branch is already created cross-repo by
///    `reroll.rs`, so a local-snapshot divergence is the only thing being
///    abandoned — and a diverged local snapshot is exactly what the
///    remediation is trying to escape.
///
/// Non-recoverable failures (operator-owned checkout with the wrong
/// `origin`; clone auth failure; network/repository errors) propagate
/// unchanged so callers can park the bead `HUMAN_HELD` for those.
pub fn ensure_managed_target_worktree_with_recovery(
    repo: &str,
    requested: &Path,
    head_sha: Option<&str>,
) -> Result<PathBuf, DaemonError> {
    clean_stale_git_locks(requested);
    match ensure_managed_target_worktree(repo, requested, head_sha) {
        Ok(path) => Ok(path),
        Err(error) if is_recoverable_worktree_failure(&error) => {
            let _ = std::fs::remove_dir_all(requested);
            ensure_managed_target_worktree(repo, requested, None)
        }
        Err(error) => Err(error),
    }
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

    #[test]
    fn clean_stale_git_locks_removes_index_lock_and_worktree_lock() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_lock_cleanup_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join("owner").join("repo");
        std::fs::create_dir_all(target.join(".git").join("worktrees").join("feature"))
            .unwrap();
        std::fs::write(target.join(".git").join("index.lock"), b"").unwrap();
        std::fs::write(
            target.join(".git").join("worktrees").join("feature").join("locked"),
            b"",
        )
        .unwrap();

        let removed = clean_stale_git_locks(&target);
        assert_eq!(removed, 2, "both lock files must be removed");
        assert!(!target.join(".git").join("index.lock").exists());
        assert!(!target
            .join(".git")
            .join("worktrees")
            .join("feature")
            .join("locked")
            .exists());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clean_stale_git_locks_no_op_on_missing_path_or_git_dir() {
        let missing = std::env::temp_dir().join(format!(
            "afd_target_worktree_lock_cleanup_missing_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing);
        assert_eq!(clean_stale_git_locks(&missing), 0);
        std::fs::create_dir_all(&missing).unwrap();
        assert_eq!(
            clean_stale_git_locks(&missing),
            0,
            "directory without .git/ must be a no-op"
        );
        std::fs::remove_dir_all(missing).unwrap();
    }

    #[test]
    fn recovery_cleans_stale_index_lock_before_git_checkout() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_recovery_lock_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let target = root.join("owner").join("repo");
        let state = root.join("head");
        let actual_head = "3333333333333333333333333333333333333333";
        let expected_head = "7777777777777777777777777777777777777777";
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(&state, actual_head).unwrap();
        let fake_git = bin.join("git");
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\ncase \"$1:$2\" in\n  remote:get-url) printf '%s\\n' 'https://github.com/owner/repo.git' ;;\n  rev-parse:HEAD) cat '{state}' ;;\n  status:--porcelain) exit 0 ;;\n  fetch:--depth=1) : ;;\n  checkout:--no-overwrite-ignore)\n    lock='{git_dir}/index.lock'\n    if [ -f \"$lock\" ]; then\n      echo \"fatal: Unable to create '.git/index.lock': File exists.\" >&2\n      exit 128\n    fi\n    printf '%s' \"$4\" > '{state}'\n    ;;\n  *) exit 1 ;;\nesac\n",
                state = state.display(),
                git_dir = target.join(".git").display(),
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
        std::fs::create_dir_all(target.join(".git")).unwrap();
        std::fs::write(target.join(".git").join("index.lock"), b"").unwrap();
        let old_path = std::env::var_os("PATH");
        let new_path = std::env::join_paths(
            std::iter::once(bin.clone()).chain(
                old_path
                    .as_deref()
                    .into_iter()
                    .flat_map(std::env::split_paths),
            ),
        )
        .unwrap();
        std::env::set_var("PATH", &new_path);

        let sanity = ensure_managed_target_worktree(
            "owner/repo",
            &target,
            Some(expected_head),
        );
        assert!(
            sanity.is_err(),
            "without recovery, a stale .git/index.lock must cause git checkout to fail: {sanity:?}"
        );
        std::fs::write(target.join(".git").join("index.lock"), b"").unwrap();
        let result = ensure_managed_target_worktree_with_recovery(
            "owner/repo",
            &target,
            Some(expected_head),
        );
        if let Some(path) = old_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        assert!(
            result.is_ok(),
            "recovery wrapper must clean the lock and proceed: {result:?}"
        );
        assert_eq!(result.unwrap(), target);
        assert!(!target.join(".git").join("index.lock").exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recovery_falls_back_to_fresh_clone_on_unresolvable_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_recovery_fallback_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let target = root.join("owner").join("repo");
        let parent = target.parent().unwrap();
        let state = root.join("head");
        let actual_head = "4444444444444444444444444444444444444444";
        let stale_head =
            "5555555555555555555555555555555555555555".to_string();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(&state, actual_head).unwrap();
        let _ = init_git_checkout(&target, "owner/repo");

        let fake_git = bin.join("git");
        std::fs::write(
            &fake_git,
            format!(
                "#!/bin/sh\ncase \"$1:$2\" in\n  remote:get-url) printf '%s\\n' 'https://github.com/owner/repo.git' ;;\n  rev-parse:HEAD) cat '{state}' ;;\n  status:--porcelain) exit 0 ;;\n  fetch:--depth=1) : ;;\n  checkout:--no-overwrite-ignore) echo 'fatal: cannot lock ref: unable to update index.lock: stale worktree snapshot' >&2 ; exit 128 ;;\n  *) case \"$1\" in clone) for arg in \"$@\"; do last=\"$arg\"; done; mkdir -p \"$last/.git\" && printf 'ref: refs/heads/main\\n' > \"$last/.git/HEAD\" ;; *) exit 1 ;; esac ;;\nesac\n",
                state = state.display()
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
        let new_path = std::env::join_paths(
            std::iter::once(bin.clone()).chain(
                old_path
                    .as_deref()
                    .into_iter()
                    .flat_map(std::env::split_paths),
            ),
        )
        .unwrap();
        std::env::set_var("PATH", &new_path);

        let sanity = ensure_managed_target_worktree(
            "owner/repo",
            &target,
            Some(&stale_head),
        );
        assert!(
            sanity.is_err(),
            "without recovery, a broken checkout must surface as an error: {sanity:?}"
        );
        assert!(
            target.is_dir(),
            "without recovery, the broken checkout must still be present so the operator can inspect it"
        );

        let result =
            ensure_managed_target_worktree_with_recovery("owner/repo", &target, Some(&stale_head));
        if let Some(path) = old_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        assert!(
            result.is_ok(),
            "recovery must remove the broken checkout and provision a fresh one: {result:?}"
        );
        let resolved = result.unwrap();
        assert!(
            resolved.is_dir(),
            "fresh checkout must exist after recovery: {}",
            resolved.display()
        );
        std::fs::remove_dir_all(root).ok();
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
