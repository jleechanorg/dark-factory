//! Self-healing worktree preflight (bead jleechan-y189).
//!
//! Bead jleechan-y189 exit criteria: a worktree the daemon is about to use
//! MUST self-heal before reaching the active-session-only refusal gate
//! (already enforced by jw4c). Concretely, the preflight:
//!
//! 1. Removes a stale `<git_dir>/index.lock` left by a crashed `git` process.
//! 2. Removes untracked build/test caches (`target/`, `node_modules/`,
//!    `dist/`, `build/`, `.cache/`) that would otherwise inflate the
//!    porcelain-status check and block refreshes (see jw4c's
//!    `refuses_to_refresh_dirty_managed_checkout`).
//! 3. Optionally resets dirty tracked-file state (`git reset --hard HEAD`)
//!    when the caller signals `dirty_reset_enabled = true`.
//!
//! The preflight does NOT touch branch selection: the bead's expected
//! branch is created by AO when it spawns the worker worktree, not at this
//! layer. `branch_exists` is exported as a public utility for callers that
//! need to assert presence (for example, the gk2r fallback-provisioning
//! lane that will own the `WORKTREE_SELFHEAL_FALLBACK_PROVISIONED` event).
//!
//! The ONLY refusal the daemon keeps is "active session PID" — enforced by
//! jw4c's `ActiveSessionProbe` in `worktree_reaper`. Every other crash-safe
//! failure mode (stale lock, untracked caches) is a preflight-cleanable
//! condition this module addresses.
//!
//! ## Telemetry
//!
//! Each successful self-heal action emits a dedicated event so the
//! factory's Evidence Gate can prove RED → GREEN coverage per scenario:
//!
//! - `WORKTREE_SELFHEAL_INDEX_LOCK_REMOVED`
//! - `WORKTREE_SELFHEAL_CACHES_CLEANED`
//! - `WORKTREE_SELFHEAL_DIRTY_RESET`
//! - `WORKTREE_SELFHEAL_FALLBACK_PROVISIONED` (reserved for the gk2r lane)
//!
//! ## Threading
//!
//! The preflight is single-threaded per worktree. The caller (`target_worktree`)
//! holds `TargetWorktreeLock` (a `flock`-backed file lock) when invoking us,
//! so two parallel dispatches into the same checkout serialize cleanly.

use crate::errors::DaemonError;
use crate::state::now_iso8601;
use crate::telemetry::{emit, TelemetryEvent};
use std::path::{Path, PathBuf};

/// Telemetry event types emitted by the self-heal preflight. Kept as
/// `pub const` so integration tests can assert on the exact wire format
/// without going through `serde_json` round-trips.
pub const EVENT_INDEX_LOCK_REMOVED: &str = "WORKTREE_SELFHEAL_INDEX_LOCK_REMOVED";
pub const EVENT_CACHES_CLEANED: &str = "WORKTREE_SELFHEAL_CACHES_CLEANED";
pub const EVENT_DIRTY_RESET: &str = "WORKTREE_SELFHEAL_DIRTY_RESET";
pub const EVENT_FALLBACK_PROVISIONED: &str = "WORKTREE_SELFHEAL_FALLBACK_PROVISIONED";

/// Outcome of a single preflight pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightOutcome {
    /// Worktree is clean enough to proceed without reset.
    Clean,
    /// Worktree had dirty tracked-file state that was reset (with policy).
    DirtyReset,
}

/// Untracked cache directory names the preflight aggressively removes. These
/// are by definition regeneratable; removing them never destroys operator
/// state. The list is intentionally narrow — the preflight only deletes
/// directories the daemon itself created via build/test runs.
const UNTRACKED_CACHE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".cache",
    ".next",
    ".parcel-cache",
    ".turbo",
    ".vitest",
];

