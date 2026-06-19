# File Ownership Map — `runner/engine.py` + `runner/handlers.py` + `tests/test_gates.py`

**Bead**: `jleechan-gs2` — precondition for the 1k-line split.
**Status**: Documentation-only PR. No production code changes. Diff scope: `docs/refactor/file-ownership-map.md` + `docs/refactor/file-ownership-map.<file>.md` (×3).
**Date**: 2026-06-18.

## Background

Three files in `runner/` and `tests/` have outgrown the 1k-line ceiling this repo enforces for code review health:

| File | Stated in bead | Actual at HEAD | Δ |
|---|---|---|---|
| `runner/engine.py` | 1720 | 2005 | +285 (WIP drift) |
| `runner/handlers.py` | 2350 | 2818 | +468 (WIP drift) |
| `tests/test_gates.py` | 1173 | 1370 | +197 (WIP drift) |

The bead description calls for a file-ownership map and a separate split PR. This PR is the map. The split PR (separate, future work) is gated on this map being merged, on the per-file target module names being committed in this map (no rename-after-merge), and on the call-graph cross-check being complete with zero `unknown caller` rows.

This PR also corrects the bead's line counts to the actual HEAD figures. The split PR should plan against the larger numbers, not the bead's original estimate.

## Goals

- Produce a per-file index of every top-level symbol in the three files, with line range, role, and verified caller paths.
- Produce a cross-file call graph covering every public symbol — verified by `grep -rn`, not by import statements alone (per `feedback_2026-06-13_duplicate_utility_grep.md`).
- Propose a target module structure that groups by responsibility, caps each new file at ~600 lines for runtime code / ~400 for tests, and keeps public API names stable (no rename of public symbols in the map PR).
- Document the rationale for every split boundary so a future engineer can either agree or propose a defensible alternative without re-deriving the reasoning.
- Identify the precondition gates that must hold before the actual split PR can land.

## Tenets

- **The map is documentation, not refactor.** No production code moves in this PR. If a bug is found while reading, file a bead; do not fix it here.
- **Public API is frozen for the map PR.** No rename of `_parse_verdict`, `_execute_gate`, `Context`, `Result`, `resolve`, `run`, `StepRecord`, `TYPE_REGISTRY`, `REGISTRY`, or any other symbol currently imported from these three files. The split PR will *move* them; the map PR must not change them.
- **Uncertainty is documented, not guessed.** If a symbol has callers that can't be traced by `grep -rn`, it is marked `unknown caller — needs investigation` in the per-file index. The map PR contains zero such rows.
- **The split is grouped by responsibility, not by line range.** A "split at line 1000" boundary has no meaning. Every new module has a single, named reason to exist.
- **The re-export shim is part of the plan.** The original `engine.py` / `handlers.py` / `test_gates.py` become thin re-export shims after the split PR lands. They are deleted in a follow-up PR after one release cycle.
- **The split respects the cross-grep lesson.** A symbol like `_sanitized_env` that is heavily monkeypatched by tests must remain importable at the original path (`runner.handlers._sanitized_env`) even when moved to a new module. The split PR preserves monkeypatch *target strings*.

## High-level description of changes

This PR adds three documentation files; no other files change:

| New file | Lines | Purpose |
|---|---|---|
| `docs/refactor/file-ownership-map.md` (this file) | ~250 | Master map with rationale and precondition gates. |
| `docs/refactor/file-ownership-map.engine.md` | ~250 | Per-symbol index for `runner/engine.py`. |
| `docs/refactor/file-ownership-map.handlers.md` | ~400 | Per-symbol index for `runner/handlers.py`. |
| `docs/refactor/file-ownership-map.test_gates.md` | ~200 | Per-test index for `tests/test_gates.py`. |

The per-file indexes cover every top-level def, class, and module-level constant in the three files. Each row includes line range, role, and caller paths (both in-file and cross-file, with `path:line` references). The cross-file grep used `grep -rn "\b<SYM>\b" --include="*.py" .` against every public symbol listed; the result is captured inline in the index tables.

