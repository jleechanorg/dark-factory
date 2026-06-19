# runner/engine.py — File Ownership Index

**Status**: Map PR for bead `jleechan-gs2`. Documentation only — no production code changes.
**Actual line count at HEAD**: 2005 lines (prompt stated 1720; difference is WIP drift since the bead was filed).
**Target**: ≤ 600 lines per new module after the follow-up split PR.

## External API surface (importers)

| Importer | Symbols imported |
|---|---|
| `runner/__main__.py` | `run` |
| `runner/structural_preflight.py` | doc references to `_render_prompt` |
| `runner/_agy_safe.py` | wraps `_codergen` for sandbox timeout safety |
| `runner/parser.py` | `_evaluate_expression` reads `_tokenize_condition` |
| `tests/test_engine.py` | `run`, `_evaluate_expression` |
| `tests/test_crash_resilience.py` | `run`, `_LOG_DIR` |
| `tests/test_slim.py` | `run` |
| `tests/test_attractor_semantics.py` | `run`, `_edge_matches` |
| `tests/test_token_tracking.py` | `run` |
| `tests/test_state_threading.py` | `run` |
| `tests/test_hardening.py` | `_attr_int`, `_edge_matches`, `run` |
| `tests/test_evidence_bundle.py` | `run` |
| `tests/test_spec_gen.py` | `run` |
| `tests/test_bug_fix_pipeline.py` | `run` |
| `tests/test_gate_sha_binding.py` | `run` |
| `tests/test_gate_red_green.py` | `run` |
| `tests/test_parallel_fanout.py` | `run`, `_find_join_node`, `_is_parallel_node`, `_is_join_node`, `_branch_context` |
| `tests/test_loop_bounds.py` | `run` |
| `tests/test_perf_log.py` | `run` |
| `tests/test_gates.py` | `run`, `_branch_context` |
| `tests/test_2gv.py` | `run` |
| `tests/test_ol7.py` | `run` |
| `benchmarks/workflow_graphgen/harness.py` | `engine.run` via attribute access |

**Implication**: `run`, `StepRecord`, and `_branch_context` are the load-bearing public API. Everything else is internal-but-asserted-on by tests (e.g. `_edge_matches`, `_evaluate_expression`, `_attr_int`, `_find_join_node`, `_is_parallel_node`, `_is_join_node`).

## Top-level index

### Module-level constants and locks (L25–L33)

| Symbol | Lines | Role | Called by (in this file / outside) |
|---|---|---|---|
| `_VALIDATION_TYPES` | 25 | frozenset used by `_is_validation_node` | `_is_validation_node` only |
| `_LOG_DIR` | 29 | monkeypatched by `tests/test_crash_resilience.py` | `_open_run_log`, plus `test_crash_resilience.py:149,171` |
| `_EVENT_DIR` | 30 | declared but unused at module level (only `ctx.event_log_path` is consumed) | none — likely dead; flag for follow-up |
| `_event_lock`, `_heartbeat_lock` | 32–33 | threading locks for parallel event/heartbeat writes | `_emit_event`, `_write_heartbeat` |
| `_READ_ONLY_BRANCH_TYPES` | 352 | frozenset gating branch isolation | `_branch_context` only |

### Run/heartbeat/log/event subsystem (L35–L218)

| Symbol | Lines | Role | Called by (in this file / outside) |
|---|---|---|---|
| `_write_heartbeat` | 35–112 | Append heartbeat JSON to `~/.dark-factory/runs/<run_id>/heartbeat.json` | `_run_single_node`, `_append_record`, `run` |
| `_emit_event` | 116–144 | Append JSONL event row to `ctx.event_log_path` | many sites inside `run`, `_run_branch_until_join`, `_handle_node_exception`, `_append_record`, `_persist` |
| `_write_transcript_sidecar` | 147–195 | Write per-attempt transcript + sha256; redact holdout output | inside `run`, `_run_branch_until_join` |
| `_open_run_log` | 198–208 | Best-effort `~/.dark-factory/logs/<run_id>.log` open | `run` only |
| `_log` | 211–218 | tee-line to log handle, swallow IO errors | many sites in `run`, `_run_branch_until_join`, `_handle_node_exception` |

