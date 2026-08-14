// Bead jleechan-ev6m: JSON-driven backend + fallback chain config.
//
// Mirrors runner/backend_config.py so the daemon, runner, and bin/dark-factory
// all resolve the same configuration. JSON takes precedence over the legacy
// DARK_FACTORY_REVIEWER_FALLBACK_CHAIN / DARK_FACTORY_REVIEWER_DEFAULT env
// vars; those env vars are still honored (with a one-time warning) for
// backward compatibility.
//
// Config file lookup order (highest first):
//   1. $DARK_FACTORY_BACKENDS_CONFIG (explicit path)
//   2. ~/.dark-factory/backends.json (user override)
//   3. <repo_root>/config/backends.json (committed default)
//
// See runner/backend_config.py for the canonical schema and the Python
// implementation; this file is a deliberately small Rust mirror.

use crate::errors::DaemonError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const ENV_EXPLICIT: &str = "DARK_FACTORY_BACKENDS_CONFIG";
const ENV_LEGACY_FALLBACK: &str = "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN";
const ENV_LEGACY_DEFAULT: &str = "DARK_FACTORY_REVIEWER_DEFAULT";
const ENV_LEGACY_BACKEND: &str = "DARK_FACTORY_BACKEND";

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct BackendSpec {
    pub cli: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub default_project: Option<String>,
    #[serde(default)]
    pub transitive_deps: Vec<String>,
    #[serde(default)]
    pub default_timeout: Option<u64>,
    #[serde(default)]
    pub default_print_timeout: Option<u64>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct BackendConfig {
    pub version: u32,
    pub default_backend: String,
    #[serde(default)]
    pub reviewer_default: Option<String>,
    #[serde(default)]
    pub fallback_chain: Vec<String>,
    #[serde(default)]
    pub alias_map: HashMap<String, String>,
    pub backends: HashMap<String, BackendSpec>,
}

#[derive(Debug)]
pub enum BackendConfigError {
    NotFound(String),
    InvalidJson { path: PathBuf, message: String },
    InvalidSchema { path: PathBuf, message: String },
}

impl std::fmt::Display for BackendConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(m) => write!(f, "backend config: {m}"),
            Self::InvalidJson { path, message } => {
                write!(f, "invalid JSON in {path:?}: {message}")
            }
            Self::InvalidSchema { path, message } => {
                write!(f, "schema validation failed for {path:?}: {message}")
            }
        }
    }
}

/// Locate the active config file by precedence. Returns ``None`` if no
/// candidate file exists.
pub fn locate_config() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(ENV_EXPLICIT) {
        let p = PathBuf::from(explicit);
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let user = PathBuf::from(home).join(".dark-factory").join("backends.json");
        if user.exists() {
            return Some(user);
        }
    }
    let repo = PathBuf::from("config").join("backends.json");
    if repo.exists() {
        return Some(repo);
    }
    None
}

/// Load + validate the active backend config. Returns ``None`` when no
/// candidate file exists (callers must fall back to legacy defaults).
pub fn load_active() -> Result<Option<BackendConfig>, BackendConfigError> {
    let Some(path) = locate_config() else {
        return Ok(None);
    };
    let cfg = load_from(&path)?;
    Ok(Some(cfg))
}

/// Load a backend config from a specific path.
pub fn load_from(path: &Path) -> Result<BackendConfig, BackendConfigError> {
    if !path.exists() {
        return Err(BackendConfigError::NotFound(format!(
            "backend config not found: {path:?}"
        )));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| {
        BackendConfigError::InvalidJson {
            path: path.to_path_buf(),
            message: format!("read failed: {e}"),
        }
    })?;
    let cfg: BackendConfig = serde_json::from_str(&raw).map_err(|e| {
        BackendConfigError::InvalidJson {
            path: path.to_path_buf(),
            message: e.to_string(),
        }
    })?;
    validate(&cfg, path)?;
    Ok(cfg)
}

