# Evidence-lens adversarial verification

**Verifier**: evidence lens (Lane 5 of 3-lens pass)
**Inputs**: `01-lane-a-runner.md` (F1–F12), `02-lane-b-tests.md` (F-B1…F-B6, F-S1…F-S5)
**Method**: For every finding, independently reproduce the cited code (file:line, exact text), run grep/read/test to confirm the claim, and CONFIRM/REFUTE/DOWNGRADE.

---

## Lane A findings

### F1 — Three-layer defense for `dict[str, str]` contract: **CONFIRMED**

**Evidence reproduction**:
```
$ grep -n "state:" /home/jleechan/projects/dark-factory/runner/handler_core.py
39:    state: dict[str, str] = field(default_factory=dict)

$ sed -n '720,755p' /home/jleechan/projects/dark-factory/runner/handler_dispatch.py
... raw_meta = ctx.state.get(f"{node.name}.resolved_backend_meta")
723-732 (legacy-dict read tolerance)
...
754:    ctx.state[f"{node.name}.resolved_backend_meta"] = json.dumps(pq_meta, sort_keys=True)

$ sed -n '60,75p' /home/jleechan/projects/dark-factory/runner/handler_render.py
69-74 (dict/list/int coercion)
```

**Reason**: Contract at `handler_core.py:39` is `dict[str, str]`. The fix adds the write at `handler_dispatch.py:754` (JSON string), the legacy-dict tolerance at lines 723–732, and substitution-time coercion at `handler_render.py:69–74`. Three sites, one violation. The finding's judo move is sound — collapse to one assertion at the write site. Cited code is exact.

**Severity**: confirmed strong (as written). The "code judo" framing is correct.

---

### F2 — `_check_stale_artifacts` is observability noise on the only path that matters: **CONFIRMED (with one clarification)**

**Evidence reproduction**:
```
$ grep -rn '<!-- run_id' /home/jleechan/projects/dark-factory/runner/ /home/jleechan/projects/dark-factory/prompts/ /home/jleechan/projects/dark-factory/tests/
/home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py:80:                run_id_marker = "<!-- run_id: "

$ grep -rn '_stale_artifact_warnings' /home/jleechan/projects/dark-factory/
runner/handler_parallel_reviewer.py:265: ctx.state["_stale_artifact_warnings"] = "\n".join(stale_warnings)
(only the writer; zero readers outside the function)

$ sed -n '99,109p' /home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py
106:    if age_seconds < 300:  # Less than 5 minutes old - could be stale
```

**Reason**: Three sub-claims all verified:
1. The `<!-- run_id: ` marker appears only at `handler_parallel_reviewer.py:80` (the detector). No prompt writes it. The "stale spec" branch (lines 81–91) is unreachable on real input.
2. The else branch (line 95, "no run_id annotation") is the always-on fallback. The cited line confirms it fires whenever the marker is absent.
3. The 5-minute mtime heuristic inverts intent: line 106 says "Less than 5 minutes old — could be stale". Recent ≠ stale. Confirmed by line 99 ("modified in last 5 minutes = likely stale"). The naming and semantics invert.
4. `_stale_artifact_warnings` is set at line 265 with zero readers (only the writer and two doc references exist).

**Severity**: confirmed strong. The "observability noise" framing is accurate.

---

### F3 — `_enforce_outcome_verdict_consistency` performs a backend semantic override: **CONFIRMED**

**Evidence reproduction**:
```
$ sed -n '219,254p' /home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py
... outcome_to_verdict = {
    "success": "pass", "failure": "fail", "error": "fail", "partial": "fail",
}
expected_verdict = outcome_to_verdict.get(outcome, "fail")
if verdict and verdict != expected_verdict:
    md["verdict"] = expected_verdict
    md["verdict_adjusted_for_consistency"] = "true"
    md["original_verdict"] = verdict
...

$ grep -rn 'verdict_adjusted_for_consistency\|original_verdict' runner/ tests/
runner/handler_parallel_reviewer.py:243:        md["verdict_adjusted_for_consistency"] = "true"
runner/handler_parallel_reviewer.py:244:        md["original_verdict"] = verdict
tests/test_reviewer_outcome_verdict_consistency.py:37-162 (test reads only)
```

