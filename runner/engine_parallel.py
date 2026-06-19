"""Parallel fan-out / fan-in primitives for the engine.

Splits out from `runner/engine.py` (see `docs/refactor/file-ownership-map.engine.md`).
Owns the parallel node-type predicates, join discovery, join-policy outcome,
branch filtering, and the worker function that runs a single branch until it
reaches the join barrier.
"""

from __future__ import annotations

import pathlib
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Optional, TYPE_CHECKING

from . import engine_branches as _branches
from . import engine_edges as _edges
from . import engine_observability as _obs
from ._classify import _classify_outcome
from .cxdb import CXDB
from .handlers import Context, Result
from .parser import Edge, Graph, Node, is_exit_node

if TYPE_CHECKING:
    from .engine_persist import StepRecord


def _parallel_join_edges(edges: list[Edge]) -> list[Edge]:
    join: list[Edge] = []
    for edge in edges:
        join_value = edge.attrs.get("join", "")
        if isinstance(join_value, bool):
            if join_value:
                join.append(edge)
            continue
        if str(join_value).strip().lower() in {"true", "1", "yes"}:
            join.append(edge)
    return join


def _parallel_branches(
    graph: Graph,
    current: Node,
    result: Result,
    ctx: Optional[Context],
) -> list[Edge]:
    branches: list[Edge] = []
    for edge in graph.outgoing(current.name):
        if not _edges._edge_matches(edge, result, ctx, current):
            continue
        if edge in _parallel_join_edges(graph.outgoing(current.name)):
            continue
        branches.append(edge)
    return branches


def _parallel_join_outcome(
    current: Node,
    branch_outcomes: list[Result],
    allow_partial: bool,
) -> str:
    success_count, partial_count, failure_count = _obs._classify_records(branch_outcomes)
    if failure_count > 0:
        return _obs._normalize_outcome_only("failure")
    effective_success = success_count + (partial_count if allow_partial else 0)
    if not branch_outcomes:
        return _obs._normalize_outcome_only("success")
    quorum = _edges._attr_int(current, "join_quorum", len(branch_outcomes))
    if quorum <= 0:
        quorum = len(branch_outcomes)
    return _obs._normalize_outcome_only("success" if effective_success >= quorum else "failure")


def _is_parallel_node(node: Node) -> bool:
    """Fan-out node: type='parallel' or (no explicit type and shape='component').

    When an explicit 'type' attribute is present, only the type is checked —
    same priority rule as resolve().  This prevents nodes like type='codergen'
    shape='component' from triggering the parallel block.
    """
    explicit_type = node.attrs.get("type")
    if explicit_type:
        return explicit_type == "parallel"
    return node.shape == "component"


def _is_join_node(node: Node) -> bool:
    """Fan-in barrier: type='join' or (no explicit type and shape='tripleoctagon').

    When an explicit 'type' attribute is present, only the type is checked —
    same priority rule as resolve().
    """
    explicit_type = node.attrs.get("type")
    if explicit_type:
        return explicit_type == "join"
    return node.shape == "tripleoctagon"


def _find_join_node(graph: Graph, fanout: Node) -> Optional[Node]:
    """BFS from fanout's direct successors to find the nearest join node.

    Uses graph structure only (ignores edge conditions), so branches that are
    filtered out at runtime by _edge_matches do not affect discovery. For
    well-formed pipelines all active branches converge on the same join; truly
    divergent topologies (branches wired to different joins) are not supported.
    """
    visited: set[str] = set()
    queue: list[Node] = []
    for edge in graph.outgoing(fanout.name):
        n = graph.nodes.get(edge.dst)
        if n is not None:
            queue.append(n)
    while queue:
        node = queue.pop(0)
        if node.name in visited:
            continue
        visited.add(node.name)
        if _is_join_node(node):
            return node
        for edge in graph.outgoing(node.name):
            n = graph.nodes.get(edge.dst)
            if n is not None and n.name not in visited:
                queue.append(n)
    return None


def _apply_join_policy(join_node: Node, results: list[Result]) -> str:
    """Compute join outcome from branch results using join_node's policy attribute."""
    if not results:
        policy = str(join_node.attrs.get("policy", "wait_all")).strip().lower()
        return "failure" if policy in ("first_success", "k_of_n") else "success"
    policy = str(join_node.attrs.get("policy", "wait_all")).strip().lower()
    successes = sum(1 for r in results if _obs._is_success_result(r.outcome))
    n = len(results)
    if policy == "first_success":
        return "success" if successes >= 1 else "failure"
    if policy == "k_of_n":
        k = _edges._attr_int(join_node, "k", n)
        if k < 1 or k > n:
            return "failure"
        return "success" if successes >= k else "failure"
    # Default: wait_all
    return "success" if successes == n else "failure"