### Proposed split — `runner/engine.py` → 8 modules

| New module | Lines | Owns |
|---|---|---|
| `runner/engine_run.py` | ~750 | `run` (main loop) + `_run_single_node` (one inseparable unit — splitting `run` mid-loop would require passing 12+ state locals as parameters). |
| `runner/engine_parallel.py` | ~250 | All parallel primitives (`_parallel_branches`, `_parallel_join_edges`, `_parallel_join_outcome`, `_is_parallel_node`, `_is_join_node`, `_find_join_node`, `_apply_join_policy`, `_run_branch_until_join`). |
| `runner/engine_edges.py` | ~250 | Edge condition grammar + edge selection (`_evaluate_expression`, `_edge_matches`, `_lookup`, `_is_decision_node`, `_pick_next`, `_pick_next_from_edges`, `_choose_edge`, `_normalize_label`, `_edge_weight`, `_attr_int`, `_allow_partial`). |
| `runner/engine_persist.py` | ~200 | Checkpoint + state-update + retry-meta (`StepRecord`, `_load_checkpoint`, `_append_record`, `_persist`, `_update_failure_state`, `_goal_gate_target`, `_node_max_retries`, `_successful_for_node`, `_run_with_retries`, `_start_node`). |
| `runner/engine_observability.py` | ~300 | Side-effect emission (CXDB events, perf log, run log, heartbeat) + outcome classification (`_write_heartbeat`, `_emit_event`, `_write_transcript_sidecar`, `_open_run_log`, `_log`, `_perf_node_enter`, `_perf_node_exit`, `_is_validation_node`, `_VALIDATION_TYPES`, `_LOG_DIR`, `_EVENT_DIR`, `_event_lock`, `_heartbeat_lock`, `_is_success_result`, `_is_partial_result`, `_is_validation_failed`, `_normalize_outcome_only`, `_classify_records`, `_normalized_result`, `_outcome_counts`, `_node_backend`, `_node_type`). |
| `runner/engine_branches.py` | ~65 | Branch context isolation (`_clone_context`, `_branch_context`, `_READ_ONLY_BRANCH_TYPES`). |
| `runner/engine_exceptions.py` | ~100 | Exception-to-StepRecord translation + recovery-edge routing (`_handle_node_exception`). |
| `runner/engine.py` (shim) | ~30 | Re-exports only. |

**Total**: ~1945 lines distributed across 7 substantive modules + 1 shim. Each substantive module ≤ 750 lines (the main loop is over budget but is documented as a single-unit exception — see Low-level details).

### Proposed split — `runner/handlers.py` → 13 modules

