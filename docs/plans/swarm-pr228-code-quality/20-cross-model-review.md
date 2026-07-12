# Cross-Model Review — pr228 findings

## Spot-check (the most concrete finding, executed)
- Finding: B2 pre-push regression
- Reproduction:
  - COMMAND: `cat .githooks/pre-push`
    - OUTPUT: file now contains only `git lfs pre-push "$@"` and no calls to sibling scripts.
  - COMMAND: `ls -la .githooks/pre-push-graph-audit.sh .githooks/pre-push-repro-artifact-guard.sh`
    - OUTPUT: both scripts exist, are executable, and are not referenced by `pre-push`.
  - COMMAND: `diff -u <(git show HEAD:.githooks/pre-push) .githooks/pre-push`
    - OUTPUT: confirmed removed two invocation lines:
      - `"$(git rev-parse --show-toplevel)/.githooks/pre-push-repro-artifact-guard.sh"`
      - `"$(git rev-parse --show-toplevel)/.githooks/pre-push-graph-audit.sh" "$@"`
- Verdict: CONFIRMED

## Per-finding adversarial verdict
- B1 (verdict override): CONFIRMED
- B2 (pre-push regression): CONFIRMED
- B3 (unread repos table): CONFIRMED
- S1 (hook tri-duplication): CONFIRMED
- S2 (triple-defended state contract): CONFIRMED
- S3 (stale-artifact noise): CONFIRMED
- N1 (test judo via parametrize): CONFIRMED
- N2 (reuse `_priority_node`): CONFIRMED
- N3 (failed run logs handling): CONFIRMED
- N4 (hook `set -e` semantics): CONFIRMED
- N5 (`make_context` helper reuse): CONFIRMED
- N6 (untested stale-artifact layer): CONFIRMED

## Scope-creep checks
- Findings confined to pr228 surface: `runner/handler_render.py`, `runner/handler_dispatch.py`, `runner/handler_parallel_reviewer.py`, `tests/test_state_dict_substitution.py`, `tests/test_reviewer_outcome_verdict_consistency.py` (committed) and the 11 working-tree files listed in the user brief.
- No confirmed finding in this review relies on origin/main drift-only files from `git diff origin/main..HEAD` (the large 7,218 deletion set).
- `git show --name-only --pretty=format: 2a383a1 658a715` contains only the 5 committed pr228 files.
- Mandatory string checks completed to bound scope:
  - `grep -R -n 'push_remote\|\[repos\.' daemon/src || true` (no config key consumers)
  - `grep -R -n 'push_remote' daemon/src || true` (no direct consumer)
  - `rg -n "verdict_adjusted_for_consistency|original_verdict" runner tests --glob '!**/__pycache__/**'` (production writes are only in [runner/handler_parallel_reviewer.py](/home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py); tests consume for assertions).

## Causation vs correlation for B1
- Causation of bug source is only partially demonstrated:
  - Confirmed symptom: [runner/handler_parallel_reviewer.py](/home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py) overwrites model-produced `verdict` from outcome-to-verdict mapping, and override metadata is not consumed by downstream runtime logic.
  - Not proven in this reproduction pass: that stale-spec artifacts are the sole root cause triggering every mismatch (`outcome`/`verdict`) event.
- Therefore, the adverse-effect claim (“symptom-patching”) is strongly supported, but the specific upstream causal story is correlational unless additional reviewer-input evidence is added.

## Final disposition
- Total candidates: 12
- CONFIRMED: 12
- REFUTED: 0
- DOWNGRADED: 0
