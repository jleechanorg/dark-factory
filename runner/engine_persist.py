"""Checkpoint + state-update + retry-meta helpers for the engine.

Splits out from `runner/engine.py` (see `docs/refactor/file-ownership-map.engine.md`).
Owns the `StepRecord` dataclass, checkpoint load, record append, CXDB persist,
and the per-node retry / goal-gate / failure-state machinery.
"""

from __future__ import annotations

import json
import pathlib
from dataclasses import asdict, dataclass, field
from typing import Optional, TYPE_CHECKING

from ._classify import _classify_outcome
from .cxdb import CXDB
from .handlers import Context
from .parser import Node, is_exit_node

if TYPE_CHECKING:
    from .engine_observability import _emit_event  # noqa: F401


@dataclass
class StepRecord:
    node: str
    outcome: str
    ts: float
    output_preview: str
    metadata: dict[str, str] = field(default_factory=dict)


# Resolve private engine helpers at runtime to keep this module free of a
# static import cycle. The shim wires these names into globals() of
# `runner.engine`, so a plain attribute lookup through this module re-reads
# the latest binding.
def _obs_attr(name: str):
    from . import engine_observability as _obs
    return getattr(_obs, name)


def _emit_event(ctx: Context, event: str, payload: dict[str, str], seq: int | None = None) -> None:
    return _obs_attr("_emit_event")(ctx, event, payload, seq)


def _is_validation_node(node: Node) -> bool:
    return _obs_attr("_is_validation_node")(node)


def _write_heartbeat(ctx: Context, graph: Optional["object"] = None, node: Optional[Node] = None, is_complete: bool = False) -> None:
    return _obs_attr("_write_heartbeat")(ctx, graph, node, is_complete)


def _attr_int(node: Node, key: str, default: int) -> int:
    from . import engine_edges as _edg
    return _edg._attr_int(node, key, default)


def _edge_matches(edge, last, ctx=None, current=None) -> bool:
    from . import engine_edges as _edg
    return _edg._edge_matches(edge, last, ctx, current)


def _load_checkpoint(path: pathlib.Path) -> list[StepRecord]:
    payload = json.loads(path.read_text())
    if not isinstance(payload, list):
        raise ValueError(f"checkpoint {path} does not contain a list")
    records: list[StepRecord] = []
    for raw in payload:
        if not isinstance(raw, dict):
            raise ValueError(f"checkpoint {path} contains non-dict entry: {raw!r}")
        records.append(
            StepRecord(
                node=str(raw["node"]),
                outcome=str(raw["outcome"]),
                ts=float(raw.get("ts", 0.0)),
                output_preview=str(raw.get("output_preview", "")),
                metadata=dict(raw.get("metadata", {})),
            )
        )
    return records


def _start_node(graph) -> Node:
    if "start" in graph.nodes:
        return graph.nodes["start"]
    for node in graph.nodes.values():
        if is_start_node(node):
            return node
    raise ValueError("graph has no start node")


def _node_max_retries(node: Node, graph) -> int:
    explicit = _attr_int(node, "max_retries", -1)
    if explicit >= 0:
        return explicit
    raw = graph.attrs.get("default_max_retries", 0)
    if isinstance(raw, int) and not isinstance(raw, bool):
        return raw
    try:
        return int(raw)
    except (TypeError, ValueError):
        return 0


def _successful_for_node(node: Node, result) -> bool:
    outcome = _classify_outcome(result.outcome)
    if outcome == "success":
        return True
    allow_partial = node.attrs.get("allow_partial", False)
    if isinstance(allow_partial, bool):
        partial_allowed = allow_partial
    else:
        partial_allowed = str(allow_partial).strip().lower() in {"true", "1", "yes"}
    return partial_allowed and outcome == "partial"


def _run_with_retries(handler, node: Node, ctx: Context, graph) -> list:
    results: list = []
    max_retries = _node_max_retries(node, graph)
    attempts = 0
    while True:
        last = handler(node, ctx)
        results.append(last)
        if _successful_for_node(node, last) or attempts >= max_retries:
            break
        attempts += 1
    retries = str(len(results) - 1)
    for index, result in enumerate(results, start=1):
        result.metadata = {
            **result.metadata,
            "attempt": str(index),
            "max_retries": str(max_retries),
            "retries": retries,
        }
    return results


def _goal_gate_target(graph, node: Node, result, ctx: Optional[Context] = None):
    goal_gate = node.attrs.get("goal_gate", False)
    if isinstance(goal_gate, bool):
        enabled = goal_gate
    else:
        enabled = str(goal_gate).strip().lower() in {"true", "1", "yes"}
    if not enabled or _successful_for_node(node, result):
        return None
    target = str(node.attrs.get("retry_target") or graph.attrs.get("retry_target") or "")
    if not target:
        return None
    for edge in graph.outgoing(node.name):
        if edge.dst == target and _edge_matches(edge, result, ctx, node):
            return graph.nodes.get(target)
    return None


def _update_failure_state(node: Node, ctx: Context, result) -> None:
    if result.outcome != "success":
        ctx.state["_unresolved_failure"] = result.outcome
        if not is_exit_node(node):
            ctx.state["_unresolved_failure_node"] = node.name
        return
    if ctx.state.get("_unresolved_failure_node") == node.name:
        ctx.state.pop("_unresolved_failure", None)
        ctx.state.pop("_unresolved_failure_node", None)
        return
    if _is_validation_node(node):
        ctx.state.pop("_unresolved_failure", None)
        ctx.state.pop("_unresolved_failure_node", None)


def _append_record(
    history: list[StepRecord],
    checkpoint: Optional[pathlib.Path],
    cxdb: Optional[CXDB],
    ctx: Context,
    seq: int,
    record: StepRecord,
    output: str,
    metadata: Optional[dict[str, str]] = None,
) -> int:
    history.append(record)
    if checkpoint is not None:
        try:
            checkpoint.write_text(json.dumps([asdict(r) for r in history], indent=2))
            _emit_event(
                ctx,
                "checkpoint",
                {
                    "node": record.node,
                    "path": str(checkpoint),
                },
                seq,
            )
        except Exception as exc:
            _emit_event(
                ctx,
                "persistence_error",
                {
                    "node": record.node,
                    "error_type": type(exc).__name__,
                    "message": str(exc),
                },
                seq,
            )
    _emit_event(
        ctx,
        "step",
        {
            "node": record.node,
            "outcome": _classify_outcome(record.outcome),
            "preview": (record.output_preview or "")[:280],
        },
        seq,
    )
    _persist(cxdb, ctx, seq, record, output, metadata)
    ctx.last_completed_seq = seq
    _write_heartbeat(ctx)
    return seq + 1


def _persist(
    cxdb: Optional[CXDB],
    ctx: Context,
    seq: int,
    record: StepRecord,
    output: str,
    metadata: Optional[dict[str, str]] = None,
) -> None:
    if cxdb is None or ctx.run_id is None:
        return
    try:
        cxdb.record_step(
            run_id=ctx.run_id,
            seq=seq,
            node=record.node,
            outcome=record.outcome,
            ts=record.ts,
            output=output,
            metadata=metadata or record.metadata,
        )
    except Exception as exc:
        _emit_event(
            ctx,
            "persistence_error",
            {
                "node": record.node,
                "error_type": type(exc).__name__,
                "message": str(exc),
            },
            seq,
        )