fn validate(cfg: &BackendConfig, path: &Path) -> Result<(), BackendConfigError> {
    if cfg.version < 1 {
        return Err(BackendConfigError::InvalidSchema {
            path: path.to_path_buf(),
            message: format!("version must be >= 1, got {}", cfg.version),
        });
    }
    if cfg.default_backend.is_empty() {
        return Err(BackendConfigError::InvalidSchema {
            path: path.to_path_buf(),
            message: "default_backend must be non-empty".to_string(),
        });
    }
    if cfg.backends.is_empty() {
        return Err(BackendConfigError::InvalidSchema {
            path: path.to_path_buf(),
            message: "backends must be non-empty".to_string(),
        });
    }
    if !cfg.backends.contains_key(&cfg.default_backend) {
        return Err(BackendConfigError::InvalidSchema {
            path: path.to_path_buf(),
            message: format!(
                "default_backend {:?} is not defined in backends",
                cfg.default_backend
            ),
        });
    }
    let known: std::collections::HashSet<&String> = cfg.backends.keys().collect();
    for entry in &cfg.fallback_chain {
        let canonical = resolve_alias(cfg, entry);
        if !known.contains(&canonical) {
            return Err(BackendConfigError::InvalidSchema {
                path: path.to_path_buf(),
                message: format!(
                    "fallback_chain references unknown backend {entry:?} (canonical: {canonical:?})"
                ),
            });
        }
    }
    for (alias, target) in &cfg.alias_map {
        let canonical = resolve_alias(cfg, target);
        if !known.contains(&canonical) {
            return Err(BackendConfigError::InvalidSchema {
                path: path.to_path_buf(),
                message: format!(
                    "alias_map[{alias:?}] -> {target:?} (canonical {canonical:?}) is not defined"
                ),
            });
        }
    }
    Ok(())
}

/// Canonicalize a vendor name via ``alias_map``; pass-through if unknown.
pub fn resolve_alias(cfg: &BackendConfig, name: &str) -> String {
    cfg.alias_map
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

/// Resolve the fallback chain with canonicalization and dedup.
pub fn resolve_fallback_chain(cfg: &BackendConfig) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |name: &str| {
        if name.is_empty() {
            return;
        }
        let canonical = resolve_alias(cfg, name);
        if seen.insert(canonical.clone()) {
            chain.push(canonical);
        }
    };
    if let Some(default) = &cfg.reviewer_default {
        push(default);
    }
    for entry in &cfg.fallback_chain {
        push(entry);
    }
    chain
}

/// Resolve the configured reviewer default, honoring the legacy
/// ``DARK_FACTORY_REVIEWER_DEFAULT`` env var when set. Emits a one-line
/// deprecation warning to stderr in that case.
pub fn resolve_reviewer_default(cfg: Option<&BackendConfig>) -> String {
    if let Ok(legacy) = std::env::var(ENV_LEGACY_DEFAULT) {
        if !legacy.is_empty() {
            eprintln!(
                "daemon: warning: environment variable {ENV_LEGACY_DEFAULT}={legacy:?} is deprecated; \
                 configure reviewer_default in config/backends.json instead"
            );
            return legacy;
        }
    }
    if let Some(cfg) = cfg {
        if let Some(default) = &cfg.reviewer_default {
            return default.clone();
        }
        return cfg.default_backend.clone();
    }
    "minimax".to_string()
}

