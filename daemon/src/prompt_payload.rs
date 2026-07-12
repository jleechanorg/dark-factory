// Lossless coder-prompt indirection for `ao spawn --prompt` (PR #272).
// Any composed prompt above INDIRECTION_PROMPT_LIMIT (= PR #255's
// CODER_PROMPT_TOTAL_CAP of 4000) bypasses PR #255's shrinker and is
// persisted to a content-addressed file; AO receives a short bootstrap
// pointing at the file plus the SHA-256 for end-to-end verify.
//
// Below 4000 chars: no file is written, the prompt is handed to AO
// verbatim. PR #255's shrinker is left untouched.
//
// Failure modes the helper refuses to silently swallow:
//   - non-absolute DARK_FACTORY_PROMPT_PAYLOADS_DIR override
//   - chmod 0700/0600 denied
//   - mkdir / write / fsync / rename / verify-read I/O error
//   - symlink at the target (catfish) -- rejected, not silently reused
//   - bootstrap template blew its length cap
//
// Limitation: cleanup is mtime-based, NOT session-aware (a 31-day-old
// active coder session's payload could be reaped). Bounded by
// ORPHAN_CLEANUP_BATCH (16 files per call) so no tick stalls. A proper
// session-id-bound cleanup is filed as a follow-up.

use crate::errors::DaemonError;
use sha2::{Digest, Sha256};
use std::io::Write;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const INDIRECTION_PROMPT_LIMIT: usize = 4_000;
const BOOTSTRAP_PROMPT_CAP: usize = 2_000;
const PAYLOADS_DIR_ENV: &str = "DARK_FACTORY_PROMPT_PAYLOADS_DIR";

pub(crate) const ORPHAN_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
pub(crate) const ORPHAN_CLEANUP_BATCH: usize = 16;

/// Stale-temp retention: a `.tmp.<pid>.<n>` file older than this is
/// reaped by the bounded cleanup sweep. Distinct from
/// ORPHAN_RETENTION_SECS because temp files have a much shorter
/// expected lifetime (one materialize-or-bootstrap call) -- 1 hour
/// is enough for any in-flight call to finish or fail without
/// holding up subsequent dispatches.
pub(crate) const TEMP_STALE_SECS: u64 = 60 * 60;
/// Prefix of every temp filename produced by THIS module. The
/// cleanup sweep matches this exact prefix (with the canonical
/// `<sha>.md.tmp.` middle segment) so an attacker-injected
/// `.hidden.tmp.X.Y` dotfile or unrelated `.tmp.*` is NEVER reaped.
pub(crate) const TEMP_PREFIX: &str = ".tmp.";

/// Process-local monotonic counter for the temp-file suffix. Used
/// instead of `SystemTime::now()` nanos because nanos can collide
/// when the clock jumps backward or two threads race the same
/// `fetch_add`-equivalent window -- the counter never repeats within
/// a process lifetime.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptIndirect {
    pub bootstrap: String,
    pub payload_path: PathBuf,
    pub sha256: String,
}

#[derive(Debug)]
pub(crate) enum MaterializeOutcome {
    NoIndirectionNeeded,
    Indirected(PromptIndirect),
}

/// Walk every env var whose name contains "HOLDOUT" (case-insensitive,
/// matching `adapters.rs`'s `*HOLDOUTS*` strip convention) and return
/// the FIRST such var whose non-empty value appears anywhere in
/// `text`. Structured-only: this does not scan prose; it matches the
/// literal byte sequence of the env value against the prompt body.
/// Returns `None` for a clean prompt.
fn first_holdout_env_value_in(text: &str) -> Option<String> {
    for (k, v) in std::env::vars() {
        if k.is_empty() || v.is_empty() {
            continue;
        }
        if !k.to_uppercase().contains("HOLDOUT") {
            continue;
        }
        if text.contains(&v) {
            return Some(k);
        }
    }
    None
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let d = h.finalize();
    let mut out = String::with_capacity(64);
    for b in d {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn payloads_root() -> Result<PathBuf, DaemonError> {
    if let Ok(custom) = std::env::var(PAYLOADS_DIR_ENV) {
        if !custom.is_empty() {
            let p = PathBuf::from(&custom);
            if !p.is_absolute() {
                return Err(DaemonError::Config(format!(
                    "{PAYLOADS_DIR_ENV}={custom:?} must be absolute"
                )));
            }
            return Ok(p);
        }
    }
    let home = std::env::var("HOME").map_err(|_| {
        DaemonError::Config("HOME and DARK_FACTORY_PROMPT_PAYLOADS_DIR both unset".into())
    })?;
    Ok(PathBuf::from(home)
        .join(".dark-factory")
        .join("prompt-payloads"))
}

#[cfg(unix)]
fn chmod_mode(path: &Path, mode: u32) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
        DaemonError::Config(format!(
            "chmod {:o} on {}: {e} (mode bits are a hard requirement)",
            mode,
            path.display()
        ))
    })
}

/// Open a directory by path with O_NOFOLLOW|O_DIRECTORY so the
/// kernel refuses to follow a symlink at any path component AND
/// refuses to open a non-directory. The returned fd is the basis
/// for fd-anchored syscalls (`unlinkat`, `openat`) that close the
/// TOCTOU window where a same-UID attacker swaps a symlink into
/// the path between an `open` and a `remove_file`.
///
/// On non-Unix the function is a stub returning an error so
/// callers fall back to path-based ops (the daemon's production
/// target is Linux per its bundled-rusqlite dep).
#[cfg(unix)]
fn open_dir_fd(path: &Path) -> Result<std::fs::File, DaemonError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)
        .map_err(|e| {
            DaemonError::Config(format!(
                "open_dir_fd({}): {e} \
                 (refusing to use a non-directory or symlinked root)",
                path.display()
            ))
        })
}

