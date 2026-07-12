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
use std::path::{Path, PathBuf};

const INDIRECTION_PROMPT_LIMIT: usize = 4_000;
const BOOTSTRAP_PROMPT_CAP: usize = 2_000;
const PAYLOADS_DIR_ENV: &str = "DARK_FACTORY_PROMPT_PAYLOADS_DIR";

pub(crate) const ORPHAN_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
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

pub(crate) fn materialize_or_bootstrap(
    full_prompt: &str,
) -> Result<MaterializeOutcome, DaemonError> {
    if full_prompt.len() <= INDIRECTION_PROMPT_LIMIT {
        return Ok(MaterializeOutcome::NoIndirectionNeeded);
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

    let sha = sha256_hex(full_prompt.as_bytes());
    let target = root.join(format!("{sha}.md"));

    // Symlink-safe reuse check. `symlink_metadata` (NOT `metadata`)
    // returns the symlink's own kind without following it, so a
    // symlink at `<sha>.md` is detected before any read. We refuse a
    // symlink outright -- the only way one exists at a content-derived
    // path is if an attacker pre-placed it (a worker / sibling process
    // can't legitimately create it).
    match std::fs::symlink_metadata(&target) {
        Ok(sm) if sm.file_type().is_symlink() => {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: target {} is a symlink (refusing)",
                target.display()
            )));
        }
        Ok(sm) if sm.is_file() => {
            let existing = std::fs::read(&target).map_err(|e| {
                DaemonError::Config(format!(
                    "prompt-indirection: re-read {}: {e}",
                    target.display()
                ))
            })?;
            if sha256_hex(&existing) == sha {
                chmod_mode(&target, 0o600)?;
                let indirect = build_indirect(full_prompt, &target, &sha);
                check_bootstrap_len(&indirect.bootstrap)?;
                return Ok(MaterializeOutcome::Indirected(indirect));
            }
            let _ = std::fs::remove_file(&target);
        }
        Ok(_) => {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: target {} exists but is not a regular file",
                target.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(DaemonError::Config(format!(
                "prompt-indirection: stat {}: {e}",
                target.display()
            )));
        }
    }

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
            "prompt-indirection: write {}: {e}",
            temp.display()
        )));
    }
    chmod_mode(&temp, 0o600)?;

    // `std::fs::rename` on Unix atomically REPLACES the destination.
    // We're past the symlink check above.
    if let Err(e) = std::fs::rename(&temp, &target) {
        let _ = std::fs::remove_file(&temp);
        return Err(DaemonError::Config(format!(
            "prompt-indirection: rename -> {}: {e}",
            target.display()
        )));
    }

    let post = std::fs::read(&target).map_err(|e| {
        DaemonError::Config(format!(
            "prompt-indirection: verify-read {}: {e}",
            target.display()
        ))
    })?;
    if sha256_hex(&post) != sha {
        return Err(DaemonError::Config(format!(
            "prompt-indirection: post-rename hash mismatch at {}",
            target.display()
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
        // Only operate on canonical payload filenames. Reduces the
        // surface for an attacker-injected `.tmp.PID.N` or dotfile
        // to be reaped; session-id-bound cleanup (which would also
        // protect active-session payloads) is filed as a follow-up.
        if !is_canonical_payload_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
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

    fn env_lock() -> &'static std::sync::Mutex<()> {
        static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(()))
    }

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
            indirect.payload_path.file_name().unwrap().to_string_lossy().trim_end_matches(".md"),
            indirect.sha256,
        );
        assert_eq!(std::fs::read_to_string(&indirect.payload_path).unwrap(), big);
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
        let err = materialize_or_bootstrap(&"w".repeat(6_000))
            .expect_err("mkdir on file must Err");
        assert!(err.to_string().contains("prompt-indirection"));
        let _ = std::fs::remove_dir_all(&blocker_dir);
    }

    /// Cleanup reaps only canonical-payload-name files; attacker-
    /// injected `.tmp.*` / dotfiles survive a full-delete sweep.
    #[test]
    fn cleanup_skips_non_canonical_filenames() {
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
        let tmp_injected = indirect.payload_path.with_file_name(
            "aaaaaaaa".repeat(8).to_string() + ".md.tmp.99999.1111111111",
        );
        std::fs::write(&tmp_injected, b"injected").unwrap();
        let _ = std::process::Command::new("touch")
            .arg("-t")
            .arg("197001010000")
            .arg(&tmp_injected)
            .status();
        let dot = indirect.payload_path.with_file_name(".hidden");
        std::fs::write(&dot, b"hidden").unwrap();

        let deleted = cleanup_orphan_payloads(1, 100).unwrap();
        assert_eq!(deleted, 1);
        assert!(!indirect.payload_path.exists());
        assert!(tmp_injected.exists(), ".tmp must NOT be reaped");
        assert!(dot.exists(), "dotfile must NOT be reaped");
    }
}