### Outcome classification (L221–L249)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_is_success_result` | 221 | `_classify_outcome(outcome) == "success"` | `_apply_join_policy`, fan-out branch aggregation in `run` |
| `_is_partial_result` | 225 | success+partial when `allow_partial=True` | only used by `_normalize_outcome_only` paths? — verify before split |
| `_is_validation_failed` | 229 | failure/error/partial classifier | none directly — likely dead; flag for follow-up |
| `_is_validation_node` | 233–249 | `type in _VALIDATION_TYPES` OR `validation="true"` attr | `_update_failure_state` |
| `StepRecord` (dataclass) | 253–258 | Public record shape used in `history: list[StepRecord]` | returned by `run`; constructed throughout `run` |

### Node attribute helpers (L261–L291)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_node_backend` | 261 | per-node `backend=` attr, falling back to `ctx.backend` | `_perf_node_enter`, `_write_heartbeat` |
| `_node_type` | 268 | per-node `type=` attr with shape/start/exit fallback | `_perf_node_enter` |
| `_outcome_counts` | 281 | success/failure/error tally from history | `run` finally block |

### Performance logging fan-out (L294–L320)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_perf_node_enter` | 294 | call `perf_log.node_enter(...)` | inside `run` main loop, fan-out branch starts |
| `_perf_node_exit` | 305 | call `perf_log.node_exit(...)` | `run` main loop, parallel branches, `_handle_node_exception` |

### Context cloning + branch isolation (L323–L385)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_clone_context` | 323 | shallow-clone `Context` for parallel branches | `_branch_context` |
| `_branch_context` | 358 | isolate file-writing branches into `mkdtemp`, keep read-only gates on parent workdir | `run` main loop (parallel fan-out); also `tests/test_gates.py::test_branch_context_keeps_parent_workdir_for_readonly_gates`, `tests/test_parallel_fanout.py:1322` |

### Checkpoint + decision-node helpers (L388–L410)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_load_checkpoint` | 388 | load `StepRecord` list from JSON file | `run` (resume path) |
| `_is_decision_node` | 407 | `shape == "hexagon"` or `type == "conditional"` | `_edge_matches` |

### Edge condition expression grammar (L413–L560)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_evaluate_expression` | 413 | Recursive-descent parser for `key=value`, `key!=value`, `key contains v`, `key in a,b`, AND/OR/NOT, parens | `_edge_matches`; also asserted directly by `tests/test_engine.py:271–282` |
| `_edge_matches` | 536 | Evaluate an `Edge.condition` against `Result` + `Context` | `_pick_next`, `_pick_next_from_edges`, `_handle_node_exception`, parallel fan-out |
| `_lookup` | 549 | Resolve `key` from `ctx.state` (decision nodes) or `last.metadata` / `outcome` | `_evaluate_expression` |

### Outcome normalization (L563–L587)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_normalize_outcome_only` | 563 | classify raw outcome string via `_classify_outcome` | `_classify_records`, `_normalized_result`, `_parallel_join_outcome`, many `run` call sites |
| `_classify_records` | 567 | success/partial/failure counts | `_parallel_join_outcome` |
| `_normalized_result` | 582 | idempotently normalize `Result.outcome` | `_run_single_node`, `run` main loop, `_handle_node_exception` |

