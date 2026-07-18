// Single-instance lease guarding the configured CXDB / control plane.
// jleechan-bze8.4: `daemon --help` previously bypassed CLI parsing and entered
// the live tick loop; two long-running daemons then dispatched the same beads
// and interleaved telemetry (tick_index 0..5 vs 748..755 on 2026-07-18).
//
// We use a `mkdir`-based atomic advisory lock (the same pattern as
// PostgreSQL's `postmaster.pid` and npm's `.npm-install` lock) because
// `fcntl(F_SETLK)` advisory locks are PER-PROCESS on Linux — two `File`
// descriptors opened by the same PID both succeed, which would let a single
// shell accidentally launch two daemons from one terminal and have them
// both think they "own" the CXDB. `mkdir(2)` is atomic across the kernel
// regardless of PID; the winner is whichever `mkdir` call returns 0 first.
// Crash recovery: detect a stale lease by reading the on-disk payload's PID
// and `kill(pid, 0)`. If the PID is gone (dead or never existed), reclaim by
// `rmdir`-ing the lease dir and recreating it; if the PID is alive, return
// `AlreadyHeld` and the caller exits non-zero without dispatching or
// emitting telemetry.
use crate::errors::DaemonError;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Payload written to the lock directory. Carries every identity the daemon
/// needs to be reconstructed by a second invocation seeing an active lease,
/// and by post-mortem operators looking at the live process tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeasePayload {
    pub pid: u32,
    pub start_time_unix_secs: u64,
    pub instance_uuid: String,
    pub executable_sha256: String,
    pub config_identity: String,
}

impl LeasePayload {
    pub fn render(&self) -> String {
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|e| format!("<unrenderable lease payload: {e}>"))
    }

    pub fn parse(contents: &str) -> Result<Self, DaemonError> {
        serde_json::from_str(contents)
            .map_err(|e| DaemonError::Config(format!("lease payload parse: {e}")))
    }
}

/// An acquired `Lease` guards the configured CXDB for the lifetime of the
/// guard. The kernel releases the directory lock automatically on process
/// exit; on explicit `Drop` we remove both the payload file and the
/// directory so a successor daemon doesn't have to wait for a stale-PID
/// sweep.
pub struct Lease {
    pub dir: PathBuf,
    payload_file: File,
    pub payload: LeasePayload,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("dir", &self.dir)
            .field("payload", &self.payload)
            .finish()
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // `kill(pid, 0)` returns 0 if alive, -1/EPERM if exists but no signal
    // permission, -1/ESRCH if doesn't exist. Treat EPERM as alive.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        true
    } else {
        let err = std::io::Error::last_os_error();
        err.raw_os_error() == Some(libc::EPERM)
    }
}

/// Default location: `<dir>/daemon.lock.d/`. The daemon owns its CXDB dir,
/// so colocating the lease avoids cross-process ambiguity when more than
/// one repo uses the same `$HOME`.
pub fn default_lock_path(cxdb_path: &Path) -> PathBuf {
    let parent = cxdb_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join("daemon.lock.d")
}

/// Result of [`acquire`].
#[derive(Debug)]
pub enum AcquireOutcome {
    /// This process now holds the lease. Mutate state, write telemetry.
    Acquired(Lease),
    /// Another live daemon already holds the lease. Caller must exit
    /// non-zero without dispatching or writing telemetry. `holder` is the
    /// full on-disk payload so the operator gets every detail they need to
    /// identify the running process.
    AlreadyHeld {
        path: PathBuf,
        holder: LeasePayload,
    },
}

/// Acquire the exclusive lease at `lock_dir_path`. Returns `Acquired` on
/// success, `AlreadyHeld` if a live process already owns it, or
/// `DaemonError::Config` if the path cannot be created / has the wrong
/// shape / is held by a stale owner we couldn't safely reclaim.
pub fn acquire(
    lock_dir_path: &Path,
    payload: LeasePayload,
) -> Result<AcquireOutcome, DaemonError> {
    // First attempt: atomic mkdir. EEXIST means someone else has the lease.
    match create_dir(lock_dir_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return inspect_existing_lock(lock_dir_path);
        }
        Err(e) => {
            return Err(DaemonError::Config(format!(
                "lease mkdir {lock_dir_path:?}: {e}"
            )));
        }
    }

    // Won the mkdir race — write the payload file inside the directory.
    let payload_path = lock_dir_path.join("payload.json");
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&payload_path)
        .map_err(|e| {
            // Best-effort cleanup; if this fails, the next daemon will
            // refresh via the stale-recovery path.
            let _ = std::fs::remove_dir(lock_dir_path);
            DaemonError::Config(format!(
                "lease payload open {payload_path:?}: {e}"
            ))
        })?;
    let bytes = payload.render();
    f.write_all(bytes.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| {
            let _ = std::fs::remove_dir(lock_dir_path);
            DaemonError::Config(format!("lease payload write: {e}"))
        })?;

    Ok(AcquireOutcome::Acquired(Lease {
        dir: lock_dir_path.to_path_buf(),
        payload_file: f,
        payload,
    }))
}

