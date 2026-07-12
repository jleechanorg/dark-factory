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
            if self.is_ao_project_collision(&ao_project, repo) {
                eprintln!(
                    "auto-factory daemon: ao_project collision: global target_repo '{}' \
                     resolves to ao_project='{}' which is already claimed by an explicit \
                     [repos.*] entry for a different repo; routing failed closed",
                    repo, ao_project
                );
                return None;
            }
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
            eprintln!(
                "auto-factory daemon: ao_project collision: derived ao_project='{}' \
                 for repo '{}' conflicts with an explicit [repos.*] entry for a \
                 different repo; routing failed closed",
                ao_project, repo
            );
            return None;
        }

        eprintln!(
            "auto-factory daemon: derived routing for '{}' -> ao_project='{}', push_remote='origin' \
             (no explicit [repos.\"{}\"] entry)",
            repo, ao_project, repo
        );

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
///
/// jleechan-dljf (issue #271): also rejects whitespace, control characters,
/// and chars not allowed in GitHub owner/repo names. Valid GitHub codepoints:
/// alphanumeric, hyphen, underscore, and (repo only) dot.
fn validate_owner_repo(s: &str) -> Option<OwnerRepo> {
    if s.len() != s.trim().len() {
        return None;
    }
    let mut parts = s.splitn(2, '/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    if !is_valid_github_name_segment(owner) || !is_valid_github_name_segment(name) {
        return None;
    }
    Some(OwnerRepo {
        name: name.to_string(),
    })
}

fn is_valid_github_name_segment(s: &str) -> bool {
    s.chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
    })
}

