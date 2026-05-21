"""Pipeline engine — traverse the graph and execute handlers."""

from __future__ import annotations

import json
import pathlib
import time
from dataclasses import asdict, dataclass, field
from typing import Optional

from .cxdb import CXDB
from .handlers import Context, Result, resolve
from .parser import Edge, Graph, Node

_VALIDATION_TYPES = {"holdout_eval", "gate_es", "gate_er", "gate_code_standards"}


@dataclass
class StepRecord:
    node: str
    outcome: str
    ts: float
    output_preview: str
    metadata: dict[str, str] = field(default_factory=dict)


def _edge_matches(edge: Edge, last: Result) -> bool:
    cond = edge.condition
    if not cond:
        return True
    # Support `key=value` and `key!=value`.
    if "!=" in cond:
        k, v = cond.split("!=", 1)
        return _lookup(k.strip(), last) != v.strip()
    if "=" in cond:
        k, v = cond.split("=", 1)
        return _lookup(k.strip(), last) == v.strip()
    return False


def _lookup(key: str, last: Result) -> str:
    if key == "outcome":
        return last.outcome
    return last.metadata.get(key, "")


def _attr_int(node: Node, key: str, default: int) -> int:
    raw = node.attrs.get(key)
    if raw is None or raw == "":
        return default
    try:
        return int(raw)
    except (TypeError, ValueError):
        return default


def run(
    graph: Graph,
    ctx: Context,
    checkpoint: Optional[pathlib.Path] = None,
    max_steps: int = 100,
) -> list[StepRecord]:
    """Execute the graph starting at 'start' until 'exit' or max_steps."""
    history: list[StepRecord] = []
    visits: dict[str, int] = {}
    current = graph.nodes["start"]
    seq = 0  # CXDB sequence — independent of history length so refactors can't desync.

    cxdb: Optional[CXDB] = None
    if ctx.cxdb_path is not None:
        cxdb = CXDB(ctx.cxdb_path)
        ctx.run_id = cxdb.start_run(pipeline=graph.name, goal=ctx.goal)

    try:
        while True:
            visits[current.name] = visits.get(current.name, 0) + 1
            max_visits = _attr_int(current, "max_visits", 0)
            if max_visits and visits[current.name] > max_visits:
                record = StepRecord(
                    node=current.name,
                    outcome="exhausted",
                    ts=time.time(),
                    output_preview=f"max_visits={max_visits} exceeded",
                )
                seq = _append_record(history, checkpoint, cxdb, ctx, seq, record, "")
                break

            handler = resolve(current)
            result = handler(current, ctx)
            record = StepRecord(
                node=current.name,
                outcome=result.outcome,
                ts=time.time(),
                output_preview=result.output[:280],
                metadata=result.metadata,
            )
            _update_failure_state(current, ctx, result)
            ctx.history.append({"node": current.name, "outcome": result.outcome})
            seq = _append_record(
                history, checkpoint, cxdb, ctx, seq, record, result.output, result.metadata
            )

            if current.name == "exit":
                break

            if len(history) >= max_steps:
                record = StepRecord(
                    node=current.name,
                    outcome="exhausted",
                    ts=time.time(),
                    output_preview=f"max_steps={max_steps} reached before exit",
                )
                seq = _append_record(history, checkpoint, cxdb, ctx, seq, record, "")
                break

            next_node = _pick_next(graph, current, result)
            if next_node is None:
                record = StepRecord(
                    node=current.name,
                    outcome="stuck",
                    ts=time.time(),
                    output_preview="no matching outgoing edge",
                )
                seq = _append_record(
                    history, checkpoint, cxdb, ctx, seq, record, "no matching outgoing edge"
                )
                break
            current = next_node
    finally:
        if cxdb is not None and ctx.run_id is not None:
            cxdb.end_run(
                ctx.run_id,
                final=history[-1].outcome if history else "empty",
            )
            cxdb.close()

    return history


def _update_failure_state(node: Node, ctx: Context, result: Result) -> None:
    if result.outcome != "success":
        ctx.state["_unresolved_failure"] = result.outcome
        return
    if node.attrs.get("type") in _VALIDATION_TYPES:
        ctx.state.pop("_unresolved_failure", None)


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
        checkpoint.write_text(json.dumps([asdict(r) for r in history], indent=2))
    _persist(cxdb, ctx, seq, record, output, metadata)
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
    cxdb.record_step(
        run_id=ctx.run_id,
        seq=seq,
        node=record.node,
        outcome=record.outcome,
        ts=record.ts,
        output=output,
        metadata=metadata or record.metadata,
    )


def _pick_next(graph: Graph, current: Node, last: Result) -> Optional[Node]:
    candidates = graph.outgoing(current.name)
    # Prefer edges with a matching explicit condition; fall back to unconditional ones.
    matching = [e for e in candidates if e.condition and _edge_matches(e, last)]
    if matching:
        return graph.nodes.get(matching[0].dst)
    unconditional = [e for e in candidates if not e.condition]
    if unconditional:
        return graph.nodes.get(unconditional[0].dst)
    return None
