# PR Issue #501 Detached HEAD Drift Gate Evidence

**Issue**: [#501 fix(daemon): af-tick refuses to run when local checkout is on detached HEAD](https://github.com/jleechanorg/dark-factory/issues/501)  
**Branch**: `factory/jleechan-501-detached-head`  
**Base**: `origin/main`  
**Generated**: 2026-08-01T13:47:00-07:00  

---

## 1. Summary of Changes

The daemon tick script `daemon/factory-af-tick.sh` is designed to run only from the `main` branch to avoid executing un-drifted or uncommitted feature code in production. However, certain deployment pipelines or tooling leave the checkout in a **detached HEAD** state, which caused `git branch --show-current` to return an empty string and the script to exit with code `10`.

This change widens the branch check:
1. If the current branch is not `main`, we check if it is a detached HEAD (empty branch name).
2. If it is detached, we resolve the current HEAD commit (`local_sha`), the local main commit (`main_sha`), and the remote main commit (`origin_main_sha`).
3. If the detached HEAD commit matches either of these two, we consider it a valid detached HEAD on the `main` commit and allow it to pass the branch check.
4. The subsequent drift-check block still guarantees that if the commit differs from `origin/main` or has uncommitted changes, it refuses with exit code `10`.

---

## 2. Test-Driven Development (TDD) Cycle

We added three new test cases to `tests/scripts/test_factory_af_tick_drift_gate.sh`:
* **Case 6**: Verifies that a detached HEAD pointing to the `main` commit passes the drift gate.
* **Case 7**: Verifies that a detached HEAD pointing to a different commit (e.g. an earlier init commit) is refused with exit code `10`.
* **Case 8**: Verifies that a detached HEAD pointing to the `origin/main` commit passes the drift gate even when the local `main` branch has diverged/differs from `origin/main`.

### Red Phase (Failing Case 6 & 8)
```
FAIL: drift gate should pass on detached HEAD pointing to main commit. Output: factory-af-tick: REFUSING TICK — checkout at /tmp/test-af-tick-drift.nz9I8H/work is on branch '<detached HEAD>', not main...
RC=10

FAIL: drift gate should pass on detached HEAD pointing to origin/main when local main differs. Output: factory-af-tick: REFUSING TICK — checkout at /tmp/test-af-tick-drift.nz9I8H/work is on branch '<detached HEAD>', not main...
RC=10
```

### Green Phase (All cases passing)
```
PASS: drift gate passes on detached HEAD pointing to main commit
PASS: drift gate refuses (rc=10) on detached HEAD pointing to a different commit
PASS: drift gate passes on detached HEAD pointing to origin/main even when local main differs
=== RESULTS: 12 passed, 0 failed ===
```

---

## 3. Evidence deliverables

All execution is captured in the following asciicast file:
* **TDD Asciicast**: [evidence/issue-501-tdd-cycle.cast](file:///home/jleechan/.worktrees/dark-factory/df-353/evidence/issue-501-tdd-cycle.cast)
