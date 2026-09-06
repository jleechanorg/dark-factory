use crate::errors::DaemonError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One `[repos."<owner>/<repo>"]` table entry (bead jleechan-35y4, Stage B of
/// the multi-repo dispatch fix — see
/// `docs/multirepo-dispatch-investigation-2026-07-11.md`).
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RepoConfig {
    pub ao_project: String,
    pub push_remote: String,
    /// Local checkout used as the working directory for workers targeting
    /// this repository. Cross-repository dispatch must not inherit the
    /// daemon's own checkout.
    #[serde(default)]
    pub local_checkout: Option<std::path::PathBuf>,
}

/// Resolved dispatch routing for a repo — the AO project to spawn into and
/// the git remote a coder must push to. Returned by [`Config::resolve_repo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRouting {
    pub ao_project: String,
    pub push_remote: String,
    pub local_checkout: Option<std::path::PathBuf>,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Config {
    pub target_repo: String,
    #[serde(default)]
    pub ao_project: Option<String>,
    pub base_branch: String,
    pub stage: u8,
    pub max_workers: usize,
    pub max_batch: usize,
    pub fast_tick_secs: u64,
    pub slow_tick_secs: u64,
    pub autonomy_timebox_secs: u64,
    pub budget_warn_usd: f64,
    pub spec_dir: String,
    /// Bead jleechan-zeij / issue #322 r3: the head-stability window (seconds)
    /// the fail-closed re-roll proceed predicate requires before superseding a
    /// worker whose AO session is still present but non-running (terminal, or
    /// idle with a quiet transcript). Codex r3 review: a ~500ms two-read
    /// stability check does not prove a mid-tool-call worker won't push again;
    /// the window must be wide enough that an active worker's next push lands
    /// inside it. `#[serde(default)]` (30s) so every pre-existing `daemon.toml`
    /// parses unchanged. The positive-death path (a re-attach loop confirming
    /// `SessionNotFound`) is the fast path and is NOT gated by this window.
    #[serde(default = "default_reroll_head_stability_window_secs")]
    pub reroll_head_stability_window_secs: u64,
    /// Bead jleechan-zeij / issue #322 r3: how long (seconds) the post-stop
    /// re-attach probe must observe a CONTINUOUS `SessionNotFound` before
    /// declaring the previous worker positively dead. `ao session kill`
    /// swallows tmux-destruction failures, so a single `SessionNotFound` right
    /// after stop() is not proof of death — requiring it to hold for this
    /// window guards against a momentary `ao status` omission. `#[serde(default)]`
    /// (5s).
    #[serde(default = "default_reroll_death_confirm_secs")]
    pub reroll_death_confirm_secs: u64,
    /// Bead jleechan-zaga / issue #348 r3: cooldown (seconds) between fast-tier
    /// re-assessments of a bead held at DISPOSITION_REQUIRED. A structural
    /// condition (CodeRabbit unavailable, unresolved bot threads) can persist
    /// for hours; without a cooldown the fast tier would re-fetch the PR
    /// snapshot every tick, hammering the SCM API for no benefit. Default 15
    /// minutes. `#[serde(default)]` so every pre-existing `daemon.toml` parses
    /// unchanged.
    #[serde(default = "default_held_recheck_cooldown_secs")]
    pub held_recheck_cooldown_secs: u64,
    /// Multi-repo routing table (bead jleechan-35y4 Stage B). Absent entirely
    /// from a config file (the common case for every pre-existing
    /// `daemon.toml`) deserializes to an empty map via `#[serde(default)]` —
    /// existing single-repo behavior is UNCHANGED when `[repos]` is absent
    /// (explicit acceptance criterion): [`resolve_repo`](Config::resolve_repo)
    /// falls back to `target_repo`/`ao_project` for the one repo every
    /// pre-migration config already names.
    #[serde(default)]
    pub repos: HashMap<String, RepoConfig>,
    /// Bead jleechan-t40t (issue #326): when true, the slow-tier fast loop
    /// runs a pre-gate validation step before every gate assessment,
    /// confirming the stored `pr_number`'s PR is OPEN and its head ref
    /// matches `overlay.branch`. Catches drift between `pr_number` and the
    /// bead's actual branch for ATTESTED beads whose stored number has not
    /// been re-resolved this tick (the dispatch→attested path's re-resolution
    /// only fires when the bead transitions through DISPATCHED).
    ///
    /// r12 (issue #326): DEFAULT TRUE. The pre-gate drift check is the
    /// primary guard against gate-assessing a stale/closed PR (jleechan-t8fd
    /// / PR #316 wedge), so it must be on out of the box; a pre-#326
    /// `daemon.toml` that omits the key now gets it enabled. Production
    /// `CliScm` always returns real `open_pr_head_ref` data. Integration
    /// tests that don't script `open_pr_head_refs` set it explicitly `false`.
    #[serde(default = "default_pre_gate_validation_enabled")]
    pub pre_gate_validation_enabled: bool,
    /// Backoff window (seconds) governing escalation re-fire dedup
    /// (1s2q-escalation-dedup): an ESCALATION_REQUIRED /
    /// ESCALATION_NOTIFICATION_FAILED event is suppressed unless its context
    /// hash changed OR the last emit for `(bead_id, reason)` is older than
    /// this window. Stops the live-incident spam where a bead with an
    /// identical permanent condition re-fired every ~40s. Default 1 hour.
    /// `#[serde(default)]` so every pre-existing `daemon.toml` parses
    /// unchanged.
    #[serde(default = "default_escalation_refire_secs")]
    pub escalation_refire_secs: u64,
    /// Bead jleechan-jw4c: root directory for per-agent worktrees spawned
    /// by AO outside of the primary checkout. When `Some`, the spawn path
    /// computes the agent's worktree as `$root/<repo>/<agent-id>` and the
    /// reaper prunes under it. When `None` (default), the daemon keeps the
    /// legacy primary-checkout layout (`$HOME/.dark-factory/agent-worktrees/`)
    /// so existing deployments are unaffected. Operators flipping this on
    /// MUST also flip the equivalent env var on AO/`aow` so the
    /// `worker_checkout` derivation stays consistent.
    #[serde(default)]
    pub agent_worktree_root: Option<String>,
    /// Bead jleechan-jw4c: max age (in seconds) before a stale agent
    /// worktree is considered prunable by the reaper. Default 14 days.
    /// `#[serde(default)]` so existing configs parse unchanged.
    #[serde(default = "default_worktree_ttl_secs")]
    pub worktree_ttl_secs: u64,
    /// Bead jleechan-jw4c: max number of agent worktrees allowed per repo
    /// under `agent_worktree_root`. New worktree creation fails closed when
    /// this cap is reached. Default 200 (matches the bead's spec).
    #[serde(default = "default_worktree_max_count")]
    pub worktree_max_count: usize,
}

/// Default head-stability window (bead jleechan-zeij / issue #322 r3): 30s,
/// per the Codex review's "configurable minimum (default ≥30s)".
fn default_reroll_head_stability_window_secs() -> u64 {
    30
}

/// Default for `pre_gate_validation_enabled` (bead jleechan-t40t / issue #326
/// r12): TRUE — the pre-gate PR OPEN/head-match drift check is on by default.
fn default_pre_gate_validation_enabled() -> bool {
    true
}

/// Default escalation re-fire backoff (1s2q-escalation-dedup): 1 hour between
/// re-emissions of an identical-context escalation event for the same
/// `(bead_id, reason)`.
fn default_escalation_refire_secs() -> u64 {
    3600
}

/// Bead jleechan-jw4c: 14 days is the default TTL for the agent worktree
/// reaper. Long enough that an active agent's worktree is never pruned
/// while it's still alive, short enough that abandoned worktrees become
/// prunable well within the 60G outage scenario the bead describes.
fn default_worktree_ttl_secs() -> u64 {
    14 * 24 * 60 * 60
}

/// Bead jleechan-jw4c: 200 worktrees per repo is the default cap. Above
/// this, the daemon refuses to register a new worktree (fail closed). The
/// number is empirical: the jw4c production RED measurement was 511
/// registrations spread across multiple repos, so 200 is a healthy per-repo
/// ceiling that still allows ~3 simultaneous active repos before the cap
/// triggers.
fn default_worktree_max_count() -> usize {
    200
}

/// Default held-recheck cooldown (bead jleechan-zaga / issue #348 r3): 15
/// minutes between re-assessments of a DISPOSITION_REQUIRED bead.
fn default_held_recheck_cooldown_secs() -> u64 {
    900
}

/// Default positive-death confirmation window (bead jleechan-zeij / issue #322
/// r3): 5s, per the Codex review's "3 attempts over ≥5s".
fn default_reroll_death_confirm_secs() -> u64 {
    5
}

impl Config {
    /// Resolve the target repository checkout used by execution-time gates.
    /// Explicit `local_checkout` wins. When omitted, resolve a daemon-owned
    /// checkout under `$DARK_FACTORY_TARGET_WORKTREE_ROOT` (defaulting to
    /// `$HOME/.dark-factory/target-worktrees`) using the complete
    /// `<owner>/<repo>` identity. The daemon's own current directory is never
    /// used: release binaries are commonly launched from an immutable
    /// uv/archive path, and a repository basename is not globally unique.
    pub fn target_worktree_path(&self, repo: &str) -> Option<PathBuf> {
        if let Some(routing) = self.resolve_repo(repo) {
            if let Some(path) = routing.local_checkout {
                return Some(path);
            }
        }
        let (owner, name) = repo.split_once('/')?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let isolated_root = std::env::var_os("DARK_FACTORY_TARGET_WORKTREE_ROOT")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".dark-factory/target-worktrees")))?;
        Some(isolated_root.join(owner).join(name))
    }

    pub fn target_worktree(&self, repo: &str) -> Option<PathBuf> {
        let path = self.target_worktree_path(repo)?;
        path.is_dir().then_some(path)
    }

    /// Resolve the mutable reroll spec outside managed source checkouts.
    /// Absolute paths are explicit operator-owned locations. Relative paths
    /// use the daemon runtime-state tree, sharded by complete repository
    /// identity, so mutation cannot dirty an exact-head target checkout.
    pub fn resolve_spec_path(&self, repo: &str, bead_id: &str) -> PathBuf {
        let filename = format!("{bead_id}.toml");
        if Path::new(&self.spec_dir).is_relative() {
            let (owner, name) = repo.split_once('/').unwrap_or(("unknown", repo));
            crate::intake::runtime_state_dir()
                .join("specs")
                .join(owner)
                .join(name)
                .join(filename)
        } else {
            Path::new(&self.spec_dir).join(&filename)
        }
    }

    /// Bead jleechan-jw4c: resolve the per-agent worktree directory under
    /// `agent_worktree_root`. Returns `None` when the operator has not
    /// flipped on the new layout (the config knob is `None`), in which
    /// case the reaper and the cwd guard treat the legacy layout as
    /// authoritative. The path is computed even when the directory does
    /// not yet exist — the caller decides whether to create or refuse.
    ///
    /// Layout: `$agent_worktree_root/<repo>/<agent_id>`. The repo's
    /// `[repos."<owner>/<repo>"]` sharding is intentional: two different
    /// repos that happen to share an agent-id name (e.g. different
    /// `df-100`) MUST land in distinct trees to avoid collision.
    pub fn agent_worktree_path(&self, repo: &str, agent_id: &str) -> Option<PathBuf> {
        let root = self.agent_worktree_root.as_deref()?;
        if root.is_empty() {
            return None;
        }
        if agent_id.is_empty() || agent_id.contains('/') || agent_id.contains("..") {
            return None;
        }
        Some(PathBuf::from(root).join(repo).join(agent_id))
    }

    /// Bead jleechan-jw4c: resolve the per-repo root of the agent worktree
    /// tree (the parent of every agent-id subtree). `None` when the knob
    /// is off. The reaper enumerates every `agent_id` directory under
    /// this root to identify prunable candidates.
    pub fn agent_worktree_root_for_repo(&self, repo: &str) -> Option<PathBuf> {
        let root = self.agent_worktree_root.as_deref()?;
        if root.is_empty() {
            return None;
        }
        Some(PathBuf::from(root).join(repo))
    }

    /// Resolve dispatch routing for `repo` (`overlay.repo(self)`'s output).
    /// `Some(routing)` when `repo` is KNOWN — either an explicit
    /// `[repos."<repo>"]` entry, or `repo == self.target_repo` (the
    /// single-repo/legacy case, using `self.ao_project` when set, else the
    /// same last-path-segment derivation `CliSessions::new` already applies,
    /// with `push_remote` defaulting to `"origin"`). `None` when `repo` is
    /// neither — the caller (`dispatch::dispatch_ready`) must park the bead
    /// `HUMAN_HELD` with reason `unmapped_target_repo` rather than guessing
    /// (fail loud, matching the jleechan-9sh5 discipline: never silently
    /// fall back to the global repo when a bead explicitly claims a
    /// different, unmapped one).
    pub fn resolve_repo(&self, repo: &str) -> Option<RepoRouting> {
        if let Some(rc) = self.repos.get(repo) {
            return Some(RepoRouting {
                ao_project: rc.ao_project.clone(),
                push_remote: rc.push_remote.clone(),
                local_checkout: rc.local_checkout.clone(),
            });
        }
        if repo == self.target_repo {
            let ao_project = self.ao_project.clone().unwrap_or_else(|| {
                // Same last-path-segment derivation `CliSessions::new`
                // already applies for the single-repo case, kept here so
                // `resolve_repo`'s `ao_project` matches what a legacy config
                // (no `[repos]`, no explicit `ao_project`) actually spawns
                // into today.
                let mut project = repo.split('/').next_back().unwrap_or(repo).to_string();
                if project == "worldarchitect.ai" {
                    project = "worldarchitect".to_string();
                }
                project
            });
            // NOTE (adversarial review of PR #245): `"origin"` is correct
            // for a single-remote clone and for dark-factory's own repo,
            // but a dual-remote worldarchitect.ai clone (`origin` =
            // jleechanclaw, `worldai` = worldarchitect.ai — see
            // docs/multirepo-dispatch-investigation-2026-07-11.md step 4)
            // needs `"worldai"`. Inert TODAY because `SpawnSpec.remote` is
            // not yet consumed by `CliSessions::spawn` (Stage C, bead
            // jleechan-bqdv) — but a `target_repo`/`cfg.target_repo` set to
            // `jleechanorg/worldarchitect.ai` with no explicit
            // `[repos."jleechanorg/worldarchitect.ai"]` entry will resolve
            // the WRONG remote here the moment Stage C starts consuming it.
            // Add that `[repos.*]` entry to `config/daemon.toml` for any
            // dual-remote repo rather than relying on this fallback.
            return Some(RepoRouting {
                ao_project,
                push_remote: "origin".to_string(),
                local_checkout: None,
            });
        }
        None
    }

    pub fn worker_checkout_is_configured(&self, repo: &str, routing: &RepoRouting) -> bool {
        if is_fixture_repo(repo) && routing.local_checkout.is_none() {
            return false;
        }
        if !self.repos.contains_key(repo) {
            return true;
        }
        match routing.local_checkout.as_ref() {
            None => !is_fixture_repo(repo),
            Some(checkout) => checkout.is_absolute() && checkout.is_dir(),
        }
    }
}

