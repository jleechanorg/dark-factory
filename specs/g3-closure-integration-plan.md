# G3 Closure Integration Plan

**Bead:** jleechan-0qy
**Design:** `~/.claude/projects/-Users-jleechan-projects-dark-factory/memory/project_2026-06-22_g3_closure_dynamic_node_design.md`
**Plan produced:** 2026-06-22

## TL;DR

Three completed lane worktrees (Lane A dynamic-handler, Lane B conformance
level5 rule, Lane C migrations + engine wiring + reference .dot) plus two
in-flight fix-lanes (handler_render.py guard, broken-test updates) form the
G3 closure. **There is exactly ONE blocking defect** in Lane A's working tree
today: a duplicate-import (`Result as _Result, Result`) in
`runner/handlers.py` that makes the module fail to import. **Lane A1 (the
reported "handler_render guard" fix-lane) is unnecessary** — main's
`_substitute_placeholders` already guards non-string `ctx.state` values via
`v if isinstance(v, str) else str(v)` at `runner/handler_render.py:42`.
Lane C's reported `TypeError` blocker is based on a stale read of main.
Recommended merge order: **Lane B → Lane A (with the duplicate-import fix)
→ Lane C → Fix Lane A1 (no-op or revert) → Fix Lane C1 (test updates).**

---

## Section 1: Final file scope (union of all lanes + fix-lanes)

### Lane A — `worktree-agent-a3f9d79655af1d56f`

| File | Change | Description |
|---|---|---|
| `runner/handlers.py` | modified (now +253 / -1) | Adds `_dynamic` handler, `_wrap_dynamic_prompt`, `_load_dynamic_wrap_template`, `_resolve_default_node`, `_dynamic_shell_claude`, registers `"dynamic"` in `TYPE_REGISTRY`. **Has a hard import defect — see Surprises.** |
| `prompts/_dynamic_wrap.md` | added | Auto-wrap template injected above the dynamic node prompt; carries the locked header + `${goal}` / `${state.context}` / `{prompt_body}` placeholders. |
| `tests/test_dynamic_node.py` | added | 7 tests: registry entry, missing `default=`, fallback delegation, missing-`_graph`, `claude -p --effort high` shell path, wrap-template shape, graph-level parse. |

### Lane B — `worktree-agent-a00e4f8cd9cf415f8`

| File | Change | Description |
|---|---|---|
| `bin/conformance` | modified (+165 lines) | Adds `_is_level5_path`, `_has_cross_vendor_reviewer`, `_check_level5`, `_LEVEL5_HARD_TIER_REVIEWERS`; wires `_check_level5` into `_validate_one`. Exempts `pipelines/slim/*.dot` and `pipelines/factory/hello.dot`. Triggers on `pipelines/factory/*.dot` (not hello) or `graph [level5="true"]`. |
| `tests/test_conformance.py` | modified (+73 lines, 6 tests) | Adds `test_level5_valid_passes`, `test_level5_missing_gate_fails`, `test_level5_with_skip_passes`, `test_slim_pipelines_exempt_from_level5`, `test_hello_dot_exempt_from_level5`, `test_graph_level5_true_attribute_enables_rule`. |
| `tests/fixtures/level5_valid.dot` | added | Compliant fixture (gate_er, gate_skeptic, cross-vendor reviewer, healer, spec_validation). |
| `tests/fixtures/level5_missing_gate.dot` | added | Missing `gate_er` to assert the level5 rule rejects it. |
| `tests/fixtures/level5_with_skip.dot` | added | Compliant hard tier + `skip_holdout`/`skip_spec_validation` opts out of soft-tier warnings. |

### Lane C — `worktree-agent-a484a107cfbb60ba7`

