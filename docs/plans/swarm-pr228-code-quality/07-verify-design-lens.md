# Design-lens adversarial verification

**Scope**: refute "code judo" / structural moves that don't actually simplify; confirm moves that genuinely delete concepts.

**Method**: read the suggested move, audit the actual production-call graph, judge whether the move deletes code, reduces concepts, or merely rearranges.

---

## Lane A findings (with "code judo" / structural moves)

### F-A1 / F-A7 / F-A10 — `delete the read-site legacy-dict branch and the substitution-site coercion`

- **Verdict**: **CONFIRMED** (this is real judo, with one footnote)
- **Reasoning**:
  - The move deletes ~12 lines of read-back tolerance (`handler_dispatch.py:721-732`) and ~6 lines of substitution coercion (`handler_render.py:69-74`), and replaces both with single-line `assert isinstance(v, str)` at the substitution site.
  - The "legacy dict" comment is self-refuting: `ctx.state` is per-process (no persistence), the runner is a single CLI, and there is no migration path. The tolerance exists only to defend against a future call site that writes a dict — exactly the contract violation the assertion should catch.
  - Net diff is negative (~15 lines deleted). Two layers collapse into one canonical assertion.
  - The `pq_meta` writer at `handler_dispatch.py:754` already does `json.dumps(...)` — the write site is already correct. The judo preserves the only layer that is doing real work (write) and deletes the two layers that are doing defensive guessing.
- **Footnote**: `_last_diff`, `_last_changed_files`, `ao.worktree`, `_last_verdict` — every other `ctx.state` write in `runner/` is already a string (paths go through `str(pathlib.Path(...))` explicitly at `runner/__main__.py:403` and `runner/handler_codergen.py:662`). The "what about other non-string call sites" worry is unsupported by the production code. F-S4's "tighten the type" move is also safe to do at the same time (see F-S4 below).
- **Confirmed move**: replace the three-layer defense with one write-side JSON encoding + one read-side assertion.

### F-A3 / F-A8 / F-A11 — `collapse verdict to a deterministic derivation from outcome`

- **Verdict**: **REFUTED** — the suggested move would break production behavior
- **Reasoning**:
  - The `outcome_to_verdict` table in `_enforce_outcome_verdict_consistency` is **incomplete**. Production code emits at least 5 verdict values, not 2:
    1. `infra_failure` — written explicitly at `handler_dispatch.py:820` when all backends fall through due to sandbox/timeout/missing-binary. Downstream consumers and operators distinguish "infra broken" from "real fail" precisely by reading this string. F-A3's table maps `error → fail`, which would collapse infra_failure into a generic fail and lose the Healer-clusterable signal (`runner/handlers.py:91,107` re-exports `_is_gate_infra_failure` specifically to avoid that).
    2. `unknown` — written by `handler_audit.py:323`, `handler_special_gates.py:124/147/157/171/180`. Downstream `engine_run.py:182` reads it as `verdict = attempt.metadata.get("verdict")` and surfaces it as `_last_verdict` — a node downstream of the gate uses this to decide next-step routing. Collapsing `unknown` to `fail` would change routing behavior.
    3. `warn` — explicitly handled as a separate verdict class by `_parse_verdict` (`runner/handler_verdict.py:28-43`: `"warn": "success"`) and by `pre_review_lint.py:15` (warn/fail match gate vocabulary). F-A3's `partial → fail` already overrides this and would force warn into a fail that the verdict layer treats as success.
    4. `pass` / `fail` — the only two values F-A3's table covers. F-A3's move is correct for the pass/fail axis, but it is silent on the other three.
  - F-A3's move **deletes a function that defends against a real failure mode** (LLM emits `verdict=pass` despite an infra-broken gate that should be `infra_failure`). The override is not ZFC violation candidate — it is the canonical place where the runner (which controls `outcome` and the infra-failure classification) reconciles a model-emitted verdict with its own classification. Removing it would re-introduce Bug 2.
  - F-A11's "fix the upstream spec" move (add `<!-- run_id: -->` markers) is a real, separate improvement — but it is not a substitute for the override. Even with run_id markers, the LLM can still misjudge for other reasons (e.g. spec changed in a way the LLM doesn't see, or model has a training-data preference for "pass"). The override is the safety net; removing it makes the system less robust, not simpler.
- **Refuted move**: the verdict-override function should stay. F-A3's table is too narrow to be the canonical truth. If anything, the override should be **moved** to `handler_verdict.py` (F-S3) so the verdict vocabulary lives in one place — but it should not be deleted.
- **Genuine judo in this cluster**: F-S3 (move to `handler_verdict.py`, share `_VERDICT_NORMALIZE`). That move is real judo; F-A3 is not.

### F-A12 — `class StringState(dict[str, str])` subclass

