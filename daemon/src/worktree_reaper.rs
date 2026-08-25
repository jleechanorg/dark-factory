//! Agent worktree reaper (bead jleechan-jw4c).
//!
//! Mirrors the queue-hygiene rule from jleechan-la67: an unbounded producer
//! with no consumer is an outage timer, just measured in disk instead of
//! latency. The reaper:
//!
//! 1. **TTL** — worktrees whose `mtime` is older than `cfg.worktree_ttl_secs`
//!    are prunable, EXCEPT those whose `agent_id` matches a live session
//!    (active-session-only refusal, folded from jleechan-y189).
//! 2. **Post-merge** — worktrees whose branch is merged or `agent_id` no
//!    longer has a live session are queued for reap.
//! 3. **Cap** — `cfg.worktree_max_count` is enforced when the reaper
//!    discovers a new directory; new worktree creation fails closed when
//!    the cap is reached (the `check_cap` helper).
//! 4. **Telemetry** — every sweep emits a `WORKTREE_REAPER_REPORT` event
//!    with `prunable_count` and `total_bytes` so the daemon's telemetry
//!    log captures the prune landscape instead of leaving it silent.
//! 5. **Flush** — explicit `flush()` callable (CLI / API surface) to force
//!    sweep without waiting for the periodic timer.
//!
//! The reaper is **pure Rust** — no subprocess calls, no LLM judgement
//! (ZFC: TTL is a wall-clock predicate, "active" is a concrete process-
//! state probe). It writes a `last_reap_at` sentinel file so a manual
//! `flush()` and a periodic `tick()` don't double-prune the same
//! candidate within the same TTL window.

use crate::config::Config;
use crate::errors::DaemonError;
use crate::telemetry::{emit, local_hostname, TelemetryEvent};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// One row of the reaper's report. The struct intentionally has the same
/// shape as the telemetry event payload so tests can assert on it
/// without going through serde_json.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaperReport {
    pub repo: String,
    pub root: PathBuf,
    pub total_worktrees: usize,
    pub prunable_count: usize,
    pub total_bytes: u64,
    pub kept_active_count: usize,
    pub skipped_oversized_count: usize,
}

/// A single candidate worktree discovered by the reaper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub agent_id: String,
    pub mtime_secs: u64,
    pub size_bytes: u64,
}

const QUARANTINE_DIR_NAME: &str = ".quarantine";
const RECOVERY_PARENT_DIR_NAME: &str = "worktree-recovery";

#[cfg(unix)]
fn stable_recovery_hash(root: &Path) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in root.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn is_plain_directory(path: &Path) -> std::io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_dir())
}

fn linked_worktree_metadata(path: &Path) -> Result<bool, DaemonError> {
    let metadata = std::fs::symlink_metadata(path.join(".git")).map_err(|e| {
        DaemonError::Config(format!(
            "worktree reaper: inspect git metadata {}: {e}",
            path.display()
        ))
    })?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(DaemonError::Config(format!(
            "worktree reaper: git metadata {} is a symlink",
            path.join(".git").display()
        )));
    }
    if file_type.is_file() {
        return Ok(true);
    }
    if file_type.is_dir() {
        return Ok(false);
    }
    Err(DaemonError::Config(format!(
        "worktree reaper: unsupported git metadata {}",
        path.join(".git").display()
    )))
}

/// Return whether git reports tracked, untracked, or ignored changes. Missing
/// or malformed Git metadata fails closed so an unknown worktree is never
/// mistaken for a clean one.
fn is_dirty_worktree(path: &Path) -> Result<bool, DaemonError> {
    let _ = linked_worktree_metadata(path)?;
    let output = Command::new("git")
        .args([
            "-C",
            path.to_string_lossy().as_ref(),
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignored",
        ])
        .output()
        .map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: git status {}: {e}",
                path.display()
            ))
        })?;
    if !output.status.success() {
        return Err(DaemonError::Config(format!(
            "worktree reaper: git status {} failed with {}",
            path.display(),
            output.status
        )));
    }
    Ok(!output.stdout.is_empty())
}

#[cfg(unix)]
struct DirectoryFd(RawFd);

#[cfg(unix)]
impl Drop for DirectoryFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

#[cfg(unix)]
fn c_path(path: &Path) -> std::io::Result<CString> {
    CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL")
    })
}

#[cfg(unix)]
fn c_name(name: &str) -> std::io::Result<CString> {
    CString::new(name).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "name contains NUL")
    })
}

#[cfg(unix)]
fn open_directory(path: &Path) -> std::io::Result<DirectoryFd> {
    let path = c_path(path)?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(DirectoryFd(fd))
}

#[cfg(unix)]
fn open_child_directory(parent: &DirectoryFd, name: &str) -> std::io::Result<DirectoryFd> {
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent.0,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(DirectoryFd(fd))
}

#[cfg(unix)]
fn ensure_private_directory(path: &Path) -> std::io::Result<DirectoryFd> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "recovery namespace is not a directory",
        ));
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)?;
    open_directory(path)
}

#[cfg(unix)]
fn recovery_namespace_path(root: &Path) -> Result<PathBuf, DaemonError> {
    let runtime_root = crate::intake::runtime_state_dir();
    if !runtime_root.exists() {
        std::fs::create_dir_all(&runtime_root).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: create daemon runtime state {}: {e}",
                runtime_root.display()
            ))
        })?;
    }
    let runtime_root = runtime_root.canonicalize().map_err(|e| {
        DaemonError::Config(format!(
            "worktree reaper: resolve daemon runtime state {}: {e}",
            runtime_root.display()
        ))
    })?;
    let worktree_root = root.canonicalize().map_err(|e| {
        DaemonError::Config(format!(
            "worktree reaper: resolve agent worktree root {}: {e}",
            root.display()
        ))
    })?;
    if runtime_root == worktree_root || runtime_root.starts_with(&worktree_root) {
        return Err(DaemonError::Config(
            "worktree reaper: daemon recovery state must be outside agent worktree root".into(),
        ));
    }
    Ok(runtime_root
        .join(RECOVERY_PARENT_DIR_NAME)
        .join(format!("{:016x}", stable_recovery_hash(&worktree_root))))
}

#[cfg(unix)]
fn open_recovery_namespace(root: &Path) -> Result<(PathBuf, DirectoryFd), DaemonError> {
    let namespace = recovery_namespace_path(root)?;
    let parent = namespace.parent().ok_or_else(|| {
        DaemonError::Config("worktree reaper: recovery namespace has no parent".into())
    })?;
    let _parent_fd = ensure_private_directory(parent).map_err(|e| {
        DaemonError::Config(format!(
            "worktree reaper: open daemon recovery parent {}: {e}",
            parent.display()
        ))
    })?;
    let namespace_fd = ensure_private_directory(&namespace).map_err(|e| {
        DaemonError::Config(format!(
            "worktree reaper: open daemon recovery namespace {}: {e}",
            namespace.display()
        ))
    })?;
    Ok((namespace, namespace_fd))
}

