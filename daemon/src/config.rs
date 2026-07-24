use crate::errors::DaemonError;
use std::collections::HashMap;
use std::path::Path;

/// One `[repos."<owner>/<repo>"]` table entry (bead jleechan-35y4, Stage B of
/// the multi-repo dispatch fix — see
/// `docs/multirepo-dispatch-investigation-2026-07-11.md`).
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RepoConfig {
    pub ao_project: String,
    pub push_remote: String,
}

/// Bead jleechan-87ea / PR #289: classifies how a bead's `target_repo`
/// resolved during `Config::resolve_repo`. `Explicit` = an explicit
/// `[repos."<owner>/<repo>"]` entry, `GlobalTarget` = the daemon's global
/// `target_repo` (single-repo/legacy case), `Derived` = an unseen-but-valid
/// repo whose AO project was derived from its name (last path segment, with
/// the `worldarchitect.ai` → `worldarchitect` special case). Surfaced in
/// `DispatchSuccess.routing_source` and `DERIVED_ROUTE_RESOLVED` telemetry
/// so a derived dispatch is durably distinguishable from a configured one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingSource {
    Explicit,
    GlobalTarget,
    Derived,
}

impl RoutingSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoutingSource::Explicit => "explicit",
            RoutingSource::GlobalTarget => "global_target",
            RoutingSource::Derived => "derived",
        }
    }
}

/// Resolved dispatch routing for a repo — the AO project to spawn into and
/// the git remote a coder must push to. Returned by [`Config::resolve_repo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRouting {
    pub ao_project: String,
    pub push_remote: String,
    /// Bead jleechan-87ea / PR #289: how this routing was resolved. New field
    /// on top of main's Stage-B `RepoRouting` (bead jleechan-35y4); existing
    /// construction sites that omit `source` are migrated to fill it in.
    pub source: RoutingSource,
}

/// Acceptable lengths for GitHub owner and repo names. Bead jleechan-87ea /
/// PR #289: load-time validation rejects overlength owner/repo strings so
/// a malformed config cannot silently route into a project whose name is
/// truncated by GitHub's API.
const MAX_OWNER_LEN: usize = 39;
const MAX_REPO_LEN: usize = 100;

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
    /// Backoff window (seconds) governing escalation re-fire dedup
    /// (1s2q-escalation-dedup, re-introduced by clean replay of PR #470):
    /// an ESCALATION_REQUIRED / ESCALATION_NOTIFICATION_FAILED event is
    /// suppressed unless its context hash changed OR the last emit for
    /// `(bead_id, reason)` is older than this window. Stops the
    /// live-incident spam where a bead with an identical permanent
    /// condition re-fired every ~40s. Default 1 hour. `#[serde(default)]`
    /// so every pre-existing `daemon.toml` parses unchanged.
    #[serde(default = "default_escalation_refire_secs")]
    pub escalation_refire_secs: u64,
    /// Multi-repo routing table (bead jleechan-35y4 Stage B). Absent entirely
    /// from a config file (the common case for every pre-existing
    /// `daemon.toml`) deserializes to an empty map via `#[serde(default)]` —
    /// existing single-repo behavior is UNCHANGED when `[repos]` is absent
    /// (explicit acceptance criterion): [`resolve_repo`](Config::resolve_repo)
    /// falls back to `target_repo`/`ao_project` for the one repo every
    /// pre-migration config already names.
    #[serde(default)]
    pub repos: HashMap<String, RepoConfig>,
}