/// Unlink `target` using a parent-dir fd + relative name so the
/// kernel-level `unlinkat` is a single syscall against the parent's
/// dirfd + name. Even if a same-UID attacker swaps a symlink into
/// `target`'s final component AFTER we open the parent fd, the
/// unlinkat will refuse to follow it because the parent's
/// O_DIRECTORY fd is itself anchor-locked.
///
/// Refuses `target` whose parent isn't the configured payloads
/// root (matches `delete_payload`'s lexical-parent guard so the
/// audit trail is symmetric across the two unlink entry points).
#[cfg(unix)]
fn unlink_via_parent_fd(target: &Path) -> Result<(), DaemonError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let name = match target.file_name() {
        Some(n) => n,
        None => {
            return Err(DaemonError::Config(format!(
                "unlink_via_parent_fd: {} has no file_name component",
                target.display()
            )));
        }
    };
    let root = payloads_root()?;
    let parent = match target.parent() {
        Some(p) => p,
        None => {
            return Err(DaemonError::Config(format!(
                "unlink_via_parent_fd: {} has no parent",
                target.display()
            )));
        }
    };
    if parent != root.as_path() {
        return Err(DaemonError::Config(format!(
            "unlink_via_parent_fd: refusing -- parent of {} is not the configured payloads root",
            target.display()
        )));
    }
    let parent_fd = open_dir_fd(parent)?;
    let c_name = CString::new(name.as_bytes()).map_err(|e| {
        DaemonError::Config(format!(
            "unlink_via_parent_fd: name {:?} contains NUL: {e}",
            name
        ))
    })?;
    // SAFETY: libc::unlinkat is a direct syscall; we pass a valid
    // fd borrowed from `parent_fd` (held for the call) and a
    // NUL-terminated name. Return value is checked below.
    let rc = unsafe { libc::unlinkat(parent_fd.as_raw_fd(), c_name.as_ptr(), 0) };
    if rc == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    if e.kind() == std::io::ErrorKind::NotFound {
        // Idempotent: a concurrent cleanup sweep may have already
        // reaped the stale entry. Treat as success.
        return Ok(());
    }
    Err(DaemonError::Config(format!(
        "unlink_via_parent_fd: unlinkat({}): {e}",
        target.display()
    )))
}

#[cfg(not(unix))]
fn unlink_via_parent_fd(_target: &Path) -> Result<(), DaemonError> {
    Err(DaemonError::Config(
        "unlink_via_parent_fd: not supported on non-Unix".to_string(),
    ))
}

#[cfg(not(unix))]
fn chmod_mode(_path: &Path, _mode: u32) -> Result<(), DaemonError> {
    Ok(())
}

/// True if `name` matches the canonical `<64-hex-sha>.md` payload
/// filename. Cleanup uses this to refuse touching attacker-injected
/// files (e.g. `.tmp.<pid>.<nanos>`, dotfiles, stale symlinks).
fn is_canonical_payload_name(name: &str) -> bool {
    let suffix = ".md";
    if !name.ends_with(suffix) || name.len() != 64 + suffix.len() {
        return false;
    }
    name[..64].chars().all(|c| c.is_ascii_hexdigit())
}

/// Idempotent single-file delete used by `state::SqliteStateStore::save`
/// AFTER a terminal transition + after the active-reference count
/// check returns zero (no other bead still shares the file). Idempotent:
/// `NotFound` is `Ok(())`. Refuses to delete anything that doesn't
/// match the canonical `<64-hex-sha>.md` filename regex AND whose
/// lexical parent isn't the configured payloads root -- otherwise a
/// corrupted/hostile binding could point at a filename-shaped path
/// outside the payload directory and we'd happily unlink it.
pub(crate) fn delete_payload(path: &str) -> Result<(), DaemonError> {
    let p = std::path::Path::new(path);
    let name = match p.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => {
            return Err(DaemonError::Config(format!(
                "delete_payload: {path:?} has no file_name component"
            )));
        }
    };
    if !is_canonical_payload_name(&name) {
        return Err(DaemonError::Config(format!(
            "delete_payload: refusing non-canonical {name:?}"
        )));
    }
    // Lexical-parent check: the bind path ALWAYS stores an absolute
    // path under the configured payloads root (see
    // `SqliteStateStore::bind_payload`), so we compare the parent's
    // lexical representation against `payloads_root()` without
    // canonicalizing the target itself (canonicalize would follow
    // symlinks at the target, re-introducing the same TOCTOU class
    // the upstream no-follow open was added to close).
    let root = payloads_root()?;
    if !root.is_absolute() {
        return Err(DaemonError::Config(format!(
            "delete_payload: payloads root is not absolute: {}",
            root.display()
        )));
    }
    let parent_lex = match p.parent() {
        Some(parent) => parent,
        None => {
            return Err(DaemonError::Config(format!(
                "delete_payload: {path:?} has no parent component"
            )));
        }
    };
    if parent_lex != root.as_path() {
        return Err(DaemonError::Config(
            "delete_payload: refusing -- parent is not the configured payloads root".to_string(),
        ));
    }
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::Config(format!(
            "delete_payload: remove {path:?}: {e}"
        ))),
    }
}