- **Verdict**: **DOWNGRADED** to "consider, but with caveats"
- **Reasoning**:
  - A `StringState` subclass is a real judo move in the abstract — it centralizes the contract at the dataclass boundary and makes violations impossible.
  - BUT: the codebase has 6+ call sites that use `ctx.state.get(...)` and 12+ that use `ctx.state[...]` (`engine_run.py:741/777/885`, `engine_edges.py:168`, `handler_codergen.py:498-503/662/738`, `handler_dispatch.py:749/754`, `handler_parallel_reviewer.py:130/145/265`). A subclass breaks any code that constructs `Context(state=some_dict)` and passes a real dict — the only safe option is a custom container that *accepts* the legacy shape and converts on insert, which adds new code rather than deleting it.
  - The simpler move: F-A1's single `assert isinstance(v, str)` at the substitution site. That catches the same bug with one line and zero new abstraction. The `StringState` subclass is over-engineering for a one-violation problem.
- **Downgraded move**: skip the subclass. The F-A1 assertion is the equivalent protection with one line of code instead of a new class.

### F-A2 — `delete _check_stale_artifacts`

- **Verdict**: **CONFIRMED with one note**
- **Reasoning**:
  - The function's headline feature (`<!-- run_id: -->` marker detection) is unreachable — `grep -rn "<!-- run_id" runner/ prompts/` returns only the detector itself.
  - The 5-minute mtime check is named backwards (modified <5 min = fresh, not stale).
  - `ctx.state["_stale_artifact_warnings"]` is written but never read (no `grep` hits in `runner/`, `tests/`, or `prompts/`).
  - Deleting it deletes ~50 lines of dead code plus its ~15-line call site, and removes a misleading observability event.
- **Note**: the spec-currency problem is real (the override at F-A3 is a partial fix). F-A2's suggested upstream fix (run_id markers in spec.md, deterministic via `git log`) is a separate, larger work item — but the detector should not exist in its current form regardless.
- **Confirmed move**: delete `_check_stale_artifacts`, its call site, and the dead state key. File a follow-up bead for the upstream run_id marker discipline.

### F-A4 — file size / `_parallel_reviewer` decomposition

- **Verdict**: **NIT** — not worth a structural move
- **Reasoning**: file is 363 lines (threshold 1000). The "five responsibilities" complaint is real but the cost of extracting a `_post_process_result` step is the same code in two places. Wait until the file is actually near the threshold.

---

## Lane B findings

### F-B1 — parametrize 11 verdict-consistency tests

- **Verdict**: **CONFIRMED**
- **Reasoning**:
  - 4 tests vary only by `(outcome, verdict)` inputs. `@pytest.mark.parametrize` collapses 4 tests to 1 with a 4-row table — same coverage, less line noise, and the table itself documents the truth-table.
  - 3 "preserves other fields" tests can collapse to 1 parametrized over the `Result` field. Same idea.
  - The "case-insensitive verdict" test is a separate scenario (it tests a parser detail, not the truth table) and should stay separate.
  - Net diff: ~70 lines saved in `test_reviewer_outcome_verdict_consistency.py` with no coverage loss.
- **Caveat**: F-A3 is REFUTED, so this test file exists in its current form. If F-A3 stands (it does), the parametrize is still a clean win.
- **Confirmed move**: parametrize. The file shrinks, the truth table becomes a literal `@pytest.mark.parametrize` row, and the test reads as a spec.

### F-S1 — reuse `_priority_node` helper

- **Verdict**: **CONFIRMED**
- **Reasoning**: `tests/test_gate_priority_queue.py:21-25` already defines `_priority_node(priority, name=...)` that builds the exact `Node` shape the new tests need. 4 hand-rolled `Node(name="review_main", attrs={"backend_priority": "codex,minimax"})` constructions become 4 `_priority_node([...], name="review_main")` calls.
- **Best path**: hoist `_priority_node` to `conftest.py` so both test files consume it. -10 lines and stops drift.

### F-S3 — move `_enforce_outcome_verdict_consistency` to `handler_verdict.py`

- **Verdict**: **CONFIRMED** — this is the real judo in the verdict cluster
- **Reasoning**:
  - The function shares a domain with `_VERDICT_NORMALIZE` (gate verdict canonicalization) and `_parse_verdict` (verdict token extraction). It currently lives in `handler_parallel_reviewer.py` because that's where the bug surfaced, but its logic is verdict-canonicalization logic, not reviewer-coordination logic.
  - Moving it deletes the `from .handler_verdict import _VERDICT_NORMALIZE` reach-around that the new test file (and any future verdict-canonicalization test) would need to do.
  - The move also makes F-A3's `outcome_to_verdict` table sit **next to** `_VERDICT_NORMALIZE`, which forces a developer to confront the question F-A3 hand-waves: what about `infra_failure`, `unknown`, `warn`? The answer (extend `_VERDICT_NORMALIZE` to cover all verdict tokens, derive `outcome_to_verdict` from it) becomes obvious in the new home.
  - Net effect: -5 lines of cross-module imports, +0 lines of new code, +1 place where verdict vocabulary lives.