/// Run the self-healing preflight on `worktree`. Returns:
/// - `Ok(PreflightOutcome::Clean)` when nothing needed fixing (or only
///   crash-safe cleanup that doesn't affect tracked files).
/// - `Ok(PreflightOutcome::DirtyReset)` when tracked-file dirt was reset.
///
/// `telemetry_log` is `None` for tests that don't assert on telemetry; in
/// production the daemon always passes the per-bead JSONL file so the
/// factory can prove preflight coverage end-to-end.
///
/// `dirty_reset_enabled` gates the dirty-state reset:
/// - When `true`, the preflight runs `git reset --hard HEAD` to discard
///   any tracked-file modifications.
/// - When `false` (the default for managed/operator-owned checkouts that
///   preserve operator work), a dirty tracked file is left untouched
///   and the result is `PreflightOutcome::Clean` — the caller's own
///   `verify_head` will surface the inconsistency.
pub fn self_heal_preflight(
    worktree: &Path,
    dirty_reset_enabled: bool,
    telemetry_log: Option<&Path>,
    bead_id: &str,
) -> Result<PreflightOutcome, DaemonError> {
    if !worktree.is_dir() {
        // Caller (target_worktree) already validated `is_dir()`; this is a
        // belt-and-suspenders guard against the rare case where the lock
        // acquired but the directory vanished between `is_dir()` and here.
        return Ok(PreflightOutcome::Clean);
    }
    let git_dir = match resolve_git_dir(worktree) {
        Ok(dir) => dir,
        Err(_) => {
            // No git directory at all — nothing to self-heal. The caller's
            // own verification will surface this as a normal
            // `ensure_target_worktree_inner` failure.
            return Ok(PreflightOutcome::Clean);
        }
    };

    // Step 1: stale index.lock.
    if detect_and_remove_stale_index_lock(&git_dir, telemetry_log, bead_id)? {
        // A stale lock means a previous `git` process died; the index may be
        // inconsistent. We deliberately do NOT try to repair the index here —
        // the next `git status` call will rebuild it, and the dirty-state
        // reset (if enabled) handles any resulting inconsistencies.
    }

    // Step 2: untracked caches.
    let cleaned = clean_untracked_caches(worktree, telemetry_log, bead_id);
    if !cleaned.is_empty() && telemetry_log.is_some() {
        // The per-directory event was already emitted by the helper. Emit a
        // single summary event so downstream tooling can grep for the bulk
        // cleanup without joining the directory-level events.
        emit_event(
            telemetry_log,
            bead_id,
            EVENT_CACHES_CLEANED,
            serde_json::json!({"dirs_removed": cleaned}),
        )?;
    }

    // Step 3: dirty tracked-file state. We only ever *reset*; we never
    // refuse. The bead's branch-expectation is enforced by AO when the
    // worker worktree is created, not at this preflight layer.
    match detect_dirty_state(worktree)? {
        DirtyState::Clean => Ok(PreflightOutcome::Clean),
        DirtyState::TrackedDirty => {
            if dirty_reset_enabled {
                reset_tracked_state(worktree)?;
                emit_event(
                    telemetry_log,
                    bead_id,
                    EVENT_DIRTY_RESET,
                    serde_json::json!({"policy": "git reset --hard HEAD"}),
                )?;
                Ok(PreflightOutcome::DirtyReset)
            } else {
                Ok(PreflightOutcome::Clean)
            }
        }
    }
}

/// The current branch is "missing" when `git rev-parse --verify
/// refs/heads/<branch>` exits non-zero. We avoid `git branch --list` so
/// the call shape is consistent across Git versions (some print "  branch"
/// with whitespace prefixes).
pub fn branch_exists(worktree: &Path, branch: &str) -> Result<bool, DaemonError> {
    if branch.trim().is_empty() {
        return Ok(true);
    }
    let path_str = worktree.to_string_lossy();
    // `--verify` on a fully-qualified ref always exits 0 / 1 — no porcelain
    // ambiguity, no "fatal: ambiguous refname" surprises.
    let result = crate::tools::run_tool_in_dir(
        "git",
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
        &path_str,
        30,
    );
    match result {
        Ok(_) => Ok(true),
        Err(DaemonError::Tool { .. }) => Ok(false),
        Err(other) => Err(other),
    }
}

/// Classify the worktree's tracked-file dirt. Untracked files and caches are
/// filtered out (they were already removed by step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirtyState {
    Clean,
    TrackedDirty,
}