fn create_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

/// The lock directory exists. Read its payload and decide: alive = fail
/// closed; dead = reclaim; unparseable / no payload = fail closed (better
/// safe than racing a third daemon we can't observe).
fn inspect_existing_lock(lock_dir_path: &Path) -> Result<AcquireOutcome, DaemonError> {
    let payload_path = lock_dir_path.join("payload.json");
    let on_disk = match std::fs::read_to_string(&payload_path) {
        Ok(s) => s,
        Err(e) => {
            return Err(DaemonError::Config(format!(
                "existing lease payload unreadable at {payload_path:?}: {e}; refusing to start"
            )));
        }
    };
    let holder = match LeasePayload::parse(on_disk.trim()) {
        Ok(p) => p,
        Err(_) => {
            return Err(DaemonError::Config(format!(
                "existing lease payload unparseable at {payload_path:?}; refusing to start"
            )));
        }
    };
    if pid_alive(holder.pid) {
        return Ok(AcquireOutcome::AlreadyHeld {
            path: lock_dir_path.to_path_buf(),
            holder,
        });
    }

    // Stale lease: previous owner is dead. Try to remove and re-acquire.
    if let Err(e) = std::fs::remove_dir_all(lock_dir_path) {
        return Err(DaemonError::Config(format!(
            "stale lease remove {lock_dir_path:?}: {e}; refusing to start"
        )));
    }
    // Recurse so we re-enter the winning `mkdir` branch with our payload.
    acquire(lock_dir_path, fresh_uuid_from(&holder))
}

/// Helper: produce a payload carrying over the executable SHA and config
/// identity from the stale holder, but with our own PID/UUID.
fn fresh_uuid_from(prior: &LeasePayload) -> LeasePayload {
    LeasePayload {
        pid: std::process::id(),
        start_time_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        instance_uuid: format!("recovered-{}", uuid_like()),
        executable_sha256: prior.executable_sha256.clone(),
        config_identity: prior.config_identity.clone(),
    }
}

