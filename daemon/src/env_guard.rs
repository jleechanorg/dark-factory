//! CI-only gate for stub-mode environment variables.
//!
//! Stub mode is the development/iteration convenience that lets the factory
//! drive end-to-end without burning real LLM tokens or waiting for real CI.
//! It is gated to CI environments so a local server, a developer's laptop, or
//! any non-CI invocation CANNOT enable stub mode — even if the env vars are
//! accidentally exported into the shell, stub_mode_allowed() still returns false.
//!
//! # Activation requirements (both required)
//!
//! - `GITHUB_ACTIONS=true` — the standard marker GitHub Actions exports. Any
//!   other CI can satisfy this by exporting it explicitly.
//! - `DARK_FACTORY_CI_ALLOW_STUB=1` — explicit operator opt-in per run, so a
//!   stray CI workflow cannot accidentally enable stub mode for downstream
//!   callers either.
//!
//! Local behaviour with stub env vars set:
//!
//! ```text
//! $ export DARK_FACTORY_ITERATION_STUB=1 DARK_FACTORY_FAKE_LLM=1
//! $ ./bin/dark-factory ...
//! -> stub_mode_allowed() == false -> real LLM calls, real CI gating.
//! ```
//!
//! CI behaviour:
//!
//! ```text
//! # .github/workflows/ci.yml sets:
//! env:
//!   GITHUB_ACTIONS: true                # exported by GitHub Actions
//!   DARK_FACTORY_CI_ALLOW_STUB: 1       # opt-in for tests that need stub mode
//! ```
//!
//! If your CI runner does not export `GITHUB_ACTIONS=true`, export it explicitly
//! alongside `DARK_FACTORY_CI_ALLOW_STUB=1`.

/// Returns true only when running in CI AND the operator has explicitly opted in.
///
/// Reading `DARK_FACTORY_ITERATION_STUB` / `DARK_FACTORY_FAKE_LLM` directly is a
/// foot-gun: a developer with those exported in their shell will see stub-mode
/// behaviour locally without realising it. Route every stub-mode decision through
/// this function.
pub fn stub_mode_allowed() -> bool {
    std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
        && std::env::var("DARK_FACTORY_CI_ALLOW_STUB").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests mutate process-wide env vars; serialise them so they cannot race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn reset_env() {
        std::env::remove_var("GITHUB_ACTIONS");
        std::env::remove_var("DARK_FACTORY_CI_ALLOW_STUB");
        std::env::remove_var("DARK_FACTORY_ITERATION_STUB");
        std::env::remove_var("DARK_FACTORY_FAKE_LLM");
    }

    #[test]
    fn local_with_no_env_returns_false() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_env();
        assert!(!stub_mode_allowed());
    }

    #[test]
    fn only_github_actions_is_not_enough() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_env();
        std::env::set_var("GITHUB_ACTIONS", "true");
        assert!(
            !stub_mode_allowed(),
            "missing the explicit DARK_FACTORY_CI_ALLOW_STUB opt-in must remain closed"
        );
    }

    #[test]
    fn only_allow_stub_is_not_enough() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_env();
        std::env::set_var("DARK_FACTORY_CI_ALLOW_STUB", "1");
        assert!(
            !stub_mode_allowed(),
            "missing the GITHUB_ACTIONS=true marker must remain closed"
        );
    }

    #[test]
    fn ci_with_both_env_vars_returns_true() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_env();
        std::env::set_var("GITHUB_ACTIONS", "true");
        std::env::set_var("DARK_FACTORY_CI_ALLOW_STUB", "1");
        assert!(stub_mode_allowed());
    }

    #[test]
    fn local_with_only_stub_env_vars_returns_false() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_env();
        // Operator accidentally exported the stub vars locally — must be ignored.
        std::env::set_var("DARK_FACTORY_ITERATION_STUB", "1");
        std::env::set_var("DARK_FACTORY_FAKE_LLM", "1");
        assert!(
            !stub_mode_allowed(),
            "local server MUST NEVER honor stub env vars without the CI gate"
        );
    }

    #[test]
    fn ci_with_disabled_allow_stub_returns_false() {
        let _g = ENV_LOCK.lock().unwrap();
        reset_env();
        std::env::set_var("GITHUB_ACTIONS", "true");
        std::env::set_var("DARK_FACTORY_CI_ALLOW_STUB", "0");
        assert!(!stub_mode_allowed());
    }
}