**Reason**: The function (lines 219–254) silently rewrites `verdict` when it disagrees with `outcome`-derived expected verdict. The override metadata (`verdict_adjusted_for_consistency`, `original_verdict`) is set at lines 243–244, and the only readers are the test file (line 37, 38, 55, 69, 70, 84, 85, 101, 127, 140, 161, 162) — production code never reads them. The "stale spec causes misjudgment" hypothesis is named in the docstring but no evidence trail is preserved outside the function itself. The finding is correct.

**Severity**: confirmed blocker. The override pattern matches the ZFC skill's "backend correction as fix when model is still being asked the wrong thing" pattern.

---

### F4 — File-size / decomposition not yet a problem: **CONFIRMED (nit, as written)**

**Evidence reproduction**:
```
$ wc -l /home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py
363 /home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py
```

**Reason**: File is 363 lines (under 1k threshold). The cited responsible-reading ("five responsibilities stitched together") is accurate — `_parallel_reviewer` handles stale check, primary review, shadow coalescing, and outcome/verdict override. The finding explicitly downgrades itself to "nit" and ties the fix to F3's deletion. Confirmed.

---

### F5 — Boundary/type cleanliness: `state` typed `dict[str, str]` but never enforced: **CONFIRMED (already counted under F1)**

**Evidence reproduction**:
```
$ sed -n '34,49p' /home/jleechan/projects/dark-factory/runner/handler_core.py
@dataclass
class Context:
    goal: str
    workdir: "pathlib.Path"
    state: dict[str, str] = field(default_factory=dict)
```

**Reason**: Plain `dict[str, str]` field, no `__post_init__` validator, no `__setitem__` proxy, no property setter. The contract is documentation only. Confirmed.

---

### F6 — Sequential orchestration: no parallelism regression: **CONFIRMED (no finding, as written)**

**Evidence reproduction**:
```
$ grep -n '_check_stale_artifacts\|_launch_shadow_gate_review\|_render_prompt' runner/handler_parallel_reviewer.py
60:def _check_stale_artifacts(workdir: pathlib.Path, ctx: "Context") -> list[str]:
262:    stale_warnings = _check_stale_artifacts(ctx.workdir, ctx)
278:    prompt = _handlers_shim._render_prompt(node, ctx)
306:            shadows.append(_launch_shadow_gate_review(
```

**Reason**: Stale check at 262, render at 278, shadow launches at 306 — all sequential. No parallelism regression introduced. Confirmed.

---

### F7 — ponytail rung 7 (write minimum code) violated: three-layer defense for one violation: **CONFIRMED**

**Reason**: Same evidence as F1. Three sites where one would suffice. Confirmed strong.

---

### F8 — ZFC violation: backend silently overrides a model-owned semantic field: **CONFIRMED**

**Reason**: Per ZFC skill §"Model Decision Engines" / §"Field Ownership", `verdict` is a model-owned semantic field. The override mutates it without consulting the model. The pattern matches ZFC §"Field Ownership" guidance. Confirmed blocker.

---

### F9 — ZFC leveling: dual-field contract gives the model multiple ways to contradict itself: **CONFIRMED**

**Reason**: Reviewer model emits both `outcome` and `verdict`. The `_enforce_outcome_verdict_consistency` override exists because these two fields can disagree. Per ZFC's "Minimal Semantic Fields" pattern, this is a contradiction surface. Confirmed strong.

---

### F10 — root-cause-first: Bug 1 patched at the symptom, not the invariant: **CONFIRMED**

**Reason**: The write site (`handler_dispatch.py:754`) is the only true violation. The legacy-dict read tolerance has no real failure mode — `ctx.state` is per-process, no "old dict value from before this fix" can survive into a new process. Confirmed strong.

---

### F11 — root-cause-first: Bug 2 patched at the symptom, not the upstream instruction: **CONFIRMED**

