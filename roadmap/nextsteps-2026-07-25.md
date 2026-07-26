# Nextsteps — dark-factory — 2026-07-25

## Table of contents

- [Executive summary](#executive-summary)
- [Context](#context)
- [Bead index](#bead-index)
- [Work queue](#work-queue)
- [PR / merge state](#pr--merge-state)
- [Learnings pointer](#learnings-pointer)
- [Roadmap pointer](#roadmap-pointer)

## Executive summary

- **Mangled Git Conflicts Resolved (PR #476):** Resolved the nested/mangled conflict blocks in `runner/skeptic_gate_cli.py` and `.github/workflows/skeptic-gate.yml` from the PR #463 squash-merge on main. Removed duplicate command-line argument parser definitions for `--contract-file` and `--bead-id`.
- **Skeptic Compliance Restored:** Restored `runner/skeptic_gate.py` and `tests/test_skeptic_gate.py` to their clean, complete versions from ancestor commit `f3009e2` to re-integrate the correct `PRIOR_FINDING:` contract-echo and evidence-assertion logic.
- **Verification Success:** Verified the compilation and execution of the skeptic gate CLI, passing all **103 skeptic gate tests** and **55 targeted echo tests** successfully. Merged with origin to resolve branch divergence and pushed to remote.
- **Daemon Health**: The `ai.dark-factory.daemon` service is active and ticking cleanly in the background, having exceeded **3,120+ consecutive ticks** with zero failures since the multi-machine claim coordination went live.

## Context

This work block (Session 2026-07-25) cleared the blocking git conflicts committed to `origin/main` that broke pytest collections and CLI validation for PR #476. By restoring compliance to the skeptic gate parsers and workflows, the PR is now fully green (CI `test`, `daemon-tests`, `Evidence Gate`, and CodeRabbit/Bugbot check-runs all SUCCESS) and ready for the human merge decision.

## Bead index

| Bead | Title | Priority | Link |
|------|-------|----------|------|
| [jleechan-2xlo](https://github.com/jleechanorg/dark-factory/issues/205) | PR#205 er_runner forks new daemon instance (fork-bomb risk) | P0 | [jleechan-2xlo](https://github.com/jleechanorg/dark-factory/issues/205) |
| [jleechan-d0wn](https://github.com/jleechanorg/dark-factory/issues/210) | daemon never reaps zombie coder sessions (slot leaks) | P0 | [jleechan-d0wn](https://github.com/jleechanorg/dark-factory/issues/210) |
| [jleechan-9k3a](https://github.com/jleechanorg/dark-factory/issues/215) | DARK_FACTORY_HOLDOUTS assumption no-ops sandbox deny-list | P0 | [jleechan-9k3a](https://github.com/jleechanorg/dark-factory/issues/215) |
| [jleechan-0qy.1](https://github.com/jleechanorg/dark-factory/issues/220) | [P0] Binary default Level-5 DOT when --pipeline is omitted | P0 | [jleechan-0qy.1](https://github.com/jleechanorg/dark-factory/issues/220) |
| [jleechan-goal-unattended-e2e-2026-07-17-bze8.1](https://github.com/jleechanorg/dark-factory/issues/230) | [factory] fail-closed exact-head 7-green merge authority | P0 | [jleechan-goal-unattended-e2e-2026-07-17-bze8.1](https://github.com/jleechanorg/dark-factory/issues/230) |
| [jleechan-goal-unattended-e2e-2026-07-17-bze8.2](https://github.com/jleechanorg/dark-factory/issues/231) | [factory] restore Mac host parity: AO lifecycle, ticks, deploy | P0 | [jleechan-goal-unattended-e2e-2026-07-17-bze8.2](https://github.com/jleechanorg/dark-factory/issues/231) |
| [jleechan-goal-unattended-e2e-2026-07-17-bze8.3](https://github.com/jleechanorg/dark-factory/issues/232) | [factory] redispatch inherits expired autonomy clock | P0 | [jleechan-goal-unattended-e2e-2026-07-17-bze8.3](https://github.com/jleechanorg/dark-factory/issues/232) |
| [jleechan-74wt](https://github.com/jleechanorg/dark-factory/issues/240) | Repair PR276 target_repo routing and collision handling | P0 | [jleechan-74wt](https://github.com/jleechanorg/dark-factory/issues/240) |
| [jleechan-af-drive-pr287-aw9y](https://github.com/jleechanorg/dark-factory/issues/250) | [dark-factory] drive PR #287 to green+merge | P0 | [jleechan-af-drive-pr287-aw9y](https://github.com/jleechanorg/dark-factory/issues/250) |
| [jleechan-t5sw](https://github.com/jleechanorg/dark-factory/issues/260) | CI bash harness fails on self-hosted runner: missing rg | P0 | [jleechan-t5sw](https://github.com/jleechanorg/dark-factory/issues/260) |

## Work queue

1. **Wait for Human Merge Decision on PR #476:** Conclude code edits and wait for human operator to approve PR #476.
2. **Prioritize Top 10 Factory Beads**: Label only the selected 10 issues (P0) with the `factory` tag.
3. **Resolve Daemon Process Leaks**: Fix the child process fork-bomb risk on `er_runner` ([jleechan-2xlo](https://github.com/jleechanorg/dark-factory/issues/205)) and coder session leaks ([jleechan-d0wn](https://github.com/jleechanorg/dark-factory/issues/210)).
4. **Harden Sandbox deny-list**: Resolve the silent sandbox skip when `DARK_FACTORY_HOLDOUTS` is unset ([jleechan-9k3a](https://github.com/jleechanorg/dark-factory/issues/215)).
5. **Implement Default Level-5 Graph Contract**: Enforce the mandatory explore/coder/reviewer/evidence contract nodes in the CLI default DOT parser ([jleechan-0qy.1](https://github.com/jleechanorg/dark-factory/issues/220)).

## PR / merge state

- https://github.com/jleechanorg/dark-factory/pull/476 — OPEN (mergeable=true, green, pending human review)
- https://github.com/jleechanorg/dark-factory/pull/475 — MERGED (CLAIMED tag coordination)
- https://github.com/jleechanorg/dark-factory/pull/477 — MERGED (bin/claim and bin/release wrappers)

## Learnings pointer

- `~/roadmap/learnings-2026-07.md` — section `2026-07-25 — skeptic gate CLI conflict resolution + f3009e2 restore`

## Roadmap pointer

- Appended `roadmap/activity/2026-07-25.md` (to be created next if missing).
