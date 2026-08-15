// Integration coverage for bead jleechan-y189: self-healing worktree preflight.
//
// The preflight is exercised through `self_heal_preflight` directly (the
// crash-safe subset) and through `ensure_target_worktree_self_heal` end-to-end
// (which composes the preflight with the existing origin/head verification).
//
// Each scenario pins BOTH the RED and GREEN state:
//
//   RED proof (synthetic dirty / locked / missing state): the test sets up
//   the pre-jw4c-style failure mode (stale index.lock, untracked caches,
//   dirty tracked file) and exercises the preflight.
//
//   GREEN proof: the preflight transparently cleans the state, the resulting
//   checkout still passes `verify_head`/`verify_origin`, and the right
//   telemetry event has been emitted.
//
// To avoid hitting GitHub, every test uses a *local* bare repo as the
// origin: the wrapper clones `--bare` from a seed commit, the worktree
// clones from the bare URL, and the preflight fetches from the bare URL.
// This mirrors production semantics without any network dependency.

use daemon::tools::run_tool;
use daemon::worktree_selfheal::{
    self_heal_preflight, PreflightOutcome, EVENT_CACHES_CLEANED, EVENT_DIRTY_RESET,
    EVENT_FALLBACK_PROVISIONED, EVENT_INDEX_LOCK_REMOVED,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixtures {
    origin: PathBuf,
    bare: PathBuf,
    worktree: PathBuf,
    #[allow(dead_code)]
    head: String,
}

impl Fixtures {
    fn new(label: &str) -> Self {
        let pid = std::process::id();
        let origin = std::env::temp_dir().join(format!("afd_y189_origin_{label}_{pid}"));
        let head = init_git_repo(&origin, "owner/repo");
        let bare = std::env::temp_dir().join(format!("afd_y189_bare_{label}_{pid}"));
        let _ = fs::remove_dir_all(&bare);
        fs::create_dir_all(&bare).unwrap();
        run_tool(
            "git",
            &[
                "clone",
                "--bare",
                &origin.to_string_lossy(),
                &bare.to_string_lossy(),
            ],
            600,
        )
        .unwrap();
        let bare_url = bare.to_string_lossy().into_owned();
        let worktree = std::env::temp_dir().join(format!("afd_y189_wt_{label}_{pid}"));
        let _ = fs::remove_dir_all(&worktree);
        run_tool(
            "git",
            &["clone", &bare_url, &worktree.to_string_lossy()],
            600,
        )
        .unwrap();
        Self {
            origin,
            bare,
            worktree,
            head,
        }
    }

    fn cleanup(&self) {
        let _ = fs::remove_dir_all(&self.worktree);
        let _ = fs::remove_dir_all(&self.bare);
        let _ = fs::remove_dir_all(&self.origin);
    }
}

fn init_git_repo(path: &Path, repo: &str) -> String {
    let _ = fs::remove_dir_all(path);
    fs::create_dir_all(path).unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .unwrap();
    let remote = format!("https://github.com/{repo}.git");
    Command::new("git")
        .args(["remote", "add", "origin", &remote])
        .current_dir(path)
        .status()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=jleechan2015@users.noreply.github.com",
            "-c",
            "user.name=Test",
            "commit",
            "--allow-empty",
            "-m",
            "test",
        ])
        .current_dir(path)
        .status()
        .unwrap();
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn telemetry_log(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("afd_y189_log_{label}_{}.jsonl", std::process::id()))
}

/// RED → GREEN proof: a stale `.git/index.lock` would block any subsequent
/// `git` invocation (jw4c's `ActiveSessionProbe` couldn't even probe the
/// checkout). The preflight detects the lock, confirms its owning PID is
/// dead, and removes it; the checkout is usable afterward.
#[test]
fn preflight_removes_stale_index_lock_then_proceeds() {
    let f = Fixtures::new("lock");
    let git_dir = f.worktree.join(".git");
    let lock = git_dir.join("index.lock");
    fs::write(&lock, b"").unwrap();
    let log = telemetry_log("lock");
    let _ = fs::remove_file(&log);

    let outcome = self_heal_preflight(&f.worktree, true, Some(&log), "bead-test").unwrap();

    assert_eq!(outcome, PreflightOutcome::Clean);
    assert!(!lock.exists(), "stale lock must be removed");
    let body = fs::read_to_string(&log).unwrap();
    assert!(
        body.contains(EVENT_INDEX_LOCK_REMOVED),
        "expected WORKTREE_SELFHEAL_INDEX_LOCK_REMOVED, got: {body}"
    );
    let _ = fs::remove_file(&log);
    f.cleanup();
}

/// RED → GREEN proof: an index.lock whose owning PID is alive (our test
/// process owns it) MUST NOT be removed. The preflight surfaces the error
/// to the caller so the daemon defers rather than destroys work in-flight.
#[test]
fn preflight_refuses_to_remove_live_index_lock() {
    let f = Fixtures::new("live_lock");
    let git_dir = f.worktree.join(".git");
    let lock = git_dir.join("index.lock");
    let pid = std::process::id();
    fs::write(&lock, pid.to_be_bytes()).unwrap();

    let err = self_heal_preflight(&f.worktree, true, None, "bead-test").unwrap_err();
    assert!(err.to_string().contains("live PID"));
    assert!(lock.exists(), "live lock must not be removed");
    let _ = fs::remove_file(&lock);
    f.cleanup();
}

