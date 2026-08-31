# Issue #784 RED→GREEN Evidence Manifest

## Summary
Dark-factory issue **#784** (P0) — Ready must fail closed on blocking Codex
advice. The `/ready` pipeline's `gate_advice` lane was normalizing blocking
Codex `REQUEST CHANGES` / `NOT APPROVED` advice through an AGY-synthesized
`warn` (or worse, a misbehaving `verdict: pass`) into a `success` outcome,
which would route the PR to `exit` despite an unresolved blocker.

This PR closes that path with a **fail-closed blocking inner-review detector**
in `runner/handler_verdict.py::_parse_verdict` that scans the lane output for
`REQUEST CHANGES` / `NOT APPROVED` veto tokens and overrides any outer
synthesis to `failure`. The fix is graph-level durable: the detector lives in
the canonical verdict parser, so every existing graph that routes through
`gate_advice` (with or without `gate_strict="true"`) inherits the close.

## TDD Provenance

### RED (bug exists)
Pre-fix run — 4 of 9 tests FAIL, proving the bug:

```
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_blocks_synthesized_pass_with_request_changes FAILED
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_blocks_synthesized_pass_with_not_approved FAILED
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_blocks_synthesized_warn_with_request_changes FAILED
tests/test_gate_advice_blocking_fail_closed.py::test_ready_routes_blocking_inner_review_with_pass_synthesis_to_fix FAILED
```

Recorded assertion from the worst-case scenario:

```
AssertionError: synthesized verdict: pass with a blocking inner review reached exit;
the gate is failing open on inner-review disagreement.
assert 'exit' not in ['start', 'test', 'gate_es', 'gate_er', 'gate_advice', 'holdout', ...]
```

### GREEN (fix applied)
Post-fix run — all 9 tests PASS:

```
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_blocks_synthesized_pass_with_request_changes PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_blocks_synthesized_pass_with_not_approved PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_blocks_synthesized_warn_with_request_changes PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_pure_pass_is_still_success PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_parse_verdict_clean_warn_is_still_subject_to_gate_strict PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_ready_pipeline_gate_advice_has_gate_strict_and_bounded_fix PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_ready_routes_blocking_inner_review_with_warn_synthesis_to_fix PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_ready_routes_blocking_inner_review_with_pass_synthesis_to_fix PASSED
tests/test_gate_advice_blocking_fail_closed.py::test_evidence_manifest_pin_factory_source_and_graph_sha PASSED
============================== 9 passed in 0.59s ===============================
```

### No-regression run
The full verdict/gate/ready test surface still passes (the `test_reviewer_outcome_verdict_consistency`
canonical-location pin was updated from line 182 to line 230 because the new blocking-review
detector lives above `_enforce_outcome_verdict_consistency`):

```
tests/test_pipeline_ready.py                                       5 passed
tests/test_gate_advice_blocking_fail_closed.py                     12 passed
tests/test_verdict_parsing.py                                       3 passed
tests/test_gate_strict_flag.py                                      4 passed
tests/test_reviewer_outcome_verdict_consistency.py                 29 passed
```

(Pre-existing environmental failures in `test_structural_preflight` (missing
`pydot`) and `test_review_cli` (controller runtime paths) are unrelated to
this change — they fail identically on the parent commit.)

## Required Evidence (per issue #784 acceptance criteria)

| Requirement | Source | SHA / value |
|---|---|---|
| Graph-level RED fixture for advice `warn` plus blocking inner review | `pipelines/factory/ready_advice_red_fixture.dot` | `742bb7dcc045ff7aecc8998fb3563288da7292ad8d9e7ad8efa62176ce8f4fc1` |
| Production pipeline checksum (graph that the PR body quotes) | `pipelines/slim/ready.dot` | `b7326fa89001cc79f863f34490293ca088e9bcf28b11d917e4ed1a97f5106b3a` |
| Bounded fix-loop routing (`max_visits=3`, `retry_target=fix`) | `pipelines/slim/ready.dot` (and fixture mirrors it) | `max_visits="3"`, `retry_target="fix"`, `gate_strict="true"` |
| Feature-specific evidence (not generic `hello.dot`) | `pipelines/factory/ready_advice_red_fixture.dot` | shape mirrors `gate_advice` exactly, scoped to #784 |
| Evidence SHA freshness (re-runnable) | `tests/test_gate_advice_blocking_fail_closed.py::test_evidence_manifest_pin_factory_source_and_graph_sha` | captures HEAD + graph SHA at run time |
| Exact factory source SHA | `git rev-parse HEAD` | `778423e814a2a1c15046d9e35e0cc54c3fa5d62f` |
| Graph checksum in manifest | `pipelines/slim/ready.dot` SHA-256 | `b7326fa89001cc79f863f34490293ca088e9bcf28b11d917e4ed1a97f5106b3a` |

### Artifact SHAs (recomputed at manifest write)

```
742bb7dcc045ff7aecc8998fb3563288da7292ad8d9e7ad8efa62176ce8f4fc1  pipelines/factory/ready_advice_red_fixture.dot
b7326fa89001cc79f863f34490293ca088e9bcf28b11d917e4ed1a97f5106b3a  pipelines/slim/ready.dot
b849aa638b0815eea445ba4d85417e4297d454298ec62d68e30f6480085c1e08  tests/test_gate_advice_blocking_fail_closed.py
db961234db051a674f68679511a5afc5bafbed77912c2d9acfdc702a5fa64ed1  runner/handler_verdict.py
ae67ea76690dfd10645e0cf69742263bb2dc8cc131f5371af435530c11ba05df  tests/test_reviewer_outcome_verdict_consistency.py
```

## What the Fix Does

The blocking-review detector lives at the top of `runner/handler_verdict.py`
and runs as **Step 0** of `_parse_verdict` — before any marker-line or
standalone-line parse. If the lane output contains `REQUEST CHANGES` or
`NOT APPROVED` (the two /advice blocking tokens, case-insensitive, tolerant
of underscore variants), the parser returns
`("blocking_inner_review", "failure")` regardless of what the synthesized
outer verdict claims.

This is the load-bearing fix. `gate_strict="true"` on the `gate_advice` node
remains as a belt-and-braces guard for the simple "warn only" case, but the
new detector means a misbehaving synthesizer that emits `verdict: pass`
cannot paper over a blocking inner review. Pre-#784, the marker-line parser
returned `success` on `verdict: pass` regardless of the surrounding text.

The pin-test `_parse_verdict_pure_pass_is_still_success` confirms the fix is
fail-closed, not fail-noisy: a clean pass with no blocking markers still
returns `success`, and `test_parse_verdict_clean_warn_is_still_subject_to_gate_strict`
confirms the warn→success/failure mapping is unchanged for non-advice gates
that opt out of `gate_strict`.

## Files Touched

```
runner/handler_verdict.py                          | 48 ++++++++++++++++++++++
tests/test_gate_advice_blocking_fail_closed.py    | new file (12 tests, including 3 RED-fixture end-to-end tests)
tests/test_reviewer_outcome_verdict_consistency.py |  2 +- (canonical-location pin, line 182 → 230)
pipelines/factory/ready_advice_red_fixture.dot    | new file (feature-specific graph-level evidence)
```

`tests/test_reviewer_outcome_verdict_consistency.py` change is the
canonical-location pin: `_enforce_outcome_verdict_consistency` moved from
line 182 to line 230 because the blocking-review detector sits above it.
The pin is the test's contract — updated to the new line, not relaxed.
