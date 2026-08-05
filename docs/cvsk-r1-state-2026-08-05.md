# State report: /af E2E 5-item campaign (jleechan-cvsk)

Branch: factory/jleechan-cvsk-r1
Date: 2026-08-05

## Items

1. **jleechan-8mlh (P0, premature CI-pending park)** — bead done in factory memory;
   on-disk branches `factory/jleechan-8mlh-r1/r2/r3`; PR #573 (r1) closed without merge,
   PR #574 (r2) closed without merge; r3 in flight per daemon telemetry.
   Item done from factory memory perspective, awaiting main integration.
2. **PR #571 (restore contract-bound echo guard)** — head `1cbb5773` pushed to
   `fix/parallel-reviewer-echo-guard`; evidence gist
   https://gist.github.com/jleechan2015/d8316f652c3582d19ba8c15365c63c5d
   pinned in both PR body and code comment; local 1485 passed, 12 skipped, 0 failed.
   New CI run queued at 1cbb5773.
3. **PR #567 (gate-8 repo_root routing)** — head `90b333e5` on `factory/jleechan-sk55-r3`;
   changes 5 files (daemon/contracts + src/reroll,tick,dispatch,config) plus test
   scaffolding (`daemon/tests/tick_integration.rs`); no conflict markers.
   **FIX APPLIED**: PR #567's gate-8 routing correctly surfaces vacuous_red_green's
   reliance on a routed repo having `local_checkout` configured. The 3 failing
   cross-model reviewer tests (previously masked by the bug) now register
   `myorg/myrepo` in `cfg.repos` and create a minimal `/tmp/myorg-myrepo/Cargo.toml`
   via a new `ensure_test_repo_checkout()` helper. All 612 daemon tests pass locally.
   Test job failure on `test_conformance_score_is_deterministic_mock_surface` is a
   flake (same as PR #571).
4. **PR #570 (gate-8 pytest backend)** — head `c9e06dd0` on `factory/jleechan-6xje-r2`;
   adds Python backend to vacuous_red_green detector; needed because 93 of 124
   unknowns are worldarchitect.ai (Python).
5. **jleechan-jw4c (P1, worktree isolation + reaper)** — bead done in factory memory;
   PR #572 closed without merge; on-disk branch `factory/jleechan-jw4c-r1/r2`;
   r2 in flight per telemetry.

## CI gates observed (2026-08-05)

- PR #571: prior `test` run was a flake on `test_conformance_score_is_deterministic_mock_surface`
  (passes locally, 17/17 conformance tests green). New commit `1cbb5773` triggers fresh CI.
- PR #567: `test` and `Evidence Gate` FAIL. `daemon-tests` FAIL on 3 cross-model
  reviewer tests — **FIXED LOCALLY**: 90b333e5 adds `ensure_test_repo_checkout()` and
  registers `myorg/myrepo` in `cfg.repos`. All 612 daemon tests pass. CI in progress.
- PR #570: same pattern as PR #567 (failing test + Evidence Gate + daemon-tests).
- PR #573/574/572: closed without merge; fixes exist on r1/r2/r3 branches but were
  not promoted to main.

## Evidence Gate

All 3 open PRs (#571, #567, #570) needed canonical evidence markers.
- PR #571: evidence URL pinned in body and code at 1cbb5773.
- PR #567: evidence line added to PR body at 90b333e5.
- PR #570: pending evidence.

## Anomalies discovered

- `test_conformance_score_is_deterministic_mock_surface` is a flaky test that fires
  on PR #567 and PR #571's prior run but passes locally. Memory entry:
  `Daemon mass-park recovery 2026-07-28` notes similar cross-PR flakes.
- PR #567's gate-8 routing correctly exposes that the prior bug was masking the
  need for `local_checkout` configuration. Tests had to be updated to either
  register the routed repo or accept the new `ManifestMissing` status.
- Multiple beads reported as "done" in memory but work never reached main
  (PR #573, #574, #572 all closed without merge; fixes sit on r1/r2/r3 branches).

## Work completed this session

- PR #571: documented evidence gist, added pinned comment to code, pushed 1cbb5773.
- PR #567: diagnosed and fixed 3 daemon-tests failures via test scaffolding only
  (no production code change). Pushed 90b333e5 to factory/jleechan-sk55-r3.
- Operator branch factory/jleechan-cvsk-r1: 2 commits pushed (eef0bda0, 94e3debd).

## Operator actions recommended

1. Re-run CI on PR #571 — likely green given local conformance pass.
2. PR #567 CI in progress; should pass once daemon-tests re-run on 90b333e5.
3. PR #570 needs the same local_checkout test scaffolding fix.
4. The 8mlh/jw4h beads are "done" in the factory ledger but not on main — the
   factory's never-green paradox is wider than this 5-item campaign.
