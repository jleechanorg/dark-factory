"""Pipeline engine — re-export shim.

The implementation lives in the per-responsibility modules:

* `runner.engine_run` — the public `run()` loop and the single-node driver
* `runner.engine_parallel` — parallel fan-out / fan-in primitives
* `runner.engine_edges` — edge condition grammar + edge selection
* `runner.engine_persist` — checkpoint + state-update + retry-meta helpers
* `runner.engine_observability` — side-effect emission + outcome classification
* `runner.engine_branches` — branch context isolation
* `runner.engine_exceptions` — exception-to-StepRecord translation

`runner.engine` re-exports every symbol that was previously importable from
this module so existing call sites (`from runner.engine import ...`) keep
working. See `docs/refactor/file-ownership-map.engine.md` for the rationale.
"""

from __future__ import annotations

# Public API re-exports -------------------------------------------------
from .engine_run import run  # noqa: E402,F401

from .engine_persist import (  # noqa: E402,F401
    StepRecord,
    _append_record,
    _goal_gate_target,
    _load_checkpoint,
    _node_max_retries,
    _persist,
    _run_with_retries,
    _start_node,
    _successful_for_node,
    _update_failure_state,
)

from .engine_parallel import (  # noqa: E402,F401
    _apply_join_policy,
    _find_join_node,
    _is_join_node,
    _is_parallel_node,
    _parallel_branches,
    _parallel_join_edges,
    _parallel_join_outcome,
    _run_branch_until_join,
)

from .engine_edges import (  # noqa: E402,F401
    _allow_partial,
    _attr_int,
    _choose_edge,
    _edge_matches,
    _edge_weight,
    _evaluate_expression,
    _is_decision_node,
    _lookup,
    _normalize_label,
    _pick_next,
    _pick_next_from_edges,
)

from .engine_observability import (  # noqa: E402,F401
    _LOG_DIR,  # tests/test_crash_resilience.py monkeypatches this path
    _VALIDATION_TYPES,
    _classify_records,
    _collect_uncommitted_state,
    _emit_event,
    _format_uncommitted_for_log,
    _is_partial_result,
    _is_success_result,
    _is_validation_failed,
    _is_validation_node,
    _log,
    _node_backend,
    _node_type,
    _normalize_outcome_only,
    _normalized_result,
    _open_run_log,
    _outcome_counts,
    _perf_node_enter,
    _perf_node_exit,
    _write_heartbeat,
    _write_transcript_sidecar,
)

from .engine_branches import (  # noqa: E402,F401
    _READ_ONLY_BRANCH_TYPES,
    _branch_context,
    _clone_context,
)

from .engine_exceptions import _handle_node_exception  # noqa: E402,F401

# Re-exported for call sites that previously reached into `runner.engine`
# (e.g. `benchmarks/workflow_graphgen/harness.py`). The underlying symbol
# comes from `runner.handlers`; we surface it here for backward compat.
from .handlers import Context, Result, resolve  # noqa: E402,F401
