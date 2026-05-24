"""Pipeline engine — traverse the graph and execute handlers."""

from __future__ import annotations

import json
import pathlib
import time
from dataclasses import asdict, dataclass, field
from typing import Optional

from .cxdb import CXDB
from .handlers import Context, Result, resolve
from .parser import Edge, Graph, Node, is_exit_node, is_start_node

_VALIDATION_TYPES = {"holdout_eval", "gate_es", "gate_er", "gate_code_standards"}


def _is_validation_node(node: Node) -> bool:
    """A node counts as a validator (clearing prior `_unresolved_failure` on
    success) if either:
      - its `type` is in the canonical validation set, or
      - it opts in via `validation="true"` (case-insensitive) on the DOT node.

    The opt-in path lets pipelines treat a generic `tool` node (e.g. pytest)
    as the verifier in a `verify -> fix -> verify -> exit` loop without
    inventing a new handler type.
    """
    if node.attrs.get("type") in _VALIDATION_TYPES:
        return True
    flag = node.attrs.get("validation", False)
    if isinstance(flag, bool):
        return flag
    flag = str(flag).strip().lower()
    return flag in {"true", "1", "yes"}


@dataclass
class StepRecord:
    node: str
    outcome: str
    ts: float
    output_preview: str
    metadata: dict[str, str] = field(default_factory=dict)

def _is_decision_node(node: Optional[Node]) -> bool:
    if node is None:
        return False
    return node.shape == "hexagon" or node.attrs.get("type") == "conditional"


def _edge_matches(
    edge: Edge,
    last: Result,
    ctx: Optional[Context] = None,
    current: Optional[Node] = None,
) -> bool:
    cond = edge.condition
    if not cond:
        return True
    is_decision = _is_decision_node(current)
    # Support `key=value` and `key!=value`.
    if "!=" in cond:
        k, v = cond.split("!=", 1)
        return _lookup(k.strip(), last, ctx, is_decision) != v.strip()
    if "=" in cond:
        k, v = cond.split("=", 1)
        return _lookup(k.strip(), last, ctx, is_decision) == v.strip()
    return False


def _lookup(
    key: str,
    last: Result,
    ctx: Optional[Context] = None,
    is_decision: bool = False,
) -> str:
    if is_decision and ctx is not None:
        if key in ctx.state:
            return str(ctx.state[key])
    if key == "outcome":
        return last.outcome
    return str(last.metadata.get(key, ""))


def _attr_int(node: Node, key: str, default: int) -> int:
    raw = node.attrs.get(key)
    if raw is None or raw == "":
        return default
    if isinstance(raw, bool):
        return default
    if isinstance(raw, int):
        return raw
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
    current = _start_node(graph)
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
            results = _run_with_retries(handler, current, ctx, graph)
            result = results[-1]
            for attempt_result in results:
                ctx.state.update(attempt_result.context_updates)
                record = StepRecord(
                    node=current.name,
                    outcome=attempt_result.outcome,
                    ts=time.time(),
                    output_preview=attempt_result.output[:280],
                    metadata=attempt_result.metadata,
                )
                _update_failure_state(current, ctx, attempt_result)
                ctx.history.append({"node": current.name, "outcome": attempt_result.outcome})
                seq = _append_record(
                    history,
                    checkpoint,
                    cxdb,
                    ctx,
                    seq,
                    record,
                    attempt_result.output,
                    attempt_result.metadata,
                )

            if is_exit_node(current):
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

            gate_target = _goal_gate_target(graph, current, result, ctx)
            next_node = gate_target or _pick_next(graph, current, result, ctx)
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


def _start_node(graph: Graph) -> Node:
    if "start" in graph.nodes:
        return graph.nodes["start"]
    for node in graph.nodes.values():
        if is_start_node(node):
            return node
    raise ValueError("graph has no start node")


def _run_with_retries(handler, node: Node, ctx: Context, graph: Graph) -> list[Result]:
    results: list[Result] = []
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


def _node_max_retries(node: Node, graph: Graph) -> int:
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


def _successful_for_node(node: Node, result: Result) -> bool:
    if result.outcome == "success":
        return True
    allow_partial = node.attrs.get("allow_partial", False)
    if isinstance(allow_partial, bool):
        partial_allowed = allow_partial
    else:
        partial_allowed = str(allow_partial).strip().lower() in {"true", "1", "yes"}
    return partial_allowed and result.outcome in {"partial", "partial_success"}


def _goal_gate_target(
    graph: Graph,
    node: Node,
    result: Result,
    ctx: Optional[Context] = None,
) -> Optional[Node]:
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


def _update_failure_state(node: Node, ctx: Context, result: Result) -> None:
    if result.outcome != "success":
        ctx.state["_unresolved_failure"] = result.outcome
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


def _pick_next(
    graph: Graph,
    current: Node,
    last: Result,
    ctx: Optional[Context] = None,
) -> Optional[Node]:
    candidates = graph.outgoing(current.name)
    # Prefer edges with a matching explicit condition; fall back to unconditional ones.
    matching = [e for e in candidates if e.condition and _edge_matches(e, last, ctx, current)]
    if matching:
        return graph.nodes.get(_choose_edge(matching, last).dst)
    unconditional = [e for e in candidates if not e.condition]
    if unconditional:
        return graph.nodes.get(_choose_edge(unconditional, last).dst)
    return None


def _choose_edge(edges: list[Edge], last: Result) -> Edge:
    preferred_label = last.preferred_label or last.metadata.get("preferred_label", "")
    suggested_next_ids = list(last.suggested_next_ids)
    suggested_meta = last.metadata.get("suggested_next_ids") or last.metadata.get("suggested_next") or ""
    if suggested_meta:
        suggested_next_ids.extend(
            item.strip() for item in str(suggested_meta).split(",") if item.strip()
        )
    suggested_rank = {node_id: idx for idx, node_id in enumerate(suggested_next_ids)}

    def rank(edge: Edge) -> tuple[int, int, int, str, str]:
        return (
            0 if preferred_label and _normalize_label(edge.label or "") == _normalize_label(preferred_label) else 1,
            suggested_rank.get(edge.dst, len(suggested_rank)),
            -_edge_weight(edge),
            edge.dst,
            edge.label or "",
        )

    return sorted(edges, key=rank)[0]


def _normalize_label(label: str) -> str:
    text = label.strip().lower()
    if text.startswith("[") and "]" in text:
        text = text.split("]", 1)[1].strip()
    if text.startswith("(") and ")" in text:
        text = text.split(")", 1)[1].strip()
    if len(text) > 2 and text[1] in {")", ".", "-", ":"}:
        text = text[2:].strip()
    return " ".join(text.split())


def _edge_weight(edge: Edge) -> int:
    raw = edge.attrs.get("weight", 0)
    if isinstance(raw, bool):
        return 0
    if isinstance(raw, int):
        return raw
    try:
        return int(raw)
    except (TypeError, ValueError):
        return 0