/// Test/fixture repository identities are not clone-eligible.
pub fn is_fixture_repo(repo: &str) -> bool {
    // Keep this allow-list explicit: production repositories whose names
    // happen to contain `test-` or `fake-` must still run the real gates.
    matches!(repo, "owner/repo" | "other/repo" | "myorg/myrepo")
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    static TARGET_WORKTREE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parses_example_config() {
        let cfg = load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("contracts/daemon.toml.example"),
        )
        .unwrap();
        assert_eq!(cfg.ao_project.as_deref(), Some("dark-factory"));
        assert_eq!(cfg.stage, 1);
        assert_eq!(cfg.max_workers, 40);
        assert_eq!(cfg.max_batch, 15);
        assert_eq!(cfg.base_branch, "main");
    }
    #[test]
    fn missing_key_is_config_error() {
        let dir = std::env::temp_dir().join("afd_cfg_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
        std::fs::write(&p, "target_repo = \"x/y\"\n").unwrap();
        assert!(matches!(
            load(&p),
            Err(crate::errors::DaemonError::Config(_))
        ));
    }
    #[test]
    fn reroll_predicate_windows_default_when_absent() {
        // Bead jleechan-zeij / issue #322 r3: a legacy daemon.toml with no
        // reroll-predicate keys must parse and default to the production
        // fail-closed windows (30s stability, 5s positive-death).
        let dir = std::env::temp_dir().join("afd_cfg_test_reroll_windows_default");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("legacy_no_reroll_windows.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "owner/repo"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.reroll_head_stability_window_secs, 30);
        assert_eq!(cfg.reroll_death_confirm_secs, 5);
        // Bead jleechan-zaga / issue #348 r3: the held-recheck cooldown
        // defaults to 15 minutes and is config-overridable.
        assert_eq!(cfg.held_recheck_cooldown_secs, 900);
        // Bead jleechan-t40t / issue #326 r12: pre-gate PR validation is ON by
        // default — a config that omits the key still gets the drift guard.
        assert!(
            cfg.pre_gate_validation_enabled,
            "pre_gate_validation_enabled must default to true when absent"
        );
        // 1s2q-escalation-dedup: escalation re-fire backoff defaults to 1h.
        assert_eq!(cfg.escalation_refire_secs, 3600);
    }

    #[test]
    fn pre_gate_validation_can_be_disabled_explicitly() {
        // The default is true, but an operator (or an integration test) may
        // still turn it off explicitly.
        let dir = std::env::temp_dir().join("afd_cfg_test_pre_gate_off");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pre_gate_off.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "owner/repo"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"
pre_gate_validation_enabled = false
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert!(!cfg.pre_gate_validation_enabled);
    }

    #[test]
    fn ao_project_is_optional_for_legacy_configs() {
        let dir = std::env::temp_dir().join("afd_cfg_test_ao_project_optional");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("legacy.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "owner/repo"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.ao_project, None);
    }

    // jleechan-35y4 Stage B: [repos] table + resolve_repo.

    /// Explicit acceptance criterion: a config file with no `[repos]` table
    /// at all (every pre-existing `daemon.toml`) must parse exactly as
    /// before, AND `resolve_repo` must still resolve the one repo that
    /// config already names — single-repo behavior is unchanged.
    #[test]
    fn repos_table_absent_is_unchanged_single_repo_behavior() {
        let dir = std::env::temp_dir().join("afd_cfg_test_repos_absent");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("no_repos_table.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "owner/repo"
ao_project = "repo"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert!(
            cfg.repos.is_empty(),
            "repos table must default to empty when absent"
        );
        assert_eq!(
            cfg.resolve_repo("owner/repo"),
            Some(RepoRouting {
                ao_project: "repo".to_string(),
                push_remote: "origin".to_string(),
                local_checkout: None,
            }),
            "the global target_repo must still resolve when [repos] is absent"
        );
    }

    #[test]
    fn repos_table_parses_multiple_entries() {
        let dir = std::env::temp_dir().join("afd_cfg_test_repos_table");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("multi_repo.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/dark-factory"
ao_project = "dark-factory"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."jleechanorg/worldarchitect.ai"]
ao_project = "worldarchitect"
push_remote = "worldai"
local_checkout = "/srv/repos/worldarchitect.ai"

[repos."jleechanorg/dark-factory"]
ao_project = "dark-factory"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.repos.len(), 2);
        assert_eq!(
            cfg.resolve_repo("jleechanorg/worldarchitect.ai"),
            Some(RepoRouting {
                ao_project: "worldarchitect".to_string(),
                push_remote: "worldai".to_string(),
                local_checkout: Some(std::path::PathBuf::from("/srv/repos/worldarchitect.ai")),
            })
        );
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            Some(RepoRouting {
                ao_project: "dark-factory".to_string(),
                push_remote: "origin".to_string(),
                local_checkout: None,
            })
        );
    }

    #[test]
    fn resolve_repo_returns_none_for_unmapped_repo() {
        let dir = std::env::temp_dir().join("afd_cfg_test_unmapped_repo");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("unmapped.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/dark-factory"
ao_project = "dark-factory"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."jleechanorg/dark-factory"]
ao_project = "dark-factory"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        // Neither the global target_repo nor a [repos.*] entry names this
        // repo — must fail loud (None), never guess/fall back.
        assert_eq!(cfg.resolve_repo("someorg/unrelated-repo"), None);
    }

    #[test]
    fn resolve_repo_falls_back_to_worldarchitect_project_derivation_when_ao_project_unset() {
        // Mirrors CliSessions::new's special-case: worldarchitect.ai's repo
        // name is NOT its AO project name. resolve_repo must derive the same
        // project a legacy config (no explicit ao_project, no [repos] entry)
        // would actually spawn into today.
        let dir = std::env::temp_dir().join("afd_cfg_test_worldarchitect_derivation");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("worldarchitect.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/worldarchitect.ai"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(
            cfg.resolve_repo("jleechanorg/worldarchitect.ai"),
            Some(RepoRouting {
                ao_project: "worldarchitect".to_string(),
                push_remote: "origin".to_string(),
                local_checkout: None,
            })
        );
    }

    #[test]
    fn target_worktree_prefers_explicit_local_checkout() {
        let root = std::env::temp_dir().join(format!("afd_target_worktree_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg_path = root.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!(
                r#"target_repo = "owner/daemon"
base_branch = "main"
stage = 1
max_workers = 1
max_batch = 1
fast_tick_secs = 1
slow_tick_secs = 1
autonomy_timebox_secs = 60
budget_warn_usd = 1.0
spec_dir = ".factory/specs/"
[repos."owner/target"]
ao_project = "target"
push_remote = "origin"
local_checkout = "{}"
"#,
                root.join("target-checkout").display()
            ),
        )
        .unwrap();
        let cfg = load(&cfg_path).unwrap();
        let expected = root.join("target-checkout");
        assert_eq!(cfg.target_worktree_path("owner/target"), Some(expected));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn target_worktree_reuses_isolated_checkout_when_local_checkout_is_missing() {
        let _lock = TARGET_WORKTREE_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = std::env::temp_dir().join(format!("afd_isolated_target_{}", std::process::id()));
        let isolated = root.join("owner").join("target");
        std::fs::create_dir_all(&isolated).unwrap();
        let previous = std::env::var_os("DARK_FACTORY_TARGET_WORKTREE_ROOT");
        std::env::set_var("DARK_FACTORY_TARGET_WORKTREE_ROOT", &root);
        let cfg_path = root.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            r#"target_repo = "owner/daemon"
base_branch = "main"
stage = 1
max_workers = 1
max_batch = 1
fast_tick_secs = 1
slow_tick_secs = 1
autonomy_timebox_secs = 60
budget_warn_usd = 1.0
spec_dir = ".factory/specs/"
[repos."owner/target"]
ao_project = "target"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&cfg_path).unwrap();
        assert_eq!(cfg.target_worktree("owner/target"), Some(isolated));
        match previous {
            Some(value) => std::env::set_var("DARK_FACTORY_TARGET_WORKTREE_ROOT", value),
            None => std::env::remove_var("DARK_FACTORY_TARGET_WORKTREE_ROOT"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Relative runtime specs must never dirty the managed target checkout.
    #[test]
    fn relative_spec_dir_uses_runtime_state_not_target_worktree() {
        let cfg = Config {
            target_repo: "owner/daemon".into(),
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
            reroll_head_stability_window_secs: 30,
            reroll_death_confirm_secs: 5,
            held_recheck_cooldown_secs: 900,
            repos: std::collections::HashMap::new(),
            pre_gate_validation_enabled: false,
            escalation_refire_secs: 3600,
            agent_worktree_root: None,
            worktree_ttl_secs: 14 * 24 * 60 * 60,
            worktree_max_count: 200,
        };

        let resolved = cfg.resolve_spec_path("owner/target", "bead-123");
        let expected = crate::intake::runtime_state_dir()
            .join("specs")
            .join("owner")
            .join("target")
            .join("bead-123.toml");
        assert_eq!(
            resolved, expected,
            "a relative spec_dir must resolve under daemon runtime state, not a managed source checkout"
        );
    }

    /// Companion to the relative case above: an ABSOLUTE `spec_dir` must be
    /// used as-is, never joined onto the target worktree root at all.
    #[test]
    fn absolute_spec_dir_ignores_target_worktree() {
        let root = std::env::temp_dir().join(format!("afd_absolute_spec_dir_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let absolute_spec_dir = root.join("shared-specs");

        let cfg = Config {
            target_repo: "owner/daemon".into(),
            ao_project: None,
            base_branch: "main".into(),
            stage: 1,
            max_workers: 1,
            max_batch: 1,
            fast_tick_secs: 1,
            slow_tick_secs: 1,
            autonomy_timebox_secs: 60,
            budget_warn_usd: 1.0,
            spec_dir: absolute_spec_dir.display().to_string(),
            reroll_head_stability_window_secs: 30,
            reroll_death_confirm_secs: 5,
            held_recheck_cooldown_secs: 900,
            repos: std::collections::HashMap::new(),
            pre_gate_validation_enabled: false,
            escalation_refire_secs: 3600,
            agent_worktree_root: None,
            worktree_ttl_secs: 14 * 24 * 60 * 60,
            worktree_max_count: 200,
        };

        let resolved = cfg.resolve_spec_path("owner/target", "bead-456");
        assert_eq!(resolved, absolute_spec_dir.join("bead-456.toml"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_production_repo_without_checkout_is_clone_eligible() {
        let cfg = Config {
            target_repo: "owner/daemon".into(),
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
            reroll_head_stability_window_secs: 30,
            reroll_death_confirm_secs: 5,
            held_recheck_cooldown_secs: 900,
            repos: HashMap::from([(
                "owner/production".into(),
                RepoConfig {
                    ao_project: "production".into(),
                    push_remote: "origin".into(),
                    local_checkout: None,
                },
            )]),
            pre_gate_validation_enabled: true,
            escalation_refire_secs: 3600,
            agent_worktree_root: None,
            worktree_ttl_secs: 14 * 24 * 60 * 60,
            worktree_max_count: 200,
        };
        let routing = cfg.resolve_repo("owner/production").unwrap();
        assert!(cfg.worker_checkout_is_configured("owner/production", &routing));
        assert!(cfg.target_worktree_path("owner/production").is_some());
    }

    #[test]
    fn explicit_missing_absolute_checkout_is_not_clone_eligible() {
        let root = std::env::temp_dir().join(format!(
            "afd_missing_checkout_{}",
            std::process::id()
        ));
        let checkout = root.join("production");
        let cfg = Config {
            target_repo: "owner/daemon".into(),
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
            reroll_head_stability_window_secs: 30,
            reroll_death_confirm_secs: 5,
            held_recheck_cooldown_secs: 900,
            repos: HashMap::from([(
                "owner/production".into(),
                RepoConfig {
                    ao_project: "production".into(),
                    push_remote: "origin".into(),
                    local_checkout: Some(checkout.clone()),
                },
            )]),
            pre_gate_validation_enabled: true,
            escalation_refire_secs: 3600,
            agent_worktree_root: None,
            worktree_ttl_secs: 14 * 24 * 60 * 60,
            worktree_max_count: 200,
        };
        let routing = cfg.resolve_repo("owner/production").unwrap();
        assert!(!cfg.worker_checkout_is_configured("owner/production", &routing));
        assert_eq!(cfg.target_worktree_path("owner/production"), Some(checkout));
    }

    #[test]
    fn production_repo_names_are_not_fixture_classified_by_substrings() {
        for repo in ["owner/test-repo", "owner/fake-repo"] {
            assert!(!is_fixture_repo(repo), "{repo} must remain production-shaped");
        }
    }

    #[test]
    fn explicit_relative_checkout_is_not_clone_eligible() {
        let cfg = Config {
            target_repo: "owner/daemon".into(),
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
            reroll_head_stability_window_secs: 30,
            reroll_death_confirm_secs: 5,
            held_recheck_cooldown_secs: 900,
            repos: HashMap::from([(
                "owner/production".into(),
                RepoConfig {
                    ao_project: "production".into(),
                    push_remote: "origin".into(),
                    local_checkout: Some(PathBuf::from("relative/checkout")),
                },
            )]),
            pre_gate_validation_enabled: true,
            escalation_refire_secs: 3600,
            agent_worktree_root: None,
            worktree_ttl_secs: 14 * 24 * 60 * 60,
            worktree_max_count: 200,
        };
        let routing = cfg.resolve_repo("owner/production").unwrap();
        assert!(!cfg.worker_checkout_is_configured("owner/production", &routing));
    }

    #[test]
    fn isolated_target_worktree_path_keeps_same_name_repositories_separate() {
        let _lock = TARGET_WORKTREE_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = std::env::temp_dir().join(format!(
            "afd_isolated_same_name_{}",
            std::process::id()
        ));
        let previous = std::env::var_os("DARK_FACTORY_TARGET_WORKTREE_ROOT");
        std::env::set_var("DARK_FACTORY_TARGET_WORKTREE_ROOT", &root);
        let cfg_path = root.join("daemon.toml");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &cfg_path,
            r#"target_repo = "owner-a/repo"
base_branch = "main"
stage = 1
max_workers = 1
max_batch = 1
fast_tick_secs = 1
slow_tick_secs = 1
autonomy_timebox_secs = 60
budget_warn_usd = 1.0
spec_dir = ".factory/specs/"
[repos."owner-a/repo"]
ao_project = "repo-a"
push_remote = "origin"
[repos."owner-b/repo"]
ao_project = "repo-b"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&cfg_path).unwrap();
        let first = cfg.target_worktree_path("owner-a/repo").unwrap();
        let second = cfg.target_worktree_path("owner-b/repo").unwrap();
        assert_ne!(first, second);
        assert!(first.ends_with(std::path::Path::new("owner-a/repo")));
        assert!(second.ends_with(std::path::Path::new("owner-b/repo")));
        match previous {
            Some(value) => std::env::set_var("DARK_FACTORY_TARGET_WORKTREE_ROOT", value),
            None => std::env::remove_var("DARK_FACTORY_TARGET_WORKTREE_ROOT"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    // Bead jleechan-jw4c: the agent_worktree_root knob defaults to OFF so
    // existing daemon.toml files parse unchanged. Operators flip it on
    // when they want worktrees relocated out of the primary checkout.

    #[test]
    fn agent_worktree_root_defaults_to_none_for_legacy_configs() {
        let dir = std::env::temp_dir().join(format!(
            "afd_agent_root_default_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("legacy.toml");
        std::fs::write(
            &p,
            r#"target_repo = "owner/repo"
base_branch = "main"
stage = 1
max_workers = 1
max_batch = 1
fast_tick_secs = 1
slow_tick_secs = 1
autonomy_timebox_secs = 60
budget_warn_usd = 1.0
spec_dir = ".factory/specs/"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert!(cfg.agent_worktree_root.is_none());
        assert!(cfg.agent_worktree_path("owner/repo", "df-100").is_none());
        assert!(cfg.agent_worktree_root_for_repo("owner/repo").is_none());
        // 14d default.
        assert_eq!(cfg.worktree_ttl_secs, 14 * 24 * 60 * 60);
        assert_eq!(cfg.worktree_max_count, 200);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn agent_worktree_path_uses_owner_repo_layout() {
        let cfg = Config {
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
            reroll_head_stability_window_secs: 30,
            reroll_death_confirm_secs: 5,
            held_recheck_cooldown_secs: 0,
            repos: HashMap::new(),
            pre_gate_validation_enabled: false,
            escalation_refire_secs: 0,
            agent_worktree_root: Some("/tmp/agent_worktrees".into()),
            worktree_ttl_secs: 60,
            worktree_max_count: 200,
        };
        let path = cfg.agent_worktree_path("owner/repo", "df-100").unwrap();
        assert!(path.starts_with("/tmp/agent_worktrees/owner/repo/df-100"));
        let root = cfg.agent_worktree_root_for_repo("owner/repo").unwrap();
        assert!(root.starts_with("/tmp/agent_worktrees/owner/repo"));
    }

    #[test]
    fn worktree_ttl_and_max_count_override_default() {
        let dir = std::env::temp_dir().join(format!(
            "afd_worktree_overrides_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("custom.toml");
        std::fs::write(
            &p,
            format!(
                r#"target_repo = "owner/repo"
base_branch = "main"
stage = 1
max_workers = 1
max_batch = 1
fast_tick_secs = 1
slow_tick_secs = 1
autonomy_timebox_secs = 60
budget_warn_usd = 1.0
spec_dir = ".factory/specs/"
agent_worktree_root = "{}"
worktree_ttl_secs = 2592000
worktree_max_count = 50
"#,
                dir.display()
            ),
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.worktree_ttl_secs, 2_592_000);
        assert_eq!(cfg.worktree_max_count, 50);
        assert!(cfg.agent_worktree_root.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