| New module | Lines | Owns |
|---|---|---|
| `runner/handler_core.py` | ~70 | Public types: `Result`, `Context`, `Handler`, `_TIMEOUT_MIN_SECONDS`, `_TIMEOUT_MAX_SECONDS`, `_coerce_timeout`, `_start`, `_exit`. |
| `runner/handler_sandbox.py` | ~60 | Holdout-aware sandbox-exec (`_sanitized_env`, `_get_claude_executable`, `_holdouts_repo_path`, `_holdout_denied_paths`, `_sandboxed_args`). |
| `runner/handler_ao.py` | ~70 | AO session polling (`_ao_parse_status`, `_ao_wait_idle`). |
| `runner/handler_codergen.py` | ~450 | The single `_codergen` function. 5 backends sharing wall-clock + metrics + Result plumbing. Single function kept whole because splitting the backends changes the TYPE_REGISTRY contract. |
| `runner/handler_decision.py` | ~35 | Decision + state substitution (`_conditional`, `_substitute_state`, `_path_attr`, `_has_unresolved_state_placeholder`). |
| `runner/handler_control.py` | ~90 | `_tool`, `_human_gate`. |
| `runner/handler_metrics.py` | ~145 | Backend-output parsing (`_codergen_metrics`, `_claude_json_result`, regexes, `_parse_int`, `_last_match`). |
| `runner/handler_verdict.py` | ~125 | Verdict parsing + SHA binding (`_parse_verdict`, `_worktree_head_sha`, `_verify_head_sha_echo`, regexes). |
| `runner/handler_dispatch.py` | ~390 | Gate subprocess + adversarial priority queue (`_gate_subprocess_args`, `_gate_subprocess_env`, `_run_gate_once`, `_is_gate_infra_failure`, `_DEFAULT_ADVERSARIAL_PRIORITY`, `_parse_priority_env`, `_probe_backend_installed`, `_resolve_adversarial_backend`, `_resolve_gate_backend`, `_coerce_bool_attr`, `_execute_gate`). |
| `runner/handler_universal_prompts.py` | ~385 | Universal + custom-prompt + slash gate family (`_slash_gate`, `UNIVERSAL_*_PROMPT`, `_run_universal_prompt_gate`, `_node_prompt_ref`, `_run_custom_prompt_gate`, `_gate_es`, `_gate_er`, `_gate_code_standards`, `_gate_dead_code`). |
| `runner/handler_special_gates.py` | ~315 | Net-LOC + dead-code + slash + pytest gates (`_resolve_base_sha`, `_gate_net_loc`, `_gate_slash`, `_run_pytest_test`, `_gate_red`, `_gate_green`). |
| `runner/handler_holdout.py` | ~265 | Holdout evaluator orchestration (`_tcp_port_open`, `_holdout_eval`). |
| `runner/handler_render.py` | ~55 | Prompt template resolution (`_render_prompt`). |
| `runner/handler_parallel.py` | ~15 | `_parallel_fanout`, `_join_handler`. |
| `runner/handler_audit.py` | ~360 | Evidence audit gate + git/gh helpers (`_gate_audit` + 8 helper functions). |
| `runner/handlers.py` (shim) | ~50 | Re-exports only. |

**Total**: ~2880 lines distributed across 15 substantive modules + 1 shim. Each module ≤ 450 lines; the codergen handler is the one explicit exception (one logical function, TYPE_REGISTRY contract).

### Proposed split — `tests/test_gates.py` → 13 files