### Parallel fan-out/fan-in (L590–L837)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_parallel_join_edges` | 590 | Filter edges with `join="true"` | `run` main loop |
| `_parallel_branches` | 603 | Filter outgoing edges for branch candidates | `run` main loop (non-component parallel branch path) |
| `_parallel_join_outcome` | 619 | Compute join outcome from branch results | `run` main loop |
| `_is_parallel_node` | 636 | `type=parallel` or (no type and shape=component) | `run`, `_load_checkpoint` resume path; `tests/test_parallel_fanout.py:1156` |
| `_is_join_node` | 649 | `type=join` or (no type and shape=tripleoctagon) | `_find_join_node`, `run`; `tests/test_parallel_fanout.py:1156` |
| `_find_join_node` | 661 | BFS from fanout successors to nearest join | `run`; `tests/test_parallel_fanout.py:860` |
| `_apply_join_policy` | 689 | `wait_all` / `first_success` / `k_of_n` policy | `run` |
| `_run_branch_until_join` | 708 | Execute one parallel branch to join barrier; thread-local CXDB | `run` fan-out executor |

### Single-node execution + retries (L840–L869)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_run_single_node` | 840 | Run handler with retries, normalize, write `_last_node`/`_last_outcome`/`_last_output` into state | `run` main loop, `_run_branch_until_join`, fan-out branch path in `run` |

### Edge selection + attribute parsing (L874–L913)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_allow_partial` | 874 | `allow_partial` bool parser | `run` fan-out join aggregation |
| `_pick_next_from_edges` | 881 | Pick next node from an explicit edges list | `run` non-component parallel branches |
| `_attr_int` | 902 | Safely coerce node attr to int | many sites in `run`, `_run_with_retries`, `_run_branch_until_join`; `tests/test_hardening.py:27` |

### Exception routing (L916–L1011)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_handle_node_exception` | 916 | Record `error` step + pick recovery edge | `run` main loop (node crash + transition crash paths) |

### Main loop: `run()` (L1014–L1768)

The single entry point. ~750 lines. Internal sub-sections:

- L1027–L1066: resume + checkpoint loading
- L1068–L1138: run_id, CXDB init, manifest, perf_log open
- L1140–L1195: main while-loop prelude + crash handling
- L1196–L1614: per-step result handling + parallel fan-out/fan-in (the largest block; do not split mid-loop)
- L1616–L1721: next-edge selection (parallel-aware)
- L1722–L1768: finally block: emit run_end, close CXDB, perf_log close

### Final helpers (L1771–L2005)

| Symbol | Lines | Role | Called by |
|---|---|---|---|
| `_start_node` | 1771 | locate graph start node | `run` resume path |
| `_run_with_retries` | 1780 | Retry handler up to `max_retries`; tag metadata | `_run_single_node` |
| `_node_max_retries` | 1801 | node `max_retries=` or graph default | `_run_with_retries` |
| `_successful_for_node` | 1814 | outcome==success OR (allow_partial AND outcome==partial) | `_run_with_retries`, `_goal_gate_target` |
| `_goal_gate_target` | 1826 | resolve `retry_target=` attr if goal_gate failed | `_load_checkpoint` resume, `_handle_node_exception`, `run` next-edge selection |
| `_update_failure_state` | 1848 | manage `_unresolved_failure[_node]` in ctx.state | `_run_single_node`, `_handle_node_exception`, `run` next-edge, `_load_checkpoint` |
| `_append_record` | 1863 | Append to history, write checkpoint, emit step event, persist CXDB | `run` (many sites), `_handle_node_exception` |
| `_persist` | 1913 | Forward to CXDB with error swallowing | `_append_record` |
| `_pick_next` | 1946 | Pick next node: matching-conditions first, else unconditional | `run` next-edge, `_load_checkpoint` resume |
| `_choose_edge` | 1963 | Rank edges by preferred_label + suggested_next_ids + weight + lexical | `_pick_next`, `_pick_next_from_edges`, `_handle_node_exception` |
| `_normalize_label` | 1985 | Strip `[label]`, `(label)`, leading `N.` / `N-` / `N)` decoration | `_choose_edge` |
| `_edge_weight` | 1996 | Read `weight=` attr (defaults to 0) | `_choose_edge` |

## Proposed split (responsibility groups, ≤ 600 lines each)

