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

impl Config {
    /// Resolve dispatch routing for `repo` (`overlay.repo(self)`'s output).
    ///
    /// Precedence (bead jleechan-dljf, issue #271):
    /// 1. Explicit `[repos."<repo>"]` entry → use that entry's
    ///    `ao_project`/`push_remote` verbatim.
    /// 2. `repo == self.target_repo` → use global config's `ao_project`
    ///    (or the same last-path-segment derivation `CliSessions::new`
    ///    already applies, with `push_remote` defaulting to `"origin"`).
    /// 3. Valid `owner/repo` string (contains exactly one `/`) →
    ///    derive safe defaults: `ao_project` = last path segment (with the
    ///    existing `worldarchitect.ai` → `worldarchitect` special case),
    ///    `push_remote` = `"origin"`. No `[repos]` entry required —
    ///    the factory must work in any repo without per-repo config
    ///    (acceptance criterion: "do not require per-repo config").
    /// 4. Collision check: if the derived `ao_project` matches an
    ///    existing explicit `[repos]` entry's `ao_project` for a
    ///    DIFFERENT repo, fail closed — that is a true AO project
    ///    collision (two repos trying to route to the same AO project).
    /// 5. Malformed repo (no `/`, or empty segments) → `None` (fail
    ///    closed — the caller parks `HUMAN_HELD` with reason
    ///    `unmapped_target_repo`).
    ///
    /// Non-goal: dual-remote repos (worldarchitect.ai's
    /// `origin≠worldai`) still need an explicit `[repos.*]` entry
    /// with the correct `push_remote`. This keeps the "require per-repo
    /// config" bar at one explicit criteria: any single-remote repo
    /// (the common case) just works; only repos with non-standard
    /// remotes or non-standard AO project names need entries.
    pub fn resolve_repo(&self, repo: &str) -> Option<RepoRouting> {
        if let Some(rc) = self.repos.get(repo) {
            return Some(RepoRouting {
                ao_project: rc.ao_project.clone(),
                push_remote: rc.push_remote.clone(),
            });
        }
        if repo == self.target_repo {
            let ao_project = self.ao_project.clone().unwrap_or_else(|| {
                let mut project = repo.split('/').next_back().unwrap_or(repo).to_string();
                if project == "worldarchitect.ai" {
                    project = "worldarchitect".to_string();
                }
                project
            });
            return Some(RepoRouting {
                ao_project,
                push_remote: "origin".to_string(),
            });
        }
        self.derive_routing_for_unseen_repo(repo)
    }

    /// Derive safe dispatch routing for a valid `owner/repo` string that is
    /// neither an explicit `[repos.*]` entry nor the global `target_repo`.
    /// Returns `None` (fail closed) only for malformed repos or true
    /// AO-project collisions (bead jleechan-dljf, issue #271).
    fn derive_routing_for_unseen_repo(&self, repo: &str) -> Option<RepoRouting> {
        let mong = validate_owner_repo(repo)?;

        let ao_project = if mong.name == "worldarchitect.ai" {
            "worldarchitect".to_string()
        } else {
            mong.name
        };

        if self.is_ao_project_collision(&ao_project, repo) {
            return None;
        }

        Some(RepoRouting {
            ao_project,
            push_remote: "origin".to_string(),
        })
    }

    /// Returns true if `ao_project` is already claimed by an explicit
    /// `[repos.*]` entry for a repo OTHER than `repo` (a true AO project
    /// collision — two different repos mapping to the same AO project).
    fn is_ao_project_collision(&self, ao_project: &str, for_repo: &str) -> bool {
        self.repos.iter().any(|(entry_repo, rc)| {
            entry_repo != for_repo && rc.ao_project == ao_project
        })
    }
}

/// Parsed owner/repo from a valid `<owner>/<repo>` string.
struct OwnerRepo {
    name: String,
}

