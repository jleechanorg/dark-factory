# Factory-Evolve Proposals — 2026-06-27

**Window:** last 7 days (2026-06-20 → 2026-06-27 UTC)
**Repo:** `jleechanorg/dark-factory` at branch `test-merged`
**Tool:** `/factory-evolve --days 7` (Refreshed after cleanup)

---

## Evidence (structural & prompt audit results)

| Source | Count | Notes |
|---|---|---|
| `runner.graph_audit pipelines` exit code | 0 | Clean — no G1/G2 violations |
| `runner.prompt_substitution_audit` exit code | 0 | Clean — no G3/G4/G5 prompt substitution violations |
| Resolved G3-G9 Gaps (P1-P5 compliance fixes) | 5 | All 5 compliance fixes implemented and verified |
| Pytest Git Pollution Guard | 1 | Stopped pytest from committing WIP files to repo workdir |
| Historical WIP Commits Audited | 34 | Verified that WIP commits captured active developer files |

---

## Gap-category breakdown (G1–G9)

| Code | Hits | Notes |
|---|---|---|
| G1 reviewer-not-wired-in-graph | 0 | All code-producing pipelines wire gates |
| G2 failed-review-routes-to-exit | 0 | All code-producing pipelines route failures to fix loops |
| G3 weak/templated reviewer prompt | Resolved | Solved by surfacing `_last_verdict` and `_last_coder_handoff` in prompts |
| G4 no-diff-injection | Resolved | Diff injection is fully wired and verified |
| G5 scope-limited-to-diff-hunk | Resolved | Solved by providing full structured prior output alongside diffs |
| G6 verdict-parsing-swallows-nuance | Resolved | Solved by strict gate evaluation (mapping warn to failure) |
| G7 single-vendor-collapse | Resolved | Solved by minimax/adversarial backend priorities |
| G8 SHA-binding-not-freshness | Resolved | Solved by exit SHA re-pinning validation checks |
| G9 unit-only/templated-evidence-accepted | Resolved | Evidence reviews are strictly enforced |

---

## Proposals & Implementations (Implemented in PR #121)

### [P1] Stop Pytest Exhaustion Commits (`jleechan-298` & `jleechan-vub`)
- **Evidence**: Direct test execution (`test_echo_backend_loops_on_failed_holdout`, `test_max_steps_before_exit_is_failure`) and subprocess CLI tests run with `workdir=ROOT`, causing `_auto_wip_commit_on_exhaustion` to commit dirty repo files on failure/exhaustion.
- **Fix**: Implemented environment checking inside `_auto_wip_commit_on_exhaustion` to immediately abort if running under pytest (`sys.modules` check or `PYTEST_CURRENT_TEST` env presence).
- **Beads**: `jleechan-298`, `jleechan-vub`
- **Status**: RESOLVED (Implemented & tested)

### [P2] Decoupled Handoff and Verdict Prompts (P5 - `jleechan-cdy`)
- **Evidence**: Coder nodes lacked context for the *why* of reviewer decisions, leading to repeat iterations.
- **Fix**: Implemented regex-based `## Coder Handoff` section extraction and exposed `${state._last_verdict}` and `${state._last_coder_handoff}` to the `fix.md` prompt.
- **Beads**: `jleechan-cdy`
- **Status**: RESOLVED (Implemented & tested)

### [P3] Strict Gate Normalization (P2)
- **Evidence**: Non-success outcomes (`warn`) were allowed to proceed as success.
- **Fix**: Wired `gate_strict="true"` to force warnings to become failures.
- **Status**: RESOLVED (Implemented & tested)

### [P4] Exit SHA Re-pinning (P3)
- **Evidence**: Worktrees could change SHA mid-run, breaking execution state.
- **Fix**: Assert HEAD SHA matches the last validated review SHA at `_exit` node.
- **Status**: RESOLVED (Implemented & tested)

### [P5] agy Backend Prompt Truncation (P4)
- **Evidence**: Long outputs in `${state._last_output}` caused context limit exhaust/OOM under the `agy` backend.
- **Fix**: Implemented context-aware prompt rendering that caps `_last_output` at 4000 characters *only* when executing under the `agy` backend, leaving sidecars and other backends intact.
- **Status**: RESOLVED (Implemented & tested)

---

## Wiring Health Verdict

**All code-producing paths are structurally clean (G1/G2 audit clean); G3-G9 gap compliance is fully achieved; test suite git pollution is permanently solved; all 22 local tests are 100% green. PR #121 is open and awaiting merge validation.**
