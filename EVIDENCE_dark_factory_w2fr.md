# Evidence — bead dark-factory-w2fr (target-identity drift guard)

## Summary
- Live incident that motivated this guard: `dark-factory-o74s` for PR
  `#9462` spawned `wa-3551`, which operated on PRs `#9512` and `#8292`
  and modified `provenance-narrow/mvp_site/schemas/prompt_tool_contracts.json`
  — paths in a DIFFERENT repo from the assignment.
- Fix: three fail-closed guards that enforce the worker's
  `worktree / branch / repo` identity match before any tracked file is
  written or `git push` is run.
  1. **Server-side, at spawn** (`daemon/src/adapters.rs` + `tools.rs`)
     — the spawn adapter now calls
     `check_target_identity_guard(expected_cwd, expected_branch, expected_repo)`
     on the resolved workspace. A drift is `DaemonError::TargetIdentityDrift`,
     which `dispatch::dispatch_ready` converts into
     `HUMAN_HELD reason=target_identity_drift`.
  2. **Worker-side, at every pre-write / pre-push**
     (`daemon/scripts/af-target-identity-guard.sh` +
     `af-push-identity-guard.sh`) — the worker runs the bash guard
     before writing any tracked file or running `git push` (via the
     `af-push-identity-guard.sh` wrapper). Reads
     `AF_TARGET_{CHECKOUT,BRANCH,REPO}` env vars injected at spawn time;
     on any mismatch writes `<cwd>/target-drift.json` (named by drift
     dimension: `worktree` / `branch` / `repo`) and exits non-zero.
  3. **Dispatch path identity injection**
     (`daemon/factory-ao-remediate.sh`) — computes the four
     `AF_TARGET_*` tokens from `[repos.<repo>]` routing + `gh pr view`
     + the SQLite overlay (with `factory/<BEAD_ID>-r1` as the final
     fallback) and exports them into the spawned AO session so the
     worker's bash guard has authoritative identity to compare against.

## TDD cycle (Red → Green)

### Initial failing run (RED)
The new tests were written first; they failed because
`check_target_identity_guard`, `af-target-identity-guard.sh`,
`af-push-identity-guard.sh`, and the identity injection in
`factory-ao-remediate.sh` did not yet exist.

```
$ bash tests/scripts/test_target_identity_guard.sh
FAIL: missing /home/.../daemon/scripts/af-target-identity-guard.sh

$ cd daemon && cargo test --lib --no-run
error[E0425]: cannot find function `check_target_identity_guard` in this scope
error[E0599]: no variant named `TargetIdentityDrift` found for enum `errors::DaemonError`
error[E0433]: failed to resolve: use of undeclared type `TargetIdentityDriftKind`
```

### Successful verification run (GREEN)
After implementing the production code, the same suite runs clean:

```
$ bash tests/scripts/test_target_identity_guard.sh
PASS: match: exit 0 on perfect identity match
PASS: match: no stderr on success
PASS: match: no target-drift.json sentinel on success
PASS: sibling-worktree: exit non-zero on drift
PASS: sibling-worktree: target-drift.json written to worker's cwd
PASS: sentinel names the worktree dimension
PASS: cross-repo: exit non-zero on drift
PASS: sentinel names the repo dimension
PASS: branch-drift: exit non-zero on branch mismatch
PASS: sentinel names the branch dimension
PASS: missing-env: refuse (non-zero) when no identity tokens supplied
PASS: missing-env: target-drift.json written when env absent
PASS: push-guard matched-identity delegates (rc=128)
PASS: push-drift: refuse (non-zero) on drift
PASS: push-guard never invoked git push on drift
PASS: remediate injects AF_TARGET_CHECKOUT
PASS: remediate injects AF_TARGET_BRANCH (refs/heads/...)
PASS: remediate injects AF_TARGET_REPO=<exact bead target_repo>
=================================
PASS=18  FAIL=0
=================================

$ cd daemon && cargo test --lib --no-fail-fast -- --test-threads=1
... (629 tests total, 0 failed)
test dispatch::tests::target_identity_drift_branch_park_bead_human_held ... ok
test dispatch::tests::target_identity_drift_repo_park_bead_human_held ... ok
test tools::tests::target_identity_guard_passes_when_cwd_branch_repo_all_match ... ok
test tools::tests::target_identity_guard_passes_when_every_expected_is_none ... ok
test tools::tests::target_identity_guard_rejects_branch_drift_on_same_repo ... ok
test tools::tests::target_identity_guard_rejects_cross_repo_worktree ... ok
test tools::tests::target_identity_guard_rejects_sibling_worktree_with_matching_repo ... ok
test result: ok. 629 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 24.68s
```

## New tests added (TDD red→green evidence)

### Bash — `tests/scripts/test_target_identity_guard.sh` (18 PASS / 0 FAIL)
1. `match: exit 0 on perfect identity match` — guard passes when all
   three dimensions align.
2. `match: no stderr on success` — guard is silent on the happy path.
3. `match: no target-drift.json sentinel on success` — no drift file
   written on the happy path.