/// Validate that `s` is a well-formed `owner/repo` string with exactly one
/// `/` and non-empty segments on both sides. Returns `None` for malformed
/// input — the caller must fail closed.
fn validate_owner_repo(s: &str) -> Option<OwnerRepo> {
    let mut parts = s.splitn(2, '/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(OwnerRepo {
        name: name.to_string(),
    })
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

    /// jleechan-dljf (issue #271): for a valid but unseen repo, derive safe
    /// defaults instead of returning `None`. The old test that expected
    /// `None` for "someorg/unrelated-repo" is replaced by this one, which
    /// verifies the derived-default behavior.
    #[test]
    fn resolve_repo_derives_safe_defaults_for_unseen_valid_repo() {
        let dir = std::env::temp_dir().join("afd_cfg_test_unseen_valid");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("unseen.toml");
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
            cfg.resolve_repo("jleechanorg/ez-gh-actions"),
            Some(RepoRouting {
                ao_project: "ez-gh-actions".to_string(),
                push_remote: "origin".to_string(),
            }),
            "unseen valid repo must derive safe defaults: ao_project=last-segment, push_remote=origin"
        );
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            Some(RepoRouting {
                ao_project: "dark-factory".to_string(),
                push_remote: "origin".to_string(),
            }),
            "global target_repo also resolves (even without [repos] entry)"
        );
    }

    /// jleechan-dljf (issue #271): a malformed repo string (no `/`
    /// separator) must still return `None` (fail closed). The daemon must
    /// never guess an AO project from unstructured strings.
    #[test]
    fn resolve_repo_returns_none_for_malformed_repo() {
        let dir = std::env::temp_dir().join("afd_cfg_test_malformed");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("malformed.toml");
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
        assert_eq!(cfg.resolve_repo("just-a-string-with-no-slash"), None);
        assert_eq!(cfg.resolve_repo(""), None);
        assert_eq!(cfg.resolve_repo("onlyslash/"), None);
        assert_eq!(cfg.resolve_repo("/onlyslash"), None);
    }

    /// jleechan-dljf (issue #271): an explicit `[repos]` entry must
    /// override the derived default. The derived default for
    /// `jleechanorg/ez-gh-actions` would be `ao_project="ez-gh-actions"`,
    /// `push_remote="origin"`, but an explicit entry changes both fields.
    #[test]
    fn resolve_repo_explicit_entry_overrides_derived_default() {
        let dir = std::env::temp_dir().join("afd_cfg_test_override_derived");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("override_derived.toml");
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

[repos."jleechanorg/ez-gh-actions"]
ao_project = "ez-gh-actions-custom"
push_remote = "ezremote"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(
            cfg.resolve_repo("jleechanorg/ez-gh-actions"),
            Some(RepoRouting {
                ao_project: "ez-gh-actions-custom".to_string(),
                push_remote: "ezremote".to_string(),
            }),
            "explicit [repos] entry must override derived defaults"
        );
    }

    /// jleechan-dljf (issue #271): true AO-project collision — the derived
    /// `ao_project` for one repo matches an EXPLICIT `[repos]` entry's
    /// `ao_project` for a DIFFERENT repo. Must fail closed (None) rather
    /// than silently routing two repos to the same AO project.
    #[test]
    fn resolve_repo_fails_for_ao_project_collision() {
        let dir = std::env::temp_dir().join("afd_cfg_test_ao_collision");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ao_collision.toml");
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

[repos."jleechanorg/foo"]
ao_project = "colliding-project"
push_remote = "origin"

[repos."jleechanorg/bar"]
ao_project = "colliding-project"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        // Explicit entries themselves resolve fine (they're explicitly configured).
        assert!(cfg.resolve_repo("jleechanorg/foo").is_some());
        assert!(cfg.resolve_repo("jleechanorg/bar").is_some());
        // But deriving defaults for a repo whose ao_project would collide
        // with an explicit entry for a different repo must fail.
        assert_eq!(
            cfg.resolve_repo("jleechanorg/colliding-project"),
            None,
            "derived ao_project 'colliding-project' collides with explicit [repos.'jleechanorg/foo'].ao_project"
        );
    }

    /// jleechan-dljf (issue #271): unseen repo that happens to share the
    /// same last-segment as an explicit entry's ao_project BUT the derived
    /// repo name IS the entry's key (not a different repo) — no collision,
    /// should resolve via explicit entry normally.
    #[test]
    fn resolve_repo_self_mapping_is_not_a_collision() {
        let dir = std::env::temp_dir().join("afd_cfg_test_self_map");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("self_map.toml");
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

[repos."jleechanorg/ez-gh-actions"]
ao_project = "ez-gh-actions"
push_remote = "myremote"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        // Explicit entry resolves with its configured values (not derived).
        assert_eq!(
            cfg.resolve_repo("jleechanorg/ez-gh-actions"),
            Some(RepoRouting {
                ao_project: "ez-gh-actions".to_string(),
                push_remote: "myremote".to_string(),
            }),
            "explicit entry where ao_project matches last-segment: no collision, uses entry values"
        );
        // An unseen repo whose derived ao_project='ez-gh-actions' would
        // collide with the explicit entry above — must fail.
        assert_eq!(
            cfg.resolve_repo("otherorg/ez-gh-actions"),
            None,
            "derived ao_project 'ez-gh-actions' collides with explicit [repos.'jleechanorg/ez-gh-actions'] entry"
        );
    }

    #[test]
    fn resolve_repo_falls_back_to_worldarchitect_project_derivation_when_ao_project_unset() {
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

    /// jleechan-dljf (issue #271): `worldarchitect.ai` derivation from
    /// an unseen repo (as target_repo, but no explicit [repos] entry).
    #[test]
    fn resolve_repo_derives_worldarchitect_for_worldarchitect_ai_name() {
        let dir = std::env::temp_dir().join("afd_cfg_test_wa_derived");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("wa_derived.toml");
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
        // worldarchitect.ai is neither the target_repo nor in [repos] —
        // it's an unseen repo, so it falls into the derive path, which must
        // still apply the worldarchitect.ai → worldarchitect special case.
        assert_eq!(
            cfg.resolve_repo("jleechanorg/worldarchitect.ai"),
            Some(RepoRouting {
                ao_project: "worldarchitect".to_string(),
                push_remote: "origin".to_string(),
            }),
            "worldarchitect.ai unseen repo derivation must preserve the worldarchitect special case"
        );
    }
}
