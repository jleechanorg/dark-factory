use crate::errors::DaemonError;
use std::collections::HashMap;
use std::path::Path;

/// One `[repos."<owner>/<repo>"]` table entry.
#[derive(serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RepoConfig {
    pub ao_project: String,
    pub push_remote: String,
}

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

/// Resolved dispatch routing for a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRouting {
    pub ao_project: String,
    pub push_remote: String,
    pub source: RoutingSource,
}

/// Acceptable lengths for GitHub owner and repo names.
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
    #[serde(default)]
    pub repos: HashMap<String, RepoConfig>,
}

impl Config {
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
                source: RoutingSource::GlobalTarget,
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

        // jleechan-dljf skeptic: collision check against [repos] entries
        // AND against the global target_repo's effective ao_project.
        if self.is_ao_project_collision(&ao_project, repo) {
            eprintln!(
                "auto-factory daemon: ao_project collision: derived ao_project='{}' \
                 for repo '{}' conflicts with an explicit [repos.*] entry for a \
                 different repo; routing failed closed",
                ao_project, repo
            );
            return None;
        }

        // jleechan-dljf skeptic: also check if the derived ao_project
        // collides with the global target_repo's effective ao_project.
        let global_ao = self.global_effective_ao_project();
        if repo != self.target_repo && ao_project == global_ao {
            eprintln!(
                "auto-factory daemon: ao_project collision: derived ao_project='{}' \
                 for repo '{}' collides with global target_repo '{}' effective \
                 ao_project='{}'; routing failed closed",
                ao_project, repo, self.target_repo, global_ao
            );
            return None;
        }