#[cfg(unix)]
fn open_manifest(
    parent: &DirectoryFd,
    name: &str,
    create_new: bool,
) -> std::io::Result<std::fs::File> {
    let name = c_name(name)?;
    let mut flags = libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    if create_new {
        flags |= libc::O_CREAT | libc::O_EXCL;
    } else {
        flags |= libc::O_APPEND;
    }
    let fd = unsafe { libc::openat(parent.0, name.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_manifest_read(parent: &DirectoryFd, name: &str) -> std::io::Result<std::fs::File> {
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            parent.0,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn sync_directory(parent: &DirectoryFd) -> std::io::Result<()> {
    let rc = unsafe { libc::fsync(parent.0) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn directory_identity(parent: &DirectoryFd) -> std::io::Result<(libc::dev_t, libc::ino_t)> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe { libc::fstat(parent.0, stat.as_mut_ptr()) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_dev, stat.st_ino))
}

#[cfg(unix)]
fn write_manifest_line(
    parent: &DirectoryFd,
    manifest_name: &str,
    record: &serde_json::Value,
    create_new: bool,
) -> std::io::Result<()> {
    let mut manifest = open_manifest(parent, manifest_name, create_new)?;
    serde_json::to_writer(&mut manifest, record)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    manifest.write_all(b"\n")?;
    manifest.sync_all()
}

#[cfg(unix)]
fn rename_into_directory(
    path: &Path,
    parent: &DirectoryFd,
    destination_name: &str,
) -> std::io::Result<()> {
    let path = c_path(path)?;
    let destination_name = c_name(destination_name)?;
    let rc = unsafe {
        libc::renameat(
            libc::AT_FDCWD,
            path.as_ptr(),
            parent.0,
            destination_name.as_ptr(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn rename_between_directories(
    source_parent: &DirectoryFd,
    source_name: &str,
    destination_parent: &DirectoryFd,
    destination_name: &str,
) -> std::io::Result<()> {
    let source_name = c_name(source_name)?;
    let destination_name = c_name(destination_name)?;
    #[cfg(target_os = "linux")]
    {
        const RENAME_NOREPLACE: libc::c_uint = 1;
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                source_parent.0,
                source_name.as_ptr(),
                destination_parent.0,
                destination_name.as_ptr(),
                RENAME_NOREPLACE,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        return Ok(());
    }
    #[cfg(not(target_os = "linux"))]
    if entry_exists(destination_parent, destination_name.to_str().unwrap_or_default())? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "rollback destination was recreated",
        ));
    }
    let rc = unsafe {
        libc::renameat(
            source_parent.0,
            source_name.as_ptr(),
            destination_parent.0,
            destination_name.as_ptr(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn rollback_quarantine_move(
    quarantine_fd: &DirectoryFd,
    stem: &str,
    root_fd: &DirectoryFd,
    original_name: &str,
    manifest_name: &str,
) -> std::io::Result<()> {
    rename_between_directories(quarantine_fd, stem, root_fd, original_name)?;
    sync_directory(quarantine_fd)?;
    sync_directory(root_fd)?;
    unlink_at(quarantine_fd, manifest_name)?;
    sync_directory(quarantine_fd)
}

#[cfg(unix)]
fn repair_linked_worktree(parent: &DirectoryFd, destination_name: &str) -> Result<(), DaemonError> {
    let destination_fd = open_child_directory(parent, destination_name).map_err(|e| {
        DaemonError::Config(format!("worktree reaper: open moved worktree for repair: {e}"))
    })?;
    let repair_fd = destination_fd.0;
    let output = unsafe {
        Command::new("git")
            .args(["worktree", "repair"])
            .pre_exec(move || {
                if libc::fchdir(repair_fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .output()
    }
        .map_err(|e| DaemonError::Config(format!("worktree reaper: git worktree repair: {e}")))?;
    if !output.status.success() {
        return Err(DaemonError::Config(format!(
            "worktree reaper: git worktree repair failed with {}",
            output.status
        )));
    }
    let status_fd = open_child_directory(parent, destination_name).map_err(|e| {
        DaemonError::Config(format!("worktree reaper: reopen repaired worktree: {e}"))
    })?;
    let status_cwd = status_fd.0;
    let status = unsafe {
        Command::new("git")
            .args(["status", "--porcelain=v1"])
            .pre_exec(move || {
                if libc::fchdir(status_cwd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .output()
    }
        .map_err(|e| DaemonError::Config(format!("worktree reaper: verify repaired worktree: {e}")))?;
    if !status.status.success() {
        return Err(DaemonError::Config(format!(
            "worktree reaper: repaired worktree status failed with {}",
            status.status
        )));
    }
    Ok(())
}

fn remove_worktree(path: &Path) -> Result<(), DaemonError> {
    if linked_worktree_metadata(path)? {
        let output = Command::new("git")
            .args([
                "-C",
                path.to_string_lossy().as_ref(),
                "worktree",
                "remove",
                path.to_string_lossy().as_ref(),
            ])
            .output()
            .map_err(|e| {
                DaemonError::Config(format!(
                    "worktree reaper: git worktree remove {}: {e}",
                    path.display()
                ))
            })?;
        if !output.status.success() {
            return Err(DaemonError::Config(format!(
                "worktree reaper: git worktree remove {} failed with {}",
                path.display(),
                output.status
            )));
        }
        Ok(())
    } else {
        std::fs::remove_dir_all(path).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: remove {}: {e}",
                path.display()
            ))
        })
    }
}

#[cfg(unix)]
fn quarantine_worktree_inner(
    root: &Path,
    path: &Path,
    agent_id: &str,
    #[cfg(test)] swap_after_open: Option<&Path>,
    #[cfg(test)] recreate_destination_after_swap: bool,
) -> Result<(), DaemonError> {
    if agent_id.is_empty() || agent_id.contains('/') || agent_id.contains("..") {
        return Err(DaemonError::Config(format!(
            "worktree reaper: unsafe agent id {agent_id:?}"
        )));
    }
    let original_name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| {
        DaemonError::Config(format!(
            "worktree reaper: worktree path {} has no safe basename",
            path.display()
        ))
    })?;
    if original_name != agent_id || path.parent() != Some(root) {
        return Err(DaemonError::Config(format!(
            "worktree reaper: worktree {} is not the expected direct child for agent {agent_id}",
            path.display()
        )));
    }
    let root_fd = open_directory(root).map_err(|e| {
        DaemonError::Config(format!(
            "worktree reaper: open worktree root {}: {e}",
            root.display()
        ))
    })?;
    let (quarantine_root, quarantine_fd) = open_recovery_namespace(root)?;
    let quarantine_identity = directory_identity(&quarantine_fd).map_err(|e| {
        DaemonError::Config(format!("worktree reaper: identify quarantine directory: {e}"))
    })?;
    let source_device = std::fs::symlink_metadata(path)
        .map_err(|e| DaemonError::Config(format!("worktree reaper: inspect source device: {e}")))?
        .dev();
    if source_device != quarantine_identity.0 as u64 {
        return Err(DaemonError::Config(
            "worktree reaper: source and daemon recovery namespace are on different filesystems; refusing move".into(),
        ));
    }
    let stamp = now_epoch_secs();
    let linked = linked_worktree_metadata(path)?;
    for sequence in 0..1000u32 {
        let stem = format!("{agent_id}-{stamp}-{}-{sequence}", std::process::id());
        let manifest_name = format!("{stem}.json");
        if entry_exists(&quarantine_fd, &stem).map_err(|e| {
            DaemonError::Config(format!("worktree reaper: inspect quarantine entry: {e}"))
        })? || entry_exists(&quarantine_fd, &manifest_name).map_err(|e| {
            DaemonError::Config(format!("worktree reaper: inspect quarantine manifest: {e}"))
        })? {
            continue;
        }
        let destination = quarantine_root.join(&stem);
        let manifest_path = quarantine_root.join(&manifest_name);
        let prepared = serde_json::json!({
            "state": "prepared",
            "agent_id": agent_id,
            "recorded_at_epoch_secs": stamp,
        });
        write_manifest_line(&quarantine_fd, &manifest_name, &prepared, true).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: record quarantine {}: {e}",
                manifest_path.display()
            ))
        })?;
        sync_directory(&quarantine_fd).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: sync quarantine {}: {e}",
                quarantine_root.display()
            ))
        })?;
        rename_into_directory(path, &quarantine_fd, &stem).map_err(|e| {
            let _ = unlink_at(&quarantine_fd, &manifest_name);
            DaemonError::Config(format!(
                "worktree reaper: quarantine {} -> {}: {e}",
                path.display(),
                destination.display()
            ))
        })?;
        sync_directory(&quarantine_fd).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: sync quarantine {} after move: {e}",
                quarantine_root.display()
            ))
        })?;
        let quarantine_unchanged = open_directory(&quarantine_root)
            .and_then(|current| directory_identity(&current))
            .map(|current_identity| current_identity == quarantine_identity)
            .unwrap_or(false);
        if !quarantine_unchanged {
            let rollback = rollback_quarantine_move(
                &quarantine_fd,
                &stem,
                &root_fd,
                original_name,
                &manifest_name,
            );
            return match rollback {
                Ok(()) => Err(DaemonError::Config(
                    "worktree reaper: quarantine directory changed during move; move rolled back"
                        .into(),
                )),
                Err(error) => Err(DaemonError::Config(format!(
                    "worktree reaper: quarantine directory changed during move and rollback failed: {error}"
                ))),
            };
        }
        #[cfg(test)]
        if let Some(outside) = swap_after_open {
            let backup = quarantine_root.with_extension("original");
            std::fs::rename(&quarantine_root, &backup).unwrap();
            std::os::unix::fs::symlink(outside, &quarantine_root).unwrap();
            if recreate_destination_after_swap {
                std::fs::create_dir_all(root.join(original_name)).unwrap();
            }
        }
        let quarantine_still_current = open_directory(&quarantine_root)
            .and_then(|current| directory_identity(&current))
            .map(|current_identity| current_identity == quarantine_identity)
            .unwrap_or(false);
        if !quarantine_still_current {
            let rollback = rollback_quarantine_move(
                &quarantine_fd,
                &stem,
                &root_fd,
                original_name,
                &manifest_name,
            );
            return match rollback {
                Ok(()) => Err(DaemonError::Config(
                    "worktree reaper: daemon recovery namespace changed during move; move rolled back"
                        .into(),
                )),
                Err(error) => Err(DaemonError::Config(format!(
                    "worktree reaper: daemon recovery namespace changed during move and rollback failed: {error}"
                ))),
            };
        }
        if linked {
            repair_linked_worktree(&quarantine_fd, &stem)?;
        }
        let moved = serde_json::json!({
            "state": "moved",
            "agent_id": agent_id,
            "original_path": path.display().to_string(),
            "quarantined_path": destination.display().to_string(),
            "recorded_at_epoch_secs": stamp,
        });
        write_manifest_line(&quarantine_fd, &manifest_name, &moved, false).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: record moved quarantine {}: {e}",
                manifest_path.display()
            ))
        })?;
        sync_directory(&quarantine_fd).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: sync quarantine {} after record: {e}",
                quarantine_root.display()
            ))
        })?;
        return Ok(());
    }
    Err(DaemonError::Config(format!(
        "worktree reaper: no available quarantine name for {}",
        path.display()
    )))
}

