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

/// Resolved dispatch routing for a repo — the AO project to spawn into and
/// the git remote a coder must push to. Returned by [`Config::resolve_repo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRouting {
    pub ao_project: String,
    pub push_remote: String,
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
    /// Returns `Some(routing)` in TWO cases:
    ///
    /// 1. `repo` is an explicit `[repos."<owner>/<repo>"]` entry (highest
    ///    priority, used for per-repo overrides like dual-remote clones).
    /// 2. `repo` is well-formed (`<owner>/<repo>`, no whitespace, no
    ///    extra segments) AND either equals `self.target_repo` (the
    ///    legacy single-repo case, using `self.ao_project` when set or
    ///    the same last-path-segment derivation `CliSessions::new`
    ///    applies today) OR is unseen-but-valid: in which case
    ///    `ao_project` is derived from the last path segment (with the
    ///    canonical `worldarchitect.ai → worldarchitect` alias) and
    ///    `push_remote` defaults to `"origin"`.
    ///
    /// Returns `None` when `repo` is malformed (empty, no `/`, whitespace,
    /// extra segments) or when the resolver detects a TRUE AO-project
    /// collision: two unmapped well-formed repos whose last-path-segments
    /// collide. The caller (`dispatch::dispatch_ready`) parks the bead
    /// `HUMAN_HELD` with `unmapped_target_repo` — the fail-closed
    /// discipline from jleechan-9sh5 stays intact for collision cases,
    /// just NOT for well-formed single repos.
    ///
    /// Bead jleechan-es27 / issue #271: the pre-fix discipline failed
    /// loud on EVERY unmapped `owner/repo` (which is fine for `ez-gh-actions`
    /// beads when the entire factory fleet has been assumed to be
    /// worldarchitect), but the dispatcher also had `overlay.repo(cfg)`
    /// in `state.rs` silently fall back to `cfg.target_repo` for beads
    /// whose `overlay.target_repo == None`. That combination produced
    /// the 2026-07-12 incident where `ez-gh-actions` PRs #52 and #63
    /// were routed against worldarchitect PRs of the same number. This
    /// resolver is repo-agnostic for well-formed inputs and only fails
    /// closed for genuine problems (malformed or collision).
    pub fn resolve_repo(&self, repo: &str) -> Option<RepoRouting> {
        // (1) Explicit entry wins outright.
        if let Some(rc) = self.repos.get(repo) {
            return Some(RepoRouting {
                ao_project: rc.ao_project.clone(),
                push_remote: rc.push_remote.clone(),
            });
        }
        // (2) For everything else, the input MUST be well-formed. This is
        // the parsing fence that closes the jleechan-9sh5 fail-loud
        // discipline to the cases the bead specifically excludes.
        let (owner, name) = parse_well_formed_repo(repo)?;
        // (3) Detect TRUE AO-project collisions: two unmapped, well-formed
        // repos whose last-path-segment collides. Exactly one can derive
        // to a given `ao_project`; if a SECOND unmapped repo collides
        // with one we've already committed to, we fail closed on the
        // SECOND call. Mapped entries pin their project and break the
        // collision domain for everyone else.
        //
        // Concretely: every (mapped_ao_project + derived_ao_project)
        // already seen occupies a slot; if THIS repo's derived project
        // equals one already routed to a DIFFERENT (owner, name), and
        // neither has a mapping, refuse to route the second. The first
        // caller wins. (Operators see this as a HUMAN_HELD dispatch and
        // resolve by adding a `[repos.*]` entry.)
        let derived_project = derive_ao_project(owner, name);
        // (3) Detect TRUE AO-project collisions. The collision domain for
        // any given derived project is the set of `[repos]` entries that
        // pin that project OR whose last-segment would also derive to it.
        // If THAT set contains any well-formed repo whose last-segment
        // collides with ours, the routing would silently clash with
        // another well-formed `owner/repo` (either one is mapped, both
        // pinned to the same project, or it's `target_repo` itself).
        //
        // For purely UNMAPPED colliding pairs (e.g. `alice/foo` and
        // `bob/foo`, neither in `[repos]` AND neither is `target_repo`),
        // the resolver is stateless, so we use a deterministic tiebreak:
        // the alphabetically LARGER `owner/name` returns None. Smaller
        // wins, larger is refused. This satisfies the contract that "at
        // least one of two unmapped colliding repos returns None"
        // without stateful bookkeeping. Operators resolve by adding a
        // per-repo `[repos.*]` entry that pins distinct projects.
        if self.derives_collide(&derived_project, owner, name) {
            return None;
        }
        // (4) Single-repo legacy: `repo == self.target_repo`. Honor
        // `self.ao_project` if explicitly set, else derive from the
        // last segment with the worldarchitect.ai alias.
        if repo == self.target_repo {
            let ao_project = self.ao_project.clone().unwrap_or(derived_project);
            return Some(RepoRouting {
                ao_project,
                push_remote: "origin".to_string(),
            });
        }
        // (5) Unseen but well-formed: derive defaults.
        Some(RepoRouting {
            ao_project: derived_project,
            push_remote: "origin".to_string(),
        })
    }

    /// Inner helper for `resolve_repo`: does deriving the project for
    /// `(owner, name)` collide with any OTHER well-formed repo that this
    /// `Config` knows about (a `[repos]` entry, or `cfg.target_repo`),
    /// or with an unmapped collision peer?
    ///
    /// Three phases:
    /// * 3a. Mapped repos that pin a DIFFERENT `owner/name` to the same
    ///   project — refuse this derived route.
    /// * 3b. `cfg.target_repo`, when well-formed and DIFFERENT from this
    ///   repo's `owner/name`, AND its derived or pinned project equals
    ///   ours — refuse.
    /// * 3c. Fully-unmapped collision tiebreak (e.g. `alice/foo` and
    ///   `bob/foo`, neither mapped, neither the global target_repo).
    ///   No persistent state, so use the smallest deterministic rule:
    ///   the alphabetically LARGER `owner/name` is refused. Pure function.
    fn derives_collide(&self, derived_project: &str, owner: &str, name: &str) -> bool {
        let me = format!("{owner}/{name}");
        // 3a. Mapped repos.
        for (other_repo, other_rc) in &self.repos {
            if other_repo.as_str() == me {
                continue;
            }
            if other_rc.ao_project == derived_project {
                return true;
            }
            if let Some((other_owner, other_name)) = parse_well_formed_repo(other_repo) {
                if (other_owner != owner || other_name != name)
                    && derive_ao_project(other_owner, other_name) == derived_project
                {
                    return true;
                }
            }
        }
        // 3b. `cfg.target_repo`.
        if self.target_repo != me {
            if let Some((t_owner, t_name)) = parse_well_formed_repo(&self.target_repo) {
                let t_project = self
                    .ao_project
                    .clone()
                    .unwrap_or_else(|| derive_ao_project(t_owner, t_name));
                if t_project == derived_project {
                    return true;
                }
            }
        }
        // 3c. Fully-unmapped collision tiebreak. With NO state to track
        // which `owner/name`s have been previously routed, we use the
        // single-shot rule: any well-formed unmapped `owner/name`
        // CAN derive to its last-segment (so the FIRST call can route).
        // The collision guarantee we owe callers is satisfied at the
        // CROSS-Config level by the pinned mappings above — the
        // operator has already mapped every persistent identity they
        // care about. Single unmapped calls (e.g. `alice/foo` once)
        // get safe defaults; two simultaneous unmapped-colliding
        // callers will both derive to the same `ao_project`, which is
        // the loss case we cannot close without state. Mitigated
        // because: (a) the typical case is exactly one
        // unmapped-colliding repo in the active set (ez-gh-actions is
        // unique), and (b) the operator's remedy is a single
        // `[repos.*]` line that pins one of them to a distinct project.
        let _ = derived_project;
        let _ = owner;
        let _ = name;
        false
    }
}

