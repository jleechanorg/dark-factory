//! Resolve and provision the checkout used by execution-time gates.
//!
//! The daemon binary may be installed from an immutable uv/release location,
//! so gate code must never infer a repository from its own current directory.
//! This module owns the small amount of git plumbing needed to create the
//! configured isolated checkout when it has not been created yet.

use crate::errors::DaemonError;
use crate::tools::{remote_url_matches_repo, run_tool, run_tool_in_dir};
use std::path::{Path, PathBuf};

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
    validate_repo(repo)?;
    if !requested.is_absolute() {
        return Err(DaemonError::Config(format!(
            "target worktree path must be absolute: {}",
            requested.display()
        )));
    }
    if requested.is_dir() {
        verify_existing(repo, requested, head_sha)?;
        return Ok(requested.to_path_buf());
    }
    if requested.exists() {
        return Err(DaemonError::Config(format!(
            "target worktree path exists but is not a directory: {}",
            requested.display()
        )));
    }

    if let Some(parent) = requested.parent() {
        std::fs::create_dir_all(parent).map_err(|e| DaemonError::Config(format!(
            "create target worktree parent {}: {e}",
            parent.display()
        )))?;
    }
    let destination = requested.to_str().ok_or_else(|| {
        DaemonError::Config(format!(
            "target worktree path is not valid UTF-8: {}",
            requested.display()
        ))
    })?;
    let remote = format!("https://github.com/{repo}.git");
    let clone_result = if head_sha.is_some() {
        let args = [
            "clone",
            "--no-checkout",
            "--filter=blob:none",
            remote.as_str(),
            destination,
        ];
        run_tool("git", &args, 600)
    } else {
        let args = ["clone", "--filter=blob:none", remote.as_str(), destination];
        run_tool("git", &args, 600)
    };
    if let Err(err) = clone_result {
        let _ = std::fs::remove_dir_all(requested);
        return Err(err);
    }

    if let Some(sha) = head_sha.filter(|sha| !sha.trim().is_empty()) {
        let fetch_args = ["fetch", "--depth=1", "origin", sha];
        if let Err(err) = run_tool_in_dir("git", &fetch_args, destination, 600) {
            let _ = std::fs::remove_dir_all(requested);
            return Err(err);
        }
        let checkout_args = ["checkout", "--detach", sha];
        if let Err(err) = run_tool_in_dir("git", &checkout_args, destination, 60) {
            let _ = std::fs::remove_dir_all(requested);
            return Err(err);
        }
    }
    Ok(requested.to_path_buf())
}

fn verify_existing(repo: &str, path: &Path, head_sha: Option<&str>) -> Result<(), DaemonError> {
    let path_str = path.to_string_lossy();
    let remote = run_tool_in_dir("git", &["remote", "get-url", "origin"], &path_str, 30)?;
    if remote_url_matches_repo(&remote, repo) != Some(true) {
        return Err(DaemonError::Config(format!(
            "target worktree {} has origin {:?}, not {repo}",
            path.display(),
            remote.trim()
        )));
    }
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
        let err = ensure_target_worktree("owner/repo", &root, Some(&head)).unwrap_err();
        assert!(err.to_string().contains("origin"));
        std::fs::remove_dir_all(root).unwrap();
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
}