/// Default head-stability window (bead jleechan-zeij / issue #322 r3): 30s,
/// per the Codex review's "configurable minimum (default ≥30s)".
fn default_reroll_head_stability_window_secs() -> u64 {
    30
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

/// Default escalation re-fire backoff (1s2q-escalation-dedup, re-introduced
/// by clean replay of PR #470): 1 hour between re-emissions of an
/// identical-context escalation event for the same `(bead_id, reason)`.
fn default_escalation_refire_secs() -> u64 {
    3600
}

impl Config {
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
                source: RoutingSource::Explicit,
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
                source: RoutingSource::GlobalTarget,
            });
        }
        self.derive_routing_for_unseen_repo(repo)
    }

    /// Bead jleechan-87ea / PR #289: derive routing for a `repo` that is NOT
    /// in `self.repos` AND is NOT `self.target_repo` (the only paths main's
    /// `resolve_repo` already handled). Validates the input via
    /// [`is_valid_owner_repo`] (rejects overlength, dot-prefixed, hyphen-edged
    /// strings, etc.), derives an `ao_project` from the last path segment
    /// with the `worldarchitect.ai` → `worldarchitect` special case, and
    /// returns `None` (fails closed) when:
    ///
    /// * the input is not a syntactically valid `owner/repo`, OR
    /// * the derived `ao_project` collides with an explicit `[repos.*]`
    ///   entry for a DIFFERENT repo (the two-direction collision check
    ///   guarding against silent cross-repo routing), OR
    /// * the derived `ao_project` collides with the global target's
    ///   effective `ao_project` (the symmetric collision check).
    ///
    /// On a successful derive, emits an `eprintln!` warn so a derived
    /// dispatch is loud at the daemon's stderr (the durable
    /// `DERIVED_ROUTE_RESOLVED` telemetry is a separate, fail-closed gate
    /// in `dispatch::dispatch_ready`).
    pub fn derive_routing_for_unseen_repo(&self, repo: &str) -> Option<RepoRouting> {
        if !is_valid_owner_repo(repo) {
            return None;
        }
        let mut ao_project = repo.split('/').next_back().unwrap_or(repo).to_string();
        if ao_project == "worldarchitect.ai" {
            ao_project = "worldarchitect".to_string();
        }
        if self.is_ao_project_collision(&ao_project, repo) {
            eprintln!(
                "auto-factory daemon: ao_project collision: derived repo '{}' \
                 resolves to ao_project='{}' which is already claimed by an explicit \
                 [repos.*] entry for a different repo; routing failed closed",
                repo, ao_project
            );
            return None;
        }
        eprintln!(
            "auto-factory daemon: derived routing for unseen repo '{}' → ao_project='{}' \
             (no explicit [repos.\"{}\"] entry); consider adding one to silence this warning",
            repo, ao_project, repo
        );
        Some(RepoRouting {
            ao_project,
            push_remote: "origin".to_string(),
            source: RoutingSource::Derived,
        })
    }

    /// Bead jleechan-87ea / PR #289: returns `true` when `ao_project` is
    /// already claimed by an existing routing — either as an explicit
    /// `[repos."<existing_repo>"].ao_project` for a DIFFERENT repo, or as the
    /// global `target_repo`'s effective `ao_project` when `for_repo` is
    /// neither that global target nor an explicit `[repos]` entry for it.
    /// Two-direction check (cross + global) ensures the daemon cannot
    /// dispatch two different repos into the same AO project.
    pub fn is_ao_project_collision(&self, ao_project: &str, for_repo: &str) -> bool {
        // Direction 1: any explicit [repos.*] entry already claims this
        // ao_project for a DIFFERENT repo.
        for (existing_repo, rc) in &self.repos {
            if existing_repo != for_repo && rc.ao_project == ao_project {
                return true;
            }
        }
        // Direction 2: the global target_repo's effective ao_project (the
        // value resolve_repo returns when for_repo == self.target_repo)
        // already claims this ao_project for a DIFFERENT repo.
        let global_ao = self.global_effective_ao_project();
        if global_ao == ao_project && self.target_repo != for_repo {
            return true;
        }
        false
    }

    /// Bead jleechan-87ea / PR #289: the effective `ao_project` for the
    /// global `target_repo` — explicit `self.ao_project` when set, else
    /// derived from `self.target_repo`'s last path segment with the
    /// `worldarchitect.ai` → `worldarchitect` rewrite. Used by
    /// [`is_ao_project_collision`] so derived repos cannot collide with the
    /// daemon's global target either.
    pub fn global_effective_ao_project(&self) -> String {
        if let Some(p) = self.ao_project.clone() {
            return p;
        }
        let mut project = self
            .target_repo
            .split('/')
            .next_back()
            .unwrap_or(&self.target_repo)
            .to_string();
        if project == "worldarchitect.ai" {
            project = "worldarchitect".to_string();
        }
        project
    }
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    let cfg: Config = toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))?;
    // Bead jleechan-87ea / PR #289: load-time validation gates malformed
    // configs at the source so the daemon cannot silently route into a
    // truncated or syntactically-invalid project. Run before any caller
    // resolves a repo, so a daemon started against a broken daemon.toml
    // fails closed at startup rather than at first dispatch.
    validate_config(&cfg)?;
    Ok(cfg)
}