| File | Change | Description |
|---|---|---|
| `pipelines/factory/level5_feature.dot` | added | Reference Level-5 pipeline: 4 dynamic nodes + 4 static fallbacks + 3 hard-tier reviewers + 3 soft-tier nodes; `graph [level5="true"]`. |
| `pipelines/factory/gates.dot` | modified (+13 / -2) | Adds `level5="true"` graph attr + `gate_skeptic` + `adversarial_reviewer` nodes + new edges (holdout → gate_skeptic → adversarial_reviewer → gate_es); preserves the fix loop. **Trailing-newline removed (cosmetic).** |
| `pipelines/factory/pr_gates.dot` | modified (+13 / -2) | Same migration as `gates.dot`. **Trailing-newline removed (cosmetic).** |
| `runner/engine_run.py` | modified (+21) | Adds opt-in `ctx.state["_graph"] = graph` wiring inside `_run_single_node`, gated by `DARK_FACTORY_G3_GRAPH_WIRING` env var. Comment claims it's blocked on Lane A's `_substitute_placeholders` guard — **the guard already exists in main**, so the env-gate is unnecessary but harmless. |
| `runner/handlers.py` | modified | **Identical to Lane A's change** (Lane C re-implements Lane A in its own worktree; see Overlap). |
| `tests/test_conformance.py` | modified | **Identical to Lane B's addition.** |
| `bin/conformance` | modified | **Identical to Lane B's addition.** |
| `tests/fixtures/level5_*.dot` | added | **Identical to Lane B's fixtures.** |
| `prompts/_dynamic_wrap.md` | added | **Identical to Lane A's prompt.** |
| `tests/test_dynamic_node.py` | added | **Identical to Lane A's test.** |
| `tests/test_level5_pipelines.py` | added | 12 tests: 6 on reference `.dot`, 3 on migrated `gates.dot`/`pr_gates.dot`, 1 on fix-loop preservation, 2 on engine wiring (opt-in via `DARK_FACTORY_G3_GRAPH_WIRING=1`), 1 end-to-end fallback test (auto-skips if Lane A's `dynamic` registry entry is absent OR if the integration bug surfaces). |

### Fix Lane A1 — `_substitute_placeholders` hardening (reported by Lane A as blocker)

| File | Change | Description |
|---|---|---|
| `runner/handler_render.py` | modified | **CLAIMED NEED:** guard non-string `ctx.state` values. **ACTUAL STATE in main:** the guard already exists at `runner/handler_render.py:42` (`v if isinstance(v, str) else str(v)`). The fix-lane is a no-op unless new test coverage wants a more explicit `isinstance(v, (str, type(None)))` form. See Section 4 + Surprises. |

### Fix Lane C1 — 20 broken-test updates (reported by Lane C as blocker)

| File | Change | Description |
|---|---|---|
| Unknown — Lane C report does not enumerate | modified | Lane C claims 20 tests in main broke after Lane A's handler landed in some downstream way. **No PR is open for either Lane A or Lane C** (`gh pr list` returns empty for all three lane branches), so there is no measurable "20 broken tests" — that count must come from a Lane C agent run that we cannot reproduce without the merge. Action: verify the count by replaying Lane A's import (post fix) + Lane B's conformance tests on main; if actual count matches, fan out per-file fixes; if not, this fix-lane can be cancelled. |

---

## Section 2: File overlap analysis

### Exact overlap (3-way)

`runner/handlers.py`, `bin/conformance`, `tests/test_conformance.py`, `tests/fixtures/level5_*.dot` (3 fixtures), `prompts/_dynamic_wrap.md`, `tests/test_dynamic_node.py` — Lane C re-implements Lane A's and Lane B's changes byte-for-byte inside its own worktree. Per `git diff --stat`, Lane C's working tree contains all of Lane A's and Lane B's additions in addition to its own unique work.

**Resolution:** **Do NOT use Lane C's worktree as-is.** Lane C should be rebuilt on top of merged Lane A + Lane B. Specifically:
1. Merge Lane B first (touches `bin/conformance` + `tests/test_conformance.py` + 3 fixtures — no overlap with Lane A).
2. Merge Lane A second (touches `runner/handlers.py` + `prompts/_dynamic_wrap.md` + `tests/test_dynamic_node.py` — no overlap with Lane B).
3. Rebase Lane C onto the merged Lane A+Lane B base; Lane C's working tree must drop the duplicate `runner/handlers.py`, `bin/conformance`, `tests/test_conformance.py`, `tests/fixtures/level5_*.dot`, `prompts/_dynamic_wrap.md`, `tests/test_dynamic_node.py` changes and keep only its unique additions: `pipelines/factory/level5_feature.dot`, the migrations to `gates.dot` / `pr_gates.dot`, the `runner/engine_run.py` wiring, and `tests/test_level5_pipelines.py`.

### No-overlap additions (Lane C unique)

`pipelines/factory/level5_feature.dot` and `tests/test_level5_pipelines.py` are Lane-C-exclusive. The migrated `gates.dot` and `pr_gates.dot` in Lane C are also Lane-C-exclusive and depend on Lane B's rule existing before they can pass `bin/conformance validate`.

### No-overlap additions (Lane B unique)

The three `tests/fixtures/level5_*.dot` files are Lane-B-exclusive.

---

## Section 3: Merge order

**Recommended order:**

1. **Lane B** (conformance rule + fixtures + 6 tests)
2. **Lane A** (with the duplicate-import fix — see Surprises)
3. **Lane C** (rebased onto 1+2; only its unique files survive)
4. **Fix Lane A1** (verify status — see Section 4)
5. **Fix Lane C1** (verify the "20 broken tests" claim)

### Justification

- **Lane B first:** it adds a new conformance rule and its own tests; nothing in Lane A or Lane C depends on Lane B's *runtime* (the conformance rule is enforced by `bin/conformance validate` and runs at PR CI), but Lane C's `gates.dot` and `pr_gates.dot` migrations will fail Lane B's rule until they include `gate_skeptic` and `adversarial_reviewer`. So Lane C needs Lane B's rule to exist before it can validate its migrations.

- **Lane A second:** it registers `type="dynamic"` in `TYPE_REGISTRY`. Lane C's reference `level5_feature.dot` references `type="dynamic"`; if Lane A hasn't merged, the .dot parses fine (parser is permissive) but `resolve()` falls through to `_codergen` and the test fixture's semantic claim ("dynamic handler exists") is unsupported. Lane A's reference `.dot` lives behind Lane C's work, so Lane C must come third.

- **Lane C third (rebased):** depends on Lane A (handler registered) and Lane B (rule exists). Lane C is the only lane that adds new `.dot` files visible to users.

- **Fix Lane A1 fourth:** only relevant if the duplicate-import bug doesn't get fixed during Lane A's merge prep, AND if `_substitute_placeholders` actually lacks the guard. Both are false; the fix-lane is most likely a no-op. Slot it in anyway to satisfy the bead's audit trail.

- **Fix Lane C1 fifth:** triggered only if running Lane C's full test suite after merging shows >=20 broken tests in pre-existing files. If the count is actually zero (which is plausible since no PR is open), this fix-lane is a no-op.

### Pre-merge checks per lane

| Lane | Pre-merge check | Pass criterion |
|---|---|---|
| B | `bin/conformance validate pipelines/factory/gates.dot pipelines/factory/pr_gates.dot pipelines/factory/hello.dot pipelines/slim/minimal_feature.dot` | gates.dot + pr_gates.dot + hello.dot + slim must pass (with their pre-Lane-C contents) — pre-Lane-C they're not level5, so the rule must exempt them. Verify exemptions: `factory/hello.dot` is exempted by name; `slim/*` exempted by path; `factory/gates.dot` and `pr_gates.dot` are NOT exempted by Lane B alone — they will FAIL until Lane C migrates them. So Lane B can land alone only if those two are migrated in a follow-up. |
| A | `python -c "import runner.handlers"` and `pytest tests/test_dynamic_node.py -v` | All 7 Lane-A tests pass; no import error. |
| C | `bin/conformance validate pipelines/factory/*.dot pipelines/slim/*.dot` and `pytest tests/test_level5_pipelines.py -v` | All factory/*.dot pass the level5 rule; all 12 Lane-C tests pass; reference `.dot` parses. |
| A1 | `grep -n "isinstance(v, str)" runner/handler_render.py` | The guard already exists; the fix-lane is a no-op or doc comment. |
| C1 | `pytest tests/ -x --tb=line 2>&1 | tail -50` | Identify actual failures, count them, compare to claimed "20." |

---

## Section 4: Blockers and how the fix-lanes close them

### Blocker 1: Lane A's `_dynamic` handler has a duplicate-import syntax error

**What:** `runner/handlers.py` lines 41-51 import `Result as _Result, Result,` from the same module — Python does not allow the same name twice in one `from … import` statement. Symptom: `python -c "import runner.handlers"` raises `SyntaxError` (or `ImportError` at module import). Every test in the suite that imports `runner.handlers` (and `tests/` has 14+ such files) will fail with collection error. **Lane A is un-importable today.**

**Which fix-lane closes it:** **None of the dispatched fix-lanes target this.** The fix is a one-line cleanup: drop the duplicate, keep only `Result as _Result, Result` (or just `Result,` and use it directly). Recommended approach: collapse to `Result,` and use `Result` directly in `_dynamic` — fewer aliases, no functional change.

**Verification step:** `python -c "import runner.handlers"` exits 0; `pytest tests/test_dynamic_node.py -v` shows 7 passes.

### Blocker 2: Lane C's claim that `_substitute_placeholders` will crash on `Graph` objects

**What:** Lane C's `runner/engine_run.py` comment (lines 99-117) claims that wiring `ctx.state["_graph"] = graph` will crash the existing `_substitute_placeholders` because `text.replace(token, v)` requires `v` to be a `str`. **This claim is stale.** Main's `_substitute_placeholders` at `runner/handler_render.py:42` already guards with `v if isinstance(v, str) else str(v)`, and `str(Graph)` works (verified — produces a dataclass-style repr, no recursion, no exception).

**Which fix-lane closes it:** **None needed.** Fix Lane A1's proposed handler_render.py change is a no-op or documentation cleanup at most.

**Verification step:** `python -c "from runner.handler_render import _substitute_placeholders; from runner.parser import Graph, Node; from runner.handler_core import Context; import pathlib; g = Graph(name='x', goal='y', nodes={'a': Node(name='a', attrs={})}, edges=[]); ctx = Context(goal='y', workdir=pathlib.Path('.'), backend='echo'); ctx.state['_graph'] = g; print(_substitute_placeholders('hello \${state._graph} world', ctx))"` prints a non-empty substituted string. If this works on main (it does), the fix-lane is unnecessary.

### Blocker 3: Lane C's "20 broken tests" claim

**What:** Lane C reports that 20 tests in pre-existing files break after Lane A's handler lands. No enumeration was provided; no PR is open for Lane A; no reproducible test run was attached.

**Which fix-lane closes it:** Fix Lane C1 is intended to close this.

**Verification step:** Run the full test suite on a worktree that combines Lane A's `runner/handlers.py` (post duplicate-import fix), Lane B's `bin/conformance`, and Lane C's unique files (`pipelines/factory/level5_feature.dot`, migrated `gates.dot`/`pr_gates.dot`, `runner/engine_run.py` wiring, `tests/test_level5_pipelines.py`). Compare the failure set to Lane C's claimed 20 tests. If actual failures exceed 5, fan out per-file fix subagents (Fix Lane C1's job). If failures are <= 2, the claim is stale and the fix-lane is unnecessary.

---

## Section 5: Verification plan

### Test commands

```bash
# Full suite on each lane's working tree (post duplicate-import fix for Lane A):
cd <worktree> && .venv/bin/python -m pytest tests/ -x --tb=short 2>&1 | tail -50

# Lane A: dynamic node tests + import smoke
cd <worktree-a> && python -c "import runner.handlers; print('OK')" && \
  .venv/bin/python -m pytest tests/test_dynamic_node.py -v

# Lane B: conformance rule tests + integration
cd <worktree-b> && .venv/bin/python -m pytest tests/test_conformance.py -v

# Lane C: pipeline tests + migrated .dot validates
cd <worktree-c> && \
  .venv/bin/python -m pytest tests/test_level5_pipelines.py -v && \
  bin/conformance validate pipelines/factory/*.dot pipelines/slim/*.dot
```

### Expected pass/fail counts

| Worktree | Expected pass | Expected fail | Notes |
|---|---|---|---|
| Lane A (post duplicate-import fix) | 7 Lane-A tests + all non-affected main tests | 0 | Lane A's change is additive; no test in main references `type="dynamic"`. |
| Lane B | 6 Lane-B tests + all non-affected main tests | 0 | Lane B's rule is new; `gates.dot` and `pr_gates.dot` will fail conformance on this worktree because Lane C hasn't migrated them yet — that's expected. |
| Lane C (rebased on A+B) | 12 Lane-C tests + 7 Lane-A + 6 Lane-B + all main tests | 0 | Reference `.dot` is new; migrated `.dot` files pass Lane B's rule; engine wiring is opt-in. |
| Final integrated | 268+ tests (pre-G3 baseline from memory) + 7 + 6 + 12 = 293+ | 0 | If Fix Lane C1's "20 broken" claim is real, this number drops by 20 and fix-lane must add them back. |

### Pre-merge hooks

- `bin/conformance validate pipelines/factory/*.dot` MUST pass (CI gate already wired per CLAUDE.md).
- `pytest tests/` MUST pass on the merged branch.
- `python -c "import runner.handlers"` MUST exit 0 (smoke check; equivalent to `pytest --collect-only` succeeding).

### Post-merge hooks

- Manual smoke: `dark-factory --pipeline pipelines/factory/hello.dot --goal "smoke test" --backend echo` exits 0 (regression check — hello.dot is exempted from level5, so it must still work).
- Manual smoke: `dark-factory --pipeline pipelines/factory/level5_feature.dot --goal "reference smoke" --backend echo` exits 0 with the fallback path (echo backend → `_dynamic` → static fallback → codergen → echo output).
- Conformance check: `bin/conformance validate pipelines/factory/gates.dot` exits 0 (proves Lane C's migration satisfies Lane B's rule).

---

## Section 6: Risk and rollback

### Lane A risk

- **Risk:** `_dynamic` handler is on the runner hot path; a typo or wrong import could break every `codergen` call that has any `default=` attr. Mitigation: the handler only fires when `resolve()` returns `_dynamic`, which only happens for `type="dynamic"` nodes — main has zero such nodes today, so blast radius is zero on main.
- **Rollback:** revert `runner/handlers.py`, `prompts/_dynamic_wrap.md`, `tests/test_dynamic_node.py`. ~254 lines removed. No downstream consumer exists.
- **Can merge independently:** Yes.

### Lane B risk

- **Risk:** The level5 rule rejects any new pipeline under `pipelines/factory/` that lacks the 4 hard-tier reviewers. If a future pipeline author adds a slim-style fast lane to `pipelines/factory/`, conformance will reject it until they add the reviewers. Mitigation: the rule explicitly exempts `hello.dot`; new authors can name their pipeline `*.dot` and use `graph [level5="false"]` to opt out (the rule checks `level5="true"` OR path match; future authors should know the contract).
- **Risk:** The `_has_cross_vendor_reviewer` heuristic looks at `command=` attribute on `tool` nodes. Future pipelines that use `codex exec` as their main coder (not a reviewer) could be misread. Mitigation: the rule only flags missing reviewers; presence is enough. Cross-vendor reviewer is required only when `graph [backend="<x>"]` is set; unset backend always passes.
- **Rollback:** revert `bin/conformance` and `tests/test_conformance.py`; the 3 fixture files can stay or go (they only matter if Lane B is live).
- **Can merge independently:** Yes (but Lane C's migrations need it).

### Lane C risk

- **Risk:** `gates.dot` and `pr_gates.dot` add `gate_skeptic` and `adversarial_reviewer` nodes, breaking any existing PR that referenced the old node set. Mitigation: this is a structural change to a public-facing pipeline; the alternative is to keep two parallel pipelines (`gates.dot` and `gates_level5.dot`). Mitigation failed — the design doc mandates one set.
- **Risk:** `engine_run.py` adds an opt-in `ctx.state["_graph"]` write. If the env flag is set in CI by accident, every prompt substitution iterates a Graph in state. Mitigation: the env flag is opt-in; nothing in main sets it. Mitigation is robust.
- **Risk:** `level5_feature.dot` references `prompts/slim/explore.md`, `plan.md`, `implement.md`, `fix.md`, `spec_validation.md`. If those prompts don't exist, parse fails (or runtime fails). Mitigation: these prompts exist in main per the memory entry on F5/F6 timeout pattern; verification step checks `ls prompts/slim/*.md`.
- **Rollback:** revert `pipelines/factory/level5_feature.dot`, `pipelines/factory/gates.dot`, `pipelines/factory/pr_gates.dot`, `runner/engine_run.py`, `tests/test_level5_pipelines.py`. ~50 lines net.
- **Can merge independently:** No — depends on Lane A (dynamic handler) and Lane B (rule). Must follow A+B.

### Fix Lane A1 risk

- **Risk:** None — the guard already exists. The fix-lane can only add a docstring or change `else str(v)` to `else json.dumps(v, default=str)` for dicts. Either way, no regression.
- **Rollback:** revert any change made to `runner/handler_render.py`.
- **Can merge independently:** Yes (or skip entirely).

### Fix Lane C1 risk

- **Risk:** Per-file fix subagents fan out across pre-existing test files; per the parallel-subagents rule, file overlap must be checked before dispatch. Risk is bounded by lane C's claim of 20 files; actual count is unknown.
- **Rollback:** revert each per-file fix commit individually.
- **Can merge independently:** Yes per file (each fix is a separate commit).

---

## Surprises discovered during planning

1. **Lane A's `runner/handlers.py` has a duplicate-import syntax error** at lines 41-51: `from .handler_core import (Result as _Result, Result, Context, …)` imports the same `Result` twice in one statement — this is invalid Python and will fail at module load. This is NOT in the lane reports and is the actual blocker for Lane A. The fix is a one-line cleanup. **Recommended action: fix this in Lane A's worktree before opening the PR — easier to merge than to add another fix-lane.**

2. **Lane C's "20 broken tests" claim and the `_substitute_placeholders` blocker are both stale.** Main's `_substitute_placeholders` at `runner/handler_render.py:42` already guards non-string state values with `v if isinstance(v, str) else str(v)`. `str(Graph)` is safe (verified — dataclass repr, no recursion). Lane C's engine wiring will not crash with `TypeError: replace() argument 2 must be str, not Graph` — that error message was probably from an older code state. The `DARK_FACTORY_G3_GRAPH_WIRING=1` env-gate is unnecessary defense in depth; it should be removed in Lane C (or kept, harmless either way).

3. **No PR is open for any of the three lanes.** `gh pr list --head worktree-agent-a3f9d79655af1d56f`, `--head worktree-agent-a00e4f8cd9cf415f8`, and `--head worktree-agent-a484a107cfbb60ba7` all return empty. The lane worktrees are sitting in the user's `.claude/worktrees/` with uncommitted changes — none have been pushed. This means the lanes cannot be merged by an automated 7-green driver yet; someone needs to commit + push + open a PR for each lane first.

4. **Lane C duplicates Lane A's and Lane B's files in its own worktree.** Per `git diff --stat`, Lane C's working tree contains Lane A's `runner/handlers.py` change, Lane A's `tests/test_dynamic_node.py`, Lane A's `prompts/_dynamic_wrap.md`, Lane B's `bin/conformance` change, Lane B's `tests/test_conformance.py` change, and Lane B's three fixtures — all alongside Lane C's own unique additions. This is a re-implementation rather than a rebase. The user must rebase Lane C onto merged A+B and drop the duplicates, OR Lane C must be applied as a single large PR (atomic with A and B). Atomic is simpler and recommended.

5. **Lane C's `gates.dot` and `pr_gates.dot` migrations strip the trailing newline.** Minor cosmetic diff (`-}\n` becomes `-}\n\ No newline at end of file`). Not a regression, but every existing pre-commit/formatting tool that checks trailing newlines will flag it. Recommended: `printf "\n" >> pipelines/factory/gates.dot` and same for `pr_gates.dot` before commit.

6. **Lane C's `level5_feature.dot` references `prompts/slim/explore.md` and four other slim prompts** that may or may not exist depending on the merged branch state. Per `pipeline-selection.md` and `pipelines/slim/spec_gen.dot`, slim prompts exist (`spec_gen.dot` uses them), but a sanity check is needed: `for p in explore plan implement fix spec_validation; do test -f prompts/slim/$p.md || echo MISSING $p; done`.

---

## Recommended merge order (1 line)

**Lane B → Lane A (with duplicate-import fix) → Lane C (rebased, atomically if practical) → Fix Lane A1 (no-op or revert) → Fix Lane C1 (verify count first).**

---

## Anything else needing fixing before merge-ready

1. **Fix the `Result, Result` duplicate import in Lane A's `runner/handlers.py`** before opening the PR. One-line edit; no design change.
2. **Verify Lane C's "20 broken tests" claim is reproducible** before dispatching Fix Lane C1. If unverifiable on main, cancel the fix-lane and re-dispatch after the merge.
3. **Commit + push + open PRs** for each lane. None are open today. Lane A and Lane B can be opened independently; Lane C should open only after A+B are merged (or atomically as one PR if practical).
4. **Restore trailing newlines** in `pipelines/factory/gates.dot` and `pr_gates.dot` (Lane C cosmetic regression).
5. **Sanity-check `prompts/slim/{explore,plan,implement,fix,spec_validation}.md` exist** before merging Lane C's reference `.dot`. If any are missing, Lane C's pipeline will fail at runtime even though it passes conformance.
6. **Consider removing the `DARK_FACTORY_G3_GRAPH_WIRING` env gate** from Lane C's `engine_run.py`. The guard it was defending against already exists in main. The env gate is dead defense; making the wiring unconditional simplifies the runner's mental model and removes a future-bug surface.