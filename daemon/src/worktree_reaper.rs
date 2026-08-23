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
use crate::telemetry::{emit, TelemetryEvent};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        if !path.is_dir() {
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
        if !candidate.path.starts_with(&report.root) {
            continue;
        }
        if !candidate.path.is_dir() {
            continue;
        }
        std::fs::remove_dir_all(&candidate.path).map_err(|e| {
            DaemonError::Config(format!(
                "worktree reaper: remove {}: {e}",
                candidate.path.display()
            ))
        })?;
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
    if !path.is_dir() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&path).map_err(|e| {
        DaemonError::Config(format!(
            "worktree reaper: clean_stale_worktree remove {}: {e}",
            path.display()
        ))
    })?;
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
        touch_dir(&target, 0);
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
        touch_dir(&target, 0);
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
