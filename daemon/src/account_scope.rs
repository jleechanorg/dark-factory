use crate::errors::DaemonError;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Canonical list of environment variables containing AI provider credentials,
/// tokens, or host configuration that must be scrubbed across all AI process launches.
pub const SCRUBBED_AUTH_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDEM_MODE",
    "CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL",
    "OPENAI_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "CODEX_HOME",
    "MINIMAX_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GOOGLE_CLOUD_PROJECT",
    "CURSOR_API_KEY",
    "CURSOR_CONFIG_DIR",
];

/// Scrub all inherited AI provider authentication, configuration, and token variables
/// from the child process command to ensure a completely clean, isolated execution environment.
pub fn scrub_all_ai_provider_auth(command: &mut Command) {
    for var in SCRUBBED_AUTH_VARS {
        command.env_remove(var);
    }
}

/// Supported AI providers for centralized account scoping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProvider {
    Claude,
    MiniMax,
    Codex,
    Antigravity,
    Cursor,
    Gemini,
}

/// Validate that `DARK_FACTORY_CLAUDE_CONFIG_DIR` is set to a non-blank,
/// existing directory, and returns its absolute canonicalized path.
/// Fails closed before any child process can be spawned.
pub fn validate_claude_config_dir() -> Result<PathBuf, DaemonError> {
    let raw = std::env::var("DARK_FACTORY_CLAUDE_CONFIG_DIR").map_err(|_| {
        DaemonError::Config(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR is not set; direct Claude launch requires explicit project-scoped directory".to_string(),
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DaemonError::Config(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR is blank; direct Claude launch requires explicit project-scoped directory".to_string(),
        ));
    }
    let path = PathBuf::from(trimmed);
    if !path.exists() {
        return Err(DaemonError::Config(format!(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR directory does not exist: {}",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(|e| {
        DaemonError::Config(format!(
            "failed to canonicalize DARK_FACTORY_CLAUDE_CONFIG_DIR {}: {e}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(DaemonError::Config(format!(
            "DARK_FACTORY_CLAUDE_CONFIG_DIR path is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Validate that `MINIMAX_API_KEY` is set to a non-blank string.
/// Never exposes or formats key values in error messages or logs.
pub fn validate_minimax_key() -> Result<String, DaemonError> {
    let raw = std::env::var("MINIMAX_API_KEY").map_err(|_| {
        DaemonError::Config("MINIMAX_API_KEY is not set".to_string())
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DaemonError::Config("MINIMAX_API_KEY is blank".to_string()));
    }
    Ok(trimmed.to_string())
}

/// Validate that `CODEX_HOME` is set to a non-blank, existing directory,
/// and returns its absolute canonicalized path.
/// Fails closed before any child process can be spawned.
pub fn validate_codex_home() -> Result<PathBuf, DaemonError> {
    let raw = std::env::var("CODEX_HOME").map_err(|_| {
        DaemonError::Config(
            "CODEX_HOME is not set; direct Codex launch requires explicit CODEX_HOME directory".to_string(),
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DaemonError::Config(
            "CODEX_HOME is blank; direct Codex launch requires explicit CODEX_HOME directory".to_string(),
        ));
    }
    let path = PathBuf::from(trimmed);
    if !path.exists() {
        return Err(DaemonError::Config(format!(
            "CODEX_HOME directory does not exist: {}",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(|e| {
        DaemonError::Config(format!(
            "failed to canonicalize CODEX_HOME {}: {e}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(DaemonError::Config(format!(
            "CODEX_HOME path is not a directory: {}",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Apply direct Claude scoping to a Command:
/// 1. Validates `DARK_FACTORY_CLAUDE_CONFIG_DIR` exists, is a directory, and canonicalizes it.
/// 2. Scrubs inherited Claude and conflicting provider authentication.
/// 3. Sets `CLAUDE_CONFIG_DIR` to that validated canonical directory.
pub fn apply_claude_scope(command: &mut Command) -> Result<(), DaemonError> {
    let validated_dir = validate_claude_config_dir()?;
    scrub_all_ai_provider_auth(command);
    command.env("CLAUDE_CONFIG_DIR", &validated_dir);
    Ok(())
}

/// Apply MiniMax scoping to a Command:
/// 1. Validates non-blank `MINIMAX_API_KEY`.
/// 2. Scrubs Claude account state/configuration and other provider auth.
/// 3. Pins endpoint and model to MiniMax.
pub fn apply_minimax_scope(command: &mut Command) -> Result<(), DaemonError> {
    let key = validate_minimax_key()?;
    scrub_all_ai_provider_auth(command);

    // Pin MiniMax-hosted Anthropic endpoint and models
    command.env("ANTHROPIC_BASE_URL", "https://api.minimax.io/anthropic");
    command.env("ANTHROPIC_MODEL", "MiniMax-M3");
    command.env("ANTHROPIC_SMALL_FAST_MODEL", "MiniMax-M3");
    command.env("ANTHROPIC_API_KEY", &key);
    command.env("ANTHROPIC_AUTH_TOKEN", &key);
    command.env("CLAUDEM_MODE", "1");
    command.env("CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL", "0");
    Ok(())
}

/// Apply Codex scoping to a Command:
/// 1. Validates `CODEX_HOME` exists, is a directory, and canonicalizes it.
/// 2. Scrubs conflicting provider authentication.
/// 3. Sets `CODEX_HOME` to that validated canonical directory.
pub fn apply_codex_scope(command: &mut Command) -> Result<(), DaemonError> {
    let validated_dir = validate_codex_home()?;
    scrub_all_ai_provider_auth(command);
    command.env("CODEX_HOME", &validated_dir);
    Ok(())
}

/// Validate that `DARK_FACTORY_AGY_HOME` is set to a non-blank, existing directory,
/// inspects actual Antigravity settings if present, and returns its absolute canonicalized path.
/// Fails closed before any child process can be spawned.
pub fn validate_agy_home() -> Result<PathBuf, DaemonError> {
    let raw = std::env::var("DARK_FACTORY_AGY_HOME").map_err(|_| {
        DaemonError::Config(
            "DARK_FACTORY_AGY_HOME is not set; direct Antigravity launch requires explicit profile home directory".to_string(),
        )
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DaemonError::Config(
            "DARK_FACTORY_AGY_HOME is blank; direct Antigravity launch requires explicit profile home directory".to_string(),
        ));
    }
    let path = PathBuf::from(trimmed);
    if !path.exists() {
        return Err(DaemonError::Config(format!(
            "DARK_FACTORY_AGY_HOME directory does not exist: {}",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(|e| {
        DaemonError::Config(format!(
            "failed to canonicalize DARK_FACTORY_AGY_HOME {}: {e}",
            path.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(DaemonError::Config(format!(
            "DARK_FACTORY_AGY_HOME path is not a directory: {}",
            canonical.display()
        )));
    }
    validate_agy_settings(&canonical)?;
    Ok(canonical)
}

/// Validate Antigravity configuration directory and settings file if present.
/// The canonical location per official Antigravity CLI documentation is:
/// `<HOME>/.gemini/antigravity-cli/settings.json`.
/// Default authentication uses OS keyring / native OAuth and requires omitting `modelProvider`.
/// API mode is supported ONLY when `settings.modelProvider` is the exact string `"gemini"`
/// and `GEMINI_API_KEY` is nonblank.
/// All unsupported explicit values, aliases, and malformed non-object settings fail closed with Err.
///
/// Returns `Ok(true)` if explicit Gemini API mode is configured and valid,
/// or `Ok(false)` if default native auth (field omitted or file absent) is configured and valid.
pub fn validate_agy_settings(home: &Path) -> Result<bool, DaemonError> {
    let settings_path = home.join(".gemini").join("antigravity-cli").join("settings.json");
    if !settings_path.exists() {
        return Ok(false);
    }
    if !settings_path.is_file() {
        return Err(DaemonError::Config(format!(
            "Antigravity settings path is not a file: {}",
            settings_path.display()
        )));
    }
    let content = std::fs::read_to_string(&settings_path).map_err(|e| {
        DaemonError::Config(format!(
            "failed to read Antigravity settings at {}: {e}",
            settings_path.display()
        ))
    })?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        DaemonError::Config(format!(
            "Antigravity settings at {} is not valid JSON: {e}",
            settings_path.display()
        ))
    })?;
    let obj = json.as_object().ok_or_else(|| {
        DaemonError::Config(format!(
            "Antigravity settings at {} must be a JSON object",
            settings_path.display()
        ))
    })?;
    if let Some(provider_val) = obj.get("modelProvider") {
        match provider_val.as_str() {
            Some("gemini") => {
                let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
                if key.trim().is_empty() {
                    return Err(DaemonError::Config(
                        "Antigravity settings specify modelProvider 'gemini' but GEMINI_API_KEY is not set or blank".to_string(),
                    ));
                }
                Ok(true)
            }
            Some(other) => {
                Err(DaemonError::Config(format!(
                    "unsupported Antigravity modelProvider '{other}'; only exact 'gemini' is supported or the field must be omitted for default auth"
                )))
            }
            None => {
                Err(DaemonError::Config(format!(
                    "unsupported Antigravity modelProvider value '{provider_val}'; must be exact string 'gemini' or omitted"
                )))
            }
        }
    } else {
        Ok(false)
    }
}

/// Check if the validated Antigravity settings explicitly specify API mode via `modelProvider == "gemini"`.
/// Reuses the shared validator so unsupported/malformed settings are rejected instead of returning false.
pub fn agy_is_gemini_api_mode(home: &Path) -> Result<bool, DaemonError> {
    validate_agy_settings(home)
}

/// Apply Antigravity scoping to a Command:
/// 1. Validates `DARK_FACTORY_AGY_HOME` exists, is a directory, inspects settings, and canonicalizes it.
/// 2. Validates optional `CODEX_HOME` and/or `DARK_FACTORY_CLAUDE_CONFIG_DIR` if present in environment.
/// 3. Scrubs conflicting AI provider authentication (including ambient GEMINI_API_KEY).
/// 4. Pins child `HOME` to the validated native AGY home directory.
/// 5. Restores validated `GEMINI_API_KEY` ONLY when explicitly configured in `modelProvider == "gemini"`.
/// 6. Pins optional validated `CODEX_HOME` and/or `CLAUDE_CONFIG_DIR` if present.
pub fn apply_agy_scope(command: &mut Command) -> Result<(), DaemonError> {
    let agy_home = validate_agy_home()?;
    let is_gemini_api = validate_agy_settings(&agy_home)?;
    let codex_home = if std::env::var_os("CODEX_HOME").is_some() {
        Some(validate_codex_home()?)
    } else {
        None
    };
    let claude_dir = if std::env::var_os("DARK_FACTORY_CLAUDE_CONFIG_DIR").is_some() {
        Some(validate_claude_config_dir()?)
    } else {
        None
    };
    scrub_all_ai_provider_auth(command);
    command.env("HOME", &agy_home);
    if is_gemini_api {
        let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
        command.env("GEMINI_API_KEY", key);
    }
    if let Some(dir) = codex_home {
        command.env("CODEX_HOME", dir);
    }
    if let Some(dir) = claude_dir {
        command.env("CLAUDE_CONFIG_DIR", dir);
    }
    Ok(())
}

/// Scoped Cursor configuration resolved from environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedCursorConfig {
    pub api_key: Option<String>,
    pub config_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

/// Scoped Gemini configuration resolved from environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedGeminiConfig {
    pub api_key: Option<String>,
    pub home: Option<PathBuf>,
}

/// Validate Cursor scoping configuration.
///
/// Requires at least one of:
/// - `CURSOR_API_KEY` set to a non-blank string
/// - `DARK_FACTORY_CURSOR_CONFIG_DIR` set to an existing directory
/// - `DARK_FACTORY_CURSOR_HOME` set to an existing directory
///
/// Returns validated `ScopedCursorConfig`.
/// Fails closed if none is set, or if specified directories do not exist.
pub fn validate_cursor_scope() -> Result<ScopedCursorConfig, DaemonError> {
    let mut key_opt = None;
    if let Ok(key) = std::env::var("CURSOR_API_KEY") {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            key_opt = Some(trimmed.to_string());
        }
    }

    let mut config_dir_opt = None;
    if let Ok(raw) = std::env::var("DARK_FACTORY_CURSOR_CONFIG_DIR") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if !path.exists() {
                return Err(DaemonError::Config(format!(
                    "DARK_FACTORY_CURSOR_CONFIG_DIR directory does not exist: {}",
                    path.display()
                )));
            }
            let canonical = path.canonicalize().map_err(|e| {
                DaemonError::Config(format!(
                    "failed to canonicalize DARK_FACTORY_CURSOR_CONFIG_DIR {}: {e}",
                    path.display()
                ))
            })?;
            if !canonical.is_dir() {
                return Err(DaemonError::Config(format!(
                    "DARK_FACTORY_CURSOR_CONFIG_DIR path is not a directory: {}",
                    canonical.display()
                )));
            }
            config_dir_opt = Some(canonical);
        }
    }

    let mut home_opt = None;
    if let Ok(raw) = std::env::var("DARK_FACTORY_CURSOR_HOME") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if !path.exists() {
                return Err(DaemonError::Config(format!(
                    "DARK_FACTORY_CURSOR_HOME directory does not exist: {}",
                    path.display()
                )));
            }
            let canonical = path.canonicalize().map_err(|e| {
                DaemonError::Config(format!(
                    "failed to canonicalize DARK_FACTORY_CURSOR_HOME {}: {e}",
                    path.display()
                ))
            })?;
            if !canonical.is_dir() {
                return Err(DaemonError::Config(format!(
                    "DARK_FACTORY_CURSOR_HOME path is not a directory: {}",
                    canonical.display()
                )));
            }
            home_opt = Some(canonical);
        }
    }

    if key_opt.is_none() && config_dir_opt.is_none() && home_opt.is_none() {
        return Err(DaemonError::Config(
            "direct Cursor launch requires explicit DARK_FACTORY_CURSOR_CONFIG_DIR, DARK_FACTORY_CURSOR_HOME, or CURSOR_API_KEY".to_string(),
        ));
    }

    Ok(ScopedCursorConfig {
        api_key: key_opt,
        config_dir: config_dir_opt,
        home: home_opt,
    })
}

/// Apply direct Cursor scoping to a Command:
/// 1. Validates `CURSOR_API_KEY`, `DARK_FACTORY_CURSOR_CONFIG_DIR`, and/or `DARK_FACTORY_CURSOR_HOME`.
/// 2. Scrubs inherited AI provider authentication.
/// 3. Applies validated `CURSOR_API_KEY`, `CURSOR_CONFIG_DIR`, and/or `HOME`.
///    Pins `HOME` to an isolated directory if neither explicit HOME nor CONFIG_DIR is provided,
///    preventing ambient ~/.cursor token leaks.
pub fn apply_cursor_scope(command: &mut Command) -> Result<(), DaemonError> {
    let config = validate_cursor_scope()?;
    scrub_all_ai_provider_auth(command);
    if let Some(key) = config.api_key {
        command.env("CURSOR_API_KEY", key);
    }
    if let Some(ref dir) = config.config_dir {
        command.env("CURSOR_CONFIG_DIR", dir);
    }
    if let Some(home) = config.home {
        command.env("HOME", home);
    } else if let Some(dir) = config.config_dir {
        command.env("HOME", dir);
    } else {
        command.env("HOME", std::env::temp_dir());
    }
    Ok(())
}

/// Validate Gemini scoping configuration.
///
/// Requires at least one of:
/// - `GEMINI_API_KEY` set to a non-blank string
/// - `DARK_FACTORY_GEMINI_HOME` set to an existing directory
///
/// Returns validated `ScopedGeminiConfig`.
/// Fails closed if neither is set, or if specified directory does not exist.
pub fn validate_gemini_scope() -> Result<ScopedGeminiConfig, DaemonError> {
    let mut key_opt = None;
    if let Ok(key) = std::env::var("GEMINI_API_KEY") {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            key_opt = Some(trimmed.to_string());
        }
    }

    let mut home_opt = None;
    if let Ok(raw) = std::env::var("DARK_FACTORY_GEMINI_HOME") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if !path.exists() {
                return Err(DaemonError::Config(format!(
                    "DARK_FACTORY_GEMINI_HOME directory does not exist: {}",
                    path.display()
                )));
            }
            let canonical = path.canonicalize().map_err(|e| {
                DaemonError::Config(format!(
                    "failed to canonicalize DARK_FACTORY_GEMINI_HOME {}: {e}",
                    path.display()
                ))
            })?;
            if !canonical.is_dir() {
                return Err(DaemonError::Config(format!(
                    "DARK_FACTORY_GEMINI_HOME path is not a directory: {}",
                    canonical.display()
                )));
            }
            home_opt = Some(canonical);
        }
    }

    if key_opt.is_none() && home_opt.is_none() {
        return Err(DaemonError::Config(
            "direct Gemini launch requires explicit DARK_FACTORY_GEMINI_HOME or GEMINI_API_KEY".to_string(),
        ));
    }

    Ok(ScopedGeminiConfig {
        api_key: key_opt,
        home: home_opt,
    })
}

/// Apply direct Gemini scoping to a Command:
/// 1. Validates `GEMINI_API_KEY` and/or `DARK_FACTORY_GEMINI_HOME`.
/// 2. Scrubs inherited AI provider authentication.
/// 3. Applies validated `GEMINI_API_KEY` and/or `HOME`.
///    Pins `HOME` to an isolated directory if explicit HOME is omitted,
///    preventing ambient ~/.gemini token leaks.
pub fn apply_gemini_scope(command: &mut Command) -> Result<(), DaemonError> {
    let config = validate_gemini_scope()?;
    scrub_all_ai_provider_auth(command);
    if let Some(key) = config.api_key {
        command.env("GEMINI_API_KEY", key);
    }
    if let Some(home) = config.home {
        command.env("HOME", home);
    } else {
        command.env("HOME", std::env::temp_dir());
    }
    Ok(())
}

/// Apply scoping for the given `AiProvider`.
pub fn apply_provider_scope(provider: AiProvider, command: &mut Command) -> Result<(), DaemonError> {
    match provider {
        AiProvider::Claude => apply_claude_scope(command),
        AiProvider::MiniMax => apply_minimax_scope(command),
        AiProvider::Codex => apply_codex_scope(command),
        AiProvider::Antigravity => apply_agy_scope(command),
        AiProvider::Cursor => apply_cursor_scope(command),
        AiProvider::Gemini => apply_gemini_scope(command),
    }
}

/// Check if a URL string has the exact hostname `api.minimax.io`.
pub fn is_minimax_api_url(url_str: &str) -> bool {
    let trimmed = url_str.trim();
    let without_scheme = if let Some(rest) = trimmed.strip_prefix("https://") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        rest
    } else {
        trimmed
    };
    let host_and_user = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim();
    let host_and_port = if let Some((_, h)) = host_and_user.rsplit_once('@') {
        h
    } else {
        host_and_user
    };
    let host = host_and_port
        .split(':')
        .next()
        .unwrap_or("")
        .trim();
    host.eq_ignore_ascii_case("api.minimax.io")
}

/// Detect whether `cmd` is an intended direct AI CLI program by inspecting its binary basename.
/// Direct CLI programs:
/// - `codex` -> `AiProvider::Codex`
/// - `agy`, `antigravity` -> `AiProvider::Antigravity`
/// - `gemini` -> `AiProvider::Gemini`
/// - `cursor-agent`, `agentf`, `cursor` -> `AiProvider::Cursor`
/// - `claude`, `claude-sonnet` -> `AiProvider::MiniMax` if explicitly indicated by `extra_env`,
///   otherwise `AiProvider::Claude`.
///   Normal non-AI commands (e.g. `git`, `br`, `gh`, `sh`, `cargo`) return `None`.
pub fn detect_direct_cli_provider(cmd: &str, extra_env: &[(&str, &str)]) -> Option<AiProvider> {
    let bin = Path::new(cmd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cmd);

    if bin == "codex" {
        Some(AiProvider::Codex)
    } else if bin == "agy" || bin == "antigravity" {
        Some(AiProvider::Antigravity)
    } else if bin == "gemini" {
        Some(AiProvider::Gemini)
    } else if bin == "cursor-agent" || bin == "agentf" || bin == "cursor" {
        Some(AiProvider::Cursor)
    } else if bin == "claude" || bin == "claude-sonnet" {
        // If extra_env explicitly indicates MiniMax, route through MiniMax scope
        if extra_env.iter().any(|(k, v)| {
            (*k == "CLAUDEM_MODE" && *v == "1")
                || (*k == "ANTHROPIC_BASE_URL" && is_minimax_api_url(v))
                || (*k == "ANTHROPIC_MODEL" && v.eq_ignore_ascii_case("MiniMax-M3"))
        }) {
            Some(AiProvider::MiniMax)
        } else {
            Some(AiProvider::Claude)
        }
    } else {
        None
    }
}

/// Apply direct CLI scoping if `cmd` is an intended AI CLI program.
/// Must be applied AFTER `extra_env` overrides so callers cannot bypass safety
/// by passing conflicting credentials in `extra_env`.
pub fn apply_direct_cli_scope(
    cmd: &str,
    extra_env: &[(&str, &str)],
    command: &mut Command,
) -> Result<(), DaemonError> {
    if let Some(provider) = detect_direct_cli_provider(cmd, extra_env) {
        apply_provider_scope(provider, command)
    } else {
        Ok(())
    }
}

/// Helper for AO worker dispatch (`ao spawn`): validates that the selected
/// agent's required account scoping is configured before spawning the worker,
/// and applies the scoped environment while scrubbing conflicting provider auth.
///
/// Supported agents:
/// - `claude` / `claude-code` / `claude-sonnet`: applies direct Claude scoping
/// - `codex`: applies direct Codex scoping
/// - `minimax` / `claudem`: applies direct MiniMax scoping
/// - `antigravity` / `agy`: requires `DARK_FACTORY_AGY_HOME`, validates optional Codex/Claude scope, and scrubs auth
/// - any other agent (including `cursor-agent`, `agentf`, `cursor`, `gemini`): scrubs all auth and returns `Err(DaemonError::Config(...))` so unknown agents fail closed.
pub fn validate_ao_worker_agent_scope(
    agent: &str,
    command: &mut Command,
) -> Result<(), DaemonError> {
    let normalized = agent.trim().to_lowercase();
    if normalized == "claude"
        || normalized == "claude-code"
        || normalized == "claude-sonnet"
    {
        apply_claude_scope(command)
    } else if normalized == "codex" {
        apply_codex_scope(command)
    } else if normalized == "minimax" || normalized == "claudem" {
        apply_minimax_scope(command)
    } else if normalized == "antigravity" || normalized == "agy" {
        apply_agy_scope(command)
    } else {
        scrub_all_ai_provider_auth(command);
        Err(DaemonError::Config(format!(
            "Unsupported AO worker agent: '{agent}'; failed closed"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn env_lock() -> &'static Mutex<()> {
        crate::test_env_lock()
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("df_test_scope_{prefix}_{}_{nanos}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct EnvRestore {
        values: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvRestore {
        fn new(keys: &[&'static str]) -> Self {
            Self {
                values: keys.iter().map(|key| (*key, std::env::var_os(key))).collect(),
            }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (key, value) in self.values.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn test_scrub_all_ai_provider_auth() {
        let mut cmd = Command::new("test");
        for var in SCRUBBED_AUTH_VARS {
            cmd.env(var, "dummy_value");
        }
        scrub_all_ai_provider_auth(&mut cmd);
        for (k, v) in cmd.get_envs() {
            let k_str = k.to_str().unwrap();
            if SCRUBBED_AUTH_VARS.contains(&k_str) {
                assert!(v.is_none(), "expected {k_str} to be removed from env");
            }
        }
    }

    #[test]
    fn test_claude_scope_validation() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("DARK_FACTORY_CLAUDE_CONFIG_DIR");

        // Missing
        std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        assert!(validate_claude_config_dir().is_err());

        // Blank
        std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", "   ");
        assert!(validate_claude_config_dir().is_err());

        // Non-existent
        std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", "/nonexistent/path/for/test/12345");
        assert!(validate_claude_config_dir().is_err());

        // Path is a file, not directory
        let temp = TempDir::new("file_not_dir");
        let file_path = temp.path.join("a_file");
        std::fs::write(&file_path, "content").unwrap();
        std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", &file_path);
        assert!(validate_claude_config_dir().is_err());

        // Valid directory (must be canonicalized)
        let dir_path = temp.path.join("valid_claude_dir");
        std::fs::create_dir_all(&dir_path).unwrap();
        std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", &dir_path);
        let res = validate_claude_config_dir();
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), dir_path.canonicalize().unwrap());

        // Restore
        match prior {
            Some(v) => std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR"),
        }
    }

    #[test]
    fn test_codex_scope_validation() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("CODEX_HOME");

        // Missing
        std::env::remove_var("CODEX_HOME");
        assert!(validate_codex_home().is_err());

        // Blank
        std::env::set_var("CODEX_HOME", "   ");
        assert!(validate_codex_home().is_err());

        // Non-existent
        std::env::set_var("CODEX_HOME", "/nonexistent/path/for/test/codex12345");
        assert!(validate_codex_home().is_err());

        // Path is a file, not directory
        let temp = TempDir::new("codex_file_not_dir");
        let file_path = temp.path.join("a_file");
        std::fs::write(&file_path, "content").unwrap();
        std::env::set_var("CODEX_HOME", &file_path);
        assert!(validate_codex_home().is_err());

        // Valid directory (must be canonicalized)
        let dir_path = temp.path.join("valid_codex_dir");
        std::fs::create_dir_all(&dir_path).unwrap();
        std::env::set_var("CODEX_HOME", &dir_path);
        let res = validate_codex_home();
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), dir_path.canonicalize().unwrap());

        // Restore
        match prior {
            Some(v) => std::env::set_var("CODEX_HOME", v),
            None => std::env::remove_var("CODEX_HOME"),
        }
    }

    #[test]
    fn test_minimax_key_validation() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("MINIMAX_API_KEY");

        // Missing
        std::env::remove_var("MINIMAX_API_KEY");
        let err = validate_minimax_key().unwrap_err();
        assert!(!format!("{err}").contains("secret"));

        // Blank
        std::env::set_var("MINIMAX_API_KEY", "   ");
        let err = validate_minimax_key().unwrap_err();
        assert!(!format!("{err}").contains("secret"));

        // Valid key
        std::env::set_var("MINIMAX_API_KEY", "test-synthetic-minimax-key-123");
        let res = validate_minimax_key();
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), "test-synthetic-minimax-key-123");

        // Restore
        match prior {
            Some(v) => std::env::set_var("MINIMAX_API_KEY", v),
            None => std::env::remove_var("MINIMAX_API_KEY"),
        }
    }

    #[test]
    fn test_minimax_url_detection() {
        assert!(is_minimax_api_url("https://api.minimax.io/anthropic"));
        assert!(is_minimax_api_url("http://api.minimax.io:8080/anthropic"));
        assert!(is_minimax_api_url("https://api.minimax.io"));
        assert!(is_minimax_api_url("https://API.MINIMAX.IO/anthropic"));
        assert!(is_minimax_api_url("api.minimax.io/anthropic"));

        // Reject non-exact hostnames
        assert!(!is_minimax_api_url("https://evil-api.minimax.io/anthropic"));
        assert!(!is_minimax_api_url("https://attacker.com/api.minimax.io"));
        assert!(!is_minimax_api_url("https://api.minimax.io.attacker.com"));
        assert!(!is_minimax_api_url("https://api.minimax.io@evil.com/path"));
        assert!(!is_minimax_api_url("https://otherhost.org/minimax.io"));
    }

    #[test]
    fn test_detect_direct_cli_provider() {
        assert_eq!(detect_direct_cli_provider("codex", &[]), Some(AiProvider::Codex));
        assert_eq!(detect_direct_cli_provider("/opt/bin/codex", &[]), Some(AiProvider::Codex));

        assert_eq!(detect_direct_cli_provider("agy", &[]), Some(AiProvider::Antigravity));
        assert_eq!(detect_direct_cli_provider("/opt/bin/agy", &[]), Some(AiProvider::Antigravity));
        assert_eq!(detect_direct_cli_provider("antigravity", &[]), Some(AiProvider::Antigravity));
        assert_eq!(detect_direct_cli_provider("gemini", &[]), Some(AiProvider::Gemini));
        assert_eq!(detect_direct_cli_provider("/opt/bin/gemini", &[]), Some(AiProvider::Gemini));

        assert_eq!(detect_direct_cli_provider("claude", &[]), Some(AiProvider::Claude));
        assert_eq!(detect_direct_cli_provider("/home/user/.nvm/versions/node/v22.22.0/bin/claude", &[]), Some(AiProvider::Claude));
        assert_eq!(detect_direct_cli_provider("claude-sonnet", &[]), Some(AiProvider::Claude));
        assert_eq!(detect_direct_cli_provider("cursor-agent", &[]), Some(AiProvider::Cursor));
        assert_eq!(detect_direct_cli_provider("/usr/local/bin/cursor-agent", &[]), Some(AiProvider::Cursor));
        assert_eq!(detect_direct_cli_provider("agentf", &[]), Some(AiProvider::Cursor));
        assert_eq!(detect_direct_cli_provider("cursor", &[]), Some(AiProvider::Cursor));

        // MiniMax via extra_env
        assert_eq!(
            detect_direct_cli_provider("claude", &[("CLAUDEM_MODE", "1")]),
            Some(AiProvider::MiniMax)
        );
        assert_eq!(
            detect_direct_cli_provider(
                "/usr/local/bin/claude",
                &[("ANTHROPIC_BASE_URL", "https://api.minimax.io/anthropic")]
            ),
            Some(AiProvider::MiniMax)
        );
        // Non-minimax host does not trigger MiniMax provider
        assert_eq!(
            detect_direct_cli_provider(
                "/usr/local/bin/claude",
                &[("ANTHROPIC_BASE_URL", "https://evil.com/minimax.io")]
            ),
            Some(AiProvider::Claude)
        );

        // Non-AI tools preserved
        assert_eq!(detect_direct_cli_provider("git", &[]), None);
        assert_eq!(detect_direct_cli_provider("br", &[]), None);
        assert_eq!(detect_direct_cli_provider("gh", &[]), None);
        assert_eq!(detect_direct_cli_provider("sh", &[]), None);
        assert_eq!(detect_direct_cli_provider("/bin/sh", &[]), None);
        assert_eq!(detect_direct_cli_provider("true", &[]), None);
    }

    #[test]
    fn test_agy_scope_validation() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::new(&["DARK_FACTORY_AGY_HOME", "GEMINI_API_KEY"]);

        // Missing
        std::env::remove_var("DARK_FACTORY_AGY_HOME");
        assert!(validate_agy_home().is_err());

        // Blank
        std::env::set_var("DARK_FACTORY_AGY_HOME", "   ");
        assert!(validate_agy_home().is_err());

        // Non-existent
        std::env::set_var("DARK_FACTORY_AGY_HOME", "/nonexistent/path/for/test/agy12345");
        assert!(validate_agy_home().is_err());

        // Path is a file, not directory
        let temp = TempDir::new("agy_file_not_dir");
        let file_path = temp.path.join("a_file");
        std::fs::write(&file_path, "content").unwrap();
        std::env::set_var("DARK_FACTORY_AGY_HOME", &file_path);
        assert!(validate_agy_home().is_err());

        // Valid directory without settings.json (default native OAuth mode)
        let dir_path = temp.path.join("valid_agy_dir");
        std::fs::create_dir_all(&dir_path).unwrap();
        std::env::set_var("DARK_FACTORY_AGY_HOME", &dir_path);
        let res = validate_agy_home();
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), dir_path.canonicalize().unwrap());

        // Valid directory with settings.json omitting modelProvider (default native OAuth mode)
        let settings_dir = dir_path.join(".gemini").join("antigravity-cli");
        std::fs::create_dir_all(&settings_dir).unwrap();
        let settings_file = settings_dir.join("settings.json");
        std::fs::write(&settings_file, r#"{"theme":"dark"}"#).unwrap();
        assert!(validate_agy_home().is_ok());
        let mut cmd = Command::new("dummy");
        assert!(apply_agy_scope(&mut cmd).is_ok());

        // Valid directory with settings.json specifying gemini API mode without GEMINI_API_KEY fails before child construction
        std::env::remove_var("GEMINI_API_KEY");
        std::fs::write(&settings_file, r#"{"modelProvider":"gemini"}"#).unwrap();
        assert!(validate_agy_home().is_err());
        let mut cmd = Command::new("dummy");
        assert!(apply_agy_scope(&mut cmd).is_err());

        // Blank GEMINI_API_KEY also fails before child construction
        std::env::set_var("GEMINI_API_KEY", "   ");
        assert!(validate_agy_home().is_err());
        let mut cmd = Command::new("dummy");
        assert!(apply_agy_scope(&mut cmd).is_err());

        // Valid directory with settings.json specifying exact string "gemini" with nonblank GEMINI_API_KEY succeeds
        std::env::set_var("GEMINI_API_KEY", "test-synthetic-gemini-key");
        assert!(validate_agy_home().is_ok());
        let mut cmd = Command::new("dummy");
        assert!(apply_agy_scope(&mut cmd).is_ok());

        // Unsupported aliases and casing/whitespace variations fail before child construction
        let unsupported_aliases = [
            r#"{"modelProvider":"google"}"#,
            r#"{"modelProvider":"native"}"#,
            r#"{"modelProvider":"oauth"}"#,
            r#"{"modelProvider":"Gemini"}"#,
            r#"{"modelProvider":"GEMINI"}"#,
            r#"{"modelProvider":" gemini "}"#,
            "{\"modelProvider\":\"gemini\\n\"}",
            r#"{"modelProvider":"openai"}"#,
            r#"{"modelProvider":"unsupported_provider"}"#,
        ];
        for alias_json in unsupported_aliases {
            std::fs::write(&settings_file, alias_json).unwrap();
            assert!(
                validate_agy_home().is_err(),
                "Expected failure for unsupported alias: {alias_json}"
            );
            let mut cmd = Command::new("dummy");
            assert!(
                apply_agy_scope(&mut cmd).is_err(),
                "apply_agy_scope must fail before child construction for: {alias_json}"
            );
        }

        // Malformed settings, non-object JSON, and explicit unsupported types fail before child construction
        let malformed_or_unsupported_types = [
            "{not json}",
            "[1, 2]",
            "\"gemini\"",
            "123",
            "true",
            r#"{"modelProvider":null}"#,
            r#"{"modelProvider":123}"#,
            r#"{"modelProvider":true}"#,
            r#"{"modelProvider":["gemini"]}"#,
            r#"{"modelProvider":{"name":"gemini"}}"#,
        ];
        for malformed in malformed_or_unsupported_types {
            std::fs::write(&settings_file, malformed).unwrap();
            assert!(
                validate_agy_home().is_err(),
                "Expected failure for malformed/unsupported settings: {malformed}"
            );
            let mut cmd = Command::new("dummy");
            assert!(
                apply_agy_scope(&mut cmd).is_err(),
                "apply_agy_scope must fail before child construction for: {malformed}"
            );
        }
    }

    #[test]
    fn test_validate_ao_worker_agent_scope() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::new(&[
            "DARK_FACTORY_CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "MINIMAX_API_KEY",
            "DARK_FACTORY_AGY_HOME",
        ]);

        // Clean env
        std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        std::env::remove_var("CODEX_HOME");
        std::env::remove_var("MINIMAX_API_KEY");
        std::env::remove_var("DARK_FACTORY_AGY_HOME");

        let mut cmd = Command::new("dummy");

        // 1. claude without config dir fails closed
        assert!(validate_ao_worker_agent_scope("claude", &mut cmd).is_err());
        assert!(validate_ao_worker_agent_scope("claude-code", &mut cmd).is_err());
        assert!(validate_ao_worker_agent_scope("claude-sonnet", &mut cmd).is_err());

        // claude with valid config dir succeeds
        let temp = TempDir::new("ao_worker_claude");
        std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", &temp.path);
        let mut cmd = Command::new("dummy");
        cmd.env("OPENAI_API_KEY", "dirty_openai");
        assert!(validate_ao_worker_agent_scope("claude", &mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("CLAUDE_CONFIG_DIR") && v.is_some()));
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("OPENAI_API_KEY") && v.is_none()));

        // 2. codex without config dir fails closed
        let mut cmd = Command::new("dummy");
        assert!(validate_ao_worker_agent_scope("codex", &mut cmd).is_err());

        // codex with valid CODEX_HOME succeeds
        let temp_codex = TempDir::new("ao_worker_codex");
        std::env::set_var("CODEX_HOME", &temp_codex.path);
        let mut cmd = Command::new("dummy");
        cmd.env("ANTHROPIC_API_KEY", "dirty_anthropic");
        assert!(validate_ao_worker_agent_scope("codex", &mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("CODEX_HOME") && v.is_some()));
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("ANTHROPIC_API_KEY") && v.is_none()));

        // 3. minimax / claudem without key fails closed
        let mut cmd = Command::new("dummy");
        assert!(validate_ao_worker_agent_scope("minimax", &mut cmd).is_err());
        assert!(validate_ao_worker_agent_scope("claudem", &mut cmd).is_err());

        // minimax with valid key succeeds
        std::env::set_var("MINIMAX_API_KEY", "valid_test_key");
        let mut cmd = Command::new("dummy");
        assert!(validate_ao_worker_agent_scope("minimax", &mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("ANTHROPIC_MODEL") && v.map(|x| x.to_str().unwrap()) == Some("MiniMax-M3")));
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("ANTHROPIC_BASE_URL") && v.map(|x| x.to_str().unwrap()) == Some("https://api.minimax.io/anthropic")));

        // 4. antigravity / agy without DARK_FACTORY_AGY_HOME fails closed
        let mut cmd = Command::new("dummy");
        assert!(validate_ao_worker_agent_scope("antigravity", &mut cmd).is_err());
        let mut cmd = Command::new("dummy");
        assert!(validate_ao_worker_agent_scope("agy", &mut cmd).is_err());

        // antigravity / agy with valid DARK_FACTORY_AGY_HOME succeeds and pins HOME
        let temp_agy = TempDir::new("ao_worker_agy");
        std::env::set_var("DARK_FACTORY_AGY_HOME", &temp_agy.path);
        let mut cmd = Command::new("dummy");
        cmd.env("ANTHROPIC_API_KEY", "dirty_key");
        assert!(validate_ao_worker_agent_scope("antigravity", &mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("HOME") && v.is_some()));
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("ANTHROPIC_API_KEY") && v.is_none()));

        let mut cmd = Command::new("dummy");
        assert!(validate_ao_worker_agent_scope("agy", &mut cmd).is_ok());

        // 5. Non-AO-worker agents (cursor, gemini, arbitrary) fail closed and scrub auth
        for non_worker in ["cursor-agent", "agentf", "cursor", "gemini", "unknown-llm-agent"] {
            let mut cmd = Command::new("dummy");
            cmd.env("OPENAI_API_KEY", "dirty_key");
            let err = validate_ao_worker_agent_scope(non_worker, &mut cmd);
            assert!(err.is_err(), "Expected {non_worker} to fail closed as AO worker");
            assert!(
                format!("{}", err.unwrap_err()).contains("Unsupported AO worker agent"),
                "Expected unsupported agent error for {non_worker}"
            );
            let envs: Vec<_> = cmd.get_envs().collect();
            assert!(envs.iter().any(|(k, v)| k.to_str() == Some("OPENAI_API_KEY") && v.is_none()));
        }
    }

    #[test]
    fn test_cursor_scope_validation() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::new(&[
            "CURSOR_API_KEY",
            "DARK_FACTORY_CURSOR_CONFIG_DIR",
            "DARK_FACTORY_CURSOR_HOME",
        ]);

        std::env::remove_var("CURSOR_API_KEY");
        std::env::remove_var("DARK_FACTORY_CURSOR_CONFIG_DIR");
        std::env::remove_var("DARK_FACTORY_CURSOR_HOME");

        // Missing all fails closed
        assert!(validate_cursor_scope().is_err());
        let mut cmd = Command::new("dummy");
        assert!(apply_cursor_scope(&mut cmd).is_err());

        // Blank fails closed
        std::env::set_var("CURSOR_API_KEY", "   ");
        std::env::set_var("DARK_FACTORY_CURSOR_CONFIG_DIR", "   ");
        std::env::set_var("DARK_FACTORY_CURSOR_HOME", "   ");
        assert!(validate_cursor_scope().is_err());
        let mut cmd = Command::new("dummy");
        assert!(apply_cursor_scope(&mut cmd).is_err());

        // Non-existent directory fails closed
        std::env::remove_var("CURSOR_API_KEY");
        std::env::set_var("DARK_FACTORY_CURSOR_CONFIG_DIR", "/nonexistent/cursor/dir/99999");
        assert!(validate_cursor_scope().is_err());

        // Valid CURSOR_API_KEY succeeds
        std::env::remove_var("DARK_FACTORY_CURSOR_CONFIG_DIR");
        std::env::remove_var("DARK_FACTORY_CURSOR_HOME");
        std::env::set_var("CURSOR_API_KEY", "test-synthetic-cursor-key");
        assert!(validate_cursor_scope().is_ok());
        let mut cmd = Command::new("dummy");
        cmd.env("OPENAI_API_KEY", "dirty_key");
        assert!(apply_cursor_scope(&mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("CURSOR_API_KEY") && v.map(|x| x.to_str().unwrap()) == Some("test-synthetic-cursor-key")));
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("OPENAI_API_KEY") && v.is_none()));

        // Valid DARK_FACTORY_CURSOR_CONFIG_DIR succeeds
        let temp = TempDir::new("cursor_config_dir");
        std::env::remove_var("CURSOR_API_KEY");
        std::env::set_var("DARK_FACTORY_CURSOR_CONFIG_DIR", &temp.path);
        let mut cmd = Command::new("dummy");
        assert!(apply_cursor_scope(&mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("CURSOR_CONFIG_DIR") && v.is_some()));

        // Valid DARK_FACTORY_CURSOR_HOME succeeds
        std::env::remove_var("DARK_FACTORY_CURSOR_CONFIG_DIR");
        std::env::set_var("DARK_FACTORY_CURSOR_HOME", &temp.path);
        let mut cmd = Command::new("dummy");
        assert!(apply_cursor_scope(&mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("HOME") && v.is_some()));
    }

    #[test]
    fn test_gemini_scope_validation() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::new(&[
            "GEMINI_API_KEY",
            "DARK_FACTORY_GEMINI_HOME",
        ]);

        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("DARK_FACTORY_GEMINI_HOME");

        // Missing fails closed
        assert!(validate_gemini_scope().is_err());
        let mut cmd = Command::new("dummy");
        assert!(apply_gemini_scope(&mut cmd).is_err());

        // Blank fails closed
        std::env::set_var("GEMINI_API_KEY", "   ");
        std::env::set_var("DARK_FACTORY_GEMINI_HOME", "   ");
        assert!(validate_gemini_scope().is_err());
        let mut cmd = Command::new("dummy");
        assert!(apply_gemini_scope(&mut cmd).is_err());

        // Non-existent directory fails closed
        std::env::remove_var("GEMINI_API_KEY");
        std::env::set_var("DARK_FACTORY_GEMINI_HOME", "/nonexistent/gemini/dir/99999");
        assert!(validate_gemini_scope().is_err());

        // Valid GEMINI_API_KEY succeeds
        std::env::remove_var("DARK_FACTORY_GEMINI_HOME");
        std::env::set_var("GEMINI_API_KEY", "test-synthetic-gemini-key");
        assert!(validate_gemini_scope().is_ok());
        let mut cmd = Command::new("dummy");
        cmd.env("ANTHROPIC_API_KEY", "dirty_key");
        assert!(apply_gemini_scope(&mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("GEMINI_API_KEY") && v.map(|x| x.to_str().unwrap()) == Some("test-synthetic-gemini-key")));
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("ANTHROPIC_API_KEY") && v.is_none()));

        // Valid DARK_FACTORY_GEMINI_HOME succeeds
        let temp = TempDir::new("gemini_home_dir");
        std::env::remove_var("GEMINI_API_KEY");
        std::env::set_var("DARK_FACTORY_GEMINI_HOME", &temp.path);
        let mut cmd = Command::new("dummy");
        assert!(apply_gemini_scope(&mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("HOME") && v.is_some()));
    }

    #[test]
    fn direct_agy_rejects_missing_invalid_and_conflicting_scope() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let _restore = EnvRestore::new(&[
            "DARK_FACTORY_AGY_HOME",
            "CODEX_HOME",
            "DARK_FACTORY_CLAUDE_CONFIG_DIR",
        ]);
        std::env::remove_var("DARK_FACTORY_AGY_HOME");
        std::env::remove_var("CODEX_HOME");
        std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        assert!(apply_direct_cli_scope("agy", &[], &mut Command::new("sh")).is_err());
        let temp = TempDir::new("agy_invalid_scope");
        std::env::set_var("DARK_FACTORY_AGY_HOME", temp.path.join("missing"));
        assert!(apply_direct_cli_scope("/synthetic/bin/agy", &[], &mut Command::new("sh")).is_err());
        std::env::set_var("DARK_FACTORY_AGY_HOME", &temp.path);
        std::env::set_var("CODEX_HOME", temp.path.join("missing_codex"));
        assert!(apply_direct_cli_scope("agy", &[], &mut Command::new("sh")).is_err());
        std::env::remove_var("CODEX_HOME");
        std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", temp.path.join("missing_claude"));
        assert!(apply_direct_cli_scope("agy", &[], &mut Command::new("sh")).is_err());
        std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        assert!(apply_direct_cli_scope("agy", &[], &mut Command::new("sh")).is_ok());
    }

    #[cfg(unix)]
    fn child_scope_env(command: &mut Command) -> String {
        let output = command
            .arg("-c")
            .arg("printf '%s\\n' \"ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY-}\" \"ANTHROPIC_AUTH_TOKEN=${ANTHROPIC_AUTH_TOKEN-}\" \"ANTHROPIC_BASE_URL=${ANTHROPIC_BASE_URL-}\" \"ANTHROPIC_MODEL=${ANTHROPIC_MODEL-}\" \"CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR-}\" \"CODEX_HOME=${CODEX_HOME-}\" \"GEMINI_API_KEY=${GEMINI_API_KEY-}\" \"CURSOR_API_KEY=${CURSOR_API_KEY-}\" \"CURSOR_CONFIG_DIR=${CURSOR_CONFIG_DIR-}\" \"HOME=${HOME-}\" \"MINIMAX_API_KEY=${MINIMAX_API_KEY-}\" \"OPENAI_API_KEY=${OPENAI_API_KEY-}\"")
            .output()
            .expect("synthetic scope child must spawn");
        assert!(output.status.success(), "synthetic scope child failed: {output:?}");
        String::from_utf8(output.stdout).expect("synthetic scope child output must be UTF-8")
    }

    #[test]
    #[cfg(unix)]
    fn scoped_provider_environment_is_scrubbed_at_child_process_boundary() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let temp = TempDir::new("child_scope");
        let claude_dir = temp.path.join("claude");
        let codex_dir = temp.path.join("codex");
        let agy_dir = temp.path.join("agy");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::create_dir_all(&agy_dir).unwrap();

        let prior_claude = std::env::var_os("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        let prior_codex = std::env::var_os("CODEX_HOME");
        let prior_minimax = std::env::var_os("MINIMAX_API_KEY");
        let prior_gemini = std::env::var_os("GEMINI_API_KEY");
        let prior_agy = std::env::var_os("DARK_FACTORY_AGY_HOME");
        let prior_cursor_key = std::env::var_os("CURSOR_API_KEY");
        let prior_cursor_dir = std::env::var_os("DARK_FACTORY_CURSOR_CONFIG_DIR");
        let prior_cursor_home = std::env::var_os("DARK_FACTORY_CURSOR_HOME");
        let prior_gemini_home = std::env::var_os("DARK_FACTORY_GEMINI_HOME");
        std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", &claude_dir);
        std::env::set_var("CODEX_HOME", &codex_dir);
        std::env::set_var("MINIMAX_API_KEY", "SYNTHETIC_MINIMAX_SENTINEL");
        std::env::set_var("DARK_FACTORY_AGY_HOME", &agy_dir);

        let mut claude = Command::new("sh");
        for var in SCRUBBED_AUTH_VARS {
            claude.env(var, "SYNTHETIC_AUTH_SENTINEL");
        }
        apply_claude_scope(&mut claude).unwrap();
        let claude_env = child_scope_env(&mut claude);
        assert!(claude_env.contains(&format!("CLAUDE_CONFIG_DIR={}", claude_dir.canonicalize().unwrap().display())), "Claude child env: {claude_env}");
        assert!(claude_env.lines().filter(|line| line.ends_with("=SYNTHETIC_AUTH_SENTINEL")).count() == 0, "Claude child leaked scoped auth: {claude_env}");
        assert!(claude_env.contains("MINIMAX_API_KEY=\n"), "Claude child inherited MiniMax auth: {claude_env}");
        assert!(claude_env.contains("GEMINI_API_KEY=\n"), "Claude child inherited Gemini auth: {claude_env}");

        let mut codex = Command::new("sh");
        for var in SCRUBBED_AUTH_VARS {
            codex.env(var, "SYNTHETIC_AUTH_SENTINEL");
        }
        apply_codex_scope(&mut codex).unwrap();
        let codex_env = child_scope_env(&mut codex);
        assert!(codex_env.contains(&format!("CODEX_HOME={}", codex_dir.canonicalize().unwrap().display())), "Codex child env: {codex_env}");
        assert!(codex_env.lines().filter(|line| line.ends_with("=SYNTHETIC_AUTH_SENTINEL")).count() == 0, "Codex child leaked scoped auth: {codex_env}");
        assert!(codex_env.contains("MINIMAX_API_KEY=\n"), "Codex child inherited MiniMax auth: {codex_env}");
        assert!(codex_env.contains("GEMINI_API_KEY=\n"), "Codex child inherited Gemini auth: {codex_env}");

        let mut minimax = Command::new("sh");
        for var in SCRUBBED_AUTH_VARS {
            minimax.env(var, "SYNTHETIC_AUTH_SENTINEL");
        }
        apply_minimax_scope(&mut minimax).unwrap();
        let minimax_env = child_scope_env(&mut minimax);
        assert!(minimax_env.contains("ANTHROPIC_API_KEY=SYNTHETIC_MINIMAX_SENTINEL\n"), "MiniMax child did not receive its synthetic key: {minimax_env}");
        assert!(minimax_env.contains("ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic\n"));
        assert!(minimax_env.contains("ANTHROPIC_MODEL=MiniMax-M3\n"));
        assert!(minimax_env.contains("MINIMAX_API_KEY=\n"), "MiniMax child inherited provider-specific key: {minimax_env}");
        assert!(minimax_env.contains("GEMINI_API_KEY=\n"), "MiniMax child inherited Gemini auth: {minimax_env}");

        // Native AGY is launched without modelProvider=gemini;
        // prove that the direct-dispatch path scrubs ambient GEMINI_API_KEY and sets HOME.
        let mut agy = Command::new("sh");
        for var in SCRUBBED_AUTH_VARS {
            agy.env(var, "SYNTHETIC_AUTH_SENTINEL");
        }
        apply_direct_cli_scope("/opt/antigravity/bin/agy", &[], &mut agy).unwrap();
        let agy_env = child_scope_env(&mut agy);
        assert!(agy_env.lines().all(|line| !line.ends_with("=SYNTHETIC_AUTH_SENTINEL")), "AGY child leaked scoped auth: {agy_env}");
        assert!(agy_env.contains("MINIMAX_API_KEY=\n"), "AGY child inherited MiniMax auth: {agy_env}");
        assert!(agy_env.contains("GEMINI_API_KEY=\n"), "Native AGY child inherited Gemini auth: {agy_env}");
        assert!(agy_env.contains(&format!("HOME={}\n", agy_dir.canonicalize().unwrap().display())), "AGY child did not receive HOME: {agy_env}");

        // Explicit Gemini API mode AGY:
        // Validates and retains GEMINI_API_KEY when configured.
        let agy_api_dir = temp.path.join("agy_api");
        let agy_api_settings = agy_api_dir.join(".gemini").join("antigravity-cli");
        std::fs::create_dir_all(&agy_api_settings).unwrap();
        std::fs::write(agy_api_settings.join("settings.json"), r#"{"modelProvider":"gemini"}"#).unwrap();
        std::env::set_var("DARK_FACTORY_AGY_HOME", &agy_api_dir);
        std::env::set_var("GEMINI_API_KEY", "SYNTHETIC_GEMINI_SENTINEL");

        let mut agy_api = Command::new("sh");
        for var in SCRUBBED_AUTH_VARS {
            agy_api.env(var, "SYNTHETIC_AUTH_SENTINEL");
        }
        apply_direct_cli_scope("/opt/antigravity/bin/agy", &[], &mut agy_api).unwrap();
        let agy_api_env = child_scope_env(&mut agy_api);
        assert!(agy_api_env.contains("GEMINI_API_KEY=SYNTHETIC_GEMINI_SENTINEL\n"), "Gemini API child did not retain GEMINI_API_KEY: {agy_api_env}");
        assert!(agy_api_env.contains("MINIMAX_API_KEY=\n"), "Gemini API child inherited MiniMax auth: {agy_api_env}");

        // Explicit Gemini API mode with missing key fails before child construction
        std::env::remove_var("GEMINI_API_KEY");
        let mut agy_api_fail = Command::new("sh");
        assert!(apply_direct_cli_scope("/opt/antigravity/bin/agy", &[], &mut agy_api_fail).is_err());

        // Cursor direct CLI test: retains CURSOR_API_KEY and CURSOR_CONFIG_DIR, scrubs others
        let cursor_dir = temp.path.join("cursor_child");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        std::env::set_var("CURSOR_API_KEY", "SYNTHETIC_CURSOR_CHILD_KEY");
        std::env::set_var("DARK_FACTORY_CURSOR_CONFIG_DIR", &cursor_dir);
        let mut cursor_cmd = Command::new("sh");
        for var in SCRUBBED_AUTH_VARS {
            cursor_cmd.env(var, "SYNTHETIC_AUTH_SENTINEL");
        }
        apply_direct_cli_scope("/usr/local/bin/cursor-agent", &[], &mut cursor_cmd).unwrap();
        let cursor_env = child_scope_env(&mut cursor_cmd);
        assert!(cursor_env.contains("CURSOR_API_KEY=SYNTHETIC_CURSOR_CHILD_KEY\n"), "Cursor child did not receive CURSOR_API_KEY: {cursor_env}");
        assert!(cursor_env.contains(&format!("CURSOR_CONFIG_DIR={}\n", cursor_dir.canonicalize().unwrap().display())), "Cursor child did not receive CURSOR_CONFIG_DIR: {cursor_env}");
        assert!(cursor_env.contains("OPENAI_API_KEY=\n"));
        assert!(cursor_env.contains("MINIMAX_API_KEY=\n"));
        assert!(cursor_env.contains("GEMINI_API_KEY=\n"));

        // Gemini direct CLI test: retains GEMINI_API_KEY and sets HOME to DARK_FACTORY_GEMINI_HOME
        let gemini_dir = temp.path.join("gemini_child");
        std::fs::create_dir_all(&gemini_dir).unwrap();
        std::env::set_var("GEMINI_API_KEY", "SYNTHETIC_GEMINI_DIRECT_KEY");
        std::env::set_var("DARK_FACTORY_GEMINI_HOME", &gemini_dir);
        let mut gemini_cmd = Command::new("sh");
        for var in SCRUBBED_AUTH_VARS {
            gemini_cmd.env(var, "SYNTHETIC_AUTH_SENTINEL");
        }
        apply_direct_cli_scope("/opt/bin/gemini", &[], &mut gemini_cmd).unwrap();
        let gemini_env = child_scope_env(&mut gemini_cmd);
        assert!(gemini_env.contains("GEMINI_API_KEY=SYNTHETIC_GEMINI_DIRECT_KEY\n"), "Gemini direct child did not receive GEMINI_API_KEY: {gemini_env}");
        assert!(gemini_env.contains(&format!("HOME={}\n", gemini_dir.canonicalize().unwrap().display())), "Gemini direct child did not receive HOME: {gemini_env}");
        assert!(gemini_env.contains("CURSOR_API_KEY=\n"));
        assert!(gemini_env.contains("MINIMAX_API_KEY=\n"));

        match prior_claude {
            Some(value) => std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", value),
            None => std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR"),
        }
        match prior_codex {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match prior_minimax {
            Some(value) => std::env::set_var("MINIMAX_API_KEY", value),
            None => std::env::remove_var("MINIMAX_API_KEY"),
        }
        match prior_gemini {
            Some(value) => std::env::set_var("GEMINI_API_KEY", value),
            None => std::env::remove_var("GEMINI_API_KEY"),
        }
        match prior_agy {
            Some(value) => std::env::set_var("DARK_FACTORY_AGY_HOME", value),
            None => std::env::remove_var("DARK_FACTORY_AGY_HOME"),
        }
        match prior_cursor_key {
            Some(value) => std::env::set_var("CURSOR_API_KEY", value),
            None => std::env::remove_var("CURSOR_API_KEY"),
        }
        match prior_cursor_dir {
            Some(value) => std::env::set_var("DARK_FACTORY_CURSOR_CONFIG_DIR", value),
            None => std::env::remove_var("DARK_FACTORY_CURSOR_CONFIG_DIR"),
        }
        match prior_cursor_home {
            Some(value) => std::env::set_var("DARK_FACTORY_CURSOR_HOME", value),
            None => std::env::remove_var("DARK_FACTORY_CURSOR_HOME"),
        }
        match prior_gemini_home {
            Some(value) => std::env::set_var("DARK_FACTORY_GEMINI_HOME", value),
            None => std::env::remove_var("DARK_FACTORY_GEMINI_HOME"),
        }
    }
}