/// Public boolean check — `true` when `s` is a well-formed `owner/repo`
/// string (bead jleechan-dljf, issue #271). Used by `intake::resolve_target_repo`
/// to reject malformed results from body field or external_ref prefix.
pub fn is_valid_owner_repo(s: &str) -> bool {
    validate_owner_repo(s).is_some()
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    let cfg: Config =
        toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))?;
    // jleechan-dljf (issue #271): two different [repos] entries with the
    // same ao_project is a configuration error — reject at load time.
    let mut seen_ao_projects: std::collections::HashMap<&str, &str> =
        std::collections::HashMap::new();
    for (repo, rc) in &cfg.repos {
        if let Some(existing_repo) = seen_ao_projects.get(rc.ao_project.as_str()) {
            return Err(DaemonError::Config(format!(
                "explicit-explicit AO project collision: [repos.\"{}\"] and \
                 [repos.\"{}\"] both claim ao_project=\"{}\" — each repo must \
                 route to a distinct AO project",
                existing_repo, repo, rc.ao_project
            )));
        }
        seen_ao_projects.insert(&rc.ao_project, repo);
    }
    Ok(cfg)
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
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        // Explicit entry resolves fine.
        assert!(cfg.resolve_repo("jleechanorg/foo").is_some());
        // Deriving defaults for a repo whose ao_project would collide
        // with the explicit entry for a different repo must fail.
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
        assert_eq!(
            cfg.resolve_repo("jleechanorg/worldarchitect.ai"),
            Some(RepoRouting {
                ao_project: "worldarchitect".to_string(),
                push_remote: "origin".to_string(),
            }),
            "worldarchitect.ai unseen repo derivation must preserve the worldarchitect special case"
        );
    }

    // jleechan-dljf (issue #271): acceptance criterion — identical PR
    // numbers across different repos must never cross-route.

    /// Two different repos with the SAME PR number must resolve to
    /// DIFFERENT AO projects — proving #52 on worldarchitect cannot
    /// cross-route to ez-gh-actions and vice versa.
    #[test]
    fn same_pr_number_different_repos_do_not_cross_route_issue_52() {
        let dir = std::env::temp_dir().join("afd_cfg_test_cross_route_52");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("cross_route_52.toml");
        std::fs::write(
            &p,
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
        )
        .unwrap();
        let cfg = load(&p).unwrap();

        let wa_routing = cfg.resolve_repo("jleechanorg/worldarchitect.ai").unwrap();
        let ez_routing = cfg.resolve_repo("jleechanorg/ez-gh-actions").unwrap();
        let df_routing = cfg.resolve_repo("jleechanorg/dark-factory").unwrap();

        assert_eq!(wa_routing.ao_project, "worldarchitect");
        assert_eq!(ez_routing.ao_project, "ez-gh-actions");
        assert_eq!(df_routing.ao_project, "dark-factory");

        assert_ne!(
            wa_routing.ao_project, ez_routing.ao_project,
            "PR #52 on worldarchitect must NOT route to same AO project as PR #52 on ez-gh-actions"
        );
        assert_ne!(
            wa_routing.ao_project, df_routing.ao_project,
            "PR #52 on worldarchitect must NOT route to same AO project as PR #52 on dark-factory"
        );
        assert_ne!(
            df_routing.ao_project, ez_routing.ao_project,
            "PR #52 on dark-factory must NOT route to same AO project as PR #52 on ez-gh-actions"
        );
    }

    /// Same as above for PR #63.
    #[test]
    fn same_pr_number_different_repos_do_not_cross_route_issue_63() {
        let dir = std::env::temp_dir().join("afd_cfg_test_cross_route_63");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("cross_route_63.toml");
        std::fs::write(
            &p,
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

[repos."jleechanorg/ez-gh-actions"]
ao_project = "ez-gh-actions-custom"
push_remote = "ezremote"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();

        let wa_routing = cfg.resolve_repo("jleechanorg/worldarchitect.ai").unwrap();
        let ez_routing = cfg.resolve_repo("jleechanorg/ez-gh-actions").unwrap();

        assert_eq!(wa_routing.ao_project, "worldarchitect");
        assert_eq!(ez_routing.ao_project, "ez-gh-actions-custom");
        assert_ne!(
            wa_routing.ao_project, ez_routing.ao_project,
            "PR #63 on worldarchitect must NOT route to same AO project as PR #63 on ez-gh-actions"
        );
    }

    // ── jleechan-dljf skeptic fixes ──────────────────────────────────────

    /// jleechan-dljf (issue #271): `is_valid_owner_repo` must reject
    /// whitespace, control characters, and chars not allowed in GitHub
    /// owner/repo names. Malformed input must fail closed.
    #[test]
    fn is_valid_owner_repo_rejects_whitespace_control_and_invalid_chars() {
        assert!(is_valid_owner_repo("jleechanorg/dark-factory"));
        assert!(is_valid_owner_repo("foo-bar/foo_bar.baz"));
        // Whitespace
        assert!(!is_valid_owner_repo(" owner/repo"));
        assert!(!is_valid_owner_repo("owner /repo"));
        assert!(!is_valid_owner_repo(" \towner/repo"));
        assert!(!is_valid_owner_repo("owner/repo "));
        assert!(!is_valid_owner_repo("owner/repo\n"));
        // Control characters
        assert!(!is_valid_owner_repo("owner\x00/repo"));
        assert!(!is_valid_owner_repo("owner/repo\x1f"));
        assert!(!is_valid_owner_repo("owner/\x7frepo"));
        // Invalid chars (not alphanumeric, hyphen, underscore, dot)
        assert!(!is_valid_owner_repo("owner/repo!"));
        assert!(!is_valid_owner_repo("owner/repo@"));
        assert!(!is_valid_owner_repo("owner/repo#"));
        assert!(!is_valid_owner_repo("owner/re$po"));
        // Empty segments
        assert!(!is_valid_owner_repo(""));
        assert!(!is_valid_owner_repo("/repo"));
        assert!(!is_valid_owner_repo("owner/"));
        // Too many slashes
        assert!(!is_valid_owner_repo("a/b/c"));
    }

    /// jleechan-dljf (issue #271): global target_repo's ao_project must be
    /// checked against explicit [repos.*] entries for collisions.
    #[test]
    fn resolve_repo_fails_for_global_ao_project_collision_with_explicit_repo() {
        let dir =
            std::env::temp_dir().join("afd_cfg_test_global_ao_collision");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("global_ao_collision.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/dark-factory"
ao_project = "shared-project"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."jleechanorg/other-repo"]
ao_project = "shared-project"
push_remote = "origin"
"#,
        )
        .unwrap();
        let cfg = load(&p).unwrap();
        // Explicit entry for other-repo resolves fine.
        assert!(cfg.resolve_repo("jleechanorg/other-repo").is_some());
        // Global target_repo (dark-factory) has ao_project="shared-project"
        // which collides with the explicit [repos."jleechanorg/other-repo"]
        // entry — must fail closed.
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            None,
            "global target_repo ao_project 'shared-project' collides with explicit [repos.'jleechanorg/other-repo'] entry"
        );
    }

    /// jleechan-dljf (issue #271): two explicit [repos] entries with the
    /// same ao_project must fail at load time (explicit-explicit collision
    /// detection).
    #[test]
    fn resolve_repo_explicit_explicit_collision_blocked_for_derived() {
        let dir =
            std::env::temp_dir().join("afd_cfg_test_explicit_explicit_collision");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("explicit_explicit_collision.toml");
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

[repos."jleechanorg/repo-a"]
ao_project = "duplicate-project"
push_remote = "origin"

[repos."jleechanorg/repo-b"]
ao_project = "duplicate-project"
push_remote = "otherremote"
"#,
        )
        .unwrap();
        // jleechan-dljf: explicit-explicit collision is caught at load time.
        let result = load(&p);
        assert!(result.is_err(), "explicit-explicit AO project collision must fail at load time");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ao_project"), "error must mention ao_project: {msg}");
        assert!(msg.contains("duplicate-project"), "error must name the colliding project: {msg}");
        assert!(msg.contains("repo-a"), "error must name both repos: {msg}");
        assert!(msg.contains("repo-b"), "error must name both repos: {msg}");
    }

    /// jleechan-dljf (issue #271): explicit-explicit AO project collision
    /// must be rejected at config load time — two different [repos] entries
    /// sharing the same ao_project is a configuration error.
    #[test]
    fn load_rejects_explicit_explicit_ao_project_collision() {
        let dir = std::env::temp_dir().join("afd_cfg_test_ee_collision_load");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ee_collision.toml");
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

[repos."jleechanorg/repo-a"]
ao_project = "shared-project"
push_remote = "origin"

[repos."jleechanorg/repo-b"]
ao_project = "shared-project"
push_remote = "origin"
"#,
        )
        .unwrap();
        let result = load(&p);
        assert!(result.is_err(), "explicit-explicit AO project collision must be rejected at load time");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ao_project"), "error must mention ao_project: {msg}");
        assert!(msg.contains("shared-project"), "error must name the colliding project: {msg}");
    }
}