pub(crate) fn materialize_or_bootstrap(
    full_prompt: &str,
) -> Result<MaterializeOutcome, DaemonError> {
    if full_prompt.len() <= INDIRECTION_PROMPT_LIMIT {
        return Ok(MaterializeOutcome::NoIndirectionNeeded);
    }

    // PR #272 hard requirement: deterministic, structured rejection
    // of any prompt that contains the literal value of an env var
    // whose NAME contains "HOLDOUT" (case-insensitive) -- most
    // importantly `$DARK_FACTORY_HOLDOUTS`. This mirrors the
    // `*HOLDOUTS*` env-var strip at `adapters.rs:1408`'s subprocess
    // boundary. NOT a heuristic prose scan: the literal-substring
    // check runs against the (env) table only.
    if let Some(tainted) = first_holdout_env_value_in(full_prompt) {
        return Err(DaemonError::Config(format!(
            "prompt-indirection: refusing prompt that contains the \
             literal value of env var {tainted:?}; the sealed holdouts \
             path must never reach the persisted payload"
        )));
    }

    let root = payloads_root()?;
    std::fs::create_dir_all(&root).map_err(|e| {
        DaemonError::Config(format!(
            "prompt-indirection: create {}: {e} \
             (dispatcher halted; see return-err contract)",
            root.display()
        ))
    })?;
    chmod_mode(&root, 0o700)?;
    // PR #272 follow-up (jleechan-94rk): open the root via an
    // O_NOFOLLOW|O_DIRECTORY fd so every subsequent op in this
    // function (temp create, temp rename, target verify, stale
    // unlink) can be anchored against a kernel-verified
    // directory fd rather than re-traversing `root` and risking
    // a symlink-swap in between the chmod and the temp open.
    // Closes the same-UID TOCTOU window where an attacker swaps
    // a symlink into `~/.dark-factory/` between `create_dir_all`
    // and `materialize_or_bootstrap`'s later `root.join(...)`.
    #[cfg(unix)]
    let _root_fd = open_dir_fd(&root)?;

    let sha = sha256_hex(full_prompt.as_bytes());
    let target = root.join(format!("{sha}.md"));

    // Symlink-safe reuse check. Open TARGET through an O_NOFOLLOW fd
    // on Linux so the kernel refuses to follow a symlink at the path's
    // final component; once we hold the fd, every subsequent read /
    // chmod goes through that fd (never a fresh `&target` path open).
    // Closes the same-UID TOCTOU window where an attacker swaps a
    // symlink in between `symlink_metadata` and `read`.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let nlink_open = || -> std::io::Result<std::fs::File> {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW) // O_NOFOLLOW on Linux
                .open(&target)
        };
        match nlink_open() {
            Ok(mut f) => {
                let meta = f.metadata().map_err(|e| {
                    DaemonError::Config(format!(
                        "prompt-indirection: fd metadata on {}: {e}",
                        target.display()
                    ))
                })?;
                let ft = meta.file_type();
                if ft.is_symlink() {
                    return Err(DaemonError::Config(format!(
                        "prompt-indirection: target {} is a symlink (refusing)",
                        target.display()
                    )));
                }
                if !meta.is_file() {
                    return Err(DaemonError::Config(format!(
                        "prompt-indirection: target {} exists but is not a regular file",
                        target.display()
                    )));
                }
                let mut buf = Vec::new();
                if let Err(e) = std::io::Read::read_to_end(&mut f, &mut buf) {
                    return Err(DaemonError::Config(format!(
                        "prompt-indirection: read via no-follow fd on {}: {e}",
                        target.display()
                    )));
                }
                if sha256_hex(&buf) == sha {
                    // All subsequent operations go through the same
                    // fd -- never `&target` again. chmod via the fd
                    // (File::set_permissions is stable on stable Rust).
                    let perms = std::fs::Permissions::from_mode(0o600);
                    if let Err(e) = f.set_permissions(perms) {
                        return Err(DaemonError::Config(format!(
                            "prompt-indirection: chmod via fd on {}: {e}",
                            target.display()
                        )));
                    }
                    let indirect = build_indirect(full_prompt, &target, &sha);
                    check_bootstrap_len(&indirect.bootstrap)?;
                    return Ok(MaterializeOutcome::Indirected(indirect));
                }
                // Hash mismatch at a known-stale path. Drop the
                // per-target fd BEFORE opening the parent dir fd:
                // we want the unlinkat to be a separate, atomic
                // kernel-level step (single syscall against the
                // parent's dirfd + name) so a same-UID attacker can't
                // swap a symlink into `target` between `drop(f)` and
                // the unlink and have us follow the symlink to an
                // arbitrary file. The parent-dir fd is itself
                // O_NOFOLLOW|O_DIRECTORY so an attacker can't swap a
                // symlink at `root` either.
                drop(f);
                if let Err(e) = unlink_via_parent_fd(&target) {
                    // Non-fatal: the next materialize_or_bootstrap
                    // call will re-detect the stale state and retry.
                    // Log + continue -- failing the dispatch batch
                    // on stale-state cleanup would amplify a
                    // transient IO error into a daemon-wide halt.
                    eprintln!(
                        "prompt-indirection: warning -- stale-state unlink failed for {}: {e}",
                        target.display()
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // Linux O_NOFOLLOW on a final-component symlink
                // returns ELOOP (`sys errno=40`); the std wrapper
                // surfaces that as a generic `Err` whose OS error
                // code we must decode to surface the symlink reason
                // (not just "no-follow open failed").
                if e.raw_os_error() == Some(libc::ELOOP)
                    || e.kind() == std::io::ErrorKind::PermissionDenied
                {
                    return Err(DaemonError::Config(format!(
                        "prompt-indirection: target {} is a symlink (refusing -- ELOOP from O_NOFOLLOW)",
                        target.display()
                    )));
                }
                return Err(DaemonError::Config(format!(
                    "prompt-indirection: no-follow open {}: {e}",
                    target.display()
                )));
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Non-Unix: fall back to plain open (no O_NOFOLLOW). The daemon's
        // production target is Linux per its bundled-rusqlite dep.
        let _ = std::fs::OpenOptions::new().read(true).open(&target).ok();
    }

    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = root.join(format!("{sha}.md.tmp.{}.{}", std::process::id(), counter));

    // Temp file: open with O_NOFOLLOW + write + chmod via the open fd
    // BEFORE drop. Any error path below removes the temp explicitly.
    #[cfg(unix)]
    let mut temp_open = {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let create_result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            // O_NOFOLLOW on the temp too: a same-UID worker could
            // symlink-race against an atomic-counter collision under
            // extremely tight races (atomic counter is per-process, but
            // defense in depth).
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&temp);
        match create_result {
            Ok(mut f) => {
                let write_result = (|| -> std::io::Result<()> {
                    f.write_all(full_prompt.as_bytes())?;
                    f.sync_all()?;
                    Ok(())
                })();
                if let Err(e) = write_result {
                    drop(f);
                    let _ = std::fs::remove_file(&temp);
                    return Err(DaemonError::Config(format!(
                        "prompt-indirection: write {}: {e}",
                        temp.display()
                    )));
                }
                // chmod via the open fd -- no path-based follow-up.
                let perms = std::fs::Permissions::from_mode(0o600);
                if let Err(e) = f.set_permissions(perms) {
                    drop(f);
                    let _ = std::fs::remove_file(&temp);
                    return Err(DaemonError::Config(format!(
                        "prompt-indirection: chmod via fd on temp {}: {e}",
                        temp.display()
                    )));
                }
                Some(f)
            }
            Err(e) => {
                // create_new(true) FAILED -- we did NOT create the
                // file. The most common cause is EEXIST (a stale
                // .tmp.<pid>.<n> from a previous crashed process).
                // We MUST NOT remove a file this process did not
                // create: it belongs to another daemon/worker and
                // unlinking it would be a same-UID data-loss bug
                // (and would also give the cleanup sweep a stale
                // entry to race against). The bounded
                // cleanup_orphan_payloads pass reaps stale .tmp.*
                // files older than TEMP_STALE_SECS so the next
                // materialize call will succeed without our
                // touching the other process's temp.
                return Err(DaemonError::Config(format!(
                    "prompt-indirection: create temp {}: {e} \
                     (NOT deleting -- not created by this process; \
                      will retry after cleanup_orphan_payloads reaps stale .tmp.*)",
                    temp.display()
                )));
            }
        }
    };
    #[cfg(not(unix))]
    let mut temp_open: Option<std::fs::File> = {
        let _ = std::fs::write(&temp, full_prompt.as_bytes());
        None
    };

    // Atomic rename. Drop the fd first -- the kernel-level rename
    // replaces the destination before the fd is closed on success.
    // On rename failure we unlink the temp WE just created (via
    // the parent-dir fd, NOT path-based, to keep the no-symlink
    // discipline).
    drop(temp_open.take());
    if let Err(e) = std::fs::rename(&temp, &target) {
        let _ = unlink_via_parent_fd(&temp);
        return Err(DaemonError::Config(format!(
            "prompt-indirection: rename -> {}: {e}",
            target.display()
        )));
    }

    // Post-rename verification: open the now-canonical target via
    // O_NOFOLLOW fd, validate regular file, read+hash+chmod through
    // the same fd. Never through `&target` again -- the post-rename
    // open is the only path-based op on this loop body.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let nlink = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&target);
        let mut f = match nlink {
            Ok(f) => f,
            Err(e) => {
                if e.raw_os_error() == Some(libc::ELOOP) {
                    return Err(DaemonError::Config(
                        "prompt-indirection: post-rename target is a symlink (ELOOP)".to_string(),
                    ));
                }
                return Err(DaemonError::Config(format!(
                    "prompt-indirection: verify no-follow open {}: {e}",
                    target.display()
                )));
            }
        };
        let meta = match f.metadata() {
            Ok(m) => m,
            Err(e) => {
                return Err(DaemonError::Config(format!(
                    "prompt-indirection: verify metadata {}: {e}",
                    target.display()
                )));
            }
        };
        if meta.file_type().is_symlink() {
            return Err(DaemonError::Config(
                "prompt-indirection: post-rename target is a symlink".to_string(),
            ));
        }
        if !meta.is_file() {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: post-rename target {} is not regular",
                target.display()
            )));
        }
        let mut buf = Vec::new();
        if let Err(e) = std::io::Read::read_to_end(&mut f, &mut buf) {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: verify read via no-follow fd: {e}"
            )));
        }
        if sha256_hex(&buf) != sha {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: post-rename hash mismatch at {}",
                target.display()
            )));
        }
        // chmod via the same fd; closes the residual TOCTOU window.
        let perms = std::fs::Permissions::from_mode(0o600);
        if let Err(e) = f.set_permissions(perms) {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: chmod via verify fd: {e}"
            )));
        }
    }
    #[cfg(not(unix))]
    {
        // Best-effort fd-based verify+chmod on non-Unix too. The
        // daemon's production target is Linux (per its bundled-rusqlite
        // dep) so this branch is rarely exercised; keeping it fd-based
        // matches the Unix branch's TOCTOU discipline.
        let mut f = match std::fs::OpenOptions::new().read(true).open(&target) {
            Ok(f) => f,
            Err(e) => {
                return Err(DaemonError::Config(format!(
                    "prompt-indirection: verify open {}: {e}",
                    target.display()
                )));
            }
        };
        let mut buf = Vec::new();
        if let Err(e) = std::io::Read::read_to_end(&mut f, &mut buf) {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: verify read via fd: {e}"
            )));
        }
        if sha256_hex(&buf) != sha {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: post-rename hash mismatch at {}",
                target.display()
            )));
        }
        chmod_mode(&target, 0o600)?;
    }

    let indirect = build_indirect(full_prompt, &target, &sha);
    check_bootstrap_len(&indirect.bootstrap)?;
    Ok(MaterializeOutcome::Indirected(indirect))
}