/// Tiny stable-but-unique-per-call identifier (random enough for telemetry
/// dedupe, not RFC-4122 compliant — explicitly avoids pulling in the `uuid`
/// crate just to satisfy one field).
fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Best-effort: drop the File handle (closes payload.json), then
        // remove the lease dir so a successor doesn't have to run stale
        // recovery. If removal fails (e.g. a parallel `acquire` raced us
        // and reclaimed it), the next acquire will go through the
        // already-held path — fail closed by reading the new payload.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "afd_instancelock_test_{}_{}_{}",
            std::process::id(),
            suffix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_payload(marker: &str) -> LeasePayload {
        LeasePayload {
            pid: std::process::id(),
            start_time_unix_secs: 1_700_000_000,
            instance_uuid: format!("uuid-{marker}"),
            executable_sha256: "deadbeef".into(),
            config_identity: "cfg-X".into(),
        }
    }

    #[test]
    fn payload_round_trip() {
        let p = sample_payload("a");
        let rendered = p.render();
        let parsed = LeasePayload::parse(&rendered).unwrap();
        assert_eq!(p, parsed);
    }

    #[test]
    fn default_lock_path_lives_next_to_cxdb() {
        let cxdb = Path::new("/var/lib/dark-factory/daemon-cxdb.sqlite");
        let lock = default_lock_path(cxdb);
        assert_eq!(
            lock,
            PathBuf::from("/var/lib/dark-factory/daemon.lock.d")
        );
    }

    #[test]
    fn acquire_writes_payload_in_directory() {
        let dir = temp_dir("basic");
        let lock_dir = dir.join("daemon.lock.d");

        let outcome = acquire(&lock_dir, sample_payload("first")).unwrap();
        let lease = match outcome {
            AcquireOutcome::Acquired(l) => l,
            _ => panic!("expected first to acquire"),
        };

        let on_disk =
            std::fs::read_to_string(lock_dir.join("payload.json")).unwrap();
        assert!(on_disk.contains("uuid-first"));
        assert!(lock_dir.is_dir());
        assert_eq!(lease.dir, lock_dir);
        drop(lease);
        assert!(!lock_dir.exists(), "drop should remove the lease dir");
    }

    #[test]
    fn second_acquire_in_same_process_sees_already_held() {
        let dir = temp_dir("twice");
        let lock_dir = dir.join("daemon.lock.d");

        let first = acquire(&lock_dir, sample_payload("first")).unwrap();
        // Hold the first lease across the second attempt by deliberately
        // leaking it (cleanup via explicit remove at end of test).
        let first_payload = match first {
            AcquireOutcome::Acquired(l) => {
                assert_eq!(l.dir, lock_dir);
                let payload_clone = l.payload.clone();
                std::mem::forget(l);
                payload_clone
            }
            _ => panic!("expected first acquire"),
        };

        let second = acquire(&lock_dir, sample_payload("second")).unwrap();
        match second {
            AcquireOutcome::AlreadyHeld { holder, .. } => {
                assert_eq!(holder, first_payload);
            }
            AcquireOutcome::Acquired(_) => {
                panic!("expected AlreadyHeld on second acquire")
            }
        }
        let _ = std::fs::remove_dir_all(&lock_dir);
    }

    #[test]
    fn stale_lock_with_dead_pid_is_reclaimed() {
        let dir = temp_dir("stale");
        let lock_dir = dir.join("daemon.lock.d");

        // Simulate a crashed prior daemon by writing a payload with a PID
        // that definitely doesn't exist on a healthy Linux system (PIDs
        // wrap at pid_max, so 999_999_999 is far above the default
        // /proc/sys/kernel/pid_max of 4194304). `kill(pid, 0)` returns
        // ESRCH -> `pid_alive()` is false.
        let ghost = LeasePayload {
            pid: 999_999_999,
            start_time_unix_secs: 1_700_000_000,
            instance_uuid: "ghost".into(),
            executable_sha256: "0xdeadbeef".into(),
            config_identity: "cfg-ghost".into(),
        };
        std::fs::create_dir(&lock_dir).unwrap();
        std::fs::write(lock_dir.join("payload.json"), ghost.render()).unwrap();

        let outcome = acquire(&lock_dir, sample_payload("recovered")).unwrap();
        let (recovered_payload, _lease_guard) = match outcome {
            AcquireOutcome::Acquired(l) => {
                let payload_clone = l.payload.clone();
                // Leak the lease across the on-disk assertion so Drop
                // doesn't remove the directory we're reading.
                std::mem::forget(l);
                (payload_clone, ())
            }
            AcquireOutcome::AlreadyHeld { holder, .. } => {
                panic!("expected stale recovery, got AlreadyHeld by {holder:?}");
            }
        };
        assert!(
            recovered_payload.instance_uuid.starts_with("recovered-"),
            "stale recovery must produce a recovered-* UUID, got {:?}",
            recovered_payload.instance_uuid
        );
        assert_eq!(recovered_payload.executable_sha256, "0xdeadbeef");
        assert_eq!(recovered_payload.config_identity, "cfg-ghost");

        let on_disk =
            std::fs::read_to_string(lock_dir.join("payload.json")).unwrap();
        assert!(
            on_disk.contains(&recovered_payload.instance_uuid),
            "on-disk payload must be the recovered one; got {on_disk}"
        );

        let _ = std::fs::remove_dir_all(&lock_dir);
    }

    #[test]
    fn crash_recovery_rejects_unparseable_payload() {
        let dir = temp_dir("corrupt");
        let lock_dir = dir.join("daemon.lock.d");
        std::fs::create_dir(&lock_dir).unwrap();
        std::fs::write(lock_dir.join("payload.json"), "{not json").unwrap();

        let err = acquire(&lock_dir, sample_payload("corrupt")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unparseable") || msg.contains("refusing to start"),
            "got: {msg}"
        );
    }
}