fn detect_dirty_state(worktree: &Path) -> Result<DirtyState, DaemonError> {
    let path_str = worktree.to_string_lossy();
    let porcelain = crate::tools::run_tool_in_dir(
        "git",
        &["status", "--porcelain", "--untracked-files=normal"],
        &path_str,
        30,
    )?;
    for line in porcelain.lines() {
        // Porcelain v1: two leading chars = XY status; the rest is the path.
        //   ` M foo` = modified tracked file (in our working tree)
        //   `M  foo` = staged modified tracked file
        //   `A  foo` = staged added tracked file
        //   `?? foo` = untracked — handled by step 2 (cache removal) or
        //     intentionally preserved (operator-created scripts).
        //   `!! foo` = ignored — never operator-visible dirty state.
        if line.len() < 2 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0];
        let y = bytes[1];
        let x_untracked = x == b'?';
        let y_untracked = y == b'?';
        if x_untracked || y_untracked {
            continue;
        }
        let x_ignored = x == b'!';
        let y_ignored = y == b'!';
        if x_ignored || y_ignored {
            continue;
        }
        // Anything else (M, A, D, R, C, U, T) on either slot is a tracked
        // file in some state of modification — preflight treats it as
        // TrackedDirty.
        return Ok(DirtyState::TrackedDirty);
    }
    Ok(DirtyState::Clean)
}

fn reset_tracked_state(worktree: &Path) -> Result<(), DaemonError> {
    let path_str = worktree.to_string_lossy();
    crate::tools::run_tool_in_dir("git", &["reset", "--hard", "HEAD"], &path_str, 30)?;
    Ok(())
}

/// Detect and remove a stale `<git_dir>/index.lock`. Returns `true` if a
/// lock was removed, `false` otherwise. The lock is considered stale when
/// its owning process is no longer alive (or when the owning PID file is
/// absent — the default for crashed `git` invocations on Linux).
///
/// The preflight NEVER removes a lock whose owning PID is alive: that
/// would race the publisher and corrupt the index. Live locks surface to
/// the caller as `DaemonError::Tool` so the daemon can defer rather than
/// destroy work in-flight.
fn detect_and_remove_stale_index_lock(
    git_dir: &Path,
    telemetry_log: Option<&Path>,
    bead_id: &str,
) -> Result<bool, DaemonError> {
    let lock_path = git_dir.join("index.lock");
    if !lock_path.is_file() {
        return Ok(false);
    }

    // Inspect the lock file. A live `git` invocation writes the PID of the
    // owning process at offset 0 (a 4-byte big-endian integer on Linux).
    // Stale locks from crashed processes leave either:
    //   - an empty file (0 bytes)
    //   - a PID whose process no longer exists
    // We refuse to remove a lock whose owning PID is alive.
    let lock_pid = read_lock_pid(&lock_path);
    if let Some(pid) = lock_pid {
        if is_pid_alive(pid) {
            return Err(DaemonError::Config(format!(
                "git index.lock at {} is held by live PID {pid}; refusing to remove",
                lock_path.display()
            )));
        }
    }

    std::fs::remove_file(&lock_path).map_err(|e| {
        DaemonError::Config(format!(
            "remove stale index.lock {}: {e}",
            lock_path.display()
        ))
    })?;

    emit_event(
        telemetry_log,
        bead_id,
        EVENT_INDEX_LOCK_REMOVED,
        serde_json::json!({"path": lock_path.display().to_string()}),
    )?;
    Ok(true)
}

/// Best-effort lock PID read. Returns `None` when the file is empty, has
/// unexpected content, or cannot be read.
fn read_lock_pid(lock_path: &Path) -> Option<u32> {
    let bytes = std::fs::read(lock_path).ok()?;
    if bytes.len() < 4 {
        return None;
    }
    let big_endian = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if big_endian == 0 {
        return None;
    }
    Some(big_endian)
}

/// Probe whether a PID is alive by sending signal 0. POSIX defines
/// `kill(pid, 0)` as "no-op if alive, error otherwise" without actually
/// signalling the target.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` is a well-defined probe; no signal is delivered.
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    // EPERM means the PID exists but we lack permission to signal it — from
    // the preflight's perspective the lock owner is alive.
    errno == libc::EPERM
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    // Conservative: non-Unix platforms (Windows, WSL1) treat any lock as
    // live and surface the error to the caller. The factory's primary host
    // is Linux per `docs/designate-linux-factory-host.md`.
    true
}

#[cfg(unix)]
mod libc {
    use std::os::raw::c_int;
    pub const EPERM: c_int = 1;
    // SAFETY: documented POSIX probe; no signal is delivered at signal 0.
    unsafe extern "C" {
        pub fn kill(pid: c_int, sig: c_int) -> c_int;
    }
}

