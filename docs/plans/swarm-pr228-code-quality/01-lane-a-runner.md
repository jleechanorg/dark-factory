# Lane A — runner/ diff review (pr228)

## Scope

- **Files reviewed (pr228 contribution only)**:
  - `runner/handler_render.py` — `_substitute_placeholders` non-str coercion (lines 60-75 in the new file)
  - `runner/handler_dispatch.py` — `resolved_backend_meta` JSON-string write + legacy-dict read tolerance (lines 720-754)
  - `runner/handler_parallel_reviewer.py` — `_check_stale_artifacts`, `_enforce_outcome_verdict_consistency`, and their call sites in `_parallel_reviewer`
- **Commits covered**: `2a383a1` (fix), `658a715` (tests)
- **Out of scope (excluded as origin/main drift)**: every other file in the `git diff origin/main..HEAD --shortstat` report (daemon/, docs/, scripts/agent-isolation/, config drift). The lane surface is the 3 runner/ files plus the 2 tests that exercise them.

The canonical contract this branch presupposes is in `runner/handler_core.py:39`:

```python
state: dict[str, str] = field(default_factory=dict)
```

i.e., `Context.state` is typed `dict[str, str]`. Every branch finding turns on whether pr228 enforces, papers over, or violates that contract.

---

## /thermo findings

### F1 — `dict[str, str]` contract is enforced at three sites instead of one (code judo)

- **Location**:
  - `runner/handler_dispatch.py:754` — writes `json.dumps(pq_meta)` to satisfy the contract
  - `runner/handler_dispatch.py:723-732` — read-back tolerates legacy `dict` / malformed `str` / `None` with a `try/except` chain
  - `runner/handler_render.py:69-74` — substitution silently coerces `dict/list/int` via `json.dumps` / `str()`
- **Rubric**: §0 (structural simplification), §5 (boundary/type cleanliness), §6 (canonical layer)
- **Evidence**: The same type invariant — "ctx.state values are strings" — is now defended in three places. The legacy-dict read tolerance at `handler_dispatch.py:721-732` exists only because the comment says "for backward compatibility during rollout" — but this is a single-file change with no multi-version migration path in the repo (the CLI runs the local runner; there is no shim for older runner processes), so "rollout" cannot mean anything other than "tolerating our own previous bug."
- **Severity**: strong
- **Code judo move**: delete the legacy-dict branch and the substitution-time coercion; keep only the write-site JSON encoding. Concretely:

  ```python
  # handler_dispatch.py:720-732 — collapse to a single assertion
  prior_meta_raw = ctx.state.get(f"{node.name}.resolved_backend_meta")
  assert isinstance(prior_meta_raw, str), (
      f"invariant: resolved_backend_meta must be JSON-string, got {type(prior_meta_raw)}"
  )
  prior_meta = json.loads(prior_meta_raw) if prior_meta_raw else {}
  ```

  ```python
  # handler_render.py:69-74 — delete entirely
  if isinstance(v, str):
      rendered = v
  elif isinstance(v, (dict, list)):
      rendered = json.dumps(v, sort_keys=True)
  else:
      rendered = str(v)
  # — replaced with:
  assert isinstance(v, str), f"ctx.state[{k!r}] violated dict[str,str] contract"
  rendered = v
  ```

  Net diff: ~15 lines deleted, the contract becomes a hard invariant, and a future write that breaks it fails immediately at the write site, not silently in the rendered prompt. Two of the three test cases in `tests/test_state_dict_substitution.py` (the dict and list cases) become impossible to express — that's the signal the contract is now real.

### F2 — `_check_stale_artifacts` is observability noise on the only path that matters

- **Location**: `runner/handler_parallel_reviewer.py:60-111` (definition) and `:262-276` (call site)
- **Rubric**: §2 (no random spaghetti growth), §4 (no hacky/magical abstraction), §7 (no sequential dead-end work)
- **Evidence**:
  1. The detector's "stale spec" path is anchored to a marker — `<!-- run_id: ` — that **no prompt in this repo ever writes** (`grep -rn "<!-- run_id" runner/ prompts/` returns only the detector itself at `runner/handler_parallel_reviewer.py:80`). On every real run, the only branch that fires is the else clause: `f"spec at {spec_path.name} has no run_id annotation (hash: {content_hash})"`. The detector's headline feature is unreachable; the always-firing fallback is pure noise.
  2. The 5-minute mtime window (`runner/handler_parallel_reviewer.py:106`) is a magic number with no documented rationale — and is the **opposite** of "stale." Anything modified in the last 5 minutes is fresh, not stale; the function name and the check invert each other.
  3. The function sets `ctx.state["_stale_artifact_warnings"]` (`runner/handler_parallel_reviewer.py:265`) and emits an observability event. A grep across `runner/`, `tests/`, and `prompts/` finds **zero readers** of either. The data is written and never consumed.
