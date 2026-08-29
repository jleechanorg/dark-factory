"""Exception-to-StepRecord translation + recovery-edge routing.

Splits out from `runner/engine.py` (see `docs/refactor/file-ownership-map.engine.md`).
Owns `_handle_node_exception` — the function that converts a node/transition
crash into an `error` StepRecord and picks a recovery edge.
"""

from __future__ import annotations

import pathlib
import time
import traceback
from typing import Optional, TextIO

from . import engine_edges as _edges
from . import engine_observability as _obs
from . import engine_persist as _persist
from .cxdb import CXDB
from .handlers import Context, Result
from .parser import Graph, Node


def _handle_node_exception(
    graph: Graph,
    current: Node,
    ctx: Context,
    exc: Exception,
    history: list,
    checkpoint: Optional[pathlib.Path],
    cxdb: Optional[CXDB],
    seq: int,
    log: Optional[TextIO],
    visits: dict[str, int],
    *,
    skip_perf_exit: bool = False,
) -> Optional[Node]:
    """Record a node/transition crash as an `error` step and pick a recovery edge.

    The full traceback is written to the per-run log file; a compact
    type+message+last-frame summary lands in the StepRecord.output_preview so
    the crash is visible in CXDB and the CLI trace without a fatal abort.

    Returns the next node to route to (a registered retry/fix edge that matches
    `outcome=error`/`outcome!=success`) or None when the run should end.

    ``skip_perf_exit`` must be True when the node already exited successfully
    before the transition raised; otherwise a second node_exit would be logged
    for the same enter/seq pair (duplicate).
    """
    tb_text = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
    _obs._log(log, f"node {current.name!r} raised {type(exc).__name__}: {exc}\n{tb_text}")

    summary_frame = tb_text.strip().splitlines()[-1] if tb_text.strip() else ""
    preview = f"{type(exc).__name__}: {exc} | {summary_frame}"

    error_result = _obs._normalized_result(
        Result(
            outcome="error",
            output=tb_text,
            metadata={"exception": type(exc).__name__},
        )
    )
    ctx.state["_last_node"] = current.name
    ctx.state["_last_outcome"] = "error"
    ctx.state["_last_output"] = tb_text
    ctx.history.append({"node": current.name, "outcome": "error"})

    record = _persist.StepRecord(
        node=current.name,
        outcome="error",
        ts=time.time(),
        output_preview=preview[:280],
        metadata={"exception": type(exc).__name__},
    )
    _persist._append_record(history, checkpoint, cxdb, ctx, seq, record, tb_text, record.metadata)
    if not skip_perf_exit:
        _obs._perf_node_exit(
            ctx,
            current.name,
            seq,
            "error",
            visits.get(current.name, 1),
            record.metadata,
        )
    _obs._emit_event(
        ctx,
        "node_exception",
        {
            "node": current.name,
            "error_type": type(exc).__name__,
            "message": str(exc),
        },
        seq,
    )
    _persist._update_failure_state(current, ctx, error_result)

    # A crash is a failure: only a deliberate recovery edge may catch it. That
    # means a goal_gate `retry_target` or a *conditional* edge that matches the
    # error outcome (e.g. `outcome!=success` / `outcome=error`). Unconditional
    # forward edges are NOT recovery — following them would silently carry a
    # crashed run onward and let it reach `exit` as success. With no recovery
    # edge, the run ends here on the recorded `error` step.
    gate_target = _persist._goal_gate_target(graph, current, error_result, ctx)
    if gate_target is not None:
        _obs._log(log, f"routing crashed node {current.name!r} -> {gate_target.name!r} (goal_gate)")
        return gate_target
    matching = [
        edge
        for edge in graph.outgoing(current.name)
        if edge.condition and _edges._edge_matches(edge, error_result, ctx, current)
    ]
    if matching:
        selected = _edges._choose_edge(matching, error_result)
        next_node = graph.nodes.get(selected.dst)
        _obs._log(log, f"routing crashed node {current.name!r} -> {selected.dst!r} (fix edge)")
        return next_node
    if (
        str(graph.attrs.get("validation_rounds", "")).strip().lower() in ("true", "1", "yes")
        or "rounds.current" in ctx.state
    ):
        next_node = _edges._pick_next(graph, current, error_result, ctx)
        if next_node is not None:
            _obs._log(log, f"routing crashed member {current.name!r} -> {next_node.name!r} (validation round continuation)")
            return next_node
    _obs._log(log, f"no recovery edge for crashed node {current.name!r}; ending run")
    return None
