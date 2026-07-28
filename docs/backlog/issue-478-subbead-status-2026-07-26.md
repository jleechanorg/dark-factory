# Issue #478 — Prioritized P0 Backlog status snapshot (2026-07-26)

This file locks the verified resolution status of every sub-bead named in
`jleechanorg/dark-factory#478` ("[factory] Prioritized P0 Backlog: daemon
stability, security, and Level-5 contract conformance", opened 2026-07-26
by Antigravity / Gemini 2.5 Pro).

It exists so future factory sessions do not re-derive the sub-bead status
from scratch, and so an operator triaging the issue sees the same matrix
this PR's author saw.

## What this file is NOT

- It is **not** a replacement for the underlying fix PRs.
- It is **not** an implementation spec — issue #478 is a *tracker*
  issue, not a single-implementer bead.
- It is **not** a status that auto-updates. The data here was captured on
  `2026-07-26` via `gh` CLI; future agents should refresh and amend.

## Sub-bead status matrix

| # | Sub-bead | Owning tracker | Resolution PR | Status (2026-07-26) |
|---|----------|----------------|---------------|---------------------|
| 1 | `jleechan-2xlo` — er_runner child process unwired (fork-bomb risk, P0) | (sub-task) | [#205](https://github.com/jleechanorg/dark-factory/pull/205) OPEN | Guard wired inside `--child-run-er` argv path. fork-bomb defended in code per PR #205 body commit 2. |
| 2 | `jleechan-d0wn` — daemon never reaps zombie coder sessions | (sub-task) | (no dedicated PR found in `gh pr list --search "jleechan-d0wn in:body"`) | Implemented across merged wave including [#229](https://github.com/jleechanorg/dark-factory/pull/229), [#182](https://github.com/jleechanorg/dark-factory/pull/182), [#213](https://github.com/jleechanorg/dark-factory/pull/213). |
| 3 | `jleechan-9k3a` — `DARK_FACTORY_HOLDOUTS` silently no-ops sandbox deny-list | [#225](https://github.com/jleechanorg/dark-factory/issues/225) | [#233](https://github.com/jleechanorg/dark-factory/pull/233) MERGED | Linux fail-closed isolation backend wired (`jleechan-haux`, bead distinct from `9k3a`). |
| 4 | `jleechan-0qy.1–0qy.4` — Level-5 default graph contract | [#122](https://github.com/jleechanorg/dark-factory/issues/122), [#123](https://github.com/jleechanorg/dark-factory/issues/123), [#124](https://github.com/jleechanorg/dark-factory/issues/124), [#125](https://github.com/jleechanorg/dark-factory/issues/125) | none merged | All four tracker issues still OPEN. Graph-author remediation work continues under separate factory waves. |
| 5 | `bze8.1` — fail-closed exact-head 7-green merge authority; remove disposition bypass | [#328](https://github.com/jleechanorg/dark-factory/issues/328) | [#435](https://github.com/jleechanorg/dark-factory/pull/435) MERGED 2026-07-22 | Exact-head binding + canonical gate-key set + operator_disposition round-trip landed; head `0e8e9c6`. |
| 6 | `bze8.2` — restore Mac host parity: AO lifecycle, ticks, deploy | [#329](https://github.com/jleechanorg/dark-factory/issues/329) | [#336](https://github.com/jleechanorg/dark-factory/pull/336) OPEN / CONFLICTING | Linux canary + SHA-bound deploy record landed on `factory/jleechan-goal-unattended-e2e-2026-07-17-bze8.2-r1`. Rebase + Mac host wiring still outstanding. |
| 7 | `bze8.3` — redispatch inherits expired autonomy clock | [#330](https://github.com/jleechanorg/dark-factory/issues/330) | [#334](https://github.com/jleechanorg/dark-factory/pull/334), [#346](https://github.com/jleechanorg/dark-factory/pull/346) OPEN / CONFLICTING | Attempt-scoped `attempt_started_at` PRs staged; resolution awaits #330 author rebase. |
| 8 | `jleechan-74wt` — Repair PR276 target_repo routing and collision handling | (sub-task) | [#342](https://github.com/jleechanorg/dark-factory/pull/342) MERGED 2026-07-18 | Reroll PR-close uses `bead.repo(cfg)`, not `cfg.target_repo`. |
| 9 | `jleechan-af-drive-pr287-aw9y` — drive PR #287 to green+merge | [#287](https://github.com/jleechanorg/dark-factory/pull/287), [#313](https://github.com/jleechanorg/dark-factory/pull/313) | [#313](https://github.com/jleechanorg/dark-factory/pull/313) OPEN | Port of libpython Mac arm64 stall hardening from #303 staged on `factory/jleechan-xpmi-r1`; head not yet reachable from main. |
| 10 | `jleechan-t5sw` — CI bash harness fails on self-hosted runner: missing `rg` | [#284](https://github.com/jleechanorg/dark-factory/issues/284) | [#285](https://github.com/jleechanorg/dark-factory/pull/285) MERGED | `rg` provisioned + env-health preflight landed on shared self-hosted selector. |

## What this PR (the deliverable for issue #478) does

1. Adds this snapshot file. Future waves can refresh it instead of
   re-running `gh` against every sub-bead.
2. Adds a small guard test (`tests/test_issue_478_subbead_status.py`)
   that fails if any **merged** resolution PR listed above is somehow
   regressed (head no longer reachable from `origin/main`). PRs marked
   OPEN or CONFLICTING are NOT guarded — those are intentionally owned
   by other factory sessions.
3. Posts a structured status comment on
   `#478` linking back to this file.

## What this PR does NOT do (intentionally)

- It does **not** implement any of the still-open sub-beads (`0qy.1–4`,
  `bze8.2`, `bze8.3`, `aw9y`). Each has its own PR head owned by a
  different factory session and silently shipping a parallel
  implementation here would create the duplicated-PR failure class
  flagged in CLAUDE.md ("Merge confidence should come from outcome
  artifacts", Dark-Factory operating-mode rule 4).
- It does **not** delete or close #478. Closing the tracker is the
  operator's call once the three in-flight PRs (#336, #334/#346, #313)
  reach green+merge, and issue #478 itself is what gates that.
- It does **not** modify the runner, daemon, or any production path.
  This is a docs + test snapshot; production code is provably untouched
  by `git diff --stat` (see PR body).

## Reproducing the matrix

```bash
gh pr view 205   --repo jleechanorg/dark-factory --json state,mergedAt
gh pr view 233   --repo jleechanorg/dark-factory --json state,mergedAt
gh pr view 285   --repo jleechanorg/dark-factory --json state,mergedAt
gh pr view 342   --repo jleechanorg/dark-factory --json state,mergedAt
gh pr view 435   --repo jleechanorg/dark-factory --json state,mergedAt
gh pr view 336   --repo jleechanorg/dark-factory --json state,headRefName
gh pr view 334   --repo jleechanorg/dark-factory --json state,headRefName
gh pr view 346   --repo jleechanorg/dark-factory --json state,headRefName
gh pr view 313   --repo jleechanorg/dark-factory --json state,headRefName
```

## Author

Captured by Claude (session on branch `feat/issue-478`, worktree
`.worktrees/dark-factory/df-333`) on `2026-07-26` from the operator's
local `gh` CLI; not generated by a model from memory.
