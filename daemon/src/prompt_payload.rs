// Lossless coder-prompt indirection for `ao spawn --prompt` above AO's
// 4096-char hard ceiling (PR #272). PR #255's shrinker handles the
// inline passthrough; this module materializes the untruncated prompt
// to a content-addressed file and returns a short read-and-verify
// bootstrap in its place. Treats the prompt as opaque -- the existing
// `adapters.rs` `*HOLDOUTS*` env-var strip remains the sole holdout
// barrier.
//
// Lifecycle / cleanup limitation (honestly stated, NOT silently fixed):
// cleanup is mtime-based, NOT session-aware. A long-running coder
// session whose payload exceeds ORPHAN_RETENTION_SECS (30 days) will
// have its payload reaped even though the session is still active.
// 30 days is generous enough that hitting this in practice means a
// coder is wedged or a session was lost -- both failure modes a real
// tick reaper will surface via the standard session-state machinery.
// A proper fix (payload tracking bound to the coder session id, with
// terminal-state-driven cleanup) is filed as a follow-up.

use crate::errors::DaemonError;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

const AO_HARD_SPAWN_LIMIT: usize = 4_096;
const BOOTSTRAP_PROMPT_CAP: usize = 2_000;
const PAYLOADS_DIR_ENV: &str = "DARK_FACTORY_PROMPT_PAYLOADS_DIR";

// Conservative lifecycle limit; see module doc.
pub(crate) const ORPHAN_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
// Bounded batch size: never let one tick's cleanup touch more than
// this many files, regardless of dir size.
pub(crate) const ORPHAN_CLEANUP_BATCH: usize = 16;

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

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d { s.push_str(&format!("{b:02x}")); }
    s
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
    Ok(PathBuf::from(home).join(".dark-factory").join("prompt-payloads"))
}

#[cfg(unix)]
fn chmod_mode(path: &Path, mode: u32) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
        DaemonError::Config(format!(
            "chmod {:o} on {}: {e} (mode bits are a hard requirement)",
            mode, path.display()
        ))
    })
}

#[cfg(not(unix))]
fn chmod_mode(_path: &Path, _mode: u32) -> Result<(), DaemonError> { Ok(()) }

pub(crate) fn materialize_or_bootstrap(
    full_prompt: &str,
) -> Result<MaterializeOutcome, DaemonError> {
    if full_prompt.len() <= AO_HARD_SPAWN_LIMIT {
        return Ok(MaterializeOutcome::NoIndirectionNeeded);
    }
    let root = payloads_root()?;
    std::fs::create_dir_all(&root).map_err(|e| {
        DaemonError::Config(format!(
            "prompt-indirection: create payloads dir {}: {e} \
             (prompt is >4096 chars; dispatcher must park HumanHeld)",
            root.display()
        ))
    })?;
    chmod_mode(&root, 0o700)?;

    let sha = sha256_hex(full_prompt.as_bytes());
    let target = root.join(format!("{sha}.md"));

    // Idempotent reuse: if the canonical file already exists with
    // the matching hash, re-verify the mode (a hostile / buggy
    // process could have weakened it) and reuse without re-writing.
    if target.is_file() {
        let existing = std::fs::read(&target).map_err(|e| {
            DaemonError::Config(format!(
                "prompt-indirection: re-read existing {}: {e}",
                target.display()
            ))
        })?;
        if sha256_hex(&existing) == sha {
            chmod_mode(&target, 0o600)?;
            return Ok(MaterializeOutcome::Indirected(build_indirect(
                full_prompt, &target, &sha,
            )));
        }
        let _ = std::fs::remove_file(&target);
    }

    // Unique temp filename: create_new (O_EXCL) + pid + nanos suffix
    // prevents a concurrent identical-dispatch temp collision.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = root.join(format!("{sha}.md.tmp.{}.{}", std::process::id(), nanos));

    let write_result = (|| -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            f.write_all(full_prompt.as_bytes())?;
            f.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&temp, full_prompt.as_bytes())?;
        }
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(DaemonError::Config(format!(
            "prompt-indirection: write {}: {e} \
             (dispatcher must park HumanHeld)",
            temp.display()
        )));
    }
    chmod_mode(&temp, 0o600)?;

    // `std::fs::rename` on Unix atomically REPLACES the destination if
    // it exists; on Windows it errors. Either way the temp's content
    // (fsync'd above) becomes the file at `target` -- readers either
    // see the prior content (now replaced) or the new content, never
    // a half-written mix.
    if let Err(e) = std::fs::rename(&temp, &target) {
        let _ = std::fs::remove_file(&temp);
        return Err(DaemonError::Config(format!(
            "prompt-indirection: rename -> {} failed: {e}",
            target.display()
        )));
    }

    // Verify-after-rename. Read the canonical file and confirm its
    // hash matches our input; an error here (read failure, hash
    // mismatch) is a real dispatch failure, not a swallowed hiccup.
    let post = std::fs::read(&target).map_err(|e| {
        DaemonError::Config(format!(
            "prompt-indirection: verify-read {} failed: {e}",
            target.display()
        ))
    })?;
    if sha256_hex(&post) != sha {
        return Err(DaemonError::Config(format!(
            "prompt-indirection: post-rename hash mismatch at {} \
             (read {} bytes, hash did not match expected {sha})",
            target.display(),
            post.len()
        )));
    }
    chmod_mode(&target, 0o600)?;
    let indirect = build_indirect(full_prompt, &target, &sha);
    check_bootstrap_len(&indirect.bootstrap)?;
    Ok(MaterializeOutcome::Indirected(indirect))
}