#[cfg(unix)]
fn quarantine_worktree(root: &Path, path: &Path, agent_id: &str) -> Result<(), DaemonError> {
    #[cfg(test)]
    {
        quarantine_worktree_inner(root, path, agent_id, None, false)
    }
    #[cfg(not(test))]
    {
        quarantine_worktree_inner(root, path, agent_id)
    }
}

#[cfg(unix)]
fn entry_exists(parent: &DirectoryFd, name: &str) -> std::io::Result<bool> {
    let name = c_name(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe {
        libc::fstatat(
            parent.0,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn unlink_at(parent: &DirectoryFd, name: &str) -> std::io::Result<()> {
    let name = c_name(name)?;
    let rc = unsafe { libc::unlinkat(parent.0, name.as_ptr(), 0) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn directory_names(parent: &DirectoryFd) -> std::io::Result<Vec<String>> {
    let duplicate = unsafe { libc::dup(parent.0) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let directory = unsafe { libc::fdopendir(duplicate) };
    if directory.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(duplicate);
        }
        return Err(error);
    }
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    unsafe {
        libc::closedir(directory);
    }
    Ok(names)
}

#[cfg(unix)]
fn linked_worktree_metadata_at(parent: &DirectoryFd, destination_name: &str) -> std::io::Result<bool> {
    let destination = open_child_directory(parent, destination_name)?;
    let name = c_name(".git")?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe {
        libc::fstatat(
            destination.0,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mode = unsafe { (*stat.as_ptr()).st_mode } & libc::S_IFMT;
    if mode == libc::S_IFREG {
        Ok(true)
    } else if mode == libc::S_IFDIR {
        Ok(false)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported .git metadata type",
        ))
    }
}

#[cfg(unix)]
fn reconcile_quarantine(root: &Path) -> Result<(), DaemonError> {
    let (quarantine_root, quarantine_fd) = open_recovery_namespace(root)?;
    let quarantine_identity = directory_identity(&quarantine_fd).map_err(|e| {
        DaemonError::Config(format!("worktree reaper: identify reconciliation directory: {e}"))
    })?;
    for manifest_name in directory_names(&quarantine_fd).map_err(|e| {
        DaemonError::Config(format!("worktree reaper: enumerate quarantine: {e}"))
    })? {
        let Some(stem) = manifest_name.strip_suffix(".json") else {
            continue;
        };
        let mut manifest = match open_manifest_read(&quarantine_fd, &manifest_name) {
            Ok(file) => file,
            Err(_) => continue,
        };
        let mut body = String::new();
        manifest.read_to_string(&mut body).map_err(|e| {
            DaemonError::Config(format!("worktree reaper: read quarantine manifest: {e}"))
        })?;
        let Some(last) = body.lines().filter(|line| !line.trim().is_empty()).last() else {
            continue;
        };
        let record: serde_json::Value = match serde_json::from_str(last) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if record.get("state").and_then(|state| state.as_str()) != Some("prepared")
            || !entry_exists(&quarantine_fd, stem).map_err(|e| {
                DaemonError::Config(format!("worktree reaper: inspect prepared quarantine: {e}"))
            })?
        {
            continue;
        }
        if linked_worktree_metadata_at(&quarantine_fd, stem).map_err(|e| {
            DaemonError::Config(format!("worktree reaper: inspect moved worktree: {e}"))
        })? {
            repair_linked_worktree(&quarantine_fd, stem)?;
        }
        let current_quarantine = open_directory(&quarantine_root).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: quarantine directory changed during reconciliation: {e}"
            ))
        })?;
        if directory_identity(&current_quarantine).map_err(|e| {
            DaemonError::Config(format!("worktree reaper: identify reconciliation directory: {e}"))
        })? != quarantine_identity
        {
            return Err(DaemonError::Config(
                "worktree reaper: quarantine directory changed during reconciliation".into(),
            ));
        }
        let moved = serde_json::json!({
            "state": "moved",
            "agent_id": record.get("agent_id").cloned().unwrap_or(serde_json::Value::Null),
            "quarantined_path": quarantine_root.join(stem).display().to_string(),
            "reconciled": true,
        });
        write_manifest_line(&quarantine_fd, &manifest_name, &moved, false).map_err(|e| {
            DaemonError::Config(format!("worktree reaper: reconcile quarantine manifest: {e}"))
        })?;
        sync_directory(&quarantine_fd).map_err(|e| {
            DaemonError::Config(format!("worktree reaper: sync reconciled quarantine: {e}"))
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn quarantine_worktree(_root: &Path, _path: &Path, _agent_id: &str) -> Result<(), DaemonError> {
    Err(DaemonError::Config(
        "worktree reaper quarantine requires a Unix filesystem".into(),
    ))
}

/// Active-session probe used by the keep-alive check. The reaper avoids
/// the `ao` CLI dependency at sweep time — callers supply a function that
/// returns `true` iff `agent_id` matches a live session. Production wiring
/// passes an `ao status` filter; tests pass a synthetic map.
pub trait ActiveSessionProbe {
    fn is_active(&self, agent_id: &str) -> bool;
}

/// No-op probe — every agent is treated as inactive. Suitable for tests,
/// unit sweeps, and post-merge flushes where the caller has already
/// proven the agent is dead.
pub struct InactiveProbe;

impl ActiveSessionProbe for InactiveProbe {
    fn is_active(&self, _agent_id: &str) -> bool {
        false
    }
}

/// Predicate probe backed by a `HashMap<String, bool>`. Each test seeds
/// `active` with the agent_ids it wants to keep alive; the reaper
/// never touches a worktree whose agent_id is `true`.
pub struct MapProbe {
    pub active: std::collections::HashMap<String, bool>,
}

impl ActiveSessionProbe for MapProbe {
    fn is_active(&self, agent_id: &str) -> bool {
        self.active.get(agent_id).copied().unwrap_or(false)
    }
}

/// Reason a candidate was kept (folded from jleechan-y189: the ONLY valid
/// refusal is "active session PID", not "dir exists" or "branch
/// not present").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepReason {
    /// The agent_id matches a live session per the active probe.
    ActiveSession,
    /// Path is not a directory (file, symlink, broken mount). The reaper
    /// refuses to delete files it did not create.
    NotADirectory,
    /// The path does not parse as an agent-id directory. The reaper only
    /// prunes agents it owns; arbitrary files under the root are
    /// untouched.
    NotAnAgentId,
}

/// Decide whether `candidate` is prunable. Active agents are kept. Stale
/// mtime (older than `ttl_secs`) is the default prunable signal.
pub fn is_prunable(
    candidate: &Candidate,
    ttl_secs: u64,
    now_secs: u64,
    probe: &dyn ActiveSessionProbe,
) -> Result<bool, KeepReason> {
    if probe.is_active(&candidate.agent_id) {
        return Err(KeepReason::ActiveSession);
    }
    let age = now_secs.saturating_sub(candidate.mtime_secs);
    Ok(age >= ttl_secs)
}

/// Enumerate every directory under `root` that parses as an agent-id
/// directory. The reaper does NOT recurse beyond depth 1 — agents own
/// `root/<agent_id>` only, and a nested directory is a sign of a bug
/// the reaper must NOT silently delete.
pub fn enumerate_candidates(root: &Path) -> Result<Vec<Candidate>, DaemonError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    #[cfg(unix)]
    reconcile_quarantine(root)?;
    let entries = std::fs::read_dir(root).map_err(|e| {
        DaemonError::Config(format!("worktree reaper: read_dir {}: {e}", root.display()))
    })?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        let agent_id = match entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        if agent_id == QUARANTINE_DIR_NAME || !is_plain_directory(&path).unwrap_or(false) {
            continue;
        }
        let mtime_secs = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let size_bytes = dir_size_bytes(&path).unwrap_or(0);
        out.push(Candidate {
            path,
            agent_id,
            mtime_secs,
            size_bytes,
        });
    }
    Ok(out)
}

/// Compute the recursive byte size of a directory. Errors are mapped to
/// `0` because the reaper treats size as a coarse-grained telemetry
/// signal — losing one entry's size is acceptable; the total is
/// dominated by the worktree's blob count.
pub fn dir_size_bytes(path: &Path) -> std::io::Result<u64> {
    let mut total: u64 = 0;
    fn walk(p: &Path, total: &mut u64) -> std::io::Result<()> {
        if p.is_file() {
            if let Ok(meta) = p.metadata() {
                *total = total.saturating_add(meta.len());
            }
            return Ok(());
        }
        for entry in std::fs::read_dir(p)? {
            let entry = entry?;
            walk(&entry.path(), total)?;
        }
        Ok(())
    }
    walk(path, &mut total)?;
    Ok(total)
}

/// Run one sweep. Returns the report; callers decide whether to emit
/// telemetry and whether to actually delete the prunable candidates.
/// The reaper does NOT delete by itself — it is a pure reporter, so the
/// caller can preview the prune landscape before committing.
pub fn sweep(
    cfg: &Config,
    repo: &str,
    now_secs: u64,
    probe: &dyn ActiveSessionProbe,
) -> Result<ReaperReport, DaemonError> {
    let root = match cfg.agent_worktree_root_for_repo(repo) {
        Some(r) => r,
        None => {
            return Ok(ReaperReport {
                repo: repo.to_string(),
                root: PathBuf::new(),
                total_worktrees: 0,
                prunable_count: 0,
                total_bytes: 0,
                kept_active_count: 0,
                skipped_oversized_count: 0,
            });
        }
    };
    let candidates = enumerate_candidates(&root)?;
    let mut total_bytes: u64 = 0;
    let mut prunable_count = 0;
    let mut kept_active_count = 0;
    let mut skipped_oversized_count = 0;
    for candidate in &candidates {
        total_bytes = total_bytes.saturating_add(candidate.size_bytes);
        match is_prunable(candidate, cfg.worktree_ttl_secs, now_secs, probe) {
            Ok(true) => prunable_count += 1,
            Ok(false) => {}
            Err(KeepReason::ActiveSession) => kept_active_count += 1,
            Err(KeepReason::NotADirectory) | Err(KeepReason::NotAnAgentId) => {
                skipped_oversized_count += 1;
            }
        }
    }
    Ok(ReaperReport {
        repo: repo.to_string(),
        root,
        total_worktrees: candidates.len(),
        prunable_count,
        total_bytes,
        kept_active_count,
        skipped_oversized_count,
    })
}

/// Force-sweep (manual flush). Same as `sweep` but emits the
/// `WORKTREE_REAPER_REPORT` telemetry event so an operator can verify
/// the floor at any time without waiting for the periodic tick.
pub fn flush(
    cfg: &Config,
    repo: &str,
    now_secs: u64,
    probe: &dyn ActiveSessionProbe,
    telemetry_log: Option<&Path>,
) -> Result<ReaperReport, DaemonError> {
    let report = sweep(cfg, repo, now_secs, probe)?;
    if let Some(log) = telemetry_log {
        let event = TelemetryEvent {
            timestamp: crate::state::now_iso8601(),
            host: local_hostname(),
            bead_id: format!("worktree-reaper:{}", repo),
            attempt_id: 0,
            lifecycle_state: "REAPER_FLUSH".to_string(),
            event_type: "WORKTREE_REAPER_REPORT".to_string(),
            metrics: serde_json::json!({
                "repo": report.repo,
                "total_worktrees": report.total_worktrees,
                "prunable_count": report.prunable_count,
                "total_bytes": report.total_bytes,
                "kept_active_count": report.kept_active_count,
                "skipped_oversized_count": report.skipped_oversized_count,
                "ttl_secs": cfg.worktree_ttl_secs,
                "max_count": cfg.worktree_max_count,
            }),
            context: serde_json::json!({"root": report.root.display().to_string()}),
        };
        emit(log, &event)?;
    }
    Ok(report)
}

/// Cap enforcement. Called when the daemon is about to register a new
/// agent worktree. Returns `Ok(())` when there is room (under the cap or
/// the cap is disabled). Returns `Err(DaemonError::Config)` when the cap
/// is exceeded.
pub fn check_cap(cfg: &Config, repo: &str, new_agent_id: &str) -> Result<(), DaemonError> {
    let Some(root) = cfg.agent_worktree_root_for_repo(repo) else {
        return Ok(());
    };
    let candidates = enumerate_candidates(&root)?;
    if candidates.len() >= cfg.worktree_max_count {
        return Err(DaemonError::Config(format!(
            "agent worktree cap exceeded for {repo}: existing {} >= max {} (new agent {new_agent_id} refused; flush the reaper or raise worktree_max_count)",
            candidates.len(),
            cfg.worktree_max_count
        )));
    }
    Ok(())
}

/// Delete the prunable candidates from a sweep report. Refuses to
/// delete any path the reaper did not enumerate (defensive: a buggy
/// caller cannot wipe arbitrary directories).
pub fn reap(report: &ReaperReport, candidates: &[Candidate]) -> Result<usize, DaemonError> {
    let mut removed = 0;
    for candidate in candidates {
        if candidate.path.parent() != Some(report.root.as_path())
            || candidate.path.file_name().and_then(|name| name.to_str())
                != Some(candidate.agent_id.as_str())
        {
            continue;
        }
        if !is_plain_directory(&candidate.path).unwrap_or(false) {
            continue;
        }
        if is_dirty_worktree(&candidate.path)? {
            quarantine_worktree(&report.root, &candidate.path, &candidate.agent_id)?;
        } else {
            remove_worktree(&candidate.path)?;
        }
        removed += 1;
    }
    Ok(removed)
}

/// Bead rev-3lm8k: immediately clean up ONE agent's worktree directory the
/// moment its coder session is known to have exited (`SessionActivity`
/// reaches `Idle`/terminal and the daemon calls `sessions.stop()`, or a
/// park-to-`HUMAN_HELD` kill). Unlike `sweep()`/`is_prunable()`, this does
/// NOT consult the active-session probe or `worktree_ttl_secs` — the
/// caller already knows the session is dead, so waiting out the TTL would
/// leave the stale AO-managed worktree blocking subsequent dispatches that
/// hash to the same orchestrator branch (the observed incident: `wa-3538`
/// under `/home/jleechan/.worktrees/worldarchitect/` blocked every
/// `slim/two_node.dot` dispatch hashing to its branch until a human
/// manually removed the directory).
///
/// Returns `Ok(true)` iff a directory was actually removed. `Ok(false)`
/// when `cfg.agent_worktree_root` is unset (knob off), `agent_id` fails
/// the path-traversal guard in `Config::agent_worktree_path`, or nothing
/// exists at the resolved path — all treated as a no-op rather than an
/// error, since "nothing to clean" is the expected steady state once a
/// worktree has already been reaped once. Refuses to touch a path that
/// exists but is not a plain directory (defensive: never delete a file,
/// symlink, or unexpected mount the reaper does not own).
pub fn clean_stale_worktree(
    cfg: &Config,
    repo: &str,
    agent_id: &str,
) -> Result<bool, DaemonError> {
    let Some(path) = cfg.agent_worktree_path(repo, agent_id) else {
        return Ok(false);
    };
    if !is_plain_directory(&path).unwrap_or(false) {
        return Ok(false);
    }
    let root = cfg
        .agent_worktree_root_for_repo(repo)
        .expect("agent_worktree_path and root must agree");
    if is_dirty_worktree(&path)? {
        quarantine_worktree(&root, &path, agent_id)?;
    } else {
        remove_worktree(&path)?;
    }
    Ok(true)
}

/// Helper for tests: epoch seconds from SystemTime.
pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_cfg(root: &Path) -> Config {
        Config {
            target_repo: "owner/repo".into(),
            ao_project: None,
            base_branch: "main".into(),
            stage: 1,
            max_workers: 1,
            max_batch: 1,
            fast_tick_secs: 1,
            slow_tick_secs: 1,
            autonomy_timebox_secs: 60,
            budget_warn_usd: 1.0,
            spec_dir: ".factory/specs/".into(),
            reroll_head_stability_window_secs: 1,
            reroll_death_confirm_secs: 0,
            held_recheck_cooldown_secs: 0,
            repos: std::collections::HashMap::new(),
            pre_gate_validation_enabled: false,
            escalation_refire_secs: 0,
            agent_worktree_root: Some(root.display().to_string()),
            worktree_ttl_secs: 60,
            worktree_max_count: 10,
        }
    }

    #[cfg(unix)]
    fn test_recovery_root(root: &Path) -> PathBuf {
        recovery_namespace_path(&root.join("owner/repo")).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn recovery_namespace_is_outside_agent_worktree_root() {
        let root = std::env::temp_dir().join(format!(
            "afd_reaper_recovery_namespace_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("owner/repo")).unwrap();
        let namespace = test_recovery_root(&root);
        assert!(!namespace.starts_with(root.join("owner/repo")));
        assert!(namespace.starts_with(
            crate::intake::runtime_state_dir().canonicalize().unwrap()
        ));
        let _ = fs::remove_dir_all(&root);
    }

    fn touch_dir(path: &Path, secs_ago: u64) {
        fs::create_dir_all(path).unwrap();
        let mtime = SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        let f = fs::File::create(path.join("marker")).unwrap();
        let _ = f.set_modified(mtime);
        // The reaper reads each entry's MODIFIED time, which on a directory
        // is the directory's own mtime; touching only the marker file is
        // not enough. `set_modified` on the directory itself is the
        // Unix-portable way to backdate the entry the reaper observes.
        let _ = fs::OpenOptions::new()
            .read(true)
            .open(path)
            .and_then(|d| d.set_modified(mtime));
    }

    fn init_git_worktree(path: &Path) {
        fs::create_dir_all(path).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(path.join("tracked.txt"), "original\n").unwrap();
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
                "add",
                "tracked.txt",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn init_linked_worktree(repo: &Path, worktree: &Path) {
        fs::create_dir_all(repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(repo.join("tracked.txt"), "original\n").unwrap();
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
                "add",
                "tracked.txt",
            ])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
                "commit",
                "-qm",
                "initial",
            ])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args(["worktree", "add", "-q", "-b", "agent-branch", worktree.to_str().unwrap()])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn sweep_does_not_enumerate_when_root_is_unset() {
        let cfg = Config {
            agent_worktree_root: None,
            ..make_cfg(Path::new("/tmp"))
        };
        let report = sweep(&cfg, "owner/repo", 1_000_000, &InactiveProbe).unwrap();
        assert_eq!(report.total_worktrees, 0);
        assert_eq!(report.prunable_count, 0);
    }

    #[test]
    fn sweep_identifies_ttl_expired_candidates_as_prunable() {
        let root = std::env::temp_dir().join(format!("afd_reaper_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let fresh = root.join("owner/repo/df-100");
        let stale = root.join("owner/repo/df-200");
        touch_dir(&fresh, 1);
        touch_dir(&stale, 24 * 60 * 60);
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        assert_eq!(report.total_worktrees, 2);
        assert_eq!(report.prunable_count, 1, "only df-200 is past TTL");
        assert_eq!(report.kept_active_count, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn active_session_probe_keeps_worktree_even_when_ttl_expired() {
        let root = std::env::temp_dir().join(format!("afd_reaper_active_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let stale = root.join("owner/repo/df-300");
        touch_dir(&stale, 24 * 60 * 60);
        let mut active = std::collections::HashMap::new();
        active.insert("df-300".to_string(), true);
        let probe = MapProbe { active };
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &probe).unwrap();
        assert_eq!(report.total_worktrees, 1);
        assert_eq!(report.prunable_count, 0);
        assert_eq!(report.kept_active_count, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn check_cap_allows_under_limit_and_refuses_at_limit() {
        let root = std::env::temp_dir().join(format!("afd_reaper_cap_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        for i in 0..5 {
            touch_dir(&root.join(format!("owner/repo/df-{}", i)), 0);
        }
        assert!(check_cap(&cfg, "owner/repo", "df-new").is_ok());
        let mut capped = cfg.clone();
        capped.worktree_max_count = 3;
        let err = check_cap(&capped, "owner/repo", "df-new").unwrap_err();
        assert!(err.to_string().contains("agent worktree cap exceeded"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn check_cap_is_inert_when_root_unset() {
        let cfg = Config {
            agent_worktree_root: None,
            ..make_cfg(Path::new("/tmp"))
        };
        assert!(check_cap(&cfg, "owner/repo", "df-anything").is_ok());
    }

    #[test]
    fn reap_deletes_only_paths_under_root() {
        let root = std::env::temp_dir().join(format!("afd_reaper_reap_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let target = root.join("owner/repo/df-r1");
        init_git_worktree(&target);
        let outside = std::env::temp_dir().join(format!("afd_outside_{}", std::process::id()));
        touch_dir(&outside, 0);
        let candidates = vec![
            Candidate {
                path: target.clone(),
                agent_id: "df-r1".into(),
                mtime_secs: 0,
                size_bytes: 0,
            },
            Candidate {
                path: outside.clone(),
                agent_id: "df-evil".into(),
                mtime_secs: 0,
                size_bytes: 0,
            },
        ];
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        let removed = reap(&report, &candidates).unwrap();
        assert_eq!(removed, 1);
        assert!(!target.exists());
        assert!(outside.exists(), "reaper must not touch paths outside its root");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn reap_quarantines_dirty_tracked_worktree_and_records_recovery_path() {
        let root = std::env::temp_dir().join(format!("afd_reaper_dirty_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let target = root.join("owner/repo/df-dirty");
        init_git_worktree(&target);
        fs::write(target.join("tracked.txt"), "uncommitted\n").unwrap();
        let candidates = enumerate_candidates(&cfg.agent_worktree_root_for_repo("owner/repo").unwrap()).unwrap();
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();

        assert_eq!(reap(&report, &candidates).unwrap(), 1);
        assert!(!target.exists());
        let quarantine = test_recovery_root(&root);
        let entries: Vec<_> = fs::read_dir(&quarantine).unwrap().map(Result::unwrap).collect();
        assert_eq!(entries.len(), 2, "manifest and quarantined worktree are both durable");
        let recovered = entries
            .iter()
            .find(|entry| entry.path().is_dir())
            .map(|entry| entry.path())
            .unwrap();
        assert_eq!(fs::read_to_string(recovered.join("tracked.txt")).unwrap(), "uncommitted\n");
        let manifest = entries
            .iter()
            .find(|entry| entry.path().is_file())
            .map(|entry| fs::read_to_string(entry.path()).unwrap())
            .unwrap();
        assert!(manifest.contains("df-dirty"));
        assert!(manifest.contains(&recovered.display().to_string()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_stale_worktree_quarantines_untracked_worktree_and_records_recovery_path() {
        let root = std::env::temp_dir().join(format!("afd_clean_untracked_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let target = root.join("owner/repo/wa-untracked");
        init_git_worktree(&target);
        fs::write(target.join("new.txt"), "untracked\n").unwrap();

        assert!(clean_stale_worktree(&cfg, "owner/repo", "wa-untracked").unwrap());
        assert!(!target.exists());
        let quarantine = test_recovery_root(&root);
        let entries: Vec<_> = fs::read_dir(&quarantine).unwrap().map(Result::unwrap).collect();
        let recovered = entries
            .iter()
            .find(|entry| entry.path().is_dir())
            .map(|entry| entry.path())
            .unwrap();
        assert_eq!(fs::read_to_string(recovered.join("new.txt")).unwrap(), "untracked\n");
        assert!(entries.iter().any(|entry| {
            entry.path().is_file()
                && fs::read_to_string(entry.path())
                    .map(|body| body.contains("wa-untracked"))
                    .unwrap_or(false)
        }));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reap_refuses_nested_candidate_without_touching_it() {
        let root = std::env::temp_dir().join(format!("afd_reaper_nested_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let target = root.join("owner/repo/df-nested/inner");
        touch_dir(&target, 0);
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        let candidate = Candidate {
            path: target.clone(),
            agent_id: "df-nested".into(),
            mtime_secs: 0,
            size_bytes: 0,
        };

        assert_eq!(reap(&report, &[candidate]).unwrap(), 0);
        assert!(target.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reap_fails_closed_when_git_metadata_is_absent_or_dangling() {
        for suffix in ["absent", "dangling"] {
            let root = std::env::temp_dir().join(format!("afd_reaper_git_meta_{suffix}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let cfg = make_cfg(&root);
            let target = root.join(format!("owner/repo/df-{suffix}"));
            touch_dir(&target, 0);
            if suffix == "dangling" {
                fs::write(target.join(".git"), "gitdir: /does/not/exist\n").unwrap();
            }
            let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
            let candidates = enumerate_candidates(&report.root).unwrap();

            assert!(reap(&report, &candidates).is_err());
            assert!(target.exists(), "worktree with invalid Git metadata must survive");
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn reap_quarantines_ignored_file_as_dirty() {
        let root = std::env::temp_dir().join(format!("afd_reaper_ignored_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let target = root.join("owner/repo/df-ignored");
        init_git_worktree(&target);
        fs::write(target.join(".gitignore"), "*.secret\n").unwrap();
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
                "add",
                ".gitignore",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=Test",
                "commit",
                "-qm",
                "ignore",
            ])
            .current_dir(&target)
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(target.join("credentials.secret"), "must preserve\n").unwrap();
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        let candidates = enumerate_candidates(&report.root).unwrap();

        assert_eq!(reap(&report, &candidates).unwrap(), 1);
        let recovered = fs::read_dir(test_recovery_root(&root))
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path().is_dir())
            .unwrap()
            .path();
        assert_eq!(fs::read_to_string(recovered.join("credentials.secret")).unwrap(), "must preserve\n");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reap_uses_git_worktree_move_for_dirty_linked_worktree() {
        let root = std::env::temp_dir().join(format!("afd_reaper_linked_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let repo = root.join("source-repo");
        let target = root.join("owner/repo/df-linked");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        init_linked_worktree(&repo, &target);
        fs::write(target.join("tracked.txt"), "linked dirty\n").unwrap();
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        let candidates = enumerate_candidates(&report.root).unwrap();

        assert_eq!(reap(&report, &candidates).unwrap(), 1);
        let recovered = fs::read_dir(test_recovery_root(&root))
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path().is_dir())
            .unwrap()
            .path();
        assert_eq!(fs::read_to_string(recovered.join("tracked.txt")).unwrap(), "linked dirty\n");
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        assert_eq!(report.total_worktrees, 0, "prune must leave quarantined worktree intact");
        let status = std::process::Command::new("git")
            .args(["-C", recovered.to_str().unwrap(), "status", "--porcelain=v1"])
            .status()
            .unwrap();
        assert!(status.success(), "quarantined linked worktree must retain Git metadata");
        let list = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let list = String::from_utf8(list.stdout).unwrap();
        assert!(list.contains(recovered.to_str().unwrap()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reap_uses_git_worktree_remove_for_clean_linked_worktree() {
        let root = std::env::temp_dir().join(format!("afd_reaper_clean_linked_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let repo = root.join("source-repo");
        let target = root.join("owner/repo/df-linked-clean");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        init_linked_worktree(&repo, &target);
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        let candidates = enumerate_candidates(&report.root).unwrap();

        assert_eq!(reap(&report, &candidates).unwrap(), 1);
        assert!(!target.exists());
        let list = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let list = String::from_utf8(list.stdout).unwrap();
        assert!(!list.contains(target.to_str().unwrap()));
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn clean_stale_worktree_does_not_use_worker_quarantine_symlink() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("afd_reaper_quarantine_link_{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("afd_reaper_quarantine_outside_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        let cfg = make_cfg(&root);
        let target = root.join("owner/repo/wa-link");
        init_git_worktree(&target);
        fs::write(target.join("tracked.txt"), "dirty\n").unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("owner/repo/.quarantine")).unwrap();

        assert!(clean_stale_worktree(&cfg, "owner/repo", "wa-link").unwrap());
        assert!(!target.exists());
        assert!(!outside.join("wa-link").exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn quarantine_directory_handle_survives_path_swap_without_escape() {
        let root = std::env::temp_dir().join(format!("afd_reaper_handle_swap_{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("afd_reaper_handle_outside_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        let repo_root = root.join("owner/repo");
        let target = repo_root.join("df-handle-race");
        fs::create_dir_all(&repo_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        init_git_worktree(&target);
        fs::write(target.join("tracked.txt"), "must survive handle swap\n").unwrap();

        assert!(
            quarantine_worktree_inner(&repo_root, &target, "df-handle-race", Some(&outside), false)
                .is_err()
        );
        assert!(target.exists(), "a swapped quarantine root must roll back the move");
        assert_eq!(
            fs::read_to_string(target.join("tracked.txt")).unwrap(),
            "must survive handle swap\n"
        );
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        let quarantine = recovery_namespace_path(&repo_root).unwrap();
        let original_quarantine = quarantine.with_extension("original");
        assert!(
            fs::read_dir(&original_quarantine)
                .unwrap()
                .next()
                .is_none(),
            "rollback must remove the prepared recovery record"
        );
        let _ = fs::remove_file(&quarantine);
        let _ = fs::remove_dir_all(&original_quarantine);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn rollback_never_overwrites_recreated_agent_directory() {
        let root = std::env::temp_dir().join(format!(
            "afd_reaper_destination_recreated_{}",
            std::process::id()
        ));
        let outside = std::env::temp_dir().join(format!(
            "afd_reaper_destination_outside_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
        let repo_root = root.join("owner/repo");
        let target = repo_root.join("df-recreated");
        fs::create_dir_all(&repo_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        init_git_worktree(&target);
        fs::write(target.join("tracked.txt"), "must survive recreate\n").unwrap();

        assert!(
            quarantine_worktree_inner(
                &repo_root,
                &target,
                "df-recreated",
                Some(&outside),
                true,
            )
            .is_err()
        );
        assert!(target.exists(), "recreated destination must be preserved");
        assert!(
            !target.join("tracked.txt").exists(),
            "rollback must not overwrite the recreated destination"
        );
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        let quarantine = recovery_namespace_path(&repo_root).unwrap();
        let original_quarantine = quarantine.with_extension("original");
        let preserved = fs::read_dir(&original_quarantine)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path().is_dir())
            .map(|entry| entry.path())
            .unwrap();
        assert_eq!(
            fs::read_to_string(preserved.join("tracked.txt")).unwrap(),
            "must survive recreate\n"
        );
        let _ = fs::remove_file(&quarantine);
        let _ = fs::remove_dir_all(&original_quarantine);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn sweep_reconciles_linked_worktree_moved_before_repair() {
        let root = std::env::temp_dir().join(format!("afd_reaper_reconcile_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let repo = root.join("source-repo");
        let target = root.join("owner/repo/df-crash");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let quarantine = test_recovery_root(&root);
        fs::create_dir_all(&quarantine).unwrap();
        init_linked_worktree(&repo, &target);
        let destination = quarantine.join("df-crash-raw");
        fs::rename(&target, &destination).unwrap();
        fs::write(
            quarantine.join("df-crash-raw.json"),
            serde_json::json!({
                "state": "prepared",
                "agent_id": "df-crash",
                "original_path": target.display().to_string(),
                "quarantined_path": destination.display().to_string(),
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        assert_eq!(report.total_worktrees, 0);
        let status = std::process::Command::new("git")
            .args(["-C", destination.to_str().unwrap(), "status", "--porcelain=v1"])
            .status()
            .unwrap();
        assert!(status.success(), "reconciled linked worktree must be usable");
        let manifest = fs::read_to_string(quarantine.join("df-crash-raw.json")).unwrap();
        assert!(manifest.contains("\"state\":\"moved\""));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dirty_status_failure_is_fail_closed() {
        let root = std::env::temp_dir().join(format!("afd_reaper_status_failure_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let target = root.join("owner/repo/df-broken");
        touch_dir(&target, 0);
        fs::write(target.join(".git"), "not a repository\n").unwrap();
        let report = sweep(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe).unwrap();
        let candidates = enumerate_candidates(&report.root).unwrap();

        assert!(reap(&report, &candidates).is_err());
        assert!(target.exists(), "status failure must never destroy the worktree");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn agent_worktree_path_rejects_empty_or_path_traversal_agent_id() {
        let cfg = make_cfg(Path::new("/tmp/agent_worktrees"));
        assert!(cfg.agent_worktree_path("owner/repo", "").is_none());
        assert!(cfg.agent_worktree_path("owner/repo", "../escape").is_none());
        assert!(cfg.agent_worktree_path("owner/repo", "nested/agent").is_none());
        assert!(cfg.agent_worktree_path("owner/repo", "df-100").is_some());
    }

    #[test]
    fn agent_worktree_root_for_repo_returns_none_when_knob_off() {
        let cfg = Config {
            agent_worktree_root: None,
            ..make_cfg(Path::new("/tmp"))
        };
        assert!(cfg.agent_worktree_root_for_repo("owner/repo").is_none());
    }

    #[test]
    fn flush_emits_telemetry_event() {
        let root = std::env::temp_dir().join(format!("afd_reaper_flush_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let stale = root.join("owner/repo/df-400");
        touch_dir(&stale, 24 * 60 * 60);
        let log = std::env::temp_dir().join(format!("afd_reaper_telem_{}.jsonl", std::process::id()));
        let _ = fs::remove_file(&log);
        let report = flush(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe, Some(&log)).unwrap();
        assert_eq!(report.prunable_count, 1);
        let body = fs::read_to_string(&log).unwrap();
        assert!(body.contains("WORKTREE_REAPER_REPORT"));
        assert!(body.contains("prunable_count"));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&log);
    }

    #[test]
    fn flush_with_no_telemetry_path_is_a_noop_for_io() {
        let root = std::env::temp_dir().join(format!("afd_reaper_notelem_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let report = flush(&cfg, "owner/repo", now_epoch_secs(), &InactiveProbe, None).unwrap();
        assert_eq!(report.total_worktrees, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn is_prunable_requires_active_session_refusal() {
        let candidate = Candidate {
            path: PathBuf::from("/tmp/df-100"),
            agent_id: "df-100".into(),
            mtime_secs: 100,
            size_bytes: 0,
        };
        let mut active = std::collections::HashMap::new();
        active.insert("df-100".to_string(), true);
        let probe = MapProbe { active };
        let result = is_prunable(&candidate, 60, 200, &probe);
        assert!(matches!(result, Err(KeepReason::ActiveSession)));
        let inactive = InactiveProbe;
        let result = is_prunable(&candidate, 60, 200, &inactive);
        assert!(matches!(result, Ok(true)));
    }

    // Bead rev-3lm8k: `clean_stale_worktree` removes a single known-dead
    // agent's worktree immediately, bypassing TTL — the fix for "AO coder
    // sessions finish work and leave behind worktree dirs" blocking later
    // dispatches until a human manually cleans up.

    #[test]
    fn clean_stale_worktree_removes_fresh_directory_immediately() {
        let root = std::env::temp_dir().join(format!("afd_clean_fresh_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        // `touch_dir` with 0 seconds ago is well within `worktree_ttl_secs`
        // (60s in `make_cfg`) — a TTL-based sweep would NOT prune this, but
        // `clean_stale_worktree` must remove it anyway because the caller
        // already knows the session exited.
        let target = root.join("owner/repo/wa-500");
        init_git_worktree(&target);
        assert!(target.is_dir());
        let removed = clean_stale_worktree(&cfg, "owner/repo", "wa-500").unwrap();
        assert!(removed, "a fresh but session-dead worktree must still be removed");
        assert!(!target.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_stale_worktree_is_noop_when_knob_is_off() {
        let cfg = Config {
            agent_worktree_root: None,
            ..make_cfg(Path::new("/tmp"))
        };
        let removed = clean_stale_worktree(&cfg, "owner/repo", "wa-500").unwrap();
        assert!(!removed, "legacy layout (knob off) must be a no-op, not an error");
    }

    #[test]
    fn clean_stale_worktree_is_noop_when_directory_absent() {
        let root = std::env::temp_dir().join(format!("afd_clean_absent_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let removed = clean_stale_worktree(&cfg, "owner/repo", "wa-nonexistent").unwrap();
        assert!(!removed, "nothing to clean is the expected steady state, not an error");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_stale_worktree_rejects_path_traversal_agent_id() {
        let root = std::env::temp_dir().join(format!("afd_clean_traversal_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let removed = clean_stale_worktree(&cfg, "owner/repo", "../escape").unwrap();
        assert!(!removed, "path-traversal agent_id must be refused, not resolved");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_stale_worktree_only_touches_directories() {
        let root = std::env::temp_dir().join(format!("afd_clean_file_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cfg = make_cfg(&root);
        let path = cfg.agent_worktree_path("owner/repo", "wa-file").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not a worktree dir").unwrap();
        let removed = clean_stale_worktree(&cfg, "owner/repo", "wa-file").unwrap();
        assert!(!removed, "a non-directory at the resolved path must be left alone");
        assert!(path.is_file(), "the file must survive untouched");
        let _ = fs::remove_dir_all(&root);
    }
}
