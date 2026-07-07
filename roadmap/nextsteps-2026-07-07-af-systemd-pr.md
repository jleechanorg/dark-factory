# Nextsteps — 2026-07-07 — Auto-factory systemd PR

## Current Status

- PR: https://github.com/jleechanorg/dark-factory/pull/188.
- Branch: `factory/jleechan-1m4-systemd-r1`.
- Commit: `565bc99 codex/gpt-5: add durable daemon service and dispatch fixes`.
- Beads: `jleechan-1m4` and `jleechan-nzia` are `in_progress`.

## What Changed

- Added a Linux `systemd --user` service template and installer for the Rust
  daemon.
- Added daemon `sd_notify` `READY=1` and `WATCHDOG=1` messages.
- Added optional `ao_project` config and set live config to `worldarchitect`.
- Fixed newly-intaken bead dispatch so the worker receives the real tracker
  title rather than an empty prompt.

## Verification So Far

- `bash tests/scripts/test_systemd_user_install.sh` passed.
- `python3 -m pytest tests/test_systemd_user_install.py tests/test_evidence_bundle.py -q` passed.
- `cargo test --manifest-path daemon/Cargo.toml -- --test-threads=1` passed.
- `cargo clippy --manifest-path daemon/Cargo.toml -- -D warnings` passed.
- `/usr/bin/python3 -m runner.graph_audit pipelines` passed.
- `git diff --check` passed.

## Not Proven

This does not yet prove `/af` end-to-end. Missing proof remains:

- live `systemctl --user` active/running evidence for the daemon;
- multiple watchdog-fed tick intervals in the journal;
- restart behavior evidence;
- a real canary or factory-labeled bead reaching READY/merge without operator
  coding intervention.

## Recommended Next Actions

1. Wait for PR #188 `test` and `skeptic` checks.
2. Resolve or document Cursor Bugbot's usage-limit skip before treating PR #188
   as green.
3. After merge, install the service and capture live evidence under `/tmp/`.
4. Continue remaining P0 blockers before claiming label-to-merge E2E.