fn build_indirect(
    full_prompt: &str,
    payload_path: &Path,
    sha: &str,
) -> PromptIndirect {
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
    // Caller asserts len <= BOOTSTRAP_PROMPT_CAP via check_bootstrap_len.
    PromptIndirect {
        bootstrap,
        payload_path: payload_path.to_path_buf(),
        sha256: sha.to_string(),
    }
}

// Real (release-mode) bootstrap length check, evaluated AFTER the
// renderer produced it. Inline call site of build_indirect from the
// two materialize flows.
fn check_bootstrap_len(bootstrap: &str) -> Result<(), DaemonError> {
    if bootstrap.len() > BOOTSTRAP_PROMPT_CAP {
        return Err(DaemonError::Config(format!(
            "prompt-indirection: bootstrap is {} chars, exceeds \
             BOOTSTRAP_PROMPT_CAP={BOOTSTRAP_PROMPT_CAP} (coding bug -- \
             the bootstrap template contains no user input; the \
             constant is wrong)",
            bootstrap.len()
        )));
    }
    Ok(())
}

pub(crate) fn cleanup_orphan_payloads(
    max_age_secs: u64,
    delete_count: usize,
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
    for entry in entries {
        if deleted >= delete_count {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else { continue };
        let Ok(mtime_secs) = mtime.duration_since(std::time::UNIX_EPOCH) else { continue };
        if now.saturating_sub(mtime_secs.as_secs()) > max_age_secs
            && std::fs::remove_file(entry.path()).is_ok()
        {
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Process-wide env-mutation lock. Mirror of `adapters.rs`'s
    // `GH_ENV_TEST_LOCK` (jleechan-9sl1 discipline) so env-var
    // mutations from sibling test modules serialize.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(()))
    }

    // Holds the env_lock AND sets a temp payloads dir. Restore on
    // Drop. Designed so callers can `let _g = ...` and ignore it
    // thereafter; the lock survives until Drop runs at scope exit.
    struct EnvScope {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        dir: PathBuf,
    }

    impl EnvScope {
        fn new() -> Self {
            let _lock = env_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

    #[test]
    fn short_prompt_is_no_op() {
        let _g = EnvScope::new();
        let out = materialize_or_bootstrap("fits").unwrap();
        assert!(matches!(out, MaterializeOutcome::NoIndirectionNeeded));
    }

    #[test]
    fn oversized_prompt_roundtrips_with_matching_sha() {
        let _g = EnvScope::new();
        let big = "x".repeat(8_000);
        let indirect = match materialize_or_bootstrap(&big).unwrap() {
            MaterializeOutcome::Indirected(i) => i,
            _ => panic!("expected Indirected"),
        };
        check_bootstrap_len(&indirect.bootstrap).unwrap();
        assert_eq!(
            indirect.payload_path.file_name().unwrap().to_string_lossy().trim_end_matches(".md"),
            indirect.sha256,
        );
        assert_eq!(std::fs::read_to_string(&indirect.payload_path).unwrap(), big);
        assert!(indirect.bootstrap.len() <= BOOTSTRAP_PROMPT_CAP);
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
            std::fs::metadata(&indirect.payload_path).unwrap().permissions().mode() & 0o7777,
            0o600,
        );
    }

    /// 8 concurrent dispatches of the same >4096 prompt must converge
    /// on one file with the same content hash and no partial bytes.
    /// Env is set ONCE in the parent thread before any spawn so the
    /// children only READ the env var -- no per-thread env mutation
    /// means no lock contention with sibling test modules.
    #[test]
    fn concurrent_identical_dispatches_share_one_file() {
        let _g = EnvScope::new();
        let big = "y".repeat(6_000);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let p = big.clone();
                std::thread::spawn(move || {
                    materialize_or_bootstrap(&p)
                })
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
        let final_path = paths.iter().next().unwrap();
        assert_eq!(std::fs::read_to_string(final_path).unwrap(), big);
    }

    #[test]
    fn relative_payload_dir_override_is_rejected() {
        let _g = EnvScope::new();
        std::env::set_var(PAYLOADS_DIR_ENV, "relative/is/illegal");
        let err = materialize_or_bootstrap(&"z".repeat(6_000)).expect_err("relative rejected");
        assert!(err.to_string().contains("must be absolute"));
        // EnvScope restores on Drop at scope exit.
    }

    /// Persistence failure propagates as Err (park-HumanHeld contract),
    /// never as None.
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
        let err = materialize_or_bootstrap(&"w".repeat(6_000))
            .expect_err("mkdir on file must Err");
        assert!(err.to_string().contains("prompt-indirection"));
        let _ = std::fs::remove_dir_all(&blocker_dir);
    }

    #[test]
    fn cleanup_is_bounded_and_reaps_old_files() {
        let _g = EnvScope::new();
        let mut paths = Vec::new();
        for i in 0..5 {
            if let MaterializeOutcome::Indirected(indirect) =
                materialize_or_bootstrap(&format!("payload-{i}-{}", "x".repeat(5_000))).unwrap()
            {
                let _ = std::process::Command::new("touch")
                    .arg("-t")
                    .arg("197001010000")
                    .arg(&indirect.payload_path)
                    .status();
                paths.push(indirect.payload_path);
            }
        }
        let deleted = cleanup_orphan_payloads(1, 2).unwrap();
        assert_eq!(deleted, 2);
        let survivors = paths.iter().filter(|p| p.exists()).count();
        assert_eq!(survivors, 3);
    }
}