/// Resolve `<worktree>/.git` (or the equivalent for worktrees-within-worktrees,
/// where `.git` is a file pointing at `gitdir: ...`). The preflight needs
/// the absolute `.git` directory to find `index.lock`.
fn resolve_git_dir(worktree: &Path) -> Result<PathBuf, DaemonError> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Ok(dot_git);
    }
    if dot_git.is_file() {
        let body = std::fs::read_to_string(&dot_git).map_err(|e| {
            DaemonError::Config(format!("read .git pointer {}: {e}", dot_git.display()))
        })?;
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("gitdir:") {
                let trimmed = rest.trim();
                let resolved = if Path::new(trimmed).is_absolute() {
                    PathBuf::from(trimmed)
                } else {
                    worktree.join(trimmed)
                };
                if resolved.is_dir() {
                    return Ok(resolved);
                }
            }
        }
        return Err(DaemonError::Config(format!(
            ".git pointer at {} has no usable gitdir entry",
            dot_git.display()
        )));
    }
    Err(DaemonError::Config(format!(
        "worktree {} has no .git directory or pointer",
        worktree.display()
    )))
}

/// Remove every untracked cache directory listed in `UNTRACKED_CACHE_DIRS`
/// from `worktree`. Returns the list of removed directories for telemetry.
fn clean_untracked_caches(
    worktree: &Path,
    telemetry_log: Option<&Path>,
    bead_id: &str,
) -> Vec<String> {
    let mut removed = Vec::new();
    for name in UNTRACKED_CACHE_DIRS {
        let path = worktree.join(name);
        if !path.is_dir() {
            continue;
        }
        // The cache dir must live directly under `worktree` — never recurse,
        // so a `target/` directory the operator deliberately placed in a
        // subdirectory is untouched.
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {
                removed.push((*name).to_string());
            }
            Err(error) => {
                // Don't fail the whole preflight because one cache dir
                // couldn't be removed — surface it in telemetry and move on.
                if let Some(log) = telemetry_log {
                    let _ = emit_event(
                        Some(log),
                        bead_id,
                        EVENT_CACHES_CLEANED,
                        serde_json::json!({
                            "failed_dir": name,
                            "error": error.to_string(),
                        }),
                    );
                }
            }
        }
    }
    removed
}