4. `sibling-worktree: exit non-zero on drift` — same-repo, different
   physical worktree path is refused (the exact wa-3551 shape).
5. `sibling-worktree: target-drift.json written to worker's cwd` —
   sentinel is deposited at the actual worker's cwd, not at the
   assignment's cwd.
6. `sentinel names the worktree dimension` — drift file includes
   `"drift_kinds": ["worktree"]` so triage does not have to grep
   stderr.
7. `cross-repo: exit non-zero on drift` — right worktree path, but
   `origin` URL belongs to a different repo.
8. `sentinel names the repo dimension` — drift file includes
   `"drift_kinds": ["repo"]`.
9. `branch-drift: exit non-zero on branch mismatch` — worker spawned
   for branch A but sitting on branch B (the wa-3551 PR-#9462-vs-#9512
   shape).
10. `sentinel names the branch dimension` — drift file includes
    `"drift_kinds": ["branch"]`.
11. `missing-env: refuse (non-zero) when no identity tokens supplied`
    — fail closed on a worker that did not receive its identity.
12. `missing-env: target-drift.json written when env absent` —
    sentinel still written so triage can see the failure mode.
13. `push-guard matched-identity delegates (rc=128)` — `git push
    --dry-run` is allowed to run when identity matches; the wrapper
    exec's the underlying push.
14. `push-drift: refuse (non-zero) on drift` — push wrapper refuses
    before exec'ing `git push`.
15. `push-guard never invoked git push on drift` — verified via a
    fake `git` binary in PATH; the wrapper does not leak through.
16. `remediate injects AF_TARGET_CHECKOUT` — `factory-ao-remediate.sh`
    injects the assigned worker's checkout path.
17. `remediate injects AF_TARGET_BRANCH (refs/heads/...)` — branch is
    fully-qualified.
18. `remediate injects AF_TARGET_REPO=<exact bead target_repo>` —
    repo matches the bead's resolved target_repo verbatim.

### Rust — `daemon/src/tools.rs` (5 new unit tests in `tools::tests`)
1. `target_identity_guard_passes_when_every_expected_is_none` — legacy
   layout (all `expected_*` = None) is silent-OK, mirroring
   `check_cwd_guard`'s pre-fix behavior.
2. `target_identity_guard_passes_when_cwd_branch_repo_all_match` —
   canonicalized cwd + normalized branch + normalized repo all match.
3. `target_identity_guard_rejects_sibling_worktree_with_matching_repo`
   — the wa-3551 shape: same repo URL, different physical worktree.
4. `target_identity_guard_rejects_cross_repo_worktree` — the
   dark-factory-ik0v shape: same worktree path, different repo.
5. `target_identity_guard_rejects_branch_drift_on_same_repo` —
   same worktree + same repo, but a different branch.

### Rust — `daemon/src/dispatch.rs` (2 new unit tests in `dispatch::tests`)
1. `target_identity_drift_branch_park_bead_human_held` — the dispatch
   path catches branch drift before the worker session is created and
   parks the bead `HUMAN_HELD reason=target_identity_drift`. Reproduces
   the wa-3551 PR-#9462→PR-#9512 shape.
2. `target_identity_drift_repo_park_bead_human_held` — same, for the
   repo dimension (the dark-factory-ik0v shape).

## Pre-existing parallel-test flakiness
The full suite runs cleanly with `--test-threads=1` (629/629 PASS).
With default parallel execution,
`config::tests::parses_example_config` occasionally fails because it
reads `contracts/daemon.toml.example` via a relative path — when
another test changes the daemon's cwd mid-run, the read fails. This
test is UNCHANGED by this PR (see `git diff -- daemon/src/config.rs`)
and the failure is pre-existing; the fix belongs in a separate
"convert relative-path test reads to absolute" bead.

## Files changed
```
daemon/factory-ao-remediate.sh       | 118 ++++++++++++++++
daemon/scripts/af-push-identity-guard.sh     (new file, executable)
daemon/scripts/af-target-identity-guard.sh  (new file, executable)
daemon/src/adapters.rs               |  51 +++++++
daemon/src/dispatch.rs               | 165 +++++++++++++++++++++-
daemon/src/errors.rs                 |  30 ++++
daemon/src/reroll.rs                 |   9 +-
daemon/src/state.rs                  |  12 ++
daemon/src/tools.rs                  | 258 ++++++++++++++++++++++++++++++++++-
daemon/tests/adapters_integration.rs |   2 +
daemon/tests/tools_fakes.rs          |   2 +
tests/scripts/test_target_identity_guard.sh (new file, executable)
```

## How to run the evidence locally

```bash
# Bash guard contract (the worker-side pre-write / pre-push guard).
bash tests/scripts/test_target_identity_guard.sh

# Rust unit tests for the dispatch + tools integration.
cd daemon && cargo test --lib --no-fail-fast -- --test-threads=1
```

## Incident evidence preservation
The provenance-narrow/mvp_site/schemas/prompt_tool_contracts.json
changes that wa-3551 produced during the incident are NOT reverted by
this PR — they remain on the affected PRs/branches as the incident
evidence itself (per bead acceptance criterion: "preserve incident
evidence; do not revert it").