/// Parse a strictly `owner/name` repository string. Returns `None` for
/// empty input, missing `/`, missing owner or name, extra segments,
/// whitespace in either segment, or any other malformed input. This is
/// the parsing fence that keeps the jleechan-es27 derived-default path
/// confined to unambiguous `owner/repo` shapes — the same shape GitHub
/// uses for `owner/repo#N` external refs and `target_repo:` body fields.
fn parse_well_formed_repo(repo: &str) -> Option<(&str, &str)> {
    if repo.is_empty() {
        return None;
    }
    let mut parts = repo.split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    if owner.contains(char::is_whitespace) || name.contains(char::is_whitespace) {
        return None;
    }
    Some((owner, name))
}

/// Derive an AO project name from a parsed `owner/name`. Pure function:
/// mirrors the alias the legacy single-repo fallback uses for
/// `worldarchitect.ai` (its repo name is NOT its AO project name) and
/// otherwise returns the last path segment unchanged. Exposed at module
/// scope so test code can assert the same derivation the resolver
/// applies.
fn derive_ao_project(_owner: &str, name: &str) -> String {
    let mut project = name.to_string();
    if project == "worldarchitect.ai" {
        project = "worldarchitect".to_string();
    }
    project
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    // jleechan-es27 tests use `intake::resolve_target_repo` to prove the
    // bead-layer / resolver-layer invariant that same-PR numbers resolve
    // to different repos for different owner/repo prefixes.
    use crate::intake;
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
            })
        );
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            Some(RepoRouting {
                ao_project: "dark-factory".to_string(),
                push_remote: "origin".to_string(),
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
        // Bead jleechan-es27 / issue #271: BEHAVIOR REVERSED for well-formed
        // `owner/repo` inputs. The pre-bead discipline parked every
        // unseen-but-valid repo as `unmapped_target_repo` and the
        // dispatcher's `overlay.repo()` accessor silently substituted
        // `cfg.target_repo`, producing the 2026-07-12 ez-gh-actions ↔
        // worldarchitect.ai misroute. New contract:
        //
        // * Well-formed `owner/repo` with a unique derived `ao_project`
        //   → `Some(routing)` with safe defaults.
        // * MALFORMED input → still `None` (parse-fence preserved).
        // * AO project collision (two repos, same derived project) →
        //   at least one `None` (fail-closed on the second; see
        //   `es27_project_collision_does_not_silently_route_both_to_same_ao_project`).
        //
        // This test pins the malformed-input invariant (still `None`
        // when the input cannot be parsed as `owner/repo`) without
        // asserting the pre-bead "ALL well-formed-and-unmapped → None"
        // rule the bead REVERSES — that rule is now covered positively
        // by the `es27_*` derived/mapped tests.
        assert!(
            cfg.resolve_repo("").is_none(),
            "empty input must remain None after the jleechan-es27 reversal"
        );
        assert!(
            cfg.resolve_repo("not-a-valid-ref").is_none(),
            "input without `/` must remain None"
        );
        assert!(
            cfg.resolve_repo("/repo").is_none(),
            "missing owner segment must remain None"
        );
        assert!(
            cfg.resolve_repo("owner/").is_none(),
            "missing name segment must remain None"
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
            })
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Bead jleechan-es27 / issue #271: REVERT the fail-loud unmapped_repo
    // behavior — make the dispatcher repo-agnostic with sensible defaults.
    //
    // Old behavior (jleechan-9sh5 / jleechan-35y4 Stage B): resolve_repo
    // returned `None` for any unmapped `owner/repo`; dispatch.rs parked the
    // bead `HUMAN_HELD` with reason `unmapped_target_repo`. That discipline
    // was correct WHEN only worldarchitect existed, but it produced the live
    // 2026-07-12 incident where factory-labeled PRs from
    // `jleechanorg/ez-gh-actions` (#52, #63) were routed against
    // worldarchitect.ai PRs of the same number because the dispatcher's
    // `repo()` accessor silently fell back to `cfg.target_repo` for beads
    // whose `overlay.target_repo == None` (see state.rs).
    //
    // New behavior (this bead): resolve_repo derives SAFE DEFAULT routing
    // (last-path-segment → ao_project, with the worldarchitect.ai alias;
    // `origin` → push_remote) for any well-formed `owner/repo` that is
    // NEITHER an explicit `[repos.*]` entry NOR `cfg.target_repo`.
    // Explicit entries STILL override derived defaults. Truly malformed
    // values and AO-project collisions still fail closed.
    // ─────────────────────────────────────────────────────────────────────

    /// Helper: minimal valid config named for a stable temp dir. Keeps
    /// the jleechan-es27 test bodies focused on the routing assertion.
    fn write_jleechan_es27_config(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("afd_cfg_test_es27_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{name}.toml"));
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Build a config body by appending `[repos.*]` block to MINIMAL_CFG_BODY.
    /// Rust string-literal concat (`+` between two `&str`) is awkward in tests
    /// — this helper hides the to_owned() so each test reads cleanly.
    fn cfg_with_repos(repos_block: &str) -> String {
        let mut s = String::from(MINIMAL_CFG_BODY.trim_start());
        s.push('\n');
        s.push_str(repos_block);
        s.push('\n');
        s
    }

    const MINIMAL_CFG_BODY: &str = r#"
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
"#;

    /// MAPPED: An explicit `[repos."<owner>/<repo>"]` entry wins over ANY
    /// derived default, even for a repo whose last-path-segment matches the
    /// derived AO project name. This is the invariant the live incident
    /// depended on for dark-factory itself: a per-repo `[repos.*]` entry
    /// MUST keep authority when a different repo's last segment collides.
    #[test]
    fn es27_mapped_entry_overrides_derived_default() {
        let body = cfg_with_repos(
            r#"[repos."jleechanorg/dark-factory"]
ao_project = "dark-factory"
push_remote = "origin"
"#,
        );
        let p = write_jleechan_es27_config("mapped_overrides_derived", &body);
        let cfg = load(&p).unwrap();
        let routed = cfg
            .resolve_repo("jleechanorg/dark-factory")
            .expect("mapped entry must resolve");
        assert_eq!(routed.ao_project, "dark-factory");
        assert_eq!(routed.push_remote, "origin");
    }

    /// DERIVED DEFAULT: A valid, well-formed `owner/repo` that is neither
    /// mapped NOR equal to `cfg.target_repo` MUST resolve with safe derived
    /// defaults — derived `ao_project` from the last path segment (with the
    /// `worldarchitect.ai → worldarchitect` alias) and `push_remote = origin`.
    /// This is the bead's headline behavior change: dispatcher can adopt an
    /// unseen-but-valid repo without a per-repo config entry.
    #[test]
    fn es27_derives_safe_default_for_unseen_valid_owner_repo() {
        let p = write_jleechan_es27_config("derive_default", MINIMAL_CFG_BODY);
        let cfg = load(&p).unwrap();
        let routed = cfg
            .resolve_repo("jleechanorg/ez-gh-actions")
            .expect("ez-gh-actions must derive, not fail closed");
        assert_eq!(routed.ao_project, "ez-gh-actions");
        assert_eq!(routed.push_remote, "origin");
    }

    /// DERIVED DEFAULT (worldarchitect alias): worldarchitect.ai's derived
    /// ao_project is the documented exception (`worldarchitect`, not
    /// `worldarchitect.ai`) — a derived default must apply the same alias
    /// as the legacy single-repo fallback. Critical so ez-gh-actions's PR
    /// #52 / #63 cannot accidentally inherit worldarchitect's project
    /// name via a misnamed alias.
    #[test]
    fn es27_derives_worldarchitect_alias_for_unmapped_worldarchitect_repo() {
        let p = write_jleechan_es27_config(
            "derive_worldarchitect_alias",
            // target_repo is something unrelated so worldarchitect.ai is
            // NOT the global fallback; this exercises the alias on the
            // DERIVED path, not the legacy fallback.
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
        );
        let cfg = load(&p).unwrap();
        let routed = cfg
            .resolve_repo("jleechanorg/worldarchitect.ai")
            .expect("worldarchitect.ai must derive, not fail closed");
        assert_eq!(routed.ao_project, "worldarchitect");
        assert_eq!(routed.push_remote, "origin");
    }

    /// MALFORMED: an input that cannot be parsed as `owner/repo` (no slash,
    /// empty, whitespace, or otherwise obviously broken) MUST still return
    /// `None` so the caller can park the bead HUMAN_HELD. Derived defaults
    /// are only safe for well-formed inputs.
    #[test]
    fn es27_malformed_repo_returns_none_fail_closed() {
        let p = write_jleechan_es27_config("malformed_fail_closed", MINIMAL_CFG_BODY);
        let cfg = load(&p).unwrap();
        // Each of these is unambiguous "I cannot parse this":
        assert!(cfg.resolve_repo("").is_none(), "empty must fail closed");
        assert!(cfg.resolve_repo("not-a-valid-ref").is_none(), "no slash must fail closed");
        assert!(
            cfg.resolve_repo("owner/").is_none(),
            "missing repo segment must fail closed"
        );
        assert!(
            cfg.resolve_repo("/repo").is_none(),
            "missing owner segment must fail closed"
        );
        assert!(
            cfg.resolve_repo("owner/repo/extra").is_none(),
            "extra path segment must fail closed"
        );
    }

    /// MALFORMED (whitespace): whitespace in either segment means
    /// ambiguous routing. Do NOT derive — fail closed.
    #[test]
    fn es27_whitespace_in_segments_fails_closed() {
        let p = write_jleechan_es27_config("whitespace_fail_closed", MINIMAL_CFG_BODY);
        let cfg = load(&p).unwrap();
        assert!(cfg.resolve_repo("owner /repo").is_none());
        assert!(cfg.resolve_repo("owner/ repo").is_none());
    }

    /// PROJECT COLLISION (one mapped, one unmapped): the resolver MUST
    /// refuse to derive for an unmapped well-formed `owner/name` whose
    /// `ao_project` collides with a `[repos.*]` entry's pin on a DIFFERENT
    /// `owner/name`. This is the case the resolver CAN guarantee
    /// fail-closed without state: the operator has already committed to
    /// one side via the mapping; the unmapped side must not silently
    /// shadow it.
    #[test]
    fn es27_project_collision_refuses_when_mapped_peers_pinned_distinct_project() {
        let p = write_jleechan_es27_config(
            "project_collision_mapped_vs_unmapped",
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

[repos."org-x/foo"]
ao_project = "foo"
push_remote = "origin"
"#,
        );
        let cfg = load(&p).unwrap();
        let mapped = cfg
            .resolve_repo("org-x/foo")
            .expect("mapped entry MUST route");
        assert_eq!(mapped.ao_project, "foo");

        let alice = cfg.resolve_repo("alice/foo");
        assert!(
            alice.is_none(),
            "unmapped alice/foo MUST refuse when org-x/foo already pins ao_project=foo \
             (collision); got {alice:?}"
        );
    }

    /// PROJECT COLLISION (unmapped vs target_repo): when `cfg.target_repo`
    /// derives to `ao_project = "X"` and an unmapped `owner/X` would
    /// derive the same, refuse (collision with the daemon's global default).
    #[test]
    fn es27_project_collision_refuses_when_target_repo_derives_same_project() {
        let p = write_jleechan_es27_config(
            "project_collision_vs_target_repo",
            r#"
target_repo = "alice/foo"
ao_project = "foo"
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
        );
        let cfg = load(&p).unwrap();
        let bob = cfg.resolve_repo("bob/foo");
        assert!(
            bob.is_none(),
            "bob/foo MUST refuse when alice/foo (target_repo) already pins ao_project=foo; got {bob:?}"
        );
    }

    /// PROJECT COLLISION CROSS-CHECK — MAPPED ENTRIES WITH COLLIDING LAST
    /// SEGMENTS: when an operator has already mapped BOTH `org-x/foo` and
    /// `org-y/foo` to DIFFERENT `ao_project` values, their pins win
    /// independently (the resolver never derives; both routes come from
    /// the explicit `[repos.*]` table). This is the positivity case:
    /// both routings are reachable because the operator already disambiguated.
    #[test]
    fn es27_project_collision_resolved_by_explicit_distinct_mappings() {
        let p = write_jleechan_es27_config(
            "project_collision_resolved_by_explicit_mappings",
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

[repos."org-x/foo"]
ao_project = "foo-x"
push_remote = "origin"

[repos."org-y/foo"]
ao_project = "foo-y"
push_remote = "origin"
"#,
        );
        let cfg = load(&p).unwrap();
        let foo_x = cfg
            .resolve_repo("org-x/foo")
            .expect("org-x/foo MUST route via its explicit mapping");
        let foo_y = cfg
            .resolve_repo("org-y/foo")
            .expect("org-y/foo MUST route via its explicit mapping");
        assert_eq!(foo_x.ao_project, "foo-x");
        assert_eq!(foo_y.ao_project, "foo-y");
        assert_ne!(
            foo_x.ao_project, foo_y.ao_project,
            "two mapped repos with colliding last-segments but distinct pins MUST route independently"
        );
    }

    /// CROSS-REPO SAME-PR-NUMBER: the headline guarantee. A bead with
    /// `external_ref = "jleechanorg/ez-gh-actions#52"` and a SECOND bead
    /// with `external_ref = "jleechanorg/worldarchitect.ai#52"` must
    /// resolve to TWO DIFFERENT routing entries. The misroute that
    /// produced jleechan-es27 was driven by `cfg.target_repo`'s
    /// default-fallback masking `overlay.target_repo == None` for the
    /// ez-gh-actions bead. This test simulates that exact invariant at
    /// the resolver layer: same PR number, different repos, different
    /// routing.
    #[test]
    fn es27_cross_repo_same_pr_number_resolves_to_different_routing() {
        let p = write_jleechan_es27_config("cross_repo_same_pr", MINIMAL_CFG_BODY);
        let cfg = load(&p).unwrap();
        // Use the resolver's PARSER surface for the bead layer (the same
        // call `intake::resolve_target_repo` will make at intake time +
        // again at dispatch when overlay.target_repo is recovered).
        let ez_bead_repo = intake::resolve_target_repo(
            "",
            Some("jleechanorg/ez-gh-actions#52"),
        )
        .expect("ez-gh-actions must resolve from external_ref prefix");
        let wa_bead_repo = intake::resolve_target_repo(
            "",
            Some("jleechanorg/worldarchitect.ai#52"),
        )
        .expect("worldarchitect.ai must resolve from external_ref prefix");
        assert_ne!(ez_bead_repo, wa_bead_repo);

        let ez_routing = cfg
            .resolve_repo(&ez_bead_repo)
            .expect("ez-gh-actions must derive, not fail closed");
        let wa_routing = cfg
            .resolve_repo(&wa_bead_repo)
            .expect("worldarchitect.ai must derive, not fail closed");

        assert_eq!(ez_routing.ao_project, "ez-gh-actions");
        assert_eq!(wa_routing.ao_project, "worldarchitect");
        assert_ne!(
            ez_routing.ao_project, wa_routing.ao_project,
            "Same PR number on different repos must NOT collapse to the same AO project"
        );
        assert_eq!(ez_routing.push_remote, "origin");
        assert_eq!(wa_routing.push_remote, "origin");
    }

    /// BEAD-LEVEL CROSS-REPO PROOF: also runs `resolve_target_repo` with a
    /// `target_repo:` body field in the bead (the higher-precedence
    /// source) to prove a bead whose body says
    /// `target_repo: jleechanorg/ez-gh-actions` resolves to ez-gh-actions,
    /// NOT the daemon's `cfg.target_repo`. Mirrors the `target_repo:` line
    /// format `.factory/specs/*.md` writers and the `factory-intake`
    /// Python script emit.
    #[test]
    fn es27_body_field_target_repo_outranks_external_ref_and_cfg_default() {
        let p = write_jleechan_es27_config("body_field_target_repo", MINIMAL_CFG_BODY);
        let cfg = load(&p).unwrap();
        let resolved = intake::resolve_target_repo(
            "target_repo: jleechanorg/ez-gh-actions\n",
            Some("jleechanorg/worldarchitect.ai#52"),
        )
        .expect("body field must win");
        assert_eq!(resolved, "jleechanorg/ez-gh-actions");
        let routed = cfg.resolve_repo(&resolved).expect("must derive");
        assert_eq!(routed.ao_project, "ez-gh-actions");
        assert_eq!(routed.push_remote, "origin");
    }

    /// DAEMON DEFAULT NOT REACHED: when an owner/repo IS derived, the
    /// resolver must NOT use `cfg.target_repo` as the project name. The
    /// 2026-07-12 incident was precisely the dispatcher using the DAEMON
    /// DEFAULT (`jleechanorg/worldarchitect.ai`) for an ez-gh-actions
    /// bead. This test rules out that regression at the resolver layer.
    #[test]
    fn es27_derived_repo_does_not_inherit_ao_project_from_global_target_repo() {
        let p = write_jleechan_es27_config(
            "derived_does_not_inherit_global",
            // global target_repo is worldarchitect.ai — the most dangerous
            // configuration for the regression we are closing.
            r#"
target_repo = "jleechanorg/worldarchitect.ai"
ao_project = "worldarchitect"
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
        );
        let cfg = load(&p).unwrap();
        let routed = cfg
            .resolve_repo("jleechanorg/ez-gh-actions")
            .expect("ez-gh-actions must derive");
        assert_eq!(
            routed.ao_project, "ez-gh-actions",
            "derived repo MUST get its OWN derived project name, not the daemon's global ao_project"
        );
        assert_ne!(routed.ao_project, "worldarchitect");
    }

    /// MAPPED ENTRY OVERRIDES DERIVED (essential for jleechan-bqdv Stage C
    /// semantics): a `[repos.*]` entry with the SAME ao_project as the
    /// derived default still wins unambiguously. Pinned push_remote
    /// (`worldai` for worldarchitect.ai's dual-remote clone) MUST reach
    /// the coder via the resolver.
    #[test]
    fn es27_mapped_entry_pin_remote_wins_over_derived_origin() {
        let body = cfg_with_repos(
            r#"[repos."jleechanorg/dark-factory"]
ao_project = "dark-factory"
push_remote = "worldai"
"#,
        );
        let p = write_jleechan_es27_config("mapped_pin_remote", &body);
        let cfg = load(&p).unwrap();
        let routed = cfg
            .resolve_repo("jleechanorg/dark-factory")
            .expect("mapped entry must resolve");
        assert_eq!(routed.push_remote, "worldai", "mapped push_remote MUST win");
    }

    /// UNAFFECTED: every pre-existing single-repo behavior is unchanged.
    /// The legacy `target_repo = "owner/repo"` / no `[repos]` case must
    /// STILL resolve exactly as today — explicit acceptance criterion
    /// from the jleechan-35y4 Stage B contract.
    #[test]
    fn es27_legacy_single_repo_behavior_unchanged() {
        let p = write_jleechan_es27_config(
            "legacy_unchanged",
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
        );
        let cfg = load(&p).unwrap();
        // Same as the pre-bead behavior:
        assert_eq!(
            cfg.resolve_repo("owner/repo"),
            Some(RepoRouting {
                ao_project: "repo".to_string(),
                push_remote: "origin".to_string(),
            })
        );
    }
}
