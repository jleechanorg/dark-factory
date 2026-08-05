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
    /// Local checkout path of this repo on the daemon host. Used by the
    /// gate-8 vacuous-red-green detector (bead jleechan-sk55) to resolve
    /// PR changed-file paths against the CORRECT working tree when a
    /// bead's routed `repo` differs from `cfg.target_repo` (the daemon's
    /// own checkout). `None` means the routed repo has no local checkout
    /// on this host — the detector MUST surface a structured
    /// `ManifestMissing` rather than silently running against the daemon
    /// cwd (issue #387 r6 follow-up; jleechan-sk55 finding). Optional for
    /// backward compat with every pre-existing `daemon.toml`; absent
    /// entries default to `None` via `#[serde(default)]`.
    #[serde(default)]
    pub local_checkout: Option<std::path::PathBuf>,
}

/// Resolved dispatch routing for a repo — the AO project to spawn into and
/// the git remote a coder must push to. Returned by [`Config::resolve_repo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRouting {
    pub ao_project: String,
    pub push_remote: String,
    /// Local checkout path of this repo on the daemon host (see
    /// [`RepoConfig::local_checkout`]). `None` when no local checkout is
    /// configured for the routed repo.
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
            //
            // jleechan-sk55 (issue #387 r6 follow-up): legacy single-repo
            // configs where `repo == self.target_repo` historically had
            // the daemon run cargo from its own cwd (the daemon's source
            // tree). For the common case where the daemon is a checkout
            // of the SAME repo it gates (i.e. dark-factory self-hosting),
            // that cwd IS the local checkout of `target_repo`. We can't
            // KNOW that without explicit config, so we conservatively
            // populate `local_checkout: None` for the fallback path —
            // operators MUST add an explicit `[repos."<repo>"]` entry with
            // `local_checkout` set to the daemon's cwd path for the
            // gate-8 detector to function. The detector surfaces a
            // structured `ManifestMissing` (Unknown) rather than silently
            // running on the wrong tree when no checkout is configured.
            return Some(RepoRouting {
                ao_project,
                push_remote: "origin".to_string(),
                local_checkout: None,
            });
        }
        None
    }
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))
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
                local_checkout: None,
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

    /// Bead jleechan-sk55: an explicit `[repos.*]` entry with
    /// `local_checkout` set must surface that path through `resolve_repo`,
    /// so the gate-8 detector can find the routed repo's working tree.
    #[test]
    fn resolve_repo_surfaces_local_checkout_for_explicit_repo_entry() {
        let dir = std::env::temp_dir().join("afd_cfg_test_local_checkout");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("local_checkout.toml");
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
local_checkout = "/home/jleechan/projects/worldarchitect.ai"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        let resolved = cfg
            .resolve_repo("jleechanorg/worldarchitect.ai")
            .expect("explicit [repos.*] entry must resolve");
        assert_eq!(resolved.ao_project, "worldarchitect");
        assert_eq!(resolved.push_remote, "worldai");
        assert_eq!(
            resolved.local_checkout.as_deref(),
            Some(std::path::Path::new("/home/jleechan/projects/worldarchitect.ai"))
        );

        // Legacy fallback path still has local_checkout = None: a missing
        // `[repos.*]` entry for `cfg.target_repo` returns the single-repo
        // routing but the operator has not told us where the local
        // checkout lives, so the gate-8 detector MUST surface
        // ManifestMissing (Unknown) rather than silently using cwd.
        let fallback = cfg
            .resolve_repo("jleechanorg/dark-factory")
            .expect("target_repo must resolve via fallback");
        assert_eq!(fallback.local_checkout, None);
    }
}