/// Bead jleechan-87ea / PR #289: run after `toml::from_str` succeeds so the
/// file deserialized cleanly. Rejects:
/// * invalid `target_repo` (not `owner/repo`, overlength, edge-hyphen,
///   leading-dot, etc.),
/// * empty or invalid `[repos.*]` keys,
/// * empty `ao_project` (would derive a useless empty project),
/// * AO project collisions across BOTH directions: explicit `[repos.*]` and
///   the global target's effective `ao_project` must be pairwise unique so
///   `dispatch_ready` can never route two repos into one AO project.
fn validate_config(cfg: &Config) -> Result<(), DaemonError> {
    if !is_valid_owner_repo(&cfg.target_repo) {
        return Err(DaemonError::Config(format!(
            "invalid target_repo '{}': must match owner/repo with owner ≤ {MAX_OWNER_LEN} \
             chars (alphanumeric + hyphen, no leading/trailing hyphen), repo ≤ {MAX_REPO_LEN} \
             chars (alphanumeric + ._-), no leading dot",
            cfg.target_repo
        )));
    }
    if let Some(p) = &cfg.ao_project {
        if p.trim().is_empty() {
            return Err(DaemonError::Config(
                "ao_project is set but empty/whitespace".into(),
            ));
        }
    }
    for (key, rc) in &cfg.repos {
        if !is_valid_owner_repo(key) {
            return Err(DaemonError::Config(format!(
                "invalid [repos.\"{key}\"] key: must match owner/repo with owner ≤ \
                 {MAX_OWNER_LEN} chars (alphanumeric + hyphen, no leading/trailing hyphen), \
                 repo ≤ {MAX_REPO_LEN} chars (alphanumeric + ._-), no leading dot"
            )));
        }
        if rc.ao_project.trim().is_empty() {
            return Err(DaemonError::Config(format!(
                "invalid [repos.\"{key}\"].ao_project: empty"
            )));
        }
        if rc.push_remote.trim().is_empty() {
            return Err(DaemonError::Config(format!(
                "invalid [repos.\"{key}\"].push_remote: empty"
            )));
        }
        if cfg.is_ao_project_collision(&rc.ao_project, key) {
            return Err(DaemonError::Config(format!(
                "[repos.\"{key}\"].ao_project='{}' collides with another routing entry",
                rc.ao_project
            )));
        }
    }
    // Cross-check the global target's effective ao_project against the
    // explicit [repos.*] table — both directions must agree.
    let global_ao = cfg.global_effective_ao_project();
    for (key, rc) in &cfg.repos {
        if rc.ao_project == global_ao && key != &cfg.target_repo {
            return Err(DaemonError::Config(format!(
                "[repos.\"{key}\"].ao_project='{}' collides with the global target_repo's \
                 effective ao_project='{}'",
                rc.ao_project, global_ao
            )));
        }
    }
    Ok(())
}

/// Bead jleechan-87ea / PR #289: validate an `owner/repo` string against
/// GitHub's published length + character rules. `None` for invalid inputs
/// (overlength, edge-hyphen, leading-dot, missing slash, empty parts, etc.).
/// Used by both [`Config::load`] (config-time gating) and
/// [`Config::derive_routing_for_unseen_repo`] (per-bead runtime gating).
pub fn is_valid_owner_repo(s: &str) -> bool {
    validate_owner_repo(s).is_some()
}