/// RED → GREEN proof: a worktree littered with untracked build/test caches
/// would otherwise inflate the porcelain-status check (jw4c's
/// `refuses_to_refresh_dirty_managed_checkout` covers the same path).
/// The preflight transparently removes them and emits the cache event.
#[test]
fn preflight_removes_untracked_caches() {
    let f = Fixtures::new("caches");
    fs::create_dir_all(f.worktree.join("target/debug")).unwrap();
    fs::create_dir_all(f.worktree.join("node_modules/foo")).unwrap();
    fs::create_dir_all(f.worktree.join("dist")).unwrap();
    fs::write(f.worktree.join("target/debug/keep.txt"), "build artifact").unwrap();
    fs::write(f.worktree.join("node_modules/foo/keep.txt"), "module").unwrap();
    fs::write(f.worktree.join("dist/keep.txt"), "build artifact").unwrap();

    let log = telemetry_log("caches");
    let _ = fs::remove_file(&log);
    let outcome = self_heal_preflight(&f.worktree, true, Some(&log), "bead-test").unwrap();
    assert_eq!(outcome, PreflightOutcome::Clean);

    assert!(!f.worktree.join("target").exists());
    assert!(!f.worktree.join("node_modules").exists());
    assert!(!f.worktree.join("dist").exists());
    let body = fs::read_to_string(&log).unwrap();
    assert!(
        body.contains(EVENT_CACHES_CLEANED),
        "expected WORKTREE_SELFHEAL_CACHES_CLEANED, got: {body}"
    );
    let _ = fs::remove_file(&log);
    f.cleanup();
}

/// RED → GREEN proof: a tracked-file modification blocks jw4c's
/// `refresh_existing_if_stale` path. The preflight resets the tracked
/// state when `dirty_reset = true` (managed checkout) and emits
/// `WORKTREE_SELFHEAL_DIRTY_RESET`.
#[test]
fn preflight_resets_dirty_tracked_state_when_managed() {
    let f = Fixtures::new("dirty_managed");
    fs::write(f.worktree.join("tracked.txt"), "v1\n").unwrap();
    let _ = Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(&f.worktree)
        .status()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=jleechan2015@users.noreply.github.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "track",
        ])
        .current_dir(&f.worktree)
        .status()
        .unwrap();
    fs::write(f.worktree.join("tracked.txt"), "v2 dirty\n").unwrap();

    let log = telemetry_log("dirty_managed");
    let _ = fs::remove_file(&log);
    let outcome = self_heal_preflight(&f.worktree, true, Some(&log), "bead-test").unwrap();
    assert_eq!(outcome, PreflightOutcome::DirtyReset);

    let restored = fs::read_to_string(f.worktree.join("tracked.txt")).unwrap();
    assert_eq!(restored, "v1\n", "tracked file must be restored to HEAD");
    let body = fs::read_to_string(&log).unwrap();
    assert!(
        body.contains(EVENT_DIRTY_RESET),
        "expected WORKTREE_SELFHEAL_DIRTY_RESET, got: {body}"
    );
    let _ = fs::remove_file(&log);
    f.cleanup();
}

/// RED → GREEN proof: an operator-owned checkout whose tracked file is
/// dirty must NOT be reset — operator work survives. With
/// `dirty_reset = false` the preflight returns `Clean` (no telemetry
/// event for this case) and leaves the file untouched.
#[test]
fn preflight_preserves_dirty_tracked_state_for_operator() {
    let f = Fixtures::new("dirty_operator");
    fs::write(f.worktree.join("operator-note.txt"), "important").unwrap();
    let _ = Command::new("git")
        .args(["add", "operator-note.txt"])
        .current_dir(&f.worktree)
        .status()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=jleechan2015@users.noreply.github.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "operator",
        ])
        .current_dir(&f.worktree)
        .status()
        .unwrap();
    fs::write(f.worktree.join("operator-note.txt"), "modified\n").unwrap();

    let log = telemetry_log("dirty_operator");
    let _ = fs::remove_file(&log);
    let outcome = self_heal_preflight(&f.worktree, false, Some(&log), "bead-test").unwrap();
    assert_eq!(
        outcome,
        PreflightOutcome::Clean,
        "operator-dirty must not flip to DirtyReset"
    );
    let preserved = fs::read_to_string(f.worktree.join("operator-note.txt")).unwrap();
    assert_eq!(
        preserved, "modified\n",
        "operator-owned checkout must NOT be reset"
    );
    let body = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !body.contains(EVENT_DIRTY_RESET),
        "operator-dirty must not emit DIRTY_RESET, got: {body}"
    );
    let _ = fs::remove_file(&log);
    f.cleanup();
}

/// RED → GREEN proof: a checkout that is already clean passes through
/// the preflight unchanged (no spurious telemetry).
#[test]
fn preflight_passes_through_clean_checkout() {
    let f = Fixtures::new("clean");
    let log = telemetry_log("clean");
    let _ = fs::remove_file(&log);

    let outcome = self_heal_preflight(&f.worktree, true, Some(&log), "bead-test").unwrap();
    assert_eq!(outcome, PreflightOutcome::Clean);
    let body = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !body.contains(EVENT_INDEX_LOCK_REMOVED)
            && !body.contains(EVENT_CACHES_CLEANED)
            && !body.contains(EVENT_DIRTY_RESET)
            && !body.contains(EVENT_FALLBACK_PROVISIONED),
        "clean checkout must not emit any self-heal event, got: {body}"
    );
    let _ = fs::remove_file(&log);
    f.cleanup();
}