/// Resolve the fallback chain, honoring the legacy
/// ``DARK_FACTORY_REVIEWER_FALLBACK_CHAIN`` env var when set.
pub fn resolve_fallback_chain_with_precedence(
    cfg: Option<&BackendConfig>,
) -> Vec<String> {
    if let Ok(legacy) = std::env::var(ENV_LEGACY_FALLBACK) {
        if !legacy.is_empty() {
            eprintln!(
                "daemon: warning: environment variable {ENV_LEGACY_FALLBACK}={legacy:?} is deprecated; \
                 configure fallback_chain in config/backends.json instead"
            );
            // Legacy format is "a->b->c" (NOT "aow->claude-code->agy->minimax"
            // which actually used "->" too). Be liberal in what we accept.
            return legacy
                .split(|c: char| c == '-' || c == '>' || c == ',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect();
        }
    }
    if let Some(cfg) = cfg {
        return resolve_fallback_chain(cfg);
    }
    vec![
        "minimax".to_string(),
        "antigravity".to_string(),
        "claude-code".to_string(),
    ]
}

/// Resolve the configured default backend, honoring the legacy
/// ``DARK_FACTORY_BACKEND`` env var when set.
pub fn resolve_default_backend(cfg: Option<&BackendConfig>) -> String {
    if let Ok(legacy) = std::env::var(ENV_LEGACY_BACKEND) {
        if !legacy.is_empty() {
            eprintln!(
                "daemon: warning: environment variable {ENV_LEGACY_BACKEND}={legacy:?} is deprecated; \
                 configure default_backend in config/backends.json instead"
            );
            return legacy;
        }
    }
    if let Some(cfg) = cfg {
        return cfg.default_backend.clone();
    }
    "ao".to_string()
}

// ---------------------------------------------------------------------------
// Error mapping for daemon API consumers
// ---------------------------------------------------------------------------

impl From<BackendConfigError> for DaemonError {
    fn from(value: BackendConfigError) -> Self {
        DaemonError::Config(value.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> &'static str {
        r#"{
            "version": 1,
            "default_backend": "ao",
            "reviewer_default": "minimax",
            "fallback_chain": ["agy", "minimax", "claude-code"],
            "alias_map": {"aow": "minimax", "agy": "antigravity"},
            "backends": {
                "ao": {"cli": "ao", "args": ["spawn"], "agent": "antigravity",
                       "default_project": "worldarchitect.ai",
                       "transitive_deps": ["sandbox-exec"]},
                "antigravity": {"cli": "agy", "args": ["--print"]},
                "minimax": {"cli": "claude", "args": ["--print"]},
                "claude-code": {"cli": "claude", "args": ["--print"]},
                "echo": {"cli": "echo", "args": []}
            }
        }"#
    }

    #[test]
    fn parses_valid_config() {
        let cfg: BackendConfig = serde_json::from_str(fixture()).expect("parse");
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.default_backend, "ao");
        assert!(cfg.backends.contains_key("ao"));
    }

    #[test]
    fn fallback_chain_dedupes_via_alias_canonicalization() {
        let cfg: BackendConfig = serde_json::from_str(fixture()).expect("parse");
        let chain = resolve_fallback_chain(&cfg);
        // reviewer_default=minimax; fallback_chain=[agy, minimax, claude-code]
        // agy -> antigravity (not in chain yet) -> added
        // minimax -> minimax (not in chain yet) -> added
        // claude-code -> claude-code -> added
        // reviewer_default=minimax prepended -> minimax already in chain
        assert_eq!(
            chain,
            vec!["minimax", "antigravity", "claude-code"]
        );
    }

    #[test]
    fn alias_map_canonicalizes_known_alias() {
        let cfg: BackendConfig = serde_json::from_str(fixture()).expect("parse");
        assert_eq!(resolve_alias(&cfg, "aow"), "minimax");
        assert_eq!(resolve_alias(&cfg, "agy"), "antigravity");
        assert_eq!(resolve_alias(&cfg, "claude-code"), "claude-code");
    }

    #[test]
    fn rejects_default_backend_missing_from_backends() {
        let bad = r#"{
            "version": 1,
            "default_backend": "ghost",
            "backends": {"ao": {"cli": "ao", "args": []}}
        }"#;
        let cfg: BackendConfig = serde_json::from_str(bad).expect("parse");
        let dir = tempfile_like();
        let err = validate(&cfg, &dir).expect_err("must reject");
        assert!(matches!(err, BackendConfigError::InvalidSchema { .. }));
    }

    #[test]
    fn rejects_alias_targeting_unknown_backend() {
        let bad = r#"{
            "version": 1,
            "default_backend": "ao",
            "alias_map": {"aow": "ghost"},
            "backends": {"ao": {"cli": "ao", "args": []}}
        }"#;
        let cfg: BackendConfig = serde_json::from_str(bad).expect("parse");
        let dir = tempfile_like();
        let err = validate(&cfg, &dir).expect_err("must reject");
        assert!(matches!(err, BackendConfigError::InvalidSchema { .. }));
    }

    fn tempfile_like() -> PathBuf {
        PathBuf::from("backends.json")
    }
}