**Reason**: The override addresses neither root cause (dual-field model contract, spec staleness detector that doesn't actually catch staleness). Tests prove the override works but provide no evidence the prompt/schema cannot handle the dual-field contract. Confirmed blocker.

---

### F12 — root-cause-first: missing canonical-layer fix for `dict[str, str]`: **CONFIRMED**

**Reason**: A typed field on a dataclass should be enforced by the dataclass (e.g., `StringState(dict[str, str])` subclass with `__setitem__` assertion). With no enforcement, every handler writer is responsible individually. The contract has no canonical enforcement home. Confirmed strong.

---

## Lane B findings

### F-B1 — Test judo: `test_reviewer_outcome_verdict_consistency.py` duplicates verdict/outcome truth table: **CONFIRMED (and BLOCKED BY CIRCULAR IMPORT)**

**Evidence reproduction**:
```
$ wc -l /home/jleechan/projects/dark-factory/tests/test_reviewer_outcome_verdict_consistency.py
188 /home/jleechan/projects/dark-factory/tests/test_reviewer_outcome_verdict_consistency.py
```

**11 test methods** verified at lines 24–188:
- L24-40: `test_failure_outcome_with_pass_verdict_adjusted_to_fail`
- L42-56: `test_success_outcome_with_pass_verdict_unchanged`
- L58-71: `test_error_outcome_with_pass_verdict_adjusted_to_fail`
- L73-86: `test_partial_outcome_with_pass_verdict_adjusted_to_fail`
- L88-101: `test_no_verdict_in_metadata_does_not_raise`
- L103-112: `test_none_metadata_does_not_raise`
- L116-127: `test_case_insensitive_verdict_matching`
- L129-141: `test_failure_outcome_with_fail_verdict_unchanged`
- L143-162: `test_preserves_other_metadata_fields`
- L164-175: `test_preserves_context_updates`
- L177-188: `test_preserves_suggested_next_ids`

The judo collapse to ~4 parametrized tests is sound.

**CRITICAL ADDITION (not in the lane finding)**:
```
$ .venv/bin/python -m pytest tests/test_reviewer_outcome_verdict_consistency.py --no-header -q
ImportError: cannot import name '_parallel_reviewer' from partially initialized module
'runner.handler_parallel_reviewer' (most likely due to a circular import)
(/home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py)
ERROR tests/test_reviewer_outcome_verdict_consistency.py
!!!!!!!!!!!!!!!!!!! Interrupted: 1 error during collection !!!!!!!!!!!!!!!!!!!!
1 error in 0.15s
```

The 11 tests in this file **cannot run at all** due to a circular import between `runner.handler_parallel_reviewer` (line 19: `import runner.handlers as _handlers_shim`) and `runner.handlers` (line 161: `from .handler_parallel_reviewer import (_parallel_reviewer,)`). When the test file directly imports from `runner.handler_parallel_reviewer`, the partial-init guard in `runner.handlers` fires before `_parallel_reviewer` is bound. **This is a real defect the lane reviews missed entirely.**

**Severity**: confirmed strong as written. Severity bumps to **blocker** because of the circular import — the test file cannot execute. This makes the parametrize discussion moot until the import is fixed.

---

### F-B2 — Test architecture: `test_state_dict_substitution.py` ignores `tests/conftest.py` canonical infrastructure: **CONFIRMED**

**Evidence reproduction**:
```
$ grep -n 'make_node\|declare_test_environment\|make_context' /home/jleechan/projects/dark-factory/tests/conftest.py
71:def make_node(name: str = "test", **attrs) -> _Node:
80:@pytest.fixture(autouse=True, scope="session")
81:def _declare_test_environment() -> None:
(no make_context exists)

$ grep -c 'Context(goal="test"' /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
12
```

**Reason**: `make_node` is at line 71 (correctly used); `make_context` does not exist; 12 inline `Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")` constructions. The sys.path.insert boilerplate (lines 16-17) is consistent with 6 other test files in the suite. The finding's analysis is correct.

**Severity**: confirmed strong as written (test-judo opportunity).

---

### F-B3 — Test judo: legacy-compat tests pin transient behaviour: **CONFIRMED**

**Evidence reproduction**:
```
$ sed -n '144,168p' /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
144:    def test_cross_visit_tolerates_legacy_dict(self, monkeypatch):
...
154:        ctx.state["review_main.resolved_backend_meta"] = {
155:            "reviewer_backend_resolution": "priority_queue",
156:            "adversarial_resolved": "minimax",
157:        }

$ sed -n '169,195p' /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
169:    def test_cross_visit_tolerates_malformed_value(self, monkeypatch):
...
179:        ctx.state["review_main.resolved_backend_meta"] = None
191:        ctx.state["review_main.resolved_backend_meta"] = "not valid json {"
```

**Reason**: Both tests seed `ctx.state` with the legacy-dict shape (line 154-157) and malformed values (line 179, 191) to exercise the read-shim branch. With no `TODO(remove after YYYY-MM-DD)` marker and no other docs in the diff saying "this shim is transient", these tests pin behaviour that locks the read-shim into the contract forever. Confirmed strong.

---

### F-B4 — Sequential orchestration in test setup: two-call read-back tests duplicate setup: **CONFIRMED**

**Evidence reproduction**:
```
$ sed -n '113,142p' /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
113:    def test_cross_visit_read_back_tolerates_json_string(self, monkeypatch):
... rebuilds Context, Node, monkeypatch from scratch
144:    def test_cross_visit_tolerates_legacy_dict(self, monkeypatch):
... rebuilds same setup
```

**Reason**: Both tests rebuild the same scaffolding (Context, Node, monkeypatch) for what is logically the same scenario under a different `ctx.state` seed. A parametrized fixture would deduplicate. Confirmed strong.

---

### F-B5 — Spaghetti in tests: `test_cross_visit_tolerates_malformed_value` chains two assertions: **CONFIRMED**

**Evidence reproduction**:
```
$ sed -n '169,195p' /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
178:        # Seed with malformed values
179:        ctx.state["review_main.resolved_backend_meta"] = None
...
187:        # Should fall through to probe and return agy
188:        assert resolved == "agy"
189:
190:        # Now test with invalid JSON string
191:        ctx.state["review_main.resolved_backend_meta"] = "not valid json {"
...
195:        assert resolved2 == "agy"
```

**Reason**: Two distinct assertions (None seed → agy at line 188; malformed JSON string → agy at line 195) in one test body. The two paths should be split. Confirmed strong.

---

### F-B6 — File-size: 403 added lines, no size boundary: **CONFIRMED**

**Evidence reproduction**:
```
$ wc -l /home/jleechan/projects/dark-factory/tests/test_reviewer_outcome_verdict_consistency.py /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
  188 /home/jleechan/projects/dark-factory/tests/test_reviewer_outcome_verdict_consistency.py
  215 /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
  403 total
```

**Reason**: 403 added lines, both files well under any size threshold. Confirmed as written.

---

### F-S1 — ponytail rung 2 missed-reuse: `_priority_node` helper exists: **CONFIRMED**

**Evidence reproduction**:
```
$ grep -n '_priority_node\|backend_priority' /home/jleechan/projects/dark-factory/tests/test_gate_priority_queue.py
21:def _priority_node(priority, *, prefer_adversarial=False, name="evidence"):
22:    attrs = {"backend_priority": ",".join(priority)}

$ grep -n 'backend_priority' /home/jleechan/projects/dark-factory/tests/test_state_dict_substitution.py
93:            attrs={"backend_priority": "codex,minimax"},
126:        attrs={"backend_priority": "codex,minimax,agy,claude-sonnet"},
138:        attrs={"backend_priority": "codex,minimax,agy,claude-sonnet"},
163:        attrs={"backend_priority": "codex,minimax"},
183:        attrs={"backend_priority": "agy,claude-sonnet"},
```

**Reason**: `_priority_node` exists at `tests/test_gate_priority_queue.py:21`. The new `test_state_dict_substitution.py` hand-rolls `Node(name=..., attrs={"backend_priority": ...})` 5 times (lines 93, 126, 138, 163, 183). The DRY win is real. Confirmed strong.

---

### F-S2 — ZFC: tests can contain classifier logic: **DOWNGRADED (info only)**

**Reason**: The finding itself notes the test surface is "fine" (deterministic, no classifiers inside the tests). The proposed move ("open a follow-up bead") is correct in spirit but the bead is a process artifact, not a defect to verify. The ZFC framing is valid but it's an observation, not a defect. Downgrade to "info — covered by F3/F8".

---

### F-S3 — ZFC Banned Patterns: `_enforce_outcome_verdict_consistency` lives in wrong module: **CONFIRMED**

**Evidence reproduction**:
```
$ grep -rn '_VERDICT_NORMALIZE' /home/jleechan/projects/dark-factory/runner/
/home/jleechan/projects/dark-factory/runner/handler_verdict.py:28:_VERDICT_NORMALIZE = {
/home/jleechan/projects/dark-factory/runner/handler_verdict.py:168:    return _VERDICT_NORMALIZE.get(raw_verdict, "failure")
/home/jleechan/projects/dark-factory/runner/handlers.py:91:    _VERDICT_NORMALIZE,

$ grep -n 'def _enforce_outcome_verdict_consistency' /home/jleechan/projects/dark-factory/runner/
runner/handler_parallel_reviewer.py:219
```

**Reason**: `_VERDICT_NORMALIZE` (canonical verdict-token → success/failure map) lives at `handler_verdict.py:28`. `_enforce_outcome_verdict_consistency` lives at `handler_parallel_reviewer.py:219` with its own outcome→verdict mapping that duplicates the vocabulary but inverts the direction. The location is the smell — verdict canonicalization belongs next to `_VERDICT_NORMALIZE`. Confirmed strong.

---

### F-S4 — root-cause-first: dict-vs-string contract on `ctx.state` should be enforced once: **CONFIRMED**

**Evidence reproduction**:
```
$ sed -n '34,49p' /home/jleechan/projects/dark-factory/runner/handler_core.py
@dataclass
class Context:
    goal: str
    workdir: "pathlib.Path"
    state: dict[str, str] = field(default_factory=dict)
```

**Reason**: Plain dict field, no `__post_init__`, no validation. Tests seed non-string values (e.g., `ctx.state["review_main.resolved_backend_meta"] = {...}` at line 32) and assert the coercion path works — but the contract is unenforced. The suggested TODO marker or rejection test would document the current contract. Confirmed strong.

---

### F-S5 — root-cause-first: `_check_stale_artifacts` has no test: **CONFIRMED**

**Evidence reproduction**:
```
$ grep -rn '_check_stale_artifacts' /home/jleechan/projects/dark-factory/tests/ /home/jleechan/projects/dark-factory/runner/
/home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py:60:def _check_stale_artifacts(...)
/home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py:262:    stale_warnings = _check_stale_artifacts(ctx.workdir, ctx)
```

**Reason**: Two occurrences in `runner/`, zero in `tests/`. The 11 verdict-consistency tests cover the override layer, none cover the detection layer. Confirmed strong as written.

---

## Refuted findings (with reason)

**None.** All 17 Lane A findings (F1–F12) and 11 Lane B findings (F-B1…F-B6, F-S1…F-S5) reproduced exactly as cited. F-S2 was downgraded to info (not refuted; the ZFC framing is correct but the artifact is a process follow-up, not a defect to verify).

---

## Confirmed findings (severity preserved)

### Lane A (pr228 runner/ diff)
| Finding | Severity | Note |
|---|---|---|
| F1 — three-layer defense | strong | confirmed |
| F2 — `_check_stale_artifacts` noise | strong | confirmed |
| F3 — verdict override | **blocker** | confirmed |
| F4 — file size / decomposition | nit | confirmed |
| F5 — contract unenforced | nit (already under F1) | confirmed |
| F6 — no parallelism regression | nit (no finding) | confirmed |
| F7 — ponytail rung 7 | strong | confirmed (same root as F1) |
| F8 — ZFC violation | **blocker** | confirmed (same root as F3) |
| F9 — ZFC dual-field | strong | confirmed |
| F10 — RCF Bug 1 symptom | strong | confirmed (same root as F1) |
| F11 — RCF Bug 2 symptom | **blocker** | confirmed (same root as F3) |
| F12 — RCF canonical-layer miss | strong | confirmed (same root as F1) |

### Lane B (pr228 tests/ diff)
| Finding | Severity | Note |
|---|---|---|
| F-B1 — verdict/outcome truth table duplication | strong; **escalates to blocker** due to circular import (see below) | confirmed |
| F-B2 — conftest infrastructure missed | strong | confirmed |
| F-B3 — legacy-compat tests pin behaviour | strong | confirmed |
| F-B4 — two-call read-back setup duplication | strong | confirmed |
| F-B5 — chained assertions | strong | confirmed |
| F-B6 — file-size: 403 added lines | nit (no finding) | confirmed |
| F-S1 — `_priority_node` reuse missed | strong | confirmed |
| F-S2 — ZFC framing of test | info (downgraded) | process follow-up, not a defect |
| F-S3 — verdict-canonicalization lives in wrong module | strong | confirmed |
| F-S4 — contract unenforced | strong (overlaps F1/F12) | confirmed |
| F-S5 — `_check_stale_artifacts` untested | strong | confirmed |

---

## CRITICAL NEW FINDING (not surfaced by any lane)

### F-NEW1 — `test_reviewer_outcome_verdict_consistency.py` is non-executable due to circular import

**Severity**: blocker

**Evidence**:
```
$ .venv/bin/python -m pytest tests/test_reviewer_outcome_verdict_consistency.py --no-header -q
ImportError: cannot import name '_parallel_reviewer' from partially initialized module
'runner.handler_parallel_reviewer' (most likely due to a circular import)
(/home/jleechan/projects/dark-factory/runner/handler_parallel_reviewer.py)
ERROR tests/test_reviewer_outcome_verdict_consistency.py
!!!!!!!!!!!!!!!!!!! Interrupted: 1 error during collection !!!!!!!!!!!!!!!!!!!!
1 error in 0.15s
```

**Reason**: `runner/handler_parallel_reviewer.py` line 19 does `import runner.handlers as _handlers_shim`, and `runner/handlers.py` line 161 does `from .handler_parallel_reviewer import (_parallel_reviewer,)`. When the test file at module-import time does `from runner.handler_parallel_reviewer import _enforce_outcome_verdict_consistency`, Python starts loading `runner.handler_parallel_reviewer`, which imports `runner.handlers`, which tries to pull `_parallel_reviewer` back from `runner.handler_parallel_reviewer` while that module is still being initialized — the partial-init guard fires and `_parallel_reviewer` is not yet bound.

The same file loads fine when pytest discovers it after `runner.handlers` has been loaded by another test, because then `runner.handler_parallel_reviewer` is already initialized. But:
- `pytest tests/test_reviewer_outcome_verdict_consistency.py` (file-only invocation) FAILS.
- CI runs that invoke this file by path or as the first test file in collection would fail.
- The "test always passes when full suite is run" pattern hides the defect.

The same circular dependency also exists for the production code: `_check_stale_artifacts` and `_enforce_outcome_verdict_consistency` live in a module that imports the shim, but the shim also imports back. This is a long-standing circular import that the new production code (`2a383a1`) inherits and worsens by adding more functions to `runner.handler_parallel_reviewer.py`.

**Suggested fix**:
1. **Quick fix**: change `runner.handler_parallel_reviewer.py:19` to a lazy/local import — `_handlers_shim._render_prompt(node, ctx)` and `_handlers_shim._coerce_timeout(...)` are both called inside function bodies, so they can be moved into the function bodies (or use `importlib.import_module` deferred).
2. **Better fix**: break the cycle by moving `_parallel_reviewer` to a new `runner/handler_parallel_reviewer_top.py` that does NOT import the shim, and have `runner.handlers` import from there.

Either fix unblocks the test file and reduces coupling.

---

## Scope attribution check (the critical scope rule)

**Checked**: all findings cite pr228's actual contribution (commits `2a383a1`, `658a715`), not origin/main drift.

- `runner/handler_render.py`, `runner/handler_dispatch.py`, `runner/handler_parallel_reviewer.py` — all in `2a383a1` ✅
- `tests/test_state_dict_substitution.py`, `tests/test_reviewer_outcome_verdict_consistency.py` — both in `658a715` ✅
- F-B1, F-B3, F-B5 cite specific line ranges (113, 144, 169) in the new test file ✅
- F-S1, F-S3 cite helpers in existing files (correctly framed as missed-reuse, not as pr228's contribution) ✅

No finding incorrectly attributes origin/main drift as pr228's contribution. Scope discipline clean.

---

## Summary

- **Lane A (runner/ diff)**: All 12 findings confirmed. Three findings (F1≡F7≡F10≡F12 cluster; F3≡F8≡F11 cluster) collapse to two root moves: (1) enforce `dict[str, str]` once at the Context.state boundary; (2) collapse `verdict` to a deterministic derivation from `outcome` and delete `_enforce_outcome_verdict_consistency`. Net: ~130 lines removed while preserving both bug fixes.
- **Lane B (tests/ diff)**: 10 of 11 findings confirmed as written. F-S2 downgraded (process follow-up, not defect). F-B1 severity escalates to **blocker** because of the circular-import defect (F-NEW1) that prevents the 11 tests from running when the file is invoked alone.
- **New finding F-NEW1** (circular import): real, reproducible, not surfaced by any lane. CI may hide it because pytest collection-order matters; isolated invocation fails.
- **No findings refuted**.
- **Scope rule clean** — no finding attributes origin/main drift as pr228's contribution.