- **Confirmed move**: relocate. Do not delete (see F-A3 refutation).

### F-S4 — tighten `Context.state` to actually reject non-strings

- **Verdict**: **CONFIRMED but not via `StringState` subclass** — do it via F-A1's `assert` at the substitution site
- **Reasoning**:
  - F-S4's claim that "legitimate call sites store non-strings (e.g., dicts for backend metadata)" is **partly wrong** for the current codebase: every `ctx.state[...]` write in `runner/` except the very `pq_meta` dict F-A1 is fixing is already a string. `ao.worktree` is wrapped in `str(pathlib.Path(...))` at `runner/__main__.py:403` and `runner/handler_codergen.py:662`. `_last_diff`, `_last_changed_files`, `_last_verdict`, all `*.outcome` keys are strings. The only outlier IS the bug.
  - F-S4's worry about breaking the dict metadata is therefore not a real worry. Tightening the contract is safe.
  - F-S4 suggests either a `NewType` (which is a static-only marker and won't actually reject at runtime) OR a runtime check at write sites (which is many sites). The F-A1 judo — single `assert isinstance(v, str)` at the substitution site — is one line, catches every violation, and requires no new class.
- **Confirmed move**: do the F-A1 assertion. Skip the subclass and the NewType. Add F-S4's suggested test `test_context_state_rejects_non_string_values` — but make it pin the *current loose contract* with a TODO marker, since tightening the dataclass field is a separate, larger refactor.

### F-S2 — verdict ownership / `original_verdict` auditability

- **Verdict**: **REFUTED in part, CONFIRMED in part**
- **Reasoning**:
  - CONFIRMED: the production function writes `original_verdict` to metadata but no consumer reads it. That is dead metadata. The test pins the lossy behavior.
  - REFUTED: F-S2's recommendation ("persist `original_verdict` alongside `verdict` so operators can audit overrides") is **additive**, not judo. It is a real DX improvement but it adds a field, not a deletion. Not in scope of "code judo" verification.
  - The honest move: drop `verdict_adjusted_for_consistency` (it's always `"true"` when it appears — never read, no useful signal). Keep `original_verdict` for the postmortem use case but only if a real consumer is added. F-B1's parametrize can include a `tracks_original_verdict` column to document which scenarios preserve the original.
- **Net**: small cleanup, not a structural refactor.

### F-S5 — `_check_stale_artifacts` is untested

- **Verdict**: **REFUTED** by F-A2
- **Reasoning**: the detector is unreachable / always-on noise / writes dead state. The right move is delete, not test. Adding tests to dead code is gold-plating the symptom.
- **Confirmed move (from F-A2)**: delete, don't test. File the spec-currency bead as a follow-up.

### F-B2 — `sys.path.insert(0, ROOT)` boilerplate + `make_context` helper

- **Verdict**: **NIT (F-B2.2 is LGTM; F-B2.1 is out of scope)**
- **Reasoning**:
  - The 12-of-13 repetition of `Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")` in `test_state_dict_substitution.py` is real boilerplate. A `make_context(backend="echo", workdir=None, state=None)` fixture in `conftest.py` would deduplicate cleanly. -15 lines, test intent becomes the only visible content.
  - The `sys.path.insert` pattern is a pre-existing style issue shared by 6+ files. Out of scope for this PR.
- **Confirmed move**: add `make_context` to `conftest.py` (or, if scope is tight, do it in a follow-up PR and let this PR land as-is).

### F-B3 / F-B4 / F-B5 — legacy-dict test deletion, two-call read-back consolidation, split malformed test

- **Verdict**: **CONFIRMED by F-A1**
- **Reasoning**: if F-A1's judo lands (assertion + delete legacy-dict branch + delete coercion), then `test_cross_visit_tolerates_legacy_dict` and `test_cross_visit_tolerates_malformed_value` exercise paths that no longer exist. They should be deleted (not marked TODO) at the same time as the F-A1 code change. F-B4 and F-B5 become moot.
- **Confirmed move**: delete those tests alongside the F-A1 production-code change.

### F-B6 — no file-size issue

- **Verdict**: **NIT (no finding)** — confirmed.

---

## Refuted / downgraded moves

| Finding | Move | Why refuted |
|---|---|---|
| F-A3 / F-A8 / F-A11 | Delete `_enforce_outcome_verdict_consistency`; collapse verdict to pass/fail only | The verdict field carries at least 5 values (`pass`, `fail`, `warn`, `unknown`, `infra_failure`). The `infra_failure` value is read by `_is_gate_infra_failure` and Healer clustering; collapsing it to `fail` loses that signal. `warn` is treated as success in `_VERDICT_NORMALIZE` and would conflict with the `partial → fail` override. `unknown` is consumed by downstream routing. The override function defends a real failure mode, not a hypothetical. |
| F-A12 | `class StringState(dict[str, str])` subclass with `__setitem__` validation | Over-engineering for a one-violation problem. The same protection is achievable with one `assert isinstance(v, str)` at the substitution site. Subclass adds new code (new class, override, tests) for a problem the F-A1 judo solves in one line. |
| F-S5 | Add tests for `_check_stale_artifacts` | The detector is dead code (unreachable marker check, inverted mtime logic, unread state). Test the upstream spec-currency discipline instead; delete the detector. |
| F-S2 (additive part) | Persist `original_verdict` as a real consumer-visible audit trail | Real DX improvement, but additive, not judo. Not in the simplify-this-code rubric. |
| F-B3 (mark TODO) | Keep `test_cross_visit_tolerates_legacy_dict` alive with a TODO marker | The legacy read-back branch is being deleted; the test should go with it, not be frozen as permanent debt. |

## Confirmed moves

| Finding | Move | Why confirmed |
|---|---|---|
| F-A1 / F-A7 / F-A10 | Replace 3-layer dict[str,str] defense with write-side JSON + read-side `assert` | Net -15 lines, contract becomes hard invariant, 2 of 3 test scenarios become impossible to express (the signal the contract is real). The only production write that ever violated the contract (`pq_meta` at `handler_dispatch.py:754`) is already fixed. |
| F-A2 | Delete `_check_stale_artifacts` and its call site | Detector's headline feature is unreachable; mtime check is inverted; emitted state is never read. Net -65 lines. File a follow-up bead for run_id marker discipline. |
| F-S3 | Move `_enforce_outcome_verdict_consistency` to `handler_verdict.py` | Verdict-canonicalization lives in one place; `_VERDICT_NORMALIZE` and the outcome table sit next to each other. Same code, better home. |
| F-S4 | Tighten `Context.state` (via F-A1's `assert` at the substitution site) | Every other `ctx.state` write in `runner/` is already a string. The single assertion catches the only real bug class without a new class or NewType. |
| F-B1 | Parametrize 11 verdict-consistency tests into ~4 | Truth table becomes a literal `@pytest.mark.parametrize` row. -70 lines, no coverage loss. |
| F-S1 | Reuse `_priority_node` from `test_gate_priority_queue.py` (or hoist to `conftest.py`) | -10 lines, stops drift. |
| F-B2.2 | Add `make_context(backend="echo", workdir=None, state=None)` to `conftest.py` | -15 lines in `test_state_dict_substitution.py`, test intent becomes visible. |
| F-B3 / F-B4 / F-B5 (test deletion) | Delete legacy-dict + malformed-value tests alongside the F-A1 production change | The tests pin paths the F-A1 judo removes. They go together. |

## Net impact if all confirmed moves land

| Move | Lines (prod) | Lines (test) | Net |
|---|---|---|---|
| F-A1: assert + delete coercion + delete legacy-dict branch | -15 | -50 (delete 2 tests, simplify others) | -65 |
| F-A2: delete `_check_stale_artifacts` | -65 | 0 | -65 |
| F-S3: relocate to `handler_verdict.py` | 0 | 0 | 0 (better organization) |
| F-B1: parametrize 11 tests | 0 | -70 | -70 |
| F-S1: reuse `_priority_node` | 0 | -10 | -10 |
| F-B2.2: `make_context` helper | 0 | -15 | -15 |
| **Total** | **-80** | **-145** | **-225** |

The F-A1 + F-A2 + F-B1 cluster is the highest-value refactor in the entire PR. F-A3 is the highest-conviction **non**-move: the suggested verdict-override deletion would break the `infra_failure` / `warn` / `unknown` handling that production gates depend on.

## Verdict for the verification pass

- **Real judo**: F-A1, F-A2, F-B1, F-S1, F-S3, F-B2.2
- **No-op judo (rearranges, doesn't simplify)**: F-S3 alone (it's a relocation, not a reduction — but the relocation is still worth doing)
- **Additive, not judo (real but out of simplify-rubric)**: F-S2 audit-trail
- **Refuted judo (would break behavior or add complexity)**: F-A3 / F-A8 / F-A11 (verdict collapse), F-A12 (`StringState` subclass)
- **Falsified premise**: F-S4's "legitimate non-string call sites" worry — only one such site exists, and it's the bug F-A1 is fixing.