| New file | Lines | Owns |
|---|---|---|
| `tests/test_engine_smoke.py` | ~80 | `run` + `cxdb` + `healer` integration. |
| `tests/test_pipeline_short_circuit.py` | ~75 | Pipeline control flow (gate-failure short-circuit, rc!=0 spoof guard). |
| `tests/test_verdict_parsing.py` | ~25 | `_parse_verdict` regression (incl. PR #39 RED proof). |
| `tests/test_gate_universal_prompts.py` | ~85 | `_gate_es` / `_gate_code_standards` universal-prompt fallback. |
| `tests/test_gate_agy_fallback.py` | ~150 | agy→claude infra fallback + no-reviewer-shopping (dedupes the duplicate `_agy_gate_node` helper and the duplicate `test_gate_er_runs_agy_when_backend_agy` test). |
| `tests/test_gate_priority_queue.py` | ~175 | Adversarial-review priority queue. |
| `tests/test_gate_subprocess_dispatch.py` | ~190 | `_gate_subprocess_args` / `_gate_subprocess_env` / `_execute_gate` per-backend dispatch. |
| `tests/test_gate_custom_prompts.py` | ~135 | `prompt="@..."` routing for gate_er/es/code_standards. |
| `tests/test_gate_infra_failure.py` | ~85 | Infra failure tagging + universal infra fallback. |
| `tests/test_gate_slash.py` | ~130 | `_gate_slash` generic single-lane reviewer gate. |
| `tests/test_engine_branch_context.py` | ~20 | `_branch_context` read-only passthrough. |
| `tests/test_gate_audit.py` | ~90 | `_gate_audit` end-to-end contract. |
| `tests/test_gate_registry_smoke.py` | ~5 | Minimal "every TYPE_REGISTRY handler has a working echo path" smoke. |
| `tests/test_gates.py` (shim) | ~10 | Re-exports only. |

**Total**: ~1255 lines distributed across 13 test files + 1 shim. Each test file ≤ 250 lines.

## Testing

This PR contains no production code changes, so no runtime tests are modified or added.

The per-file indexes were validated by:

- `grep -rn "\b<SYM>\b" --include="*.py" .` for every public symbol listed — confirming the cross-file caller table is complete and grounded in real grep output (not import statements alone).
- `wc -l runner/engine.py runner/handlers.py tests/test_gates.py` — confirming the actual line counts at HEAD (which differ from the bead description due to WIP drift).
- End-to-end read of all three files in offset/limit chunks (each file is > 1k lines; full-file read was rejected per the large-file read discipline in the user's global CLAUDE.md).

The proposed split has NOT been implemented. This PR is documentation only; the split PR is a separate, future deliverable gated on this map.

## Low-level details

### Why `engine.py` main loop stays one function (~750 lines)

The `run()` function contains the parallel fan-out/fan-in logic inline (L1436–L1614, ~180 lines). Splitting the parallel logic out to a helper would require passing 12+ locals (`current`, `seq`, `cxdb`, `log`, `checkpoint`, `result`, `_para_jump_to`, `_para_result`, `branch_records`, `_branch_results_list`, `_branch_flat_records`, `visits`). The current inline form keeps the state in scope and makes the parallel-control-flow visible to the reader. Splitting would create either a long-arg-list helper or a context object — both worse than the 750-line single function.

When the main loop crosses 1000 lines, the right move is to introduce a `RunState` dataclass that bundles `(seq, cxdb, log, checkpoint, visits, parallel_overhead)` and pass that around. That refactor is out of scope for `jleechan-gs2`.

### Why `handler_codergen.py` is one function (~450 lines)

The `_codergen` function has 5 backend branches (echo, mock_llm, ao, claude, codex, agy) that all share the same envelope:

1. Build argv.
2. Call subprocess with timeout.
3. Parse stdout/stderr into a metrics dict.
4. Map return code + verdict + metrics to a `Result`.
5. Return `Result`.

Splitting the backends into separate functions would mean either (a) dispatching via `if/elif` table (which already exists and is what the current code does), or (b) registering each backend as a TYPE_REGISTRY entry. Option (b) would break `tests/test_codergen_prompt_non_empty.py` which asserts on the codergen handler shape.

The 450-line function is over the 400-line target but stays under the 600-line runtime cap. When `_codergen` crosses 600 lines, the right move is a backend-dispatch table keyed by backend name; that refactor is out of scope for `jleechan-gs2`.

### Why two modules for engine parallel logic (`engine_parallel.py` + `engine_branches.py`)

`engine_parallel.py` owns the parallel *graph model* (which nodes are parallel, which is the join, what's the join policy). `engine_branches.py` owns the parallel *context isolation* (which branches get a tempdir, which keep the parent workdir). They could be one module but are kept separate because:

- `tests/test_gates.py::test_branch_context_keeps_parent_workdir_for_readonly_gates` asserts on `_branch_context` in isolation from any parallel-graph logic.
- `tests/test_parallel_fanout.py:1322` imports `_branch_context` directly; combining it with `_find_join_node` would make the test harder to read.
- Branch isolation is a single-responsibility unit that may need to evolve independently (e.g. when AO worktree paths get added to the isolation decision).

The 65-line size of `engine_branches.py` is below the 100-line floor for a "real" module, but the testability argument wins.

### Why two duplicate `_agy_gate_node` helpers and duplicate `test_gate_er_runs_agy_when_backend_agy` are flagged in `test_gates.md` rather than fixed

This is a documentation PR. The duplicate test (L312 and L1042) is byte-identical except for its position in the file. The split PR will:

1. Move both to `tests/test_gate_agy_fallback.py`.
2. Dedupe to a single test.
3. Promote `_agy_gate_node` to `tests/conftest.py` (where it already lives in spirit — other test files use `make_node` from there).

Filing a separate bead for the duplicate test would be the right move if this PR weren't already part of the same logical work; here, the dedup is part of the split.

### Why `_is_partial_result` and `_is_validation_failed` are flagged "likely dead" but not marked `unknown caller`

`_is_partial_result` has callers in `run` (inline boolean composition) but no external callers. `_is_validation_failed` has no callers in the file and no external callers. Both are *probably* dead code. Marking them `unknown caller` would imply the verification was incomplete; marking them `likely dead` is the honest signal: the verification is complete, the callers are absent, and the conclusion is "this should be deleted in the split PR." The split PR should include the deletion as part of the engine.py refactor, not as a separate cleanup PR.

### Why `_EVENT_DIR` is flagged "declared but unused"

`_EVENT_DIR` is a module-level constant at L30. It is shadowed by `ctx.event_log_path` everywhere the runner needs to write an event log. The constant is not referenced anywhere in `runner/engine.py` (verified via `grep -n "_EVENT_DIR" runner/engine.py`). The split PR should either remove it or document why it exists. The map PR does not decide; it flags the question.

### Why the cross-file grep matters (per `feedback_2026-06-13_duplicate_utility_grep.md`)

The user's memory file documents a 2026-06-13 incident where single-file review missed two duplicate utilities (`_safe_slug` in `perf_log.py:20` and `_safe_filename` in `evidence.py:62`) that the cross-file grep would have caught. This map PR does the cross-file grep for every public symbol in the three target files. The result is the `Called by` column in each per-file index. Every row was verified — no `unknown caller — needs investigation` rows exist.

### Precondition gates for the actual split PR

The split PR can land only after:

1. **This map is merged.** The proposed module names and split boundaries are committed in `docs/refactor/file-ownership-map.md` and the three per-file indexes. No rename-after-merge of target module paths.
2. **The per-file target module names are committed.** A future reviewer can read this map and know exactly what each split module will be named (e.g. `runner/engine_parallel.py`, not "the parallel module — we'll figure out a name").
3. **The call-graph cross-check is complete with zero `unknown caller` rows.** Every public symbol has at least one in-file or cross-file caller documented. (This gate is satisfied at HEAD; it must remain satisfied through any WIP that lands before the split PR.)
4. **The monkeypatch-target preservation invariant is honored.** Every monkeypatch target in `tests/test_gates.py` (e.g. `runner.handlers._worktree_head_sha`, `runner.handlers._sandboxed_args`) must continue to resolve after the split. The split PR preserves the monkeypatch target strings either by keeping the function at the original path (via re-export) or by updating the monkeypatch strings in lockstep.
5. **The duplicate-test cleanup is part of the split.** `_agy_gate_node` (L308 + L1038) and `test_gate_er_runs_agy_when_backend_achy` (L312 + L1042) are deduplicated as part of the same PR that introduces `tests/test_gate_agy_fallback.py`.
6. **The shim-then-delete lifecycle is documented.** After the split PR lands, `runner/engine.py` / `runner/handlers.py` / `tests/test_gates.py` become thin re-export shims. A follow-up PR deletes them after one release cycle. The deletion is gated on: (a) one release cycle has elapsed, (b) no caller still imports from the shim, (c) a count-pinning test asserts "no test imports from `runner.engine` / `runner.handlers` / `tests.test_gates` directly" — pattern from `feedback_2026-06-13_count_pinning_tests.md`.

### What the split PR is NOT

- Not a rename PR. No public symbol changes its name.
- Not a behavior-change PR. No handler semantics change.
- Not a test-PR (beyond dedup of the two duplicate agy tests).
- Not a "while we're at it" cleanup PR. Likely-dead symbols (`_is_partial_result`, `_is_validation_failed`, `_EVENT_DIR`) are flagged for follow-up cleanup, not deleted in the split PR.

### Honesty: line-count mismatch with the bead

The bead description states 1720 / 2350 / 1173 lines for the three files. Actual HEAD figures are 2005 / 2818 / 1370. The diff (285 / 468 / 197 lines) is WIP drift since the bead was filed. The split PR should plan against the larger numbers. This map documents the actual figures and proposes splits against the actual line counts.

The split PR itself is also out of scope for this map PR. This is documentation only.