        Some(RepoRouting {
            ao_project,
            push_remote: "origin".to_string(),
            source: RoutingSource::Derived,
        })
    }

    fn is_ao_project_collision(&self, ao_project: &str, for_repo: &str) -> bool {
        self.repos
            .iter()
            .any(|(entry_repo, rc)| entry_repo != for_repo && rc.ao_project == ao_project)
    }

    /// Effective AO project for the global `target_repo`: explicit
    /// `ao_project` if set, else the last-path-segment derivation.
    pub fn global_effective_ao_project(&self) -> String {
        self.ao_project.clone().unwrap_or_else(|| {
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
    let (owner, name) = s.split_once('/')?;
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

/// GitHub owner names: alphanumeric + hyphen only, max 39 chars,
/// cannot start or end with hyphen, cannot be all hyphens.
fn is_valid_github_owner_segment(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_OWNER_LEN {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return false;
    }
    let mut has_alnum = false;
    for &b in bytes {
        if b.is_ascii_alphanumeric() {
            has_alnum = true;
        } else if b != b'-' {
            return false;
        }
    }
    has_alnum
}

/// GitHub repo names: alphanumeric + hyphen + underscore + dot,
/// max 100 chars, cannot end with `.git` (reserved), cannot be
/// just `.` or `..`, cannot start or end with hyphen or dot.
fn is_valid_github_repo_segment(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_REPO_LEN {
        return false;
    }
    if s == "." || s == ".." {
        return false;
    }
    if s.to_ascii_lowercase().ends_with(".git") {
        return false;
    }
    let bytes = s.as_bytes();
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if first == b'-' || first == b'.' || last == b'-' || last == b'.' {
        return false;
    }
    for &b in bytes {
        if !(b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.') {
            return false;
        }
    }
    true
}

pub fn is_valid_owner_repo(s: &str) -> bool {
    validate_owner_repo(s).is_some()
}

pub fn load(path: &Path) -> Result<Config, DaemonError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| DaemonError::Config(format!("{}: {e}", path.display())))?;
    let cfg: Config = toml::from_str(&raw).map_err(|e| DaemonError::Config(e.to_string()))?;

    if !is_valid_owner_repo(&cfg.target_repo) {
        return Err(DaemonError::Config(format!(
            "invalid target_repo {:?}: must be owner/repo format (alphanumeric, \
             hyphens allowed in owner; alphanumeric, hyphens, underscores, dots \
             allowed in repo name)",
            cfg.target_repo
        )));
    }

    for (repo, rc) in &cfg.repos {
        if repo.is_empty() {
            return Err(DaemonError::Config(
                "empty [repos] key: each entry must be in owner/repo format".to_string(),
            ));
        }
        if !is_valid_owner_repo(repo) {
            return Err(DaemonError::Config(format!(
                "invalid [repos] key {:?}: must be owner/repo format",
                repo
            )));
        }
        if rc.ao_project.is_empty() {
            return Err(DaemonError::Config(format!(
                "empty ao_project for [repos.\"{}\"]",
                repo
            )));
        }
    }

    let mut seen_ao_projects: HashMap<&str, &str> = HashMap::new();

    // jleechan-dljf symmetric: check the global target_repo's effective
    // ao_project (explicit OR derived) against [repos] entries at load time.
    let global_ao = cfg.global_effective_ao_project();
    for (repo, rc) in &cfg.repos {
        if repo != &cfg.target_repo && rc.ao_project == global_ao {
            return Err(DaemonError::Config(format!(
                "global-explicit AO project collision: {} global ao_project=\"{}\" \
                 collides with [repos.\"{}\"].ao_project=\"{}\" — each repo must \
                 route to a distinct AO project",
                if cfg.ao_project.is_some() {
                    "explicit"
                } else {
                    "derived"
                },
                global_ao,
                repo,
                rc.ao_project
            )));
        }
    }
    seen_ao_projects.insert(global_ao.as_str(), cfg.target_repo.as_str());

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

    fn repo(ao: &str, remote: &str, source: RoutingSource) -> RepoRouting {
        RepoRouting {
            ao_project: ao.to_string(),
            push_remote: remote.to_string(),
            source,
        }
    }

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
        assert!(
            cfg.repos.is_empty(),
            "repos table must default to empty when absent"
        );
        assert_eq!(
            cfg.resolve_repo("owner/repo"),
            Some(repo("repo", "origin", RoutingSource::GlobalTarget)),
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
            Some(repo("worldarchitect", "worldai", RoutingSource::Explicit))
        );
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            Some(repo("dark-factory", "origin", RoutingSource::Explicit))
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
            Some(repo("ez-gh-actions", "origin", RoutingSource::Derived)),
            "unseen valid repo must derive safe defaults"
        );
        assert_eq!(
            cfg.resolve_repo("jleechanorg/dark-factory"),
            Some(repo("dark-factory", "origin", RoutingSource::GlobalTarget)),
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
            Some(repo(
                "ez-gh-actions-custom",
                "ezremote",
                RoutingSource::Explicit
            )),
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
            Some(repo("ez-gh-actions", "myremote", RoutingSource::Explicit)),
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
            Some(repo(
                "worldarchitect",
                "origin",
                RoutingSource::GlobalTarget
            ))
        );
    }

    // ── jleechan-dljf skeptic: validation ─────────────────────────────

    #[test]
    fn is_valid_owner_repo_rejects_whitespace_control_and_invalid_chars() {
        assert!(is_valid_owner_repo("jleechanorg/dark-factory"));
        assert!(is_valid_owner_repo("foo-bar/foo_bar.baz"));
        assert!(is_valid_owner_repo("a/my.repo"));
        // Dots in owner (user/org) names are invalid
        assert!(!is_valid_owner_repo("owner.name/repo"));
        // Underscores in owner names are invalid
        assert!(!is_valid_owner_repo("owner_name/repo"));
        // Whitespace
        assert!(!is_valid_owner_repo(" owner/repo"));
        assert!(!is_valid_owner_repo("owner /repo"));
        assert!(!is_valid_owner_repo(" \towner/repo"));
        assert!(!is_valid_owner_repo("owner/repo "));
        assert!(!is_valid_owner_repo("owner/repo\n"));
        // Control chars
        assert!(!is_valid_owner_repo("owner\x00/repo"));
        assert!(!is_valid_owner_repo("owner/repo\x1f"));
        assert!(!is_valid_owner_repo("owner/\x7frepo"));
        // Invalid chars
        assert!(!is_valid_owner_repo("owner/repo!"));
        assert!(!is_valid_owner_repo("owner/re$po"));
        // Empty segments
        assert!(!is_valid_owner_repo(""));
        assert!(!is_valid_owner_repo("/repo"));
        assert!(!is_valid_owner_repo("owner/"));
        assert!(!is_valid_owner_repo("a/b/c"));
    }

    /// jleechan-dljf skeptic: owner segment hyphen rules + max length.
    #[test]
    fn is_valid_owner_repo_rejects_hyphen_edge_cases() {
        assert!(
            !is_valid_owner_repo("-owner/repo"),
            "cannot start with hyphen"
        );
        assert!(
            !is_valid_owner_repo("owner-/repo"),
            "cannot end with hyphen"
        );
        assert!(
            !is_valid_owner_repo("---/repo"),
            "all-hyphens owner rejected"
        );
        // Max owner length 39
        let long = "a".repeat(40);
        assert!(
            !is_valid_owner_repo(&format!("{long}/repo")),
            "owner >39 chars rejected"
        );
        assert!(
            is_valid_owner_repo(&format!("{}/repo", "a".repeat(39))),
            "owner 39 chars ok"
        );
    }

    /// jleechan-dljf skeptic: repo segment hyphen/dot rules + max length.
    #[test]
    fn is_valid_owner_repo_rejects_repo_edge_cases() {
        assert!(
            !is_valid_owner_repo("owner/-repo"),
            "cannot start with hyphen"
        );
        assert!(
            !is_valid_owner_repo("owner/repo-"),
            "cannot end with hyphen"
        );
        assert!(!is_valid_owner_repo("owner/.repo"), "cannot start with dot");
        assert!(!is_valid_owner_repo("owner/repo."), "cannot end with dot");
        assert!(!is_valid_owner_repo("owner/."), "dot-only rejected");
        assert!(!is_valid_owner_repo("owner/.."), "dotdot rejected");
        assert!(
            !is_valid_owner_repo("owner/myrepo.git"),
            ".git suffix rejected"
        );
        // Max repo length 100
        let long = "a".repeat(101);
        assert!(
            !is_valid_owner_repo(&format!("owner/{long}")),
            "repo >100 chars rejected"
        );
        assert!(
            is_valid_owner_repo(&format!("owner/{}", "a".repeat(100))),
            "repo 100 chars ok"
        );
    }

    // ── jleechan-dljf skeptic: symmetric collision ────────────────────

    /// jleechan-dljf skeptic: global TARGET_REPO's derived ao_project must
    /// be checked against [repos] entries at load time (symmetric with
    /// explicit ao_project check).
    #[test]
    fn load_rejects_global_derived_ao_project_collision_with_repos_entry() {
        let dir = std::env::temp_dir().join("afd_cfg_test_global_derived_collision");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("global_derived_collision.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/ez-gh-actions"
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
ao_project = "ez-gh-actions"
push_remote = "origin"
"#,
        )
        .unwrap();
        let result = load(&p);
        assert!(
            result.is_err(),
            "global derived ao_project must fail at load when colliding with [repos]"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("ez-gh-actions"));
    }

    /// jleechan-dljf skeptic: unseen repo's derived ao_project collides
    /// with global target_repo's effective ao_project → fail closed.
    #[test]
    fn resolve_repo_fails_for_derived_vs_global_ao_project_collision() {
        let dir = std::env::temp_dir().join("afd_cfg_test_derived_vs_global");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("derived_vs_global.toml");
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
        // Global target_repo resolves fine.
        assert!(cfg.resolve_repo("jleechanorg/dark-factory").is_some());
        // Unseen repo whose derived ao_project == global effective ao_project
        // must fail closed — two repos cannot route to same AO project.
        assert_eq!(
            cfg.resolve_repo("otherorg/dark-factory"),
            None,
            "derived ao_project 'dark-factory' collides with global target_repo's effective ao_project"
        );
    }

    // ── jleechan-dljf skeptic: same-number cross-repo ────────────────

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

    // ── remaining collision tests ─────────────────────────────────────

    #[test]
    fn resolve_repo_fails_for_global_ao_project_collision_with_explicit_repo() {
        let dir = std::env::temp_dir().join("afd_cfg_test_global_ao_collision");
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
        assert!(
            result.is_err(),
            "global-explicit AO project collision must fail at load time"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("global-explicit") || msg.contains("global "));
        assert!(msg.contains("shared-project"));
    }

    #[test]
    fn resolve_repo_explicit_explicit_collision_blocked_for_derived() {
        let dir = std::env::temp_dir().join("afd_cfg_test_explicit_explicit_collision");
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
        assert!(msg.contains("global-explicit") || msg.contains("global "));
        assert!(msg.contains("shared-project"));
        assert!(msg.contains("other-repo"));
    }

    #[test]
    fn load_rejects_malformed_target_repo() {
        let dir = std::env::temp_dir().join("afd_cfg_test_malformed_target_repo");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("malformed_target.toml");
        std::fs::write(
            &p,
            r#"target_repo = "not-valid"
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
        let result = load(&p);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("owner/repo format"),
            "malformed target_repo must be rejected at load time"
        );
    }

    #[test]
    fn load_rejects_empty_target_repo() {
        let dir = std::env::temp_dir().join("afd_cfg_test_empty_target_repo");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty_target.toml");
        std::fs::write(
            &p,
            r#"target_repo = ""
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
        let result = load(&p);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("owner/repo format"));
    }

    #[test]
    fn load_rejects_malformed_repos_key() {
        let dir = std::env::temp_dir().join("afd_cfg_test_malformed_repos_key");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("malformed_repos.toml");
        std::fs::write(
            &p,
            r#"
target_repo = "jleechanorg/dark-factory"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."no-slash"]
ao_project = "bad"
push_remote = "origin"
"#,
        )
        .unwrap();
        let result = load(&p);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("owner/repo format"));
        assert!(msg.contains("no-slash"));
    }

    #[test]
    fn load_rejects_empty_repos_key() {
        let dir = std::env::temp_dir().join("afd_cfg_test_empty_repos_key");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty_repos_key.toml");
        std::fs::write(
            &p,
            r#"target_repo = "jleechanorg/dark-factory"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos.""]
ao_project = "bad"
push_remote = "origin"
"#,
        )
        .unwrap();
        let result = load(&p);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("empty [repos] key"));
    }

    #[test]
    fn load_rejects_empty_ao_project_in_repos() {
        let dir = std::env::temp_dir().join("afd_cfg_test_empty_ao_project_repos");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty_ao_project.toml");
        std::fs::write(
            &p,
            r#"target_repo = "jleechanorg/dark-factory"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."jleechanorg/other"]
ao_project = ""
push_remote = "origin"
"#,
        )
        .unwrap();
        let result = load(&p);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty ao_project"));
    }

    #[test]
    fn load_rejects_target_repo_with_git_suffix() {
        let dir = std::env::temp_dir().join("afd_cfg_test_git_suffix_target");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("git_suffix_target.toml");
        std::fs::write(
            &p,
            r#"target_repo = "owner/repo.git"
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
        let result = load(&p);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("owner/repo format"));
    }

    #[test]
    fn load_rejects_target_repo_with_too_many_slashes() {
        let dir = std::env::temp_dir().join("afd_cfg_test_multi_slash_target");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("multi_slash_target.toml");
        std::fs::write(
            &p,
            r#"target_repo = "a/b/c"
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
        let result = load(&p);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("owner/repo format"));
    }

    /// Duplicate repos keys: TOML parser rejects duplicate table keys at the
    /// parser level, which surfaces as a DaemonError::Config. This test
    /// proves the parser-level guard exists.
    #[test]
    fn load_rejects_duplicate_repos_key() {
        let dir = std::env::temp_dir().join("afd_cfg_test_duplicate_repos_key");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("duplicate_repos.toml");
        std::fs::write(
            &p,
            r#"target_repo = "jleechanorg/dark-factory"
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
ao_project = "foo-proj"
push_remote = "origin"

[repos."jleechanorg/foo"]
ao_project = "foo-proj-dup"
push_remote = "origin"
"#,
        )
        .unwrap();
        let result = load(&p);
        assert!(result.is_err(), "duplicate repos keys must be rejected");
    }

    /// jleechan-dljf skeptic: overlength owner segment in target_repo must
    /// be rejected at load time (regression: is_valid_owner_repo is covered,
    /// but Config::load wasn't explicitly tested for this path).
    #[test]
    fn load_rejects_overlength_target_repo() {
        // Owner > 39 chars
        let dir = std::env::temp_dir().join("afd_cfg_test_overlength_target_owner");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("overlength_owner_target.toml");
        let long_owner = "a".repeat(40);
        std::fs::write(
            &p,
            format!(
                r#"target_repo = "{long_owner}/repo"
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
            ),
        )
        .unwrap();
        let result = load(&p);
        assert!(
            result.is_err(),
            "target_repo with owner > 39 chars must be rejected at load time"
        );

        // Repo > 100 chars
        let dir2 = std::env::temp_dir().join("afd_cfg_test_overlength_target_repo");
        std::fs::create_dir_all(&dir2).unwrap();
        let p2 = dir2.join("overlength_repo_target.toml");
        let long_repo = "a".repeat(101);
        std::fs::write(
            &p2,
            format!(
                r#"target_repo = "owner/{long_repo}"
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
            ),
        )
        .unwrap();
        let result2 = load(&p2);
        assert!(
            result2.is_err(),
            "target_repo with repo > 100 chars must be rejected at load time"
        );
    }

    /// jleechan-dljf skeptic: overlength owner or repo in a [repos] key
    /// must be rejected at load time (regression: is_valid_owner_repo is
    /// covered for standalone strings, but the load-time validation path
    /// through repos keys wasn't explicitly tested for overlength).
    #[test]
    fn load_rejects_overlength_repos_key() {
        // Owner > 39 chars in [repos] key
        let dir = std::env::temp_dir().join("afd_cfg_test_overlength_repos_owner");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("overlength_owner_repos.toml");
        let long_owner = "a".repeat(40);
        std::fs::write(
            &p,
            format!(
                r#"target_repo = "jleechanorg/dark-factory"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."{long_owner}/valid-repo"]
ao_project = "test-proj"
push_remote = "origin"
"#,
            ),
        )
        .unwrap();
        let result = load(&p);
        assert!(
            result.is_err(),
            "repos key with owner > 39 chars must be rejected at load time"
        );

        // Repo > 100 chars in [repos] key
        let dir2 = std::env::temp_dir().join("afd_cfg_test_overlength_repos_repo");
        std::fs::create_dir_all(&dir2).unwrap();
        let p2 = dir2.join("overlength_repo_repos.toml");
        let long_repo = "a".repeat(101);
        std::fs::write(
            &p2,
            format!(
                r#"target_repo = "jleechanorg/dark-factory"
base_branch = "main"
stage = 1
max_workers = 30
max_batch = 15
fast_tick_secs = 10
slow_tick_secs = 30
autonomy_timebox_secs = 10800
budget_warn_usd = 20.0
spec_dir = ".factory/specs/"

[repos."valid-owner/{long_repo}"]
ao_project = "test-proj"
push_remote = "origin"
"#,
            ),
        )
        .unwrap();
        let result2 = load(&p2);
        assert!(
            result2.is_err(),
            "repos key with repo > 100 chars must be rejected at load time"
        );
    }
}
