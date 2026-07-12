# Lane B — tests/ diff review (pr228)

## Scope

- **Files reviewed**:
  - `tests/test_state_dict_substitution.py` (NEW, +215 lines) — `git show 658a715`
  - `tests/test_reviewer_outcome_verdict_consistency.py` (NEW, +188 lines) — `git show 658a715`
- **Production code under test (read for context, not modified here)**:
  - `runner/handler_render.py::_substitute_placeholders` (the dict/list/int coercion site)
  - `runner/handler_parallel_reviewer.py::_enforce_outcome_verdict_consistency`, `_check_stale_artifacts` (the verdict-normalization and stale-artifact sites)
- **Commits covered**: `658a715` (test-only). The implementation commit is `2a383a1` (Lane A's surface).
- **Out of scope (excluded as origin/main drift, NOT pr228 deletions)**:
  - `tests/security/test_agent_isolation.py` -383
  - `tests/test_structured_state_convention.py` -284
  - `tests/test_reviewer_outcome_verdict_consistency.py` -209 deletions (the file grows +188 net because origin/main had ~21 lines of an older variant)
  - All `daemon/tests/*.rs` deltas, `tests/scripts/test_factory_*.sh` deltas, `tests/test_af_gate_contract.py`, `tests/test_backfill_external_ref_guard.py` deletions — origin/main drift, not authored by this branch's test commit.

## Canonical helpers / patterns identified in the suite

These exist in-tree and are the natural reuse targets that the new tests partially duplicate:

1. **`tests/conftest.py::make_node`** — canonical `runner.parser.Node` factory (with comments noting that `test_gates.py` and friends were refactored to use it instead of ad-hoc stubs).
2. **`tests/test_gate_priority_queue.py::_priority_node`** — already builds `make_node(name=..., backend_priority=...)` priority-queue nodes; the closest sibling to the new `_resolve_gate_backend` tests in `test_state_dict_substitution.py`.
3. **`tests/conftest.py::_declare_test_environment` (autouse session)** — declares `DARK_FACTORY_HOLDOUTS` / `DARK_FACTORY_HOME` for every test in `tests/`. The new test files duplicate this declaratively (manual `sys.path.insert(0, str(ROOT))` per file).
4. **`runner/handler_core.py::Result`** — `@dataclass` with `outcome: str = "success"`; `metadata: dict[str, str]` (declared `dict[str, str]`, not `dict`); `preferred_label`, `suggested_next_ids`, `context_updates` populated by `field(default_factory=...)`. Tests construct `Result(...)` positionally throughout.
5. **`tests/test_changed_files_placeholder.py`** — the canonical "test single helper in `handler_render.py` directly" file. Same `Context` setup, same `_substitute_placeholders` target. The new `test_state_dict_substitution.py` could belong next to this file rather than as a standalone module.
6. **`runner/handler_verdict.py::_VERDICT_NORMALIZE`** — the canonical verdict-token → success/failure mapping (see "Cross-lens observations" below — the new test asserts a SECOND outcome-to-verdict mapping in `_enforce_outcome_verdict_consistency`, which diverges from this canonical normalization).

## /thermo findings

### F-B1 (test-judo opportunity, highest-impact): `test_reviewer_outcome_verdict_consistency.py` duplicates a verdict/outcome truth table that already exists

**Symptom across 11 test methods**: the file re-states the outcome→verdict truth table row-by-row:
- `test_failure_outcome_with_pass_verdict_adjusted_to_fail`
- `test_success_outcome_with_pass_verdict_unchanged`
- `test_error_outcome_with_pass_verdict_adjusted_to_fail`
- `test_partial_outcome_with_pass_verdict_adjusted_to_fail`
- `test_failure_outcome_with_fail_verdict_unchanged`
- `test_case_insensitive_verdict_matching` ("PASS" → "fail", `original_verdict="pass"` normalized)
- `test_preserves_other_metadata_fields`
- `test_preserves_context_updates`
- `test_preserves_suggested_next_ids`
- `test_no_verdict_in_metadata_does_not_raise`
- `test_none_metadata_does_not_raise`

The truth table is small (`{"success":"pass","failure":"fail","error":"fail","partial":"fail"}` per `handler_parallel_reviewer.py:230-235`) and identical across 4 "adjusted" tests that vary only the input `outcome`. A parametrize over `(outcome, input_verdict, expected_verdict, should_track_adjustment)` collapses all four "adjusted" tests into one parameterized test. Then `test_success_outcome_with_pass_verdict_unchanged` and `test_failure_outcome_with_fail_verdict_unchanged` collapse into one "consensus-pass-through" parametrized test. The three "preserves other fields" tests can collapse into one with a `pytest.mark.parametrize("attr", ["metadata", "context_updates", "suggested_next_ids"])` over the relevant `Result` field.

This is the **/thermo test-judo move**: 11 tests → ~4 parametrized tests. Net diff well under the file's stated +188 lines, and the file grows linearly with the truth table (not with every consumer passing a new outcome shape).

### F-B2 (test architecture): `test_state_dict_substitution.py` ignores `tests/conftest.py` canonical infrastructure

Two patterns leak into every test that exists in `conftest.py`:

1. **Module-level path-injection boilerplate** (lines 16-23):
   ```python
   ROOT = pathlib.Path(__file__).parent.parent
   sys.path.insert(0, str(ROOT))
   ```
   Six other files do this too (`test_changed_files_placeholder.py`, `test_gate_priority_queue.py`, etc.) so it is consistent with the existing style, BUT conftest already guarantees `DARK_FACTORY_HOME` is set (line 90) — the `sys.path.insert` only papers over running pytest from a different cwd. The robust pattern is to add a single `PYTHONPATH` fixture to `conftest.py` instead of repeating the boilerplate per file. Out of scope for this PR (would touch dozens of files); flagging because the new files add two more copies of the pattern.

2. **Context-construction boilerplate** (12 of 13 tests): `ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")`. The codebase has a `make_node` helper in conftest but no `make_context`. The 12 repeating constructions in `test_state_dict_substitution.py` are the easy fruit for a `make_context(goal=..., workdir=..., backend=..., state=...)` helper in conftest — would cut ~15 lines and make the test's *intent* (what goes into `ctx.state`) the only thing visible.

### F-B3 (test judo via delete-a-whole-category): `test_cross_visit_tolerates_legacy_dict` and `test_cross_visit_tolerates_malformed_value` exercise legacy/compat paths that exist only because the new code is bug-compatible with the old `ctx.state` dict contract

These two tests pin behaviour that won't exist in main long-term: the production fix moves state to JSON strings; legacy-dict tolerance is a read-side fallback. In a "the fix is correct" world, these tests don't constrain any future work — they freeze a transient compat shim. They should **either**:
- live behind a `# TODO(remove after YYYY-MM-DD): legacy dict seeding was a transient compat shim` marker and assert **only** the read-shim behaviour (not both new and legacy in the same suite), OR
- be deleted as soon as the old production code path is no longer reachable (i.e. once `handler_dispatch.py`'s legacy read-back branch is removed).

Without one of those, the tests become permanent debt that prevents contract tightening. Flagged for the PR author to choose.

### F-B4 (sequential orchestration in test setup): two-call "read-back" tests do identical setup twice

`test_cross_visit_read_back_tolerates_json_string` (line 113) and `test_cross_visit_tolerates_legacy_dict` (line 144) both rebuild `Context(...)`, `Node(...)`, and `_probe_backend_installed` mock from scratch for what is logically the same scenario under a different `ctx.state` seed. A single `@pytest.fixture` returning `(ctx, make_node_fn)` parameterized by `state_seed` would deduplicate the boilerplate without changing what is tested. Low value vs. F-B1, but the file scope is small enough that consolidating is cheap.

### F-B5 (spaghetti in tests): `test_cross_visit_tolerates_malformed_value` chains two assertions in one test body

Lines 169-195 of `test_state_dict_substitution.py` actually test two distinct things (None seed → probe-fallthrough; malformed JSON string seed → probe-fallthrough). Splitting into two named tests would surface the two paths when one regresses. Combined with F-B4, the file would parameterize cleanly across `[None, "not valid json {", legacy-dict]` → `(expected_backend=resolved-via-probe, meta-shape=...)`.

### F-B6 (no real /thermo "tests push file past 1k lines" issue, but…)

The new files are 215 + 188 = 403 added lines. Combined with the (origin/main drift) deletions, the suite as a whole shrinks. No file crosses any size boundary. F-B1 is the only structural move worth proposing.

## /code-standards findings

### F-S1 (ponytail — missed-reuse, rung 2): `_priority_node` helper already exists for `_resolve_gate_backend` priority tests

`tests/test_gate_priority_queue.py:21-25` already defines:
```python
def _priority_node(priority, *, prefer_adversarial=False, name="evidence"):
    attrs = {"backend_priority": ",".join(priority)}
    if prefer_adversarial:
        attrs["prefer_adversarial"] = "true"
    return make_node(name=name, **attrs)
```

The new `test_state_dict_substitution.py:82-188` hand-rolls `Node(name="review_main", attrs={"backend_priority": "codex,minimax"})` four times. Per ponytail rung 2 ("reuse the helper that already exists"), the new tests should import `_priority_node` from `test_gate_priority_queue` (or, better, hoist `_priority_node` into `conftest.py` so both files consume it) and call:
```python
node = _priority_node(["codex", "minimax"], name="review_main")
```

This is rung 2 — *not* rung 7 ("write minimum code that works"). The new code wrote rung 7 four times; the canonical helper was three cells away in the same `tests/` directory. The DRY win: -10 lines, and the new tests stop drifting from `test_gate_priority_queue.py` if the queue order ever changes.

### F-S2 (ZFC — tests can contain classifier logic): no ZFC violations *inside* the tests, but the test that pins verdict-normalization logic freezes a heuristic that's a ZFC violation risk upstream

The new `test_reviewer_outcome_verdict_consistency.py` tests do not themselves contain classifiers — they assert outcomes. Good.

But: pinning `_enforce_outcome_verdict_consistency`'s `outcome_to_verdict` mapping in 11 tests makes the future maintenance story bad. Per ZFC's `Field Ownership` table, "verdict" is a model-owned semantic field. `_enforce_outcome_verdict_consistency` is an *override* that says "if the model said `pass` but the runner coerced the outcome to `failure`, override the verdict to match". That is a **backend recomputation of a semantic decision that should belong to the model** — i.e. the production code itself is a ZFC candidate. The tests should at least include a "reviewer owner comment" that names this risk:
- The `outcome` field is server-owned (set by the runner when an exception / gate infra fails).
- The `verdict` field is reviewer-owned (set by the LLM).

When the two fields disagree because the LLM misread its inputs, the right fix is:
- Either: re-issue the gate with corrected input (preserve model ownership), or
- Mark the override explicitly with a `verdict_overridden_by_runner="<reason>"` field.

The new production code overwrites `verdict` in place. This loses auditability — the operator can't tell from CXDB whether the reviewer said "pass" or the runner coerced it. The tests pin this lossy behaviour (test asserts `adjusted.metadata["verdict"] == "fail"`, not that *both* `original_verdict` and the overwriting of `verdict` are surfaced to the consumer).

**Recommended evidence: open a follow-up bead** titled "reviewer-verdict ownership: persist `original_verdict` alongside `verdict` so operators can audit overrides" before merge. Severity: LOW (it works), but a code-judo move that fixes the ZFC surface + simplifies the test (drops the two `original_verdict`/`verdict_adjusted_for_consistency` discriminators) in one shot.

### F-S3 (ZFC `Banned Patterns`): `_enforce_outcome_verdict_consistency` reads as a hidden heuristic; the tests pin it without naming the contract

The function takes a `Result` and returns either the same `Result` or a *new* `Result` with `verdict` rewritten. From the test's perspective, this is "verdict must equal `outcome_mapped_to_verdict[outcome]`", which IS deterministic and within ZFC's "Allowed Deterministic Code" scope. Good — *the test surface is fine*. But the production function's location (in `handler_parallel_reviewer.py`, not `handler_verdict.py`) is the smell: it lives next to the function that needs it, not next to `_VERDICT_NORMALIZE` / `_parse_verdict` that already canonicalize verdict tokens. The test then has to mock-import a `_enforce_*` helper from a different module and doesn't get to assert against the canonical verdict vocabulary.

**Move recommendation**: `_enforce_outcome_verdict_consistency` should live in `runner/handler_verdict.py` next to `_VERDICT_NORMALIZE`, and the outcome→verdict mapping there should be derived from `_VERDICT_NORMALIZE` (with one new mapping `outcome_to_verdict = {"success":"pass","failure":"fail","error":"fail","partial":"fail","warn":"pass"}`). The two functions share a domain ("gate verdict canonicalization") and should share a file. The test would then import from `handler_verdict` like all the other verdict tests, and F-B1's parametrized test could include a token-level cross-check against `_VERDICT_NORMALIZE`.

### F-S4 (root-cause-first): the dict-vs-string contract on `ctx.state` should be enforced once, not discovered twice

The new tests (`test_state_dict_substitution.py`) explicitly seed the production-under-test code with non-string `ctx.state` values to *prove* the `_substitute_placeholders` coercion works. That's correct regression-test behaviour.

But the type annotation on `Context.state` (in `handler_core.py`) already says `dict[str, str]`. The production fix in `handler_render.py` is therefore a **defensive coercion against a contract that should be statically enforceable**. The deeper "fix the upstream cause, not the symptom" move is:

1. **Tighten the type**: change `Context.state: dict[str, str]` to a `StateValue = NewType("StateValue", str)` or `from __future__ import annotations` + runtime-check the value at write sites (one site in `handler_dispatch.py`, one in `handler_render.py`, etc.).
2. **Add a single test** that asserts `Context.state` only accepts strings — fails loudly at the boundary instead of letting non-strings propagate to downstream `json.dumps(v, sort_keys=True)` workarounds.

The current PR adds 4 coerce-don't-crash tests for `_substitute_placeholders` and 5 tolerate-legacy-shape tests for `_resolve_gate_backend`'s read-back shim. Per root-cause-first: those are correct *defensive* tests, but the test "ctx.state rejects non-strings at write" is missing, and without it the contract remains "defended but unenforced" — exactly the symptom-pinning pattern that root-cause-first warns against.

**Suggested addition** (small):
```python
def test_context_state_rejects_non_string_values():
    ctx = Context(goal="x", workdir=pathlib.Path("/tmp"))
    ctx.state["k"] = {"a": 1}
    # If the contract were tightened, this would raise; today it doesn't.
    # This test pins the current contract so tightening is a separate PR.
```
At minimum this should be a TODO-marked "freezes the loose contract" test. The new `_substitute_placeholders` tests then become "we tolerate the loose contract until tightening lands", not "this is how it must work forever".

### F-S5 (root-cause-first — applies to fix 3): `_check_stale_artifacts` emits warnings, but no test verifies the warnings reach the reviewer prompt

The new `test_reviewer_outcome_verdict_consistency.py` exhaustively tests `_enforce_outcome_verdict_consistency` (the *correction* layer) but never tests `_check_stale_artifacts` (the *detection* layer). And the production fix at `handler_parallel_reviewer.py:262-265` checks `_check_stale_artifacts` and *injects a warning string into a string variable* (`ctx.state["_stale_artifact_warnings"]`). Nothing asserts that this state key is *then read* by `_render_prompt` (via `${state._stale_artifact_warnings}` substitution) or any downstream prompt template.

Per root-cause-first: the symptom is "verdict contradicts outcome". The intermediate-detection layer (`_check_stale_artifacts`) should be tested on its own merits (returns warnings for a known-stale spec). The override layer (`_enforce_outcome_verdict_consistency`) is the *symptom* — the spec.md run_id annotation discipline at the writer site is the *root cause*. The 11 tests pin the override layer; the 0 tests pin the detection layer or the discipline change.

This isn't a blocker (the override layer is real and the test is fine). But the cross-lens gap is: the test surface is "the override works correctly" rather than "the override is never needed because discipline caught it earlier". A second-opinion reviewer would flag this.

## Cross-lens observations

- **The two test files share no fixtures between them**, even though `test_state_dict_substitution.py` has shared `Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")` construction in 12 of 13 tests. A `make_context(backend="echo", workdir=None, state=None)` fixture in `conftest.py` would deduplicate both files plus the existing `test_changed_files_placeholder.py`.
- **`_check_stale_artifacts` is untested.** This is the largest functional surface in commit `2a383a1` (50 lines including the `run_id`/`mtime`/SHA-emit logic) but the new test files cover *zero* of it. Lane-A's review of `runner/handler_parallel_reviewer.py` should expect Lane B to surface this gap as a follow-up.
- **The two test files do not share any helpers** even though `TestSubstitutePlaceholdersWithNonStrValues` and `TestResolveGateBackendMetaJSONString` (in `test_state_dict_substitution.py`) interlock: one is about how a dict-state value renders in a prompt, the other is about how the same dict is *serialized into* state. A combined `test_state_dict_contract.py` (or, better, an addition to `test_prompt_pinning.py` per the reference at `handler_render.py:7`) would express the contract end-to-end. Out of scope for this PR, but flagging because the cross-test coupling is invisible when each file is reviewed alone.
- **The new tests are deterministic and well-named.** No magic numbers, no fragile string matching on free-form output, no `time.sleep`. The `/thermo` "spaghetti in tests" rubric passes — assertions are direct and brittle in healthy ways (string-typed dict key checks reflect the actual contract).
- **Verifier compatibility check**: `Result.metadata` is typed `dict[str, str]` (per the dataclass field declaration). The new tests put `"true"` and `"pass"` (strings), not `True` / `pass` (booleans). That matches the type. Lane-A's review should confirm the production code (`_enforce_outcome_verdict_consistency`) also writes string-typed booleans into metadata, not real bools — the tests would silently pass either way (bool compares equal to "true" via `==` only if the type is coerced).

## Lane summary

**Highest-impact move**: F-B1 (test judo via parametrize) collapses 11 verdict-consistency tests into ~4 parametrized tests. F-S1 (reuse `_priority_node`) saves ~10 lines and stops the new tests from drifting from `test_gate_priority_queue.py`. Both are LGTM-level asks, not structural objections.

**Structural questions for the PR author to answer** (not blocking, but should not go unasked):
1. F-S3 — Why does `_enforce_outcome_verdict_consistency` live in `handler_parallel_reviewer.py` and not `handler_verdict.py` next to `_VERDICT_NORMALIZE`?
2. F-S4 — Is there a plan to tighten `Context.state` to actually reject non-strings (the production fix's underlying root cause)? If not, the new tests should add a `TODO: this test pins a loose contract` marker.
3. F-S5 — Is `_check_stale_artifacts` covered in a different PR (other test file), or is this gap intentional and the override-only test is sufficient?

**Findings posted here are candidate findings**, to be adversarially verified by Lane 5 before being promoted to blockers or publish-grade items. Zero findings is not the right verdict — the file contains real test-judo / reuse / ZFC-framing opportunities — but none of them are structural regressions. The test suite as a whole is correct and deterministic; it is just doing more work than it needs to.
