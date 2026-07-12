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
    ///    (or the same last-path-segment derivation, with
    ///    `push_remote` defaulting to `"origin"`).
    /// 3. Valid `owner/repo` string → derive safe defaults:
    ///    `ao_project` = last path segment (with `worldarchitect.ai` →
    ///    `worldarchitect` special case), `push_remote` = `"origin"`.
    /// 4. Collision check: if the derived `ao_project` matches an
    ///    explicit `[repos]` entry's `ao_project` for a DIFFERENT repo,
    ///    fail closed.
    /// 5. Global target_repo's ao_project also checked for collisions.
    /// 6. Malformed repo → `None` (fail closed).
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
            "auto-factory daemon: derived-route repo={} ao_project={} push_remote=origin \
             source=derived (no explicit [repos.\"{}\"] entry)",
            repo, ao_project, repo
        );

        Some(RepoRouting {
            ao_project,
            push_remote: "origin".to_string(),
        })
    }

    fn is_ao_project_collision(&self, ao_project: &str, for_repo: &str) -> bool {
        self.repos.iter().any(|(entry_repo, rc)| {
            entry_repo != for_repo && rc.ao_project == ao_project
        })
    }
}

struct OwnerRepo {
    name: String,
}

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
    if !is_valid_github_owner_segment(owner) || !is_valid_github_repo_segment(name) {
        return None;
    }
    Some(OwnerRepo {
        name: name.to_string(),
    })
}

fn is_valid_github_owner_segment(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn is_valid_github_repo_segment(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

pub fn is_valid_owner_repo(s: &str) -> bool {
    validate_owner_repo(s).is_some()
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    let cfg: Config =
        toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))?;
    let mut seen_ao_projects: HashMap<&str, &str> = HashMap::new();
    if let Some(ref global_ao) = cfg.ao_project {
        for (repo, rc) in &cfg.repos {
            if repo != &cfg.target_repo && rc.ao_project == *global_ao {
                return Err(DaemonError::Config(format!(
                    "global-explicit AO project collision: global ao_project=\"{}\" \
                     collides with [repos.\"{}\"].ao_project=\"{}\" — each repo must \
                     route to a distinct AO project",
                    global_ao, repo, rc.ao_project
                )));
            }
        }
        seen_ao_projects.insert(global_ao.as_str(), cfg.target_repo.as_str());
    }
    for (repo, rc) in &cfg.repos {
        if let Some(existing_repo) = seen_ao_projects.get(rc.ao_project.as_str()) {
            if existing_repo != repo {
                return Err(DaemonError::Config(format!(
                    "explicit-explicit AO project collision: [repos.\"{}\"] and \
                     [repos.\"{}\"] both claim ao_project=\"{}\" — each repo must \
                     route to a distinct AO project",
                    existing_repo, repo, rc.ao_project
                )));
            }
            continue;
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
            "unseen valid repo must derive safe defaults"
        );
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            Some(RepoRouting {
                ao_project: "dark-factory".to_string(),
                push_remote: "origin".to_string(),
            }),
            "global target_repo also resolves"
        );
    }

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
        assert!(cfg.resolve_repo("jleechanorg/foo").is_some());
        assert_eq!(
            cfg.resolve_repo("jleechanorg/colliding-project"),
            None,
            "derived ao_project 'colliding-project' collides with explicit entry"
        );
    }

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
        assert_eq!(
            cfg.resolve_repo("jleechanorg/ez-gh-actions"),
            Some(RepoRouting {
                ao_project: "ez-gh-actions".to_string(),
                push_remote: "myremote".to_string(),
            }),
            "explicit entry where ao_project matches last-segment: no collision"
        );
        assert_eq!(
            cfg.resolve_repo("otherorg/ez-gh-actions"),
            None,
            "derived ao_project collides with explicit entry for a different repo"
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
            "worldarchitect.ai unseen repo derivation must preserve special case"
        );
    }

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
        assert_ne!(wa_routing.ao_project, ez_routing.ao_project);
        assert_ne!(wa_routing.ao_project, df_routing.ao_project);
        assert_ne!(df_routing.ao_project, ez_routing.ao_project);
    }

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
        assert_ne!(wa_routing.ao_project, ez_routing.ao_project);
    }

    #[test]
    fn is_valid_owner_repo_rejects_whitespace_control_and_invalid_chars() {
        assert!(is_valid_owner_repo("jleechanorg/dark-factory"));
        assert!(is_valid_owner_repo("foo-bar/foo_bar.baz"));
        assert!(is_valid_owner_repo("a/my.repo"));
        assert!(!is_valid_owner_repo("owner.name/repo"));
        assert!(!is_valid_owner_repo("owner_name/repo"));
        assert!(!is_valid_owner_repo(" owner/repo"));
        assert!(!is_valid_owner_repo("owner /repo"));
        assert!(!is_valid_owner_repo(" \towner/repo"));
        assert!(!is_valid_owner_repo("owner/repo "));
        assert!(!is_valid_owner_repo("owner/repo\n"));
        assert!(!is_valid_owner_repo("owner\x00/repo"));
        assert!(!is_valid_owner_repo("owner/repo\x1f"));
        assert!(!is_valid_owner_repo("owner/\x7frepo"));
        assert!(!is_valid_owner_repo("owner/repo!"));
        assert!(!is_valid_owner_repo("owner/repo@"));
        assert!(!is_valid_owner_repo("owner/repo#"));
        assert!(!is_valid_owner_repo("owner/re$po"));
        assert!(!is_valid_owner_repo(""));
        assert!(!is_valid_owner_repo("/repo"));
        assert!(!is_valid_owner_repo("owner/"));
        assert!(!is_valid_owner_repo("a/b/c"));
    }

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
        let result = load(&p);
        assert!(result.is_err(), "global-explicit AO project collision must fail at load time");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("global-explicit"));
        assert!(msg.contains("shared-project"));
    }

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
        let result = load(&p);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ao_project"));
        assert!(msg.contains("duplicate-project"));
        assert!(msg.contains("repo-a"));
        assert!(msg.contains("repo-b"));
    }

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
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ao_project"));
        assert!(msg.contains("shared-project"));
    }

    #[test]
    fn load_rejects_global_explicit_ao_project_collision() {
        let dir = std::env::temp_dir().join("afd_cfg_test_global_explicit_collision_load");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("global_explicit_collision.toml");
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
        let result = load(&p);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("global-explicit"));
        assert!(msg.contains("shared-project"));
        assert!(msg.contains("other-repo"));
    }
}