pub fn validate_owner_repo(s: &str) -> Option<(String, String)> {
    let (owner, repo) = s.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    if owner.len() > MAX_OWNER_LEN || repo.len() > MAX_REPO_LEN {
        return None;
    }
    if owner.starts_with('-') || owner.ends_with('-') {
        return None;
    }
    if !owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    if repo.starts_with('.') || repo.starts_with('-') {
        return None;
    }
    if !repo
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_example_config() {
        let cfg = load(std::path::Path::new("contracts/daemon.toml.example")).unwrap();
        assert_eq!(cfg.ao_project.as_deref(), Some("dark-factory"));
        assert_eq!(cfg.stage, 1);
        assert_eq!(cfg.max_workers, 30);
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
        // Bead jleechan-rouf (PR #470 clean replay): the escalation
        // re-fire backoff defaults to 1 hour and is config-overridable.
        assert_eq!(cfg.escalation_refire_secs, 3600);
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
        assert!(cfg.repos.is_empty(), "repos table must default to empty when absent");
        assert_eq!(
            cfg.resolve_repo("owner/repo"),
            Some(RepoRouting {
                ao_project: "repo".to_string(),
                push_remote: "origin".to_string(),
                source: RoutingSource::GlobalTarget,
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
                source: RoutingSource::Explicit,
            })
        );
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            Some(RepoRouting {
                ao_project: "dark-factory".to_string(),
                push_remote: "origin".to_string(),
                source: RoutingSource::Explicit,
            })
        );
    }

    #[test]
    fn resolve_repo_derives_for_unmapped_repo() {
        // Bead jleechan-87ea / PR #289: AC #2 — a syntactically valid but
        // unmapped `owner/repo` derives safe routing (last path segment as
        // ao_project, "origin" as push_remote, source=Derived) rather than
        // failing closed at dispatch time. This is the explicit acceptance
        // criterion: a valid `owner/repo` MUST be dispatchable so the daemon
        // does not silently park a bead whose repo is just outside the
        // operator's [repos.*] table. The fail-closed path (None) is reserved
        // for syntactically-invalid repos and AO-project collisions.
        let dir = std::env::temp_dir().join("afd_cfg_test_derived_repo");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("derived.toml");
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
        assert_eq!(
            cfg.resolve_repo("someorg/unrelated-repo"),
            Some(RepoRouting {
                ao_project: "unrelated-repo".to_string(),
                push_remote: "origin".to_string(),
                source: RoutingSource::Derived,
            })
        );
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
                source: RoutingSource::GlobalTarget,
            })
        );
    }

    // Bead jleechan-87ea / PR #289 — net-new tests covering derived routing,
    // collision detection, validation, and load-time gating.

    #[test]
    fn validate_owner_repo_accepts_well_formed() {
        assert!(is_valid_owner_repo("jleechanorg/dark-factory"));
        assert!(is_valid_owner_repo("a/b"));
        assert!(is_valid_owner_repo("foo-bar/baz_quux.d"));
    }

    #[test]
    fn validate_owner_repo_rejects_malformed() {
        // Missing slash.
        assert!(!is_valid_owner_repo("noslash"));
        // Empty parts.
        assert!(!is_valid_owner_repo("/repo"));
        assert!(!is_valid_owner_repo("owner/"));
        // Overlength owner.
        let long_owner = "a".repeat(MAX_OWNER_LEN + 1);
        assert!(!is_valid_owner_repo(&format!("{long_owner}/repo")));
        // Overlength repo.
        let long_repo = "a".repeat(MAX_REPO_LEN + 1);
        assert!(!is_valid_owner_repo(&format!("owner/{long_repo}")));
        // Edge hyphens.
        assert!(!is_valid_owner_repo("-owner/repo"));
        assert!(!is_valid_owner_repo("owner-/repo"));
        assert!(!is_valid_owner_repo("owner/-repo"));
        // Disallowed chars.
        assert!(!is_valid_owner_repo("own er/repo"));
        assert!(!is_valid_owner_repo("owner/re po"));
        assert!(!is_valid_owner_repo("owner/repo!"));
        // Leading dot.
        assert!(!is_valid_owner_repo("owner/.repo"));
    }

    #[test]
    fn derive_routing_for_unseen_repo_returns_some_for_valid_input() {
        let dir = std::env::temp_dir().join("afd_cfg_test_derive_valid");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("derive.toml");
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
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(
            cfg.derive_routing_for_unseen_repo("someorg/cool-thing"),
            Some(RepoRouting {
                ao_project: "cool-thing".to_string(),
                push_remote: "origin".to_string(),
                source: RoutingSource::Derived,
            })
        );
    }

    #[test]
    fn derive_routing_for_unseen_repo_returns_none_for_invalid_input() {
        let dir = std::env::temp_dir().join("afd_cfg_test_derive_invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("derive.toml");
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
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.derive_routing_for_unseen_repo("nope"), None);
        assert_eq!(cfg.derive_routing_for_unseen_repo("/repo"), None);
        assert_eq!(cfg.derive_routing_for_unseen_repo("owner/"), None);
        assert_eq!(
            cfg.derive_routing_for_unseen_repo("owner/.leading-dot"),
            None
        );
    }

    #[test]
    fn derive_routing_for_unseen_repo_returns_none_on_collision_with_explicit_repos() {
        let dir = std::env::temp_dir().join("afd_cfg_test_derive_collision");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("derive.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/dark-factory"
ao_project = "dark-factory-main"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."someorg/other-thing"]
ao_project = "dark-factory"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        // Try to derive "someorg/dark-factory" — its last segment
        // collides with an explicit [repos] entry that claims
        // ao_project="dark-factory" for a DIFFERENT repo. The global
        // ao_project is "dark-factory-main" so no load-time collision.
        assert_eq!(
            cfg.derive_routing_for_unseen_repo("someorg/dark-factory"),
            None
        );
    }

    #[test]
    fn derive_routing_for_unseen_repo_returns_none_on_collision_with_global_target() {
        let dir = std::env::temp_dir().join("afd_cfg_test_derive_global_collision");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("derive.toml");
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
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        // global_ao is "dark-factory"; trying to derive for a DIFFERENT
        // repo whose last segment is also "dark-factory" must fail closed.
        assert_eq!(
            cfg.derive_routing_for_unseen_repo("someorg/dark-factory"),
            None
        );
    }

    #[test]
    fn cross_repo_same_pr_number_resolves_to_distinct_ao_projects() {
        // Bead jleechan-87ea / PR #289 AC #3: the same PR number in two
        // different repos must not collapse into a single AO project.
        let dir = std::env::temp_dir().join("afd_cfg_test_cross_repo_pr");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("cross.toml");
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

[repos."someorg/other-thing"]
ao_project = "other-thing"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        let a = cfg.resolve_repo("someorg/other-thing").unwrap();
        let b = cfg.resolve_repo("jleechanorg/dark-factory").unwrap();
        assert_ne!(a.ao_project, b.ao_project);
        assert_eq!(a.source, RoutingSource::Explicit);
        assert_eq!(b.source, RoutingSource::GlobalTarget);
    }

    #[test]
    fn config_load_rejects_invalid_target_repo() {
        let dir = std::env::temp_dir().join("afd_cfg_test_invalid_target");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "no-slash"
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
        let err = load(&p).unwrap_err();
        assert!(
            err.to_string().contains("invalid target_repo"),
            "expected invalid target_repo error, got: {err}"
        );
    }

    #[test]
    fn config_load_rejects_invalid_repos_key() {
        let dir = std::env::temp_dir().join("afd_cfg_test_invalid_repos_key");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
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

[repos."not-a-valid-key"]
ao_project = "x"
push_remote = "origin"
"#,
        )
        .unwrap();
        let err = load(&p).unwrap_err();
        assert!(
            err.to_string().contains("invalid [repos"),
            "expected invalid [repos] error, got: {err}"
        );
    }

    #[test]
    fn config_load_rejects_empty_ao_project() {
        let dir = std::env::temp_dir().join("afd_cfg_test_empty_ao");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/dark-factory"
ao_project = "   "
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
        let err = load(&p).unwrap_err();
        assert!(
            err.to_string().contains("ao_project"),
            "expected ao_project error, got: {err}"
        );
    }

    #[test]
    fn config_load_rejects_ao_project_collision() {
        let dir = std::env::temp_dir().join("afd_cfg_test_collision");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bad.toml");
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

[repos."jleechanorg/other-thing"]
ao_project = "dark-factory"
push_remote = "origin"
"#,
        )
        .unwrap();
        let err = load(&p).unwrap_err();
        assert!(
            err.to_string().contains("collides"),
            "expected collision error, got: {err}"
        );
    }
}
