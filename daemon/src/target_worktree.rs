//! Resolve and provision the checkout used by execution-time gates.
//!
//! The daemon binary may be installed from an immutable uv/release location,
//! so gate code must never infer a repository from its own current directory.
//! This module owns the small amount of git plumbing needed to create the
//! configured isolated checkout when it has not been created yet.

use crate::errors::DaemonError;
use crate::tools::{run_tool, run_tool_in_dir};
use std::path::{Path, PathBuf};

/// Reuse an existing target checkout or provision one by cloning the named
/// GitHub repository into `requested`.
///
/// A missing checkout is intentionally created outside the daemon release
/// tree. `head_sha` is optional because callers that only need a source root
/// can provision the checkout first and resolve a revision later. Existing
/// directories are never checked out or reset: gate 8 mutates its dedicated
/// checkout while it runs, and overwriting a possibly-dirty operator checkout
/// would be unsafe.
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
    let clone_args = ["clone", "--no-checkout", "--filter=blob:none", remote.as_str(), destination];
    if let Err(err) = run_tool("git", &clone_args, 600) {
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
    fn reuses_existing_directory_without_git_mutation() {
        let root = std::env::temp_dir().join(format!(
            "afd_target_worktree_existing_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let resolved = ensure_target_worktree("owner/repo", &root, Some("deadbeef")).unwrap();
        assert_eq!(resolved, root);
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
}