def _run_branch_until_join(
    graph: Graph,
    start: Node,
    ctx: Context,
    join_node: Node,
    seq_ref: list[int],
    seq_lock: threading.Lock,
    cxdb_path: Optional[pathlib.Path],
    max_branch_steps: int = 50,
) -> tuple[list, Result]:
    """Execute a single parallel branch from start until join_node (exclusive).

    Opens its own CXDB connection (SQLite objects are not thread-safe) and
    persists branch steps with a thread-safe monotonic seq.
    Respects max_visits per node and max_branch_steps to prevent runaway loops.
    Returns (branch_records, last_result).
    """
    # Local imports to break the engine_run <-> engine_parallel cycle.
    from . import engine_run as _engine_run
    from . import engine_persist as _persist

    branch_records: list = []
    current: Optional[Node] = start
    last_result = Result(outcome="success")
    visits: dict[str, int] = {}
    steps = 0
    thread_cxdb: Optional[CXDB] = CXDB(cxdb_path) if cxdb_path is not None else None
    try:
        while current is not None and current.name != join_node.name:
            if is_exit_node(current):
                last_result = Result(
                    outcome="failure",
                    output=f"branch reached exit before join '{join_node.name}'",
                )
                break
            if steps >= max_branch_steps:
                last_result = Result(
                    outcome="exhausted",
                    output=f"branch max_branch_steps={max_branch_steps} reached at {current.name}",
                )
                break
            visits[current.name] = visits.get(current.name, 0) + 1
            max_visits = _edges._attr_int(current, "max_visits", 0)
            if max_visits and visits[current.name] > max_visits:
                last_result = Result(
                    outcome="exhausted",
                    output=f"branch max_visits={max_visits} exceeded at {current.name}",
                )
                break
            with seq_lock:
                start_seq = seq_ref[0]
            _obs._emit_event(
                ctx,
                "node_start",
                {
                    "node": current.name,
                    "attempt": "1",
                    "parallel": "true",
                },
                start_seq,
            )

            results, records = _engine_run._run_single_node(current, ctx, graph)
            steps += 1
            step_result = results[-1] if results else Result(outcome="success")
            # Preserve first failure: don't let a mid-branch join's success mask it.
            if not _obs._is_success_result(last_result.outcome):
                pass
            else:
                last_result = step_result

            for i, attempt in enumerate(results):
                record = records[i]
                record.metadata = {**record.metadata, "_branch_overhead": "true"}
                with seq_lock:
                    local_seq = seq_ref[0]
                    seq_ref[0] += 1
                    ctx.last_completed_seq = local_seq

                if i > 0:
                    _obs._emit_event(
                        ctx,
                        "retry",
                        {
                            "node": current.name,
                            "attempt": str(i + 1),
                            "max_retries": record.metadata.get("max_retries", "0"),
                            "previous_outcome": results[i - 1].outcome,
                            "parallel": "true",
                        },
                        local_seq,
                    )
                    _obs._emit_event(
                        ctx,
                        "node_start",
                        {
                            "node": current.name,
                            "attempt": str(i + 1),
                            "parallel": "true",
                        },
                        local_seq,
                    )

                transcript_path, transcript_sha256 = _obs._write_transcript_sidecar(
                    ctx, local_seq, current.name, i + 1, attempt.output
                )

                payload = {
                    "node": current.name,
                    "outcome": _classify_outcome(record.outcome),
                    "attempt": str(i + 1),
                    "max_retries": record.metadata.get("max_retries", "0"),
                    "parallel": "true",
                }
                if transcript_path:
                    payload["transcript_path"] = transcript_path
                    payload["transcript_sha256"] = transcript_sha256

                _obs._emit_event(ctx, "node_result", payload, local_seq)
                _persist._persist(thread_cxdb, ctx, local_seq, record, attempt.output, record.metadata)
                branch_records.append(record)
            # Route using current step's actual result, not the preserved first failure.
            # last_result still tracks the first failure for branch outcome attribution.
            current = _edges._pick_next(graph, current, step_result, ctx)
    finally:
        if thread_cxdb is not None:
            thread_cxdb.close()
    # Detect stuck branch: _pick_next returned None before reaching join
    if current is None:
        last_result = Result(
            outcome="failure",
            output=f"branch stuck: no successor before join '{join_node.name}'",
        )
    return branch_records, last_result