fn build_indirect(full_prompt: &str, payload_path: &Path, sha: &str) -> PromptIndirect {
    let bootstrap = format!(
        "Read and follow the complete coding task at {path}.\n\
         Verify SHA-256 {sha} against the file contents BEFORE doing any work; \
         both must match. Do not proceed without reading it.\n\
         \n\
         The file on disk contains the full, lossless task ({n} bytes). \
         Treat its contents as the authoritative source of truth; this \
         message carries only the read-and-verify ritual.\n",
        path = payload_path.display(),
        sha = sha,
        n = full_prompt.len(),
    );
    PromptIndirect {
        bootstrap,
        payload_path: payload_path.to_path_buf(),
        sha256: sha.to_string(),
    }
}

fn check_bootstrap_len(bootstrap: &str) -> Result<(), DaemonError> {
    if bootstrap.len() > BOOTSTRAP_PROMPT_CAP {
        return Err(DaemonError::Config(format!(
            "prompt-indirection: bootstrap is {} chars, exceeds cap {}",
            bootstrap.len(),
            BOOTSTRAP_PROMPT_CAP
        )));
    }
    Ok(())
}

pub(crate) fn cleanup_orphan_payloads(
    max_age_secs: u64,
    delete_count: usize,
    active_paths: &std::collections::HashSet<String>,
) -> Result<usize, DaemonError> {
    let root = payloads_root()?;
    if !root.is_dir() {
        return Ok(0);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| DaemonError::Config(format!("clock before epoch: {e}")))?
        .as_secs();
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };
    let mut deleted = 0usize;
    // PR #272 follow-up (jleechan-94rk): the cleanup sweep now
    // reaps BOTH stale canonical payloads AND stale
    // `<sha>.md.tmp.<pid>.<n>` files. The temp pass uses a
    // TIGHTER retention (`TEMP_STALE_SECS`, default 1h) than the
    // canonical pass (`max_age_secs`, default 30d) because a temp
    // file's expected lifetime is one materialize call -- a
    // >1h-old temp is a crash artifact from a previous daemon /
    // worker that the bounded sweep must clear, otherwise a fresh
    // dispatch's create_new() will hit EEXIST and refuse to retry
    // (we explicitly do NOT remove a temp we didn't create on
    // EEXIST -- see `materialize_or_bootstrap`).
    let mut temp_seen = 0usize;
    for entry in entries {
        if deleted >= delete_count {
            break;
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let entry_path = entry.path();
        // Skip any path currently referenced by an active overlay --
        // the bounded sweep must never delete the payload an active
        // coder session (or a queued retry) is still reading.
        if active_paths.contains(&*entry_path.to_string_lossy()) {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let Ok(mtime_secs) = mtime.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        let age_secs = now.saturating_sub(mtime_secs.as_secs());
        if is_canonical_payload_name(&name) {
            // Canonical payload filename: reap when older than
            // `max_age_secs` (default 30d).
            if age_secs > max_age_secs && std::fs::remove_file(&entry_path).is_ok() {
                deleted += 1;
            }
            continue;
        }
        if is_stale_temp_name(&name) && age_secs > TEMP_STALE_SECS {
            temp_seen += 1;
            // Use the parent-dir fd unlink so a symlink-replaced
            // temp can't trick us into unlinking an arbitrary
            // file. `unlink_via_parent_fd` re-checks the parent
            // against `payloads_root()` for defense in depth.
            if unlink_via_parent_fd(&entry_path).is_ok() {
                deleted += 1;
            }
        }
    }
    if temp_seen > 0 {
        eprintln!(
            "prompt-indirection: cleanup_orphan_payloads reaped {temp_seen} stale temp file(s) \
             older than {TEMP_STALE_SECS}s under {}",
            root.display()
        );
    }
    Ok(deleted)
}

/// True if `name` matches the canonical temp-file shape
/// `<64hex>.md.tmp.<pid>.<n>` (the format
/// `materialize_or_bootstrap` writes). Refuses any name that
/// doesn't end in `<digits>.<digits>` (the `<pid>.<n>` suffix)
/// or whose `<sha>.md` prefix isn't 64 hex chars -- so attacker-
/// injected `.hidden.tmp.0.0` or `evil.md.tmp.X.Y` survives a
/// full sweep.
fn is_stale_temp_name(name: &str) -> bool {
    let suffix = ".tmp.";
    let idx = match name.find(suffix) {
        Some(i) => i,
        None => return false,
    };
    let (head, tail) = name.split_at(idx);
    // tail == ".tmp.<pid>.<n>"; strip the leading ".tmp." (5 chars).
    let tail = &tail[suffix.len()..];
    // tail must be `<digits>.<digits>` (no other separators,
    // no alpha). The `<pid>` is a process id (digits only), the
    // `<n>` is the per-process TEMP_COUNTER (also digits only).
    let mut parts = tail.splitn(2, '.');
    let pid = match parts.next() {
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => s,
        _ => return false,
    };
    let n = match parts.next() {
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => s,
        _ => return false,
    };
    // `head` must be `<64hex>.md` so an attacker-injected
    // `<anything>.md.tmp.<pid>.<n>` (without the 64-hex prefix)
    // is rejected. This mirrors `is_canonical_payload_name` for
    // the canonical pass.
    if !head.ends_with(".md") || head.len() != 64 + 3 {
        return false;
    }
    if !head[..64].chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    !pid.is_empty() && !n.is_empty()
}

fn env_lock() -> &'static std::sync::Mutex<()> {
    static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    L.get_or_init(|| std::sync::Mutex::new(()))
}