- **Severity**: strong
- **Suggested fix**: delete the entire `_check_stale_artifacts` function, its call site at `_parallel_reviewer:262-276`, and the always-firing fallback. If the team genuinely wants stale-artifact detection, the right home is:
  1. Have the plan/explore prompt actually emit the `<!-- run_id: <id> -->` marker (and the gate-checked spec.md lifetime rule) — push it to the upstream instruction layer.
  2. Replace the mtime heuristic with `git log -1 --format=%H -- spec.md` against `ctx.run_id` — deterministic, no magic number, no clock dependency.

### F3 — `_enforce_outcome_verdict_consistency` performs a backend semantic override

- **Location**: `runner/handler_parallel_reviewer.py:219-254` (definition); `:333` and `:363` (call sites)
- **Rubric**: §4 (hacky abstraction), §5 (boundary/type cleanliness)
- **Evidence**:
  ```python
  outcome_to_verdict = {
      "success": "pass",
      "failure": "fail",
      "error": "fail",
      "partial": "fail",
  }
  expected_verdict = outcome_to_verdict.get(outcome, "fail")
  if verdict and verdict != expected_verdict:
      md["verdict"] = expected_verdict
      md["verdict_adjusted_for_consistency"] = "true"
      md["original_verdict"] = verdict
  ```
  This function decides that **the reviewer's verdict was wrong** and silently rewrites it. The fix is described in the commit message as defending against "stale spec artifacts cause the reviewer to misjudge" — i.e., when the reviewer reads the wrong spec, the model emits a wrong verdict. The right place to fix "the reviewer misjudged because it read the wrong spec" is:
  - the prompt (tell the reviewer to verify spec currency), or
  - the spec writer (give specs run_id markers so staleness is detectable upstream), or
  - a re-run after fixing the spec (don't override what the reviewer actually concluded).

  Muting the model output and stamping `original_verdict` in metadata so a downstream reader can "see" what happened is exactly the pattern the ZFC skill calls out: "Treating a backend correction as the fix when the model is still being asked the wrong thing."

  Also: `verdict_adjusted_for_consistency` / `original_verdict` are set but never read anywhere in `runner/` or `tests/` outside of the function that wrote them — the override is invisible to consumers.
- **Severity**: blocker
- **Suggested fix**: pick one of three cleaner moves.
  1. **Collapse the dual field.** Ask the reviewer prompt for `outcome` only; derive `verdict` server-side from the same `outcome_to_verdict` table. No contradiction surface, no override, no metadata trail. This is the ZFC minimal-semantic-fields pattern.
  2. **Surface the disagreement instead of hiding it.** When `verdict != expected_verdict`, fail loudly (a Healer-flagged outcome, an observability event with both values), do not silently mutate the metadata.
  3. **Address the actual upstream.** If stale specs cause misjudgment, fix the spec currency check (see F2) so the reviewer reads current content. Do not paper over the symptom in the post-processing step.

  Move (1) is the strongest judo: it deletes the override function, the override call sites, and 70 lines of regression test.

### F4 — File-size / decomposition: not yet a problem, but the new helpers push toward it

- **Location**: `runner/handler_parallel_reviewer.py` (363 lines, +53 from this branch)
- **Rubric**: §1 (do not push a file from <1k to >1k)
- **Evidence**: The file is at 363 lines; threshold is 1000. Not a near-term concern. However, `_parallel_reviewer` (the entry point) is now the place where stale-artifact detection, observability event emission, primary review, shadow review coalescing, *and* outcome/verdict override all live. The single function reads as five separate responsibilities stitched together.
- **Severity**: nit (today)
- **Suggested fix**: if F3's override is deleted, this concern largely goes away. If F3 is kept, consider extracting a `_post_process_result(result)` step that bundles the override, so `_parallel_reviewer` reads as a pipeline: `detect-stale → primary → coalesce-shadovs → post-process → return`.

### F5 — Boundary/type cleanliness: `state` typed `dict[str, str]` but never enforced

- **Location**: `runner/handler_core.py:39` (the type contract) + `runner/handler_render.py:62` (the only consumer that would care)
- **Rubric**: §5 (boundary/type cleanliness)
- **Evidence**: The dataclass field is `dict[str, str]`. With the coercion in `_substitute_placeholders`, a dict/list/int write no longer crashes — it silently round-trips. There is no enforcement at the write site, no enforcement at the read site, and no runtime check on either. The "contract" is documentation; the actual invariant is "callers do whatever they want, and the substitution layer accommodates it."
- **Severity**: nit (already counted under F1)
- **Suggested fix**: the F1 judo fix (assertion at write site, deletion of coercion) resolves this.

### F6 — Sequential orchestration: no parallelism regression introduced

- **Location**: `runner/handler_parallel_reviewer.py:262-265` (new code), `:300-322` (existing)
- **Rubric**: §7
- **Evidence**: The new `_check_stale_artifacts` runs synchronously before `_handlers_shim._render_prompt` (which itself blocks on prompt rendering) and before `_launch_shadow_gate_review` (which already parallel-launches shadow reviewers). The file-scan is small (two files + one glob) and not a wall-clock concern. No regression.
- **Severity**: nit (no finding)
- **Suggested fix**: none.

---

## /code-standards findings

### F7 — ponytail rung 7 (write minimum code) violated: three-layer defense for one violation

- **Lane**: ponytail
- **Location**: same as F1 — `handler_dispatch.py:721-732`, `handler_dispatch.py:754`, `handler_render.py:69-74`
- **Evidence**: Only one site (the dispatch writer at `handler_dispatch.py:754`) ever violated `dict[str, str]`. The minimal fix is one line at that site (already present: `json.dumps(...)`). The other two sites — legacy-dict tolerance and silent coercion — are extra surface for a contract that has no second source.
- **Severity**: strong
- **Suggested fix**: same as F1.

### F8 — ZFC violation: backend silently overrides a model-owned semantic field

- **Lane**: ZFC (zero-framework-cognition)
- **Location**: `runner/handler_parallel_reviewer.py:219-254` (`_enforce_outcome_verdict_consistency`)
- **Evidence**: Per ZFC §"Model Decision Engines" and §"Field Ownership", `verdict` is a model-owned semantic field ("decisions, selected intent, target state, narrative meaning, choices, reasons"). The override mutates it without consulting the model that produced it. The ZFC skill explicitly classifies this kind of guard as a "ZFC violation candidate": *"backend logic performs semantic judgment, classification, routing, or choice generation that belongs to the model."*
- **Severity**: blocker
- **Suggested fix**: see F3 move (1) — collapse `verdict` to a deterministic derivation from `outcome` and stop asking the model for a field the backend will overwrite anyway.

### F9 — ZFC leveling: dual-field contract gives the model multiple ways to contradict itself

- **Lane**: ZFC-leveling
- **Location**: model contract for reviewer prompts — `prompts/slim/review_main.md` and equivalents (not in the diff but implied)
- **Evidence**: The reviewer model emits both `outcome` and `verdict`. Per ZFC's "Minimal Semantic Fields": *"The first contract has one semantic fact... The second contract gives the model many chances to contradict itself."* `outcome` and `verdict` are two expressions of the same fact. The override exists because the contract allows them to disagree.
- **Severity**: strong
- **Suggested fix**: same as F3 move (1). Remove `verdict` from the model contract; compute it server-side from `outcome`.

### F10 — root-cause-first: Bug 1 patched at the symptom, not the invariant

- **Lane**: root-cause-first
- **Location**: `runner/handler_dispatch.py:754` (write site) + `runner/handler_render.py:69-74` (substitution site)
- **Evidence**: Bug 1 is `TypeError: can only concatenate str (not "dict") to str` when `${state.<node>.resolved_backend_meta}` is rendered. Root cause: `handler_dispatch.py:754` violated the `dict[str, str]` contract by writing a dict. The patch does fix the write site, but ALSO adds coercion at the substitution site AND legacy-dict tolerance at the read site. Per root-cause-first's "Backend protection rules": keep the guard narrow, log when it fires, document why prompt/schema correction was insufficient. The justification was "we may encounter old ctx.state values from before this fix" — but ctx.state is per-process, not persistent. There is no path for an old dict value to survive into a new process. The two extra layers have no real failure mode they protect against.
- **Severity**: strong
- **Suggested fix**: keep the write-site fix; delete the read-site legacy-dict branch (it has nothing to be backwards-compatible with); delete the substitution-time coercion; add a single `assert isinstance(v, str)` at the substitution site as the contract enforcement.

### F11 — root-cause-first: Bug 2 patched at the symptom, not the upstream instruction

- **Lane**: root-cause-first
- **Location**: `runner/handler_parallel_reviewer.py:219-254` (override) + `:60-111` (stale check that doesn't actually catch staleness)
- **Evidence**: Bug 2 is "outcome=failure with verdict=pass" emitted by the reviewer. The reviewer emitted that combination because (a) the model contract lets it, and (b) the spec the reviewer read was stale. The patch addresses neither root cause:
  - It does not remove the dual-field contradiction surface (see F8/F9).
  - It does not fix spec staleness (the only staleness check that fires is the always-on "no run_id annotation" branch, see F2).
  Instead, it overrides the verdict silently. Per root-cause-first's proof taxonomy: this is **Unproven fallback** — the tests prove the override works, but there is no raw request/response evidence that prompt/schema cannot handle the dual-field contract.
- **Severity**: blocker
- **Suggested fix**: see F3.

### F12 — root-cause-first: missing canonical-layer fix for the `dict[str, str]` contract

- **Lane**: root-cause-first / code-centralization
- **Location**: should be in `runner/handler_core.py` (Context dataclass) or a `StateStore` helper; not in three separate handlers.
- **Evidence**: A `dict[str, str]` typed field on a dataclass should be enforced by the dataclass itself — either a `__post_init__` validator, a property setter, or a `__setitem__` proxy via a custom container. With none of those, every handler writer is responsible for upholding the contract individually, and the only consumer (`_substitute_placeholders`) silently absorbs violations. That is exactly the inverse of how a type contract should work.
- **Severity**: strong
- **Suggested fix**: introduce a `class StringState(dict[str, str])` subclass that asserts on `__setitem__` with non-str values; have `Context.state` default to `StringState()`. Then every writer either complies or fails at the source. Delete the legacy-dict branch and the coercion site — they become unreachable.

---

## Overlap / cross-lens observations

| Convergence point | /thermo | /code-standards |
|---|---|---|
| Three-layer defense for a `dict[str, str]` contract | F1 (judo) | F7 (ponytail), F10 (RCF), F12 (canonical layer) |
| `_enforce_outcome_verdict_consistency` overrides model judgment | F3 (hacky abstraction) | F8 (ZFC), F9 (ZFC leveling), F11 (RCF) |
| `_check_stale_artifacts` writes unread state, fires always-on noise | F2 (spaghetti) | (ponytail rung 6: not one line) |

The strongest convergence: **F3 ≡ F8 ≡ F11** — the verdict override is simultaneously a code-judo missed opportunity, a ZFC violation, and an unproven root-cause-first fix. This is the highest-conviction finding in the lane.

Second-strongest: **F1 ≡ F7 ≡ F10 ≡ F12** — the triple-defended type contract is a missed judo move, a ponytail violation (more code than the fix needs), an unproven root-cause-first guard (no second-version-survives scenario), and a canonical-layer miss (the contract has no canonical enforcement home).

---

## Lane summary

| Severity | Count |
|---|---|
| blocker | 2 (F3, F8) — same root finding, listed once per lens |
| strong | 5 (F1, F2, F4-adjacent, F7, F9, F10, F11, F12) |
| nit | 2 (F4, F5, F6) |

The 2 blocker findings both point at the same move: **collapse `verdict` to a deterministic derivation from `outcome` and delete `_enforce_outcome_verdict_consistency`**. That single refactor deletes ~35 lines of production code, ~70 lines of regression test, the entire override call sites at `handler_parallel_reviewer.py:333` and `:363`, and removes the contradiction surface that prompted the bug. The 5 strong findings collapse into a related move: **enforce `dict[str, str]` once at the Context.state write boundary**, then delete the read-site legacy-dict branch, the substitution-site coercion, and the corresponding tests. Together these two refactors remove approximately 130 lines from pr228's net contribution while preserving the actual bug fix (no more TypeError; no more contradictory verdicts).

**Verdict**: pr228 fixes two real symptoms but reaches for the wrong instrument in both cases — defensive coercion layered on top of an unenforced contract, and silent override of a model-owned semantic field. Both should land at the canonical layer (Context.state validation; reviewer contract schema) rather than at the read/promotion sites. The cross-lens convergence on F3/F8/F11 makes the verdict-override deletion the single highest-value change; the F1/F7/F10/F12 cluster makes the contract-enforcement refactor the second.