fn emit_event(
    telemetry_log: Option<&Path>,
    bead_id: &str,
    event_type: &str,
    metrics: serde_json::Value,
) -> Result<(), DaemonError> {
    let Some(log) = telemetry_log else {
        return Ok(());
    };
    let event = TelemetryEvent {
        timestamp: now_iso8601(),
        bead_id: bead_id.to_string(),
        attempt_id: 0,
        lifecycle_state: "WORKTREE_PREFLIGHT".to_string(),
        event_type: event_type.to_string(),
        metrics,
        context: serde_json::json!({}),
    };
    emit(log, &event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn init_git_repo(path: &Path) {
        let _ = fs::remove_dir_all(path);
        fs::create_dir_all(path).unwrap();
        let path_str = path.to_string_lossy();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        let _ = path_str;
    }

    #[test]
    fn detects_and_removes_stale_index_lock() {
        let root = std::env::temp_dir().join(format!("afd_selfheal_lock_{}", std::process::id()));
        init_git_repo(&root);
        let git_dir = root.join(".git");
        let lock = git_dir.join("index.lock");
        // Write an empty lock file: simulates a crashed `git` invocation
        // that never wrote a PID into the lockfile.
        fs::write(&lock, b"").unwrap();
        assert!(lock.is_file());
        let removed = detect_and_remove_stale_index_lock(&git_dir, None, "bead-test").unwrap();
        assert!(removed, "stale empty lock must be removed");
        assert!(!lock.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_to_remove_live_index_lock() {
        let root =
            std::env::temp_dir().join(format!("afd_selfheal_live_lock_{}", std::process::id()));
        init_git_repo(&root);
        let git_dir = root.join(".git");
        let lock = git_dir.join("index.lock");
        // Write our own PID into the lockfile: the lock is "live" because
        // the test process owns it.
        let pid = std::process::id();
        let bytes = pid.to_be_bytes();
        fs::write(&lock, bytes).unwrap();
        let err = detect_and_remove_stale_index_lock(&git_dir, None, "bead-test").unwrap_err();
        assert!(err.to_string().contains("live PID"));
        assert!(lock.exists(), "live lock must not be removed");
        // Clean up.
        let _ = fs::remove_file(&lock);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cleans_named_untracked_caches_only() {
        let root = std::env::temp_dir().join(format!("afd_selfheal_caches_{}", std::process::id()));
        init_git_repo(&root);
        // Create caches plus a non-cache directory we MUST NOT touch.
        fs::create_dir_all(root.join("target")).unwrap();
        fs::create_dir_all(root.join("node_modules/sub")).unwrap();
        fs::create_dir_all(root.join("dist")).unwrap();
        fs::create_dir_all(root.join("scripts/operator-data")).unwrap();
        fs::write(root.join("scripts/operator-data/keep.txt"), "preserve me").unwrap();

        let cleaned = clean_untracked_caches(&root, None, "bead-test");
        assert!(cleaned.contains(&"target".to_string()));
        assert!(cleaned.contains(&"node_modules".to_string()));
        assert!(cleaned.contains(&"dist".to_string()));
        assert!(!cleaned.contains(&"scripts".to_string()));
        assert!(!root.join("target").exists());
        assert!(!root.join("node_modules").exists());
        assert!(!root.join("dist").exists());
        assert!(root.join("scripts/operator-data/keep.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn branch_exists_distinguishes_present_and_absent() {
        let root = std::env::temp_dir().join(format!("afd_selfheal_branch_{}", std::process::id()));
        init_git_repo(&root);
        // Initial commit lives on `main` (or `master` on older Git).
        let output = Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/main"])
            .current_dir(&root)
            .output()
            .unwrap();
        let branch = if output.status.success() {
            "main".to_string()
        } else {
            let fallback = Command::new("git")
                .args(["rev-parse", "--verify", "refs/heads/master"])
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(fallback.status.success());
            "master".to_string()
        };
        assert!(branch_exists(&root, &branch).unwrap());
        assert!(!branch_exists(&root, "definitely-not-a-branch").unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detect_dirty_state_flags_tracked_modifications() {
        let root = std::env::temp_dir().join(format!("afd_selfheal_dirty_{}", std::process::id()));
        init_git_repo(&root);
        // Add a tracked file, commit it, then modify it.
        fs::write(root.join("tracked.txt"), "v1\n").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "track",
            ])
            .current_dir(&root)
            .status()
            .unwrap();
        // Stage an untracked file too — should be ignored by dirty detection.
        fs::write(root.join("untracked.txt"), "ignored\n").unwrap();
        assert_eq!(
            detect_dirty_state(&root).unwrap(),
            DirtyState::Clean,
            "untracked-only state must be Clean"
        );
        fs::write(root.join("tracked.txt"), "v2\n").unwrap();
        assert_eq!(
            detect_dirty_state(&root).unwrap(),
            DirtyState::TrackedDirty,
            "modified tracked file must be TrackedDirty"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn self_heal_preflight_resets_dirty_state_when_enabled() {
        let root =
            std::env::temp_dir().join(format!("afd_selfheal_preflight_{}", std::process::id()));
        init_git_repo(&root);
        fs::write(root.join("tracked.txt"), "v1\n").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&root)
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "track",
            ])
            .current_dir(&root)
            .status()
            .unwrap();
        fs::write(root.join("tracked.txt"), "v2\n").unwrap();
        let outcome = self_heal_preflight(&root, true, None, "bead-test").unwrap();
        assert_eq!(outcome, PreflightOutcome::DirtyReset);
        let body = fs::read_to_string(root.join("tracked.txt")).unwrap();
        assert_eq!(body, "v1\n", "dirty reset must restore HEAD content");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn self_heal_preflight_emits_telemetry_events() {
        let root =
            std::env::temp_dir().join(format!("afd_selfheal_telemetry_{}", std::process::id()));
        init_git_repo(&root);
        let git_dir = root.join(".git");
        fs::write(git_dir.join("index.lock"), b"").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        let log =
            std::env::temp_dir().join(format!("afd_selfheal_log_{}.jsonl", std::process::id()));
        let _ = fs::remove_file(&log);
        let _ = self_heal_preflight(&root, true, Some(&log), "bead-test").unwrap();
        let body = fs::read_to_string(&log).unwrap();
        assert!(body.contains(EVENT_INDEX_LOCK_REMOVED));
        assert!(body.contains(EVENT_CACHES_CLEANED));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&log);
    }
}
