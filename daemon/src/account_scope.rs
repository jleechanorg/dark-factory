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

/// Apply scoping for the given `AiProvider`.
pub fn apply_provider_scope(provider: AiProvider, command: &mut Command) -> Result<(), DaemonError> {
    match provider {
        AiProvider::Claude => apply_claude_scope(command),
        AiProvider::MiniMax => apply_minimax_scope(command),
        AiProvider::Codex => apply_codex_scope(command),
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
/// - `claude`, `claude-sonnet` -> `AiProvider::MiniMax` if explicitly indicated by `extra_env`,
///   otherwise `AiProvider::Claude`.
///
/// Normal non-AI commands (e.g. `git`, `br`, `gh`, `sh`, `cargo`) return `None`.
pub fn detect_direct_cli_provider(cmd: &str, extra_env: &[(&str, &str)]) -> Option<AiProvider> {
    let bin = Path::new(cmd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cmd);

    if bin == "codex" {
        Some(AiProvider::Codex)
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
/// - `antigravity` / `agy`: scrubs all auth, forwards validated `CLAUDE_CONFIG_DIR` and `CODEX_HOME` if set
/// - any other agent: scrubs all auth and returns `Err(DaemonError::Config(...))` so unknown agents fail closed.
pub fn validate_ao_worker_agent_scope(
    agent: &str,
    command: &mut Command,
) -> Result<(), DaemonError> {
    let normalized = agent.trim().to_lowercase();
    if normalized == "claude" || normalized == "claude-code" || normalized == "claude-sonnet" {
        apply_claude_scope(command)
    } else if normalized == "codex" {
        apply_codex_scope(command)
    } else if normalized == "minimax" || normalized == "claudem" {
        apply_minimax_scope(command)
    } else if normalized == "antigravity" || normalized == "agy" {
        scrub_all_ai_provider_auth(command);
        if let Ok(dir) = validate_claude_config_dir() {
            command.env("CLAUDE_CONFIG_DIR", dir);
        }
        if let Ok(home) = validate_codex_home() {
            command.env("CODEX_HOME", home);
        }
        Ok(())
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

        assert_eq!(detect_direct_cli_provider("claude", &[]), Some(AiProvider::Claude));
        assert_eq!(detect_direct_cli_provider("/home/user/.nvm/versions/node/v22.22.0/bin/claude", &[]), Some(AiProvider::Claude));
        assert_eq!(detect_direct_cli_provider("claude-sonnet", &[]), Some(AiProvider::Claude));

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
    fn test_validate_ao_worker_agent_scope() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let prior_claude = std::env::var_os("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        let prior_codex = std::env::var_os("CODEX_HOME");
        let prior_minimax = std::env::var_os("MINIMAX_API_KEY");

        // Clean env
        std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR");
        std::env::remove_var("CODEX_HOME");
        std::env::remove_var("MINIMAX_API_KEY");

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

        // 4. antigravity / agy scrubs auth and succeeds
        let mut cmd = Command::new("dummy");
        cmd.env("ANTHROPIC_API_KEY", "dirty_key");
        assert!(validate_ao_worker_agent_scope("antigravity", &mut cmd).is_ok());
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("ANTHROPIC_API_KEY") && v.is_none()));

        let mut cmd = Command::new("dummy");
        assert!(validate_ao_worker_agent_scope("agy", &mut cmd).is_ok());

        // 5. Unknown agent fails closed and scrubs auth
        let mut cmd = Command::new("dummy");
        cmd.env("OPENAI_API_KEY", "dirty_key");
        let err = validate_ao_worker_agent_scope("unknown-llm-agent", &mut cmd);
        assert!(err.is_err());
        assert!(format!("{}", err.unwrap_err()).contains("Unsupported AO worker agent"));
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs.iter().any(|(k, v)| k.to_str() == Some("OPENAI_API_KEY") && v.is_none()));

        // Restore
        match prior_claude {
            Some(v) => std::env::set_var("DARK_FACTORY_CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("DARK_FACTORY_CLAUDE_CONFIG_DIR"),
        }
        match prior_codex {
            Some(v) => std::env::set_var("CODEX_HOME", v),
            None => std::env::remove_var("CODEX_HOME"),
        }
        match prior_minimax {
            Some(v) => std::env::set_var("MINIMAX_API_KEY", v),
            None => std::env::remove_var("MINIMAX_API_KEY"),
        }
    }
}