| New module | Lines moved | Public API (re-exported from engine.py) | Rationale |
|---|---|---|---|
| `runner/engine_run.py` | `run` (L1014–L1768) + `_run_single_node` (L840–L869) | `run` | The main loop and single-node driver are one inseparable unit (fan-out lives inside `run`'s body). Splitting `run` mid-loop would require passing 12+ state locals as parameters. |
| `runner/engine_parallel.py` | `_parallel_branches`, `_parallel_join_edges`, `_parallel_join_outcome`, `_is_parallel_node`, `_is_join_node`, `_find_join_node`, `_apply_join_policy`, `_run_branch_until_join` (L590–L837, ~250 lines) | none new (private to engine) | Pure parallel-fanout/fanin logic. Self-contained; tests in `tests/test_parallel_fanout.py` assert these directly. Grouping all parallel primitives in one file makes the model testable in isolation. |
| `runner/engine_edges.py` | `_evaluate_expression`, `_edge_matches`, `_lookup`, `_is_decision_node`, `_pick_next`, `_pick_next_from_edges`, `_choose_edge`, `_normalize_label`, `_edge_weight`, `_attr_int`, `_allow_partial` (L413–L587 + L874–L913 + L1946–L2005, ~250 lines) | none new (private) | Edge selection + condition grammar. Distinct from main loop. |
| `runner/engine_persist.py` | `StepRecord`, `_load_checkpoint`, `_append_record`, `_persist`, `_update_failure_state`, `_goal_gate_target`, `_node_max_retries`, `_successful_for_node`, `_run_with_retries`, `_start_node` (L253–L258 + L388–L405 + L1801–L1860 + L1913–L1920, ~200 lines) | `StepRecord` | Checkpoint / state-update / retry-meta helpers. Tightly coupled; one self-contained unit. |
| `runner/engine_observability.py` | `_write_heartbeat`, `_emit_event`, `_write_transcript_sidecar`, `_open_run_log`, `_log`, `_perf_node_enter`, `_perf_node_exit`, `_is_validation_node`, `_VALIDATION_TYPES`, `_LOG_DIR`, `_EVENT_DIR`, `_event_lock`, `_heartbeat_lock`, module-level outcome classifiers (`_is_success_result`, `_is_partial_result`, `_is_validation_failed`, `_normalize_outcome_only`, `_classify_records`, `_normalized_result`, `_outcome_counts`, `_node_backend`, `_node_type`) (L25–L33 + L221–L291 + L294–L320, ~300 lines) | none new (private) | All side-effect emission (CXDB events, perf log, run log, heartbeat) lives here. Note: `_is_validation_failed` and `_is_partial_result` are likely dead — flag for follow-up cleanup. |
| `runner/engine_branches.py` | `_clone_context`, `_branch_context`, `_READ_ONLY_BRANCH_TYPES` (L323–L385, ~65 lines) | none new (private) | Branch context isolation. Tiny but conceptually distinct (file-writing isolation vs read-only passthrough). Could fold into `engine_parallel.py` but `tests/test_gates.py::test_branch_context_keeps_parent_workdir_for_readonly_gates` and `tests/test_parallel_fanout.py:1322` assert it independently — keep separate for testability. |
| `runner/engine_exceptions.py` | `_handle_node_exception` (L916–L1011, ~100 lines) | none new (private) | Exception-to-StepRecord translation + recovery-edge routing. Single-purpose; isolated. |
| `runner/engine.py` (re-export shim) | only imports + re-exports | `run`, `StepRecord`, plus everything currently imported by other files via `from runner.engine import …` | Additive re-export. The original `engine.py` becomes a thin shim. The split PR deletes it after one release cycle. |

**Line-count budget**: each new module is ≤ 300 lines; the main loop (`engine_run.py`) is ~750 lines — over budget. See "Low-level details" in the master map for the rationale + the per-feature test split that mirrors this.

**Unknown callers**: None. Every symbol listed above has a verified caller (in-file or external). Marked: `unknown caller — needs investigation` — none.