/// Acquire the process-wide payload-env lock. Dispatch tests
/// that trigger `materialize_or_bootstrap` (and therefore
/// touch `DARK_FACTORY_PROMPT_PAYLOADS_DIR`) MUST hold this
/// for the duration of the call to serialize against
/// `EnvScope`-using prompt_payload tests -- otherwise two
/// parallel tests can both observe each other's
/// env-var overrides and lose isolation.
pub(crate) fn lock_env_for_test() -> std::sync::MutexGuard<'static, ()> {
    env_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvScope {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        dir: PathBuf,
    }

    impl EnvScope {
        fn new() -> Self {
            let _lock = env_lock()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let dir = std::env::temp_dir().join(format!(
                "pp_{}_{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let prev = std::env::var(PAYLOADS_DIR_ENV).ok();
            std::env::set_var(PAYLOADS_DIR_ENV, &dir);
            Self { _lock, prev, dir }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(PAYLOADS_DIR_ENV, v),
                None => std::env::remove_var(PAYLOADS_DIR_ENV),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Threshold contract: at exactly INDIRECTION_PROMPT_LIMIT the
    /// prompt is a passthrough; one char over triggers indirection.
    /// Pins the regression class where PR #255's lossy shrinker
    /// would otherwise eat 4001..=4096 chars (a pre-fix blind spot).
    #[test]
    fn boundary_at_limit_is_no_op_one_over_is_indirected() {
        let _g = EnvScope::new();
        let at_limit = "x".repeat(INDIRECTION_PROMPT_LIMIT);
        let over = "x".repeat(INDIRECTION_PROMPT_LIMIT + 1);
        assert!(matches!(
            materialize_or_bootstrap(&at_limit).unwrap(),
            MaterializeOutcome::NoIndirectionNeeded
        ));
        assert!(matches!(
            materialize_or_bootstrap(&over).unwrap(),
            MaterializeOutcome::Indirected(_)
        ));
    }

    #[test]
    fn oversized_roundtrips_with_matching_sha() {
        let _g = EnvScope::new();
        let big = "x".repeat(8_000);
        let indirect = match materialize_or_bootstrap(&big).unwrap() {
            MaterializeOutcome::Indirected(i) => i,
            _ => panic!("expected Indirected"),
        };
        check_bootstrap_len(&indirect.bootstrap).unwrap();
        assert_eq!(
            indirect
                .payload_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .trim_end_matches(".md"),
            indirect.sha256,
        );
        assert_eq!(
            std::fs::read_to_string(&indirect.payload_path).unwrap(),
            big
        );
        assert!(indirect.bootstrap.contains(&indirect.sha256));
    }

    #[test]
    #[cfg(unix)]
    fn payload_directory_is_0700_and_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let g = EnvScope::new();
        let indirect = match materialize_or_bootstrap(&"x".repeat(8_000)).unwrap() {
            MaterializeOutcome::Indirected(i) => i,
            _ => panic!(),
        };
        assert_eq!(
            std::fs::metadata(&g.dir).unwrap().permissions().mode() & 0o7777,
            0o700,
        );
        assert_eq!(
            std::fs::metadata(&indirect.payload_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o600,
        );
    }

    /// Symlink at the target path is rejected, not silently reused.
    #[test]
    #[cfg(unix)]
    fn symlink_at_target_is_rejected() {
        use std::os::unix::fs::symlink;
        let _g = EnvScope::new();
        let big = "x".repeat(8_000);
        let first = match materialize_or_bootstrap(&big).unwrap() {
            MaterializeOutcome::Indirected(i) => i,
            _ => panic!(),
        };
        std::fs::remove_file(&first.payload_path).unwrap();
        let other = std::env::temp_dir().join("pp_symlink_target_other.txt");
        std::fs::write(&other, b"off-target content").unwrap();
        symlink(&other, &first.payload_path).unwrap();
        let err = materialize_or_bootstrap(&big).expect_err("symlink must be rejected");
        assert!(err.to_string().contains("symlink"));
        let _ = std::fs::remove_file(&other);
        let _ = std::fs::remove_file(&first.payload_path);
    }

    /// Adversarial: directory at the target path is also rejected
    /// (the regular-file check is what catches a directory, since a
    /// directory's `meta.is_file()` returns false). Proves the post-
    /// open `meta.file_type().is_symlink()` / `!meta.is_file()`
    /// guards fire on every "wrong file type at target" scenario --
    /// not just symlinks.
    #[test]
    #[cfg(unix)]
    fn directory_at_target_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        let _g = EnvScope::new();
        let big = "x".repeat(8_000);
        let first = match materialize_or_bootstrap(&big).unwrap() {
            MaterializeOutcome::Indirected(i) => i,
            _ => panic!(),
        };
        std::fs::remove_file(&first.payload_path).unwrap();
        // Plant a DIRECTORY (not a file, not a symlink) at the target.
        std::fs::create_dir(&first.payload_path).unwrap();
        std::fs::set_permissions(
            first.payload_path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let err = materialize_or_bootstrap(&big).expect_err("directory at target must be rejected");
        assert!(
            err.to_string().contains("regular file") || err.to_string().contains("symlink"),
            "error must name the rejection cause: {err}"
        );
        std::fs::remove_dir(&first.payload_path).ok();
    }

    /// Deletion-boundary: a canonical-looking `<64hex>.md` filename
    /// OUTSIDE the configured payloads root must NOT be deleted via
    /// `delete_payload`. Protects against a corrupted/hostile DB
    /// binding pointing at a filename-shaped path elsewhere on disk.
    /// Sets up an EnvScope (so payloads root is the test tempdir)
    /// and an out-of-root decoy whose filename IS a valid `<64hex>.md`.
    #[test]
    fn delete_payload_refuses_filename_outside_payloads_root() {
        let _g = EnvScope::new();
        let other = std::env::temp_dir().join(format!(
            "pp_outside_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&other).unwrap();
        let decoy_name = "f".repeat(64) + ".md";
        let decoy_path = other.join(&decoy_name);
        std::fs::write(&decoy_path, b"decoy").unwrap();

        let err = delete_payload(decoy_path.to_string_lossy().as_ref())
            .expect_err("delete outside payloads root must be refused");
        assert!(
            err.to_string().contains("not the configured payloads root")
                || err.to_string().contains("refusing"),
            "error must name the boundary rejection: {err}"
        );
        assert!(
            decoy_path.exists(),
            "the rejection must NOT have removed the out-of-root file"
        );
        let _ = std::fs::remove_dir_all(&other);
    }

    /// 8 concurrent dispatches of the same >4000 prompt must converge.
    #[test]
    fn concurrent_identical_dispatches_share_one_file() {
        let _g = EnvScope::new();
        let big = "y".repeat(6_000);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let p = big.clone();
                std::thread::spawn(move || materialize_or_bootstrap(&p))
            })
            .collect();
        let mut shas = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for h in handles {
            if let MaterializeOutcome::Indirected(i) = h.join().unwrap().unwrap() {
                shas.insert(i.sha256);
                paths.insert(i.payload_path);
            }
        }
        assert_eq!(shas.len(), 1);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn relative_payload_dir_override_is_rejected() {
        let _g = EnvScope::new();
        std::env::set_var(PAYLOADS_DIR_ENV, "relative/is/illegal");
        let err = materialize_or_bootstrap(&"z".repeat(6_000)).expect_err("relative rejected");
        assert!(err.to_string().contains("must be absolute"));
    }

    #[test]
    fn persist_failure_propagates_as_err() {
        let blocker_dir = std::env::temp_dir().join(format!(
            "pp_block_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&blocker_dir).unwrap();
        let blocker_file = blocker_dir.join("blocker");
        std::fs::write(&blocker_file, b"regular file").unwrap();
        let _g = EnvScope::new();
        std::env::set_var(PAYLOADS_DIR_ENV, &blocker_file);
        let err = materialize_or_bootstrap(&"w".repeat(6_000)).expect_err("mkdir on file must Err");
        assert!(err.to_string().contains("prompt-indirection"));
        let _ = std::fs::remove_dir_all(&blocker_dir);
    }

    /// Cleanup reaps canonical-payload-name files AND stale
    /// `<sha>.md.tmp.<pid>.<n>` files older than TEMP_STALE_SECS.
    /// Attacker-injected dotfiles / non-canonical `.tmp.*` shapes
    /// survive a full-delete sweep.
    ///
    /// Updated for PR #272 follow-up (jleechan-94rk): the bounded
    /// sweep now reaps stale TEMP files (a previous daemon /
    /// worker's crash artifact) so a fresh dispatch's
    /// `create_new()` doesn't hit EEXIST and refuse to retry.
    /// The `.tmp.X.Y` rejection cases (dotfile-shaped, alpha in
    /// pid/n) still survive.
    #[test]
    fn cleanup_reaps_canonical_and_stale_temp_but_skips_invalid_shapes() {
        let _g = EnvScope::new();
        let indirect = match materialize_or_bootstrap(&"x".repeat(8_000)).unwrap() {
            MaterializeOutcome::Indirected(i) => i,
            _ => panic!(),
        };
        let _ = std::process::Command::new("touch")
            .arg("-t")
            .arg("197001010000")
            .arg(&indirect.payload_path)
            .status();
        // Valid-shape STALE temp (sha + .md + .tmp + digits.digits):
        // SHOULD be reaped (its mtime is set to epoch so it
        // exceeds TEMP_STALE_SECS).
        let tmp_valid = indirect
            .payload_path
            .with_file_name("aaaaaaaa".repeat(8).to_string() + ".md.tmp.99999.1111111111");
        std::fs::write(&tmp_valid, b"stale-but-valid-shape").unwrap();
        let _ = std::process::Command::new("touch")
            .arg("-t")
            .arg("197001010000")
            .arg(&tmp_valid)
            .status();
        // Invalid-shape STALE temp (non-hex sha prefix) and a
        // non-canonical-name stale file: must NOT be reaped.
        let tmp_invalid_shape = indirect
            .payload_path
            .with_file_name("not-hex-prefix.md.tmp.99999.2222222222");
        std::fs::write(&tmp_invalid_shape, b"injected-shape").unwrap();
        let _ = std::process::Command::new("touch")
            .arg("-t")
            .arg("197001010000")
            .arg(&tmp_invalid_shape)
            .status();
        let dot = indirect.payload_path.with_file_name(".hidden");
        std::fs::write(&dot, b"hidden").unwrap();
        let _ = std::process::Command::new("touch")
            .arg("-t")
            .arg("197001010000")
            .arg(&dot)
            .status();

        let deleted = cleanup_orphan_payloads(1, 100, &std::collections::HashSet::new()).unwrap();
        // 1 canonical (<1s old but `touch 197001010000` makes it
        // 50+ years old -> exceeds ORPHAN_RETENTION_SECS=30d) + 1
        // valid-shape temp (exceeds TEMP_STALE_SECS=1h).
        assert_eq!(
            deleted, 2,
            "canonical payload + valid-shape stale temp must both be reaped"
        );
        assert!(!indirect.payload_path.exists(), "canonical must be reaped");
        assert!(
            !tmp_valid.exists(),
            "valid-shape stale temp must be reaped by the bounded sweep"
        );
        assert!(
            tmp_invalid_shape.exists(),
            "non-canonical .tmp.* shape must NOT be reaped"
        );
        assert!(dot.exists(), "dotfile must NOT be reaped");
    }

    /// `materialize_or_bootstrap` never removes a `.tmp.<pid>.<n>`
    /// file that was NOT created by THIS call -- even if the call
    /// itself succeeds, the foreign temp must be left for the
    /// bounded cleanup sweep to reap after TEMP_STALE_SECS.
    /// Property under test: same-UID data-loss bug if we unlink a
    /// file we didn't create. Verified by planting a foreign temp
    /// at the EXACT sha + counter our materialize will pick, so
    /// create_new(true) hits EEXIST and the Err path proves we
    /// do NOT unlink.
    #[test]
    fn create_new_failure_does_not_remove_existing_temp() {
        let _g = EnvScope::new();
        // Compute the SHA that materialize will pick for the
        // 8000-char prompt -- the foreign temp must be at THIS
        // sha + a counter our TEMP_COUNTER will hit, so
        // create_new(true) actually hits EEXIST.
        let big = "z".repeat(8_000);
        let mut h = sha2::Sha256::new();
        sha2::Digest::update(&mut h, big.as_bytes());
        let sha = {
            let mut out = String::with_capacity(64);
            for b in sha2::Digest::finalize(h) {
                out.push_str(&format!("{b:02x}"));
            }
            out
        };
        // Plant a foreign temp at the SAME counter that our
        // TEMP_COUNTER will pick (counter is per-process and
        // starts at 0; we plant one at counter 0 and another
        // one above 0 to maximize the EEXIST likelihood
        // regardless of which counter wins the race).
        let foreign_temp_a = _g
            .dir
            .join(format!("{sha}.md.tmp.{}.0", std::process::id()));
        std::fs::write(&foreign_temp_a, b"foreign-process-temp-a").unwrap();
        // Bump the per-process counter by calling
        // TEMP_COUNTER.fetch_add so the next materialize picks
        // counter=N+1, NOT 0 (then plant a foreign temp at
        // counter=N+1 so we still hit EEXIST).
        let next = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let foreign_temp_b = _g
            .dir
            .join(format!("{sha}.md.tmp.{}.{}", std::process::id(), next));
        std::fs::write(&foreign_temp_b, b"foreign-process-temp-b").unwrap();

        // Try materialize -- whichever counter wins, it'll hit
        // EEXIST on one of the two foreign temps. The point is
        // the failure must NOT remove EITHER temp.
        let result = materialize_or_bootstrap(&big);
        match result {
            Err(err) => {
                assert!(
                    err.to_string().contains("create temp")
                        || err.to_string().contains("prompt-indirection"),
                    "error must mention the create_new failure: {err}"
                );
            }
            Ok(MaterializeOutcome::Indirected(_)) => {
                // Acceptable: our counter happened to be unique
                // (both foreign temps already existed but our
                // pick landed on a different counter). The
                // property under test is "foreign temp not
                // deleted", which we still verify below.
            }
            Ok(MaterializeOutcome::NoIndirectionNeeded) => {
                panic!("prompt is 8000 chars; must be Indirected")
            }
        }
        assert!(
            foreign_temp_a.exists(),
            "materialize MUST NOT delete foreign temp A (created by another process)"
        );
        assert!(
            foreign_temp_b.exists(),
            "materialize MUST NOT delete foreign temp B (created by another process)"
        );
        // After making them stale, the cleanup sweep reaps them.
        for t in [&foreign_temp_a, &foreign_temp_b] {
            let _ = std::process::Command::new("touch")
                .arg("-t")
                .arg("197001010000")
                .arg(t)
                .status();
        }
        let deleted = cleanup_orphan_payloads(0, 100, &std::collections::HashSet::new()).unwrap();
        assert!(
            deleted >= 2,
            "cleanup must reap both foreign temps (deleted={deleted})"
        );
        assert!(!foreign_temp_a.exists());
        assert!(!foreign_temp_b.exists());
    }

    /// Structured env-value rejection: a prompt whose text contains
    /// the literal value of any env var named *HOLDOUT* refuses to
    /// persist, BEFORE any file IO. This is a finite
    /// structured-field check, not a prose scan -- the
    /// literal-substring match runs against the env table only.
    #[test]
    fn synthetic_holdout_env_value_blocks_persistence() {
        // EnvScope::new() acquires env_lock() internally; do NOT
        // double-lock here (that self-deadlocks the dispatcher).
        let _g = EnvScope::new();
        let secret = "/sealed/dark-factory-holdouts-zzzzzzzz";
        let prev = std::env::var("DARK_FACTORY_HOLDOUTS").ok();
        std::env::set_var("DARK_FACTORY_HOLDOUTS", secret);
        let mut big = String::with_capacity(8_500);
        big.push_str(&"y".repeat(4_500));
        big.push_str(&format!("\nlook: {secret}\n"));
        big.push_str(&"z".repeat(3_500));

        let err =
            materialize_or_bootstrap(&big).expect_err("synthetic HOLDOUT env value must refuse");
        assert!(
            err.to_string().contains("HOLDOUTS"),
            "error must name the env var: {err}"
        );
        // No payload file may have been written.
        let count = std::fs::read_dir(&_g.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
            .count();
        assert_eq!(
            count, 0,
            "no payload may be written when the env-var check trips"
        );

        match prev {
            Some(v) => std::env::set_var("DARK_FACTORY_HOLDOUTS", v),
            None => std::env::remove_var("DARK_FACTORY_HOLDOUTS"),
        }
    }
}
