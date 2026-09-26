# Codex Code Mode host warning: research and repair

Observed 2026-09-07 on the operator Mac.

## Finding

The warning reports a missing companion executable at the former NVM npm installation path. Live filesystem inspection found that the entire old `@openai/codex` installation was absent. `ps -Ao pid,comm` nevertheless showed existing Codex processes and host processes running from that former path. The current `/opt/homebrew/bin/codex` resolves to `~/.codex/packages/standalone/current/bin/codex`, reports `codex-cli 0.153.4`, and includes its executable companion `codex-code-mode-host`.

These observations support an installation migration leaving already-running sessions with stale helper paths. They do not establish which earlier operation removed the npm installation. This was not a missing helper in the current standalone bundle. No configuration change was needed to correct the missing executable.

## Primary-source research

[Official Codex CLI documentation](https://learn.chatgpt.com/docs/codex/cli) documents the local Codex CLI and installation workflow. The fetched public documentation and a targeted official-domain search did not establish the internal host discovery algorithm or document this exact error. The diagnosis above is therefore based on direct local filesystem/process evidence, not an unsupported claim about documented internals.

Local primary artifacts: `/opt/homebrew/bin/codex` symlink; standalone release directory `/Users/jleechan/.codex/packages/standalone/releases/0.153.4-aarch64-apple-darwin/bin`; direct `codex --version` and helper `--help` output; `ps -Ao pid,comm` executable paths.

## Repair and validation

Created one compatibility symlink at:

`/Users/jleechan/.nvm/versions/node/v22.22.0/lib/node_modules/@openai/codex/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex-code-mode-host`

Target: `/Users/jleechan/.codex/packages/standalone/releases/0.153.4-aarch64-apple-darwin/bin/codex-code-mode-host`.

The target is pinned to installed version 0.153.4; this does not recreate an npm package or change PATH, wrappers, model defaults, feature flags, credentials, or account settings. Missing parent directories were created solely to supply the reported path. No existing file was overwritten.

Deterministic Python subprocess checks against the exact reported path:

- Before repair: invoking `--help` raised `FileNotFoundError`.
- After repair: `--help` exited successfully and printed `Usage: codex-code-mode-host [OPTIONS]`.
- Actual host startup with `--listen stdio`, empty stdin followed by EOF: exit code 0, empty stderr.
- Symlink resolution matched the pinned standalone executable.

This proves the reported executable can now be spawned and its stdio transport starts. It does not prove an already-failed session automatically retries feature initialization, nor a complete model-to-host RPC execution. Sessions whose Code Mode state was already marked unavailable may need a fresh Codex session; existing sessions were not terminated. Newly launched Codex resolves the current standalone installation.

The compatibility symlink is recoverable by removing that exact link after old sessions have exited. No cleanup was performed as part of this repair.
