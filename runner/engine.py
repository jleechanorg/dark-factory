"""Pipeline engine — traverse the graph and execute handlers."""

from __future__ import annotations

import json
import pathlib
import sys
import tempfile
import threading
import time
import traceback
import uuid
import warnings
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import asdict, dataclass, field
from typing import Optional, TextIO

from .cxdb import CXDB
from .handlers import Context, Result, resolve
from .parser import Edge, Graph, Node, is_exit_node, is_start_node
from . import perf_log

_VALIDATION_TYPES = {"holdout_eval", "gate_es", "gate_er", "gate_code_standards", "gate_evidence_review"}

# Per-run runner logs land here so a crash always leaves a diagnosable
# traceback on disk even when no CXDB is attached. Monkeypatchable in tests.
_LOG_DIR = pathlib.Path.home() / ".dark-factory" / "logs"
_EVENT_DIR = pathlib.Path.home() / ".dark-factory" / "events"


def _emit_event(ctx: Context, event: str, payload: dict[str, str], seq: int | None = None) -> None:
    """Append a structured JSONL event for this run.

    Event logging is best-effort and never fails the run. Invalid event writes are
    surfaced to stderr so observability is not fully silent.
    """
    event_path = getattr(ctx, "event_log_path", None)
    if event_path is None:
        return
    payload_text = str(event)
    try:
        path = pathlib.Path(event_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        record = {
            "ts": time.time(),
            "run_id": ctx.run_id,
            "event": event,
        }
        if seq is not None:
            record["seq"] = seq
        record.update(payload)
        payload_text = json.dumps(record, sort_keys=True)
        path.open("a", encoding="utf-8").write(payload_text + "\n")
    except Exception as exc:
        try:
            print(f"[runner:emit_event:{type(exc).__name__}] {payload_text}", file=sys.stderr, flush=True)
        except Exception:
            pass


def _open_run_log(run_id: str) -> Optional[TextIO]:
    """Open ~/.dark-factory/logs/<run_id>.log for append-mode tee logging.

    Failures to open the log must never take down a run, so this swallows
    filesystem errors and returns None (logging becomes a no-op).
    """
    try:
        _LOG_DIR.mkdir(parents=True, exist_ok=True)
        return (_LOG_DIR / f"{run_id}.log").open("a", encoding="utf-8")
    except OSError:
        return None


def _log(handle: Optional[TextIO], message: str) -> None:
    if handle is None:
        return
    try:
        handle.write(f"[{time.time():.3f}] {message}\n")
        handle.flush()
    except (OSError, ValueError):
        pass


def _classify_outcome(raw: str) -> str:
    """Normalize diverse outcomes to a small engine outcome taxonomy.

    Notes:
      - `pass` and `warn` are treated as success for routing.
      - `partial` is preserved as partial so caller can opt into
        `allow_partial` semantics.
      - Any non-empty unknown token collapses to failure.
    """
    value = str(raw).strip().lower()
    if value in {"success", "pass", "warn"}:
        return "success"
    if value in {"partial", "partial_success"}:
        return "partial"
    if value == "error":
        return "error"
    if value in {"failure", "fail", "exhausted", "stuck", "inconclusive"}:
        return "failure"
    return "failure"


def _is_success_result(outcome: str) -> bool:
    return _classify_outcome(outcome) == "success"


def _is_partial_result(outcome: str, allow_partial: bool) -> bool:
    return allow_partial and _classify_outcome(outcome) == "partial"


def _is_validation_failed(outcome: str) -> bool:
    return _classify_outcome(outcome) in {"failure", "error", "partial"}


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


def _node_backend(node: Node, ctx: Context) -> str:
    backend = node.attrs.get("backend")
    if backend:
        return str(backend)
    return ctx.backend


def _node_type(node: Node) -> str:
    node_type = node.attrs.get("type")
    if node_type:
        return str(node_type)
    if is_start_node(node):
        return "start"
    if is_exit_node(node):
        return "exit"
    if node.shape == "hexagon":
        return "conditional"
    return "codergen"


def _outcome_counts(history: list[StepRecord]) -> tuple[int, int, int]:
    success = failure = error = 0
    for record in history:
        classified = _classify_outcome(record.outcome)
        if classified == "success":
            success += 1
        elif classified == "error":
            error += 1
        else:
            failure += 1
    return success, failure, error


def _perf_node_enter(ctx: Context, node: Node, seq: int, visit: int) -> None:
    perf_log.node_enter(
        getattr(ctx, "perf_run", None),
        node=node.name,
        seq=seq,
        node_type=_node_type(node),
        visit=visit,
        backend=_node_backend(node, ctx),
    )


def _perf_node_exit(
    ctx: Context,
    node: str,
    seq: int,
    raw_outcome: str,
    visit: int,
    metadata: Optional[dict[str, str]] = None,
) -> None:
    perf_log.node_exit(
        getattr(ctx, "perf_run", None),
        node=node,
        seq=seq,
        raw_outcome=raw_outcome,
        visit=visit,
        metadata=metadata,
    )


def _clone_context(ctx: Context) -> Context:
    return Context(
        goal=ctx.goal,
        workdir=ctx.workdir,
        state=dict(ctx.state),
        history=[dict(entry) for entry in ctx.history],
        backend=ctx.backend,
        cxdb_path=ctx.cxdb_path,
        run_id=ctx.run_id,
        event_log_path=getattr(ctx, "event_log_path", None),
        perf_log_root=getattr(ctx, "perf_log_root", None),
        git_ctx=getattr(ctx, "git_ctx", None),
        perf_run=getattr(ctx, "perf_run", None),
    )


def _branch_context(ctx: Context, branch_name: str) -> Context:
    """Clone ctx and assign a unique per-branch workdir subdirectory.

    File-writing backends (claude, codex, agy) spawn subprocesses with
    cwd=ctx.workdir; without isolation concurrent branches would race on
    shared files.  Each branch gets its own tempdir under the parent workdir
    so their file operations are independent.  If the parent workdir is not a
    valid directory (e.g. None or a non-existent path) fall back to a
    system-managed tmp dir.
    """
    cloned = _clone_context(ctx)
    parent = pathlib.Path(ctx.workdir) if ctx.workdir else None
    try:
        base = parent if (parent and parent.is_dir()) else None
        branch_dir = pathlib.Path(tempfile.mkdtemp(prefix=f"branch_{branch_name}_", dir=base))
        cloned.workdir = branch_dir
    except OSError as exc:
        warnings.warn(
            f"_branch_context: mkdtemp failed for '{branch_name}', branch isolation disabled: {exc}",
            RuntimeWarning,
            stacklevel=2,
        )
    return cloned


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

def _is_decision_node(node: Optional[Node]) -> bool:
    if node is None:
        return False
    return node.shape == "hexagon" or node.attrs.get("type") == "conditional"


def _evaluate_expression(
    cond: str,
    last: Result,
    ctx: Optional[Context],
    is_decision: bool,
) -> bool:
    cond = cond.strip()
    if not cond:
        return True

    import re

    token_specification = [
        ('PAREN', r'[()]'),
        ('AND', r'&&|and\b'),
        ('OR', r'\|\||or\b'),
        ('NOT_CONTAINS', r'not contains\b'),
        ('CONTAINS', r'contains\b'),
        ('NOT_IN', r'not in\b'),
        ('IN', r'in\b'),
        ('NEQ', r'!='),
        ('EQ', r'==|='),
        ('NOT', r'!|not\b(?=\s|\(|\)|$)'),
        ('STRING', r'"[^"]*"|\'[^\']*\''),
        ('WORD', r'[a-zA-Z_0-9\-\.\*]+'),
        ('SPACE', r'\s+'),
    ]
    tok_regex = '|'.join(f'(?P<{name}>{pattern})' for name, pattern in token_specification)

    token_re = re.compile(tok_regex)
    tokens = []
    pos = 0
    for mo in token_re.finditer(cond):
        if mo.start() != pos:
            return False
        pos = mo.end()
        kind = mo.lastgroup
        value = mo.group()
        if kind == 'SPACE':
            continue
        elif kind == 'STRING':
            value = value[1:-1]
            tokens.append((kind, value))
        else:
            tokens.append((kind, value))

    if not tokens:
        return False
    if pos != len(cond):
        return False

    idx = 0

    def peek() -> Optional[tuple[str, str]]:
        if idx < len(tokens):
            return tokens[idx]
        return None

    def consume(expected_kind: Optional[str] = None) -> tuple[str, str]:
        nonlocal idx
        if idx >= len(tokens):
            raise ValueError("Unexpected end of expression")
        tok = tokens[idx]
        if expected_kind and tok[0] != expected_kind:
            raise ValueError(f"Expected {expected_kind}, got {tok[0]}")
        idx += 1
        return tok

    def parse_expression() -> bool:
        left = parse_term()
        while True:
            t = peek()
            if t and t[0] == 'OR':
                consume()
                right = parse_term()
                left = left or right
            else:
                break
        return left

    def parse_term() -> bool:
        left = parse_factor()
        while True:
            t = peek()
            if t and t[0] == 'AND':
                consume()
                right = parse_factor()
                left = left and right
            else:
                break
        return left

    def parse_factor() -> bool:
        t = peek()
        if t and t[0] == 'NOT':
            consume()
            return not parse_factor()
        if t and t[0] == 'PAREN' and t[1] == '(':
            consume()
            val = parse_expression()
            consume('PAREN')
            return val

        key_tok = consume('WORD')
        k_val = key_tok[1]
        if k_val and not (k_val[0].isalpha() or k_val[0] == '_'):
            raise ValueError(f"Invalid key name: {k_val!r}")
        op_tok = peek()
        if not op_tok:
            outcome = _lookup("outcome", last, ctx, is_decision)
            return outcome == k_val

        op_kind = op_tok[0]
        if op_kind not in {'EQ', 'NEQ', 'CONTAINS', 'NOT_CONTAINS', 'IN', 'NOT_IN'}:
            outcome = _lookup("outcome", last, ctx, is_decision)
            return outcome == k_val

        consume()
        val_tok = consume()

        k = k_val
        v = val_tok[1]
        actual_val = _lookup(k, last, ctx, is_decision)

        if op_kind == 'EQ':
            return actual_val == v
        elif op_kind == 'NEQ':
            return actual_val != v
        elif op_kind == 'CONTAINS':
            return v in actual_val
        elif op_kind == 'NOT_CONTAINS':
            return v not in actual_val
        elif op_kind == 'IN':
            parts = [p.strip() for p in v.split(",")]
            return actual_val in parts
        elif op_kind == 'NOT_IN':
            parts = [p.strip() for p in v.split(",")]
            return actual_val not in parts

        return False

    try:
        res = parse_expression()
        if idx < len(tokens):
            return False
        return res
    except Exception:
        if "!=" in cond:
            k, v = cond.split("!=", 1)
            return _lookup(k.strip(), last, ctx, is_decision) != v.strip()
        if "=" in cond:
            k, v = cond.split("=", 1)
            return _lookup(k.strip(), last, ctx, is_decision) == v.strip()
        return False


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
    return _evaluate_expression(cond, last, ctx, is_decision)


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


def _normalize_outcome_only(value: str) -> str:
    return _classify_outcome(value)


def _classify_records(records: list[Result]) -> tuple[int, int, int]:
    success = 0
    partial = 0
    failure = 0
    for record in records:
        outcome = _normalize_outcome_only(record.outcome)
        if outcome == "success":
            success += 1
        elif outcome == "partial":
            partial += 1
        else:
            failure += 1
    return success, partial, failure


def _normalized_result(result: Result) -> Result:
    outcome = _normalize_outcome_only(result.outcome)
    if outcome == result.outcome:
        return result
    result.outcome = outcome
    return result


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
        if not _edge_matches(edge, result, ctx, current):
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
    success_count, partial_count, failure_count = _classify_records(branch_outcomes)
    if failure_count > 0:
        return _normalize_outcome_only("failure")
    effective_success = success_count + (partial_count if allow_partial else 0)
    if not branch_outcomes:
        return _normalize_outcome_only("success")
    quorum = _attr_int(current, "join_quorum", len(branch_outcomes))
    if quorum <= 0:
        quorum = len(branch_outcomes)
    return _normalize_outcome_only("success" if effective_success >= quorum else "failure")


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
    successes = sum(1 for r in results if _is_success_result(r.outcome))
    n = len(results)
    if policy == "first_success":
        return "success" if successes >= 1 else "failure"
    if policy == "k_of_n":
        k = _attr_int(join_node, "k", n)
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
) -> tuple[list[StepRecord], Result]:
    """Execute a single parallel branch from start until join_node (exclusive).

    Opens its own CXDB connection (SQLite objects are not thread-safe) and
    persists branch steps with a thread-safe monotonic seq.
    Respects max_visits per node and max_branch_steps to prevent runaway loops.
    Returns (branch_records, last_result).
    """
    branch_records: list[StepRecord] = []
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
            max_visits = _attr_int(current, "max_visits", 0)
            if max_visits and visits[current.name] > max_visits:
                last_result = Result(
                    outcome="exhausted",
                    output=f"branch max_visits={max_visits} exceeded at {current.name}",
                )
                break
            results, records = _run_single_node(current, ctx, graph)
            steps += 1
            step_result = results[-1] if results else Result(outcome="success")
            # Preserve first failure: don't let a mid-branch join's success mask it.
            if not _is_success_result(last_result.outcome):
                pass
            else:
                last_result = step_result
            for i, attempt in enumerate(results):
                record = records[i]
                record.metadata = {**record.metadata, "_branch_overhead": "true"}
                with seq_lock:
                    local_seq = seq_ref[0]
                    seq_ref[0] += 1
                _persist(thread_cxdb, ctx, local_seq, record, attempt.output, record.metadata)
                branch_records.append(record)
            # Route using current step's actual result, not the preserved first failure.
            # last_result still tracks the first failure for branch outcome attribution.
            current = _pick_next(graph, current, step_result, ctx)
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


def _run_single_node(
    node: Node,
    ctx: Context,
    graph: Graph,
) -> tuple[list[Result], list[StepRecord]]:
    handler = resolve(node)
    results = _run_with_retries(handler, node, ctx, graph)
    normalized_results: list[Result] = []
    records: list[StepRecord] = []
    for attempt in results:
        attempt = _normalized_result(attempt)
        ctx.state.update(attempt.context_updates)
        ctx.state["_last_node"] = node.name
        ctx.state["_last_outcome"] = attempt.outcome
        ctx.state["_last_output"] = attempt.output[:4000]
        normalized_results.append(attempt)
        records.append(
            StepRecord(
                node=node.name,
                outcome=attempt.outcome,
                ts=time.time(),
                output_preview=attempt.output[:280],
                metadata=attempt.metadata,
            )
        )
        _update_failure_state(node, ctx, attempt)
        ctx.history.append({"node": node.name, "outcome": attempt.outcome})
    return normalized_results, records


def _allow_partial(node: Node) -> bool:
    allow_partial = node.attrs.get("allow_partial", False)
    if isinstance(allow_partial, bool):
        return allow_partial
    return str(allow_partial).strip().lower() in {"true", "1", "yes"}


def _pick_next_from_edges(
    graph: Graph,
    current: Node,
    edges: list[Edge],
    last: Result,
    ctx: Optional[Context] = None,
) -> Optional[Node]:
    # Keep behavior consistent with _pick_next() while allowing explicit edges.
    if not edges:
        return None
    matching = [edge for edge in edges if edge.condition and _edge_matches(edge, last, ctx, current)]
    if matching:
        selected = _choose_edge(matching, last)
        return graph.nodes.get(selected.dst)
    unconditional = [edge for edge in edges if not edge.condition]
    if unconditional:
        selected = _choose_edge(unconditional, last)
        return graph.nodes.get(selected.dst)
    return None


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


def _handle_node_exception(
    graph: Graph,
    current: Node,
    ctx: Context,
    exc: Exception,
    history: list[StepRecord],
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
    _log(log, f"node {current.name!r} raised {type(exc).__name__}: {exc}\n{tb_text}")

    summary_frame = tb_text.strip().splitlines()[-1] if tb_text.strip() else ""
    preview = f"{type(exc).__name__}: {exc} | {summary_frame}"

    error_result = _normalized_result(
        Result(
            outcome="error",
            output=tb_text,
            metadata={"exception": type(exc).__name__},
        )
    )
    ctx.state["_last_node"] = current.name
    ctx.state["_last_outcome"] = "error"
    ctx.state["_last_output"] = tb_text[:4000]
    ctx.history.append({"node": current.name, "outcome": "error"})

    record = StepRecord(
        node=current.name,
        outcome="error",
        ts=time.time(),
        output_preview=preview[:280],
        metadata={"exception": type(exc).__name__},
    )
    _append_record(history, checkpoint, cxdb, ctx, seq, record, tb_text, record.metadata)
    if not skip_perf_exit:
        _perf_node_exit(
            ctx,
            current.name,
            seq,
            "error",
            visits.get(current.name, 1),
            record.metadata,
        )
    _emit_event(
        ctx,
        "node_exception",
        {
            "node": current.name,
            "error_type": type(exc).__name__,
            "message": str(exc),
        },
        seq,
    )
    _update_failure_state(current, ctx, error_result)

    # A crash is a failure: only a deliberate recovery edge may catch it. That
    # means a goal_gate `retry_target` or a *conditional* edge that matches the
    # error outcome (e.g. `outcome!=success` / `outcome=error`). Unconditional
    # forward edges are NOT recovery — following them would silently carry a
    # crashed run onward and let it reach `exit` as success. With no recovery
    # edge, the run ends here on the recorded `error` step.
    gate_target = _goal_gate_target(graph, current, error_result, ctx)
    if gate_target is not None:
        _log(log, f"routing crashed node {current.name!r} -> {gate_target.name!r} (goal_gate)")
        return gate_target
    matching = [
        edge
        for edge in graph.outgoing(current.name)
        if edge.condition and _edge_matches(edge, error_result, ctx, current)
    ]
    if matching:
        selected = _choose_edge(matching, error_result)
        next_node = graph.nodes.get(selected.dst)
        _log(log, f"routing crashed node {current.name!r} -> {selected.dst!r} (fix edge)")
        return next_node
    _log(log, f"no recovery edge for crashed node {current.name!r}; ending run")
    return None


def run(
    graph: Graph,
    ctx: Context,
    checkpoint: Optional[pathlib.Path] = None,
    resume: Optional[pathlib.Path] = None,
    max_steps: int = 100,
) -> list[StepRecord]:
    """Execute the graph starting at 'start' until 'exit' or max_steps.

    If `resume` is provided, execution restarts from the successor of the
    checkpointed last step.
    """
    history: list[StepRecord] = []
    visits: dict[str, int] = {}
    current = _start_node(graph)
    seq = 0  # CXDB sequence — independent of history length so refactors can't desync.
    _resumed_overhead = 0

    if resume is not None:
        resumed = _load_checkpoint(resume)
        history.extend(resumed)
        _resumed_overhead = sum(
            1 for s in resumed if s.metadata.get("_branch_overhead") == "true"
        )
        for step in resumed:
            visits[step.node] = visits.get(step.node, 0) + 1

        if resumed:
            last = resumed[-1]
            last_node = graph.nodes.get(last.node)
            if last_node is None:
                raise ValueError(f"checkpoint node missing from graph: {last.node!r}")
            if is_exit_node(last_node):
                return history
            if len(history) - _resumed_overhead >= max_steps:
                return history
            synthetic = _normalized_result(Result(outcome=last.outcome))
            goal_gate_node = _goal_gate_target(graph, last_node, synthetic, ctx)
            next_node = goal_gate_node or _pick_next(graph, last_node, synthetic, ctx)
            if next_node is None:
                return history
            current = next_node

    # Always have an addressable run_id so diagnostics are locatable even when
    # no CXDB is attached (ad-hoc smoke runs, echo backend, etc.).
    if not ctx.run_id:
        ctx.run_id = uuid.uuid4().hex[:12]
    event_path_is_default = getattr(ctx, "event_log_path", None) is None

    cxdb: Optional[CXDB] = None
    if ctx.cxdb_path is not None:
        try:
            cxdb = CXDB(ctx.cxdb_path)
            ctx.run_id = cxdb.start_run(pipeline=graph.name, goal=ctx.goal)
        except Exception as exc:
            _emit_event(
                ctx,
                "cxdb_init_failed",
                {
                    "path": str(ctx.cxdb_path),
                    "error": f"{type(exc).__name__}: {exc}",
                },
                seq,
            )
            cxdb = None
    if event_path_is_default and ctx.run_id is not None:
        ctx.event_log_path = _EVENT_DIR / f"{ctx.run_id}.jsonl"

    perf_root = getattr(ctx, "perf_log_root", None)
    if perf_root is not None and ctx.run_id is not None:
        ctx.git_ctx = perf_log.resolve_git_context(ctx.workdir, ctx.state)
        ctx.perf_run = perf_log.open_run(
            perf_root,
            ctx.git_ctx,
            ctx.run_id,
            pipeline=graph.name,
            goal=ctx.goal,
            backend=ctx.backend,
        )

    log = _open_run_log(ctx.run_id)
    _log(log, f"run start pipeline={graph.name!r} goal={ctx.goal!r} backend={ctx.backend!r}")
    if ctx.run_id is not None:
        _emit_event(
            ctx,
            "run_start",
            {
                "pipeline": graph.name,
                "goal": ctx.goal,
                "backend": ctx.backend,
                "run_id": ctx.run_id,
            },
            seq,
        )

    ended_at_exit = False

    try:
        # Branch StepRecords are internal to a fan-out step and should not count
        # against the main pipeline's step budget (max_steps).  Track overhead so
        # the check uses only main-pipeline steps.  On resume, initialize from the
        # count of branch records already in the checkpoint so the budget is correct.
        _parallel_overhead = _resumed_overhead
        while True:
            if len(history) - _parallel_overhead >= max_steps:
                record = StepRecord(
                    node=current.name,
                    outcome="exhausted",
                    ts=time.time(),
                    output_preview=f"max_steps={max_steps} reached before exit",
                )
                seq = _append_record(history, checkpoint, cxdb, ctx, seq, record, "")
                break


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

            try:
                _emit_event(
                    ctx,
                    "node_start",
                    {
                        "node": current.name,
                    },
                    seq,
                )
                enter_seq = seq  # capture before _append_record increments seq
                _perf_node_enter(ctx, current, enter_seq, visits[current.name])
                results, records = _run_single_node(current, ctx, graph)
            except Exception as exc:  # noqa: BLE001 — any node crash must be recorded, not fatal
                next_node = _handle_node_exception(
                    graph, current, ctx, exc, history, checkpoint, cxdb, seq, log, visits
                )
                seq += 1
                if next_node is None:
                    break
                current = next_node
                continue

            if records:
                result = results[-1]
            else:
                result = _normalized_result(Result(outcome="success"))

            branch_records: list[tuple[StepRecord, str, dict[str, str]]] = []
            if current.attrs.get("parallel", False) and not _is_parallel_node(current):
                branch_edges = _parallel_branches(graph, current, result, ctx)
                branch_results: list[Result] = []
                for edge in branch_edges:
                    target = graph.nodes.get(edge.dst)
                    if target is None:
                        continue
                    branch_seq = seq
                    branch_visit = 1
                    _emit_event(
                        ctx,
                        "node_start",
                        {"node": target.name, "parallel": "true"},
                        branch_seq,
                    )
                    _perf_node_enter(ctx, target, branch_seq, branch_visit)
                    try:
                        b_results, b_records = _run_single_node(target, _clone_context(ctx), graph)
                    except Exception as exc:  # noqa: BLE001
                        b_tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
                        summary_line = b_tb.strip().splitlines()[-1] if b_tb.strip() else ""
                        b_record = StepRecord(
                            node=target.name,
                            outcome="error",
                            ts=time.time(),
                            output_preview=(f"{type(exc).__name__}: {exc} | {summary_line}")[:280],
                            metadata={"exception": type(exc).__name__, "parallel": "true"},
                        )
                        branch_records.append((b_record, b_tb, {"exception": type(exc).__name__, "parallel": "true"}))
                        branch_results.append(Result(outcome="error", output=b_tb, metadata={"exception": type(exc).__name__}))
                        _perf_node_exit(
                            ctx,
                            target.name,
                            branch_seq,
                            "error",
                            branch_visit,
                            {"exception": type(exc).__name__, "parallel": "true"},
                        )
                        _log(log, f"parallel branch {target.name!r} crashed: {type(exc).__name__}: {exc}")
                        _emit_event(
                            ctx,
                            "branch_exception",
                            {
                                "node": target.name,
                                "error_type": type(exc).__name__,
                                "message": str(exc),
                            },
                            seq,
                        )
                        continue

                    if b_records:
                        for branch_index, b_result in enumerate(b_results):
                            b_record = b_records[branch_index]
                            branch_records.append((b_record, b_result.output, b_record.metadata))
                        # Emit a single exit for the last result — calling inside the loop
                        # would produce N exits for one enter when a node retries.
                        final_b_record = b_records[-1]
                        _perf_node_exit(
                            ctx,
                            target.name,
                            branch_seq,
                            b_results[-1].outcome,
                            branch_visit,
                            final_b_record.metadata,
                        )
                        branch_results.append(b_results[-1])
                        _emit_event(
                            ctx,
                            "node_complete",
                            {
                                "node": target.name,
                                "outcome": _classify_outcome(b_results[-1].outcome),
                                "parallel": "true",
                            },
                            branch_seq,
                        )

                if branch_records:
                    join_outcome = _parallel_join_outcome(current, branch_results, _allow_partial(current))
                    join_metadata = {
                        "parallel_branches": str(len(branch_results)),
                        "parallel_successes": str(
                            sum(1 for branch in branch_results if _is_success_result(branch.outcome))
                        ),
                        "join_quorum": str(_attr_int(current, "join_quorum", len(branch_results))),
                        "parallel": "true",
                    }
                    result = _normalized_result(
                        Result(
                            outcome=join_outcome,
                            output=result.output,
                            metadata={**result.metadata, **join_metadata},
                        )
                    )
                    if records:
                        records[-1].outcome = result.outcome
                        records[-1].metadata = result.metadata

            for index, attempt in enumerate(results):
                record = records[index]
                seq = _append_record(
                    history,
                    checkpoint,
                    cxdb,
                    ctx,
                    seq,
                    record,
                    attempt.output,
                    record.metadata,
                )

            for b_record, b_output, b_metadata in branch_records:
                seq = _append_record(
                    history,
                    checkpoint,
                    cxdb,
                    ctx,
                    seq,
                    b_record,
                    b_output,
                    b_metadata,
                )

            # --- parallel fan-out/fan-in: type=parallel / shape=component ---
            _para_jump_to: Optional[Node] = None
            _para_result: Optional[Result] = None

            if _is_parallel_node(current):
                _jn = _find_join_node(graph, current)
                if _jn is None:
                    # No join node reachable — miswired graph; report failure and stop.
                    _err_msg = f"parallel node '{current.name}' has no reachable join node"
                    _err_rec = StepRecord(
                        node=current.name,
                        outcome="failure",
                        ts=time.time(),
                        output_preview=_err_msg,
                        metadata={"error": "no_join_node"},
                    )
                    seq = _append_record(
                        history, checkpoint, cxdb, ctx, seq, _err_rec, _err_msg,
                        {"error": "no_join_node"},
                    )
                    ctx.state["_last_node"] = current.name
                    ctx.state["_last_outcome"] = "failure"
                    ctx.state[current.name + ".outcome"] = "failure"
                    _update_failure_state(
                        current, ctx, Result(outcome="failure", output=_err_msg)
                    )
                    _perf_node_exit(
                        ctx, current.name, enter_seq, "failure",
                        visits[current.name], {"error": "no_join_node"},
                    )
                    _emit_event(
                        ctx, "node_complete",
                        {
                            "node": current.name,
                            "outcome": "failure",
                            "preview": _err_msg,
                            "is_exit": str(is_exit_node(current)),
                        },
                        seq,
                    )
                    break
                else:
                    # Filter by edge conditions; deduplicate by node name so multiple
                    # edges to the same target don't launch duplicate branch workers.
                    _seen_branch_names: set[str] = set()
                    _branch_starts = []
                    for _e in graph.outgoing(current.name):
                        _bn = graph.nodes.get(_e.dst)
                        if (
                            _bn is not None
                            and not _is_join_node(_bn)
                            and _edge_matches(_e, result, ctx, current)
                            and _bn.name not in _seen_branch_names
                        ):
                            _seen_branch_names.add(_bn.name)
                            _branch_starts.append(_bn)
                    
                    _branch_results_list: list[Result] = []
                    _branch_flat_records: list[StepRecord] = []
                    
                    if _branch_starts:
                        _seq_ref: list[int] = [seq]
                        _seq_lock = threading.Lock()
                        _name_to_br: dict[str, tuple[list[StepRecord], Result]] = {}

                        _cxdb_path = cxdb.path if cxdb is not None else None
                        with ThreadPoolExecutor(max_workers=len(_branch_starts)) as _executor:
                            _futures = {
                                _executor.submit(
                                    _run_branch_until_join,
                                    graph, _bs, _branch_context(ctx, _bs.name), _jn,
                                    _seq_ref, _seq_lock, _cxdb_path,
                                    max_steps,  # pass outer limit to prevent branch hangs
                                ): _bs.name
                                for _bs in _branch_starts
                            }
                            for _f in as_completed(_futures):
                                try:
                                    _name_to_br[_futures[_f]] = _f.result()
                                except Exception as _exc:
                                    _name_to_br[_futures[_f]] = (
                                        [],
                                        Result(outcome="failure", output=f"branch exception: {_exc}"),
                                    )

                        seq = _seq_ref[0]

                        for _bs in _branch_starts:
                            _b_recs, _b_res = _name_to_br.get(_bs.name, ([], Result(outcome="failure")))
                            _branch_flat_records.extend(_b_recs)
                            _branch_results_list.append(_b_res)
                    
                    # Apply join policy; fall back to join_quorum on fanout node
                    # if the join node has no explicit policy (legacy compat).
                    _fanout_quorum = _attr_int(current, "join_quorum", 0)
                    _jn_policy = str(_jn.attrs.get("policy", "")).strip().lower()
                    if _fanout_quorum and not _jn_policy and _branch_results_list:
                        _n_b = len(_branch_results_list)
                        _n_ok = sum(1 for _r in _branch_results_list if _is_success_result(_r.outcome))
                        _join_outcome = "success" if _n_ok >= _fanout_quorum else "failure"
                    else:
                        _join_outcome = _apply_join_policy(_jn, _branch_results_list)
                    _join_meta: dict[str, str] = {
                        "policy": str(_jn.attrs.get("policy", "wait_all")),
                        "branches": str(len(_branch_results_list)),
                        "successes": str(
                            sum(1 for _r in _branch_results_list if _is_success_result(_r.outcome))
                        ),
                    }
                    _join_rec = StepRecord(
                        node=_jn.name,
                        outcome=_join_outcome,
                        ts=time.time(),
                        output_preview=(
                            f"join {_jn.attrs.get('policy', 'wait_all')} "
                            f"{len(_branch_results_list)} branches"
                        ),
                        metadata=_join_meta,
                    )
                    _para_result = Result(outcome=_join_outcome, metadata=_join_meta)

                    ctx.state["_last_node"] = _jn.name
                    ctx.state["_last_outcome"] = _join_outcome
                    ctx.state["_last_output"] = _join_rec.output_preview
                    ctx.state[_jn.name + ".outcome"] = _join_outcome

                    # Branch records already in CXDB (written thread-safely in _run_branch_until_join)
                    for _br in _branch_flat_records:
                        history.append(_br)
                    # _append_record writes the checkpoint atomically including the join step.
                    seq = _append_record(
                        history, checkpoint, cxdb, ctx, seq,
                        _join_rec, _join_rec.output_preview, _join_meta,
                    )
                    result = _para_result
                    _para_jump_to = _jn
                    # Branch records are internal overhead; don't count them
                    # against the main pipeline's max_steps budget.
                    _parallel_overhead += len(_branch_flat_records)

                    # Enforce max_visits on the join node (it's never set as
                    # `current`, so the top-of-loop visit check never fires).
                    visits[_jn.name] = visits.get(_jn.name, 0) + 1
                    _jn_max = _attr_int(_jn, "max_visits", 0)
                    if _jn_max and visits[_jn.name] > _jn_max:
                        _ex_rec = StepRecord(
                            node=_jn.name,
                            outcome="exhausted",
                            ts=time.time(),
                            output_preview=f"max_visits={_jn_max} exceeded",
                        )
                        seq = _append_record(history, checkpoint, cxdb, ctx, seq, _ex_rec, "")
                        ctx.state["_last_node"] = _jn.name
                        ctx.state["_last_outcome"] = "exhausted"
                        ctx.state[_jn.name + ".outcome"] = "exhausted"
                        _update_failure_state(_jn, ctx, Result(outcome="exhausted", output=_ex_rec.output_preview))
                        _perf_node_exit(
                            ctx, current.name, enter_seq, "exhausted",
                            visits[current.name], {},
                        )
                        _emit_event(
                            ctx, "node_complete",
                            {
                                "node": current.name,
                                "outcome": "exhausted",
                                "preview": _ex_rec.output_preview,
                                "is_exit": str(is_exit_node(current)),
                            },
                            seq,
                        )
                        break
            # --- end parallel fan-out/fan-in ---

            if records:
                # Fix: attribute failure state to join node (not fanout) when parallel ran
                _failure_node = _para_jump_to if _para_jump_to is not None else current
                _update_failure_state(_failure_node, ctx, result)

            _perf_node_exit(
                ctx,
                current.name,
                enter_seq,  # use enter_seq so key matches node_enter_ts entry
                result.outcome,
                visits[current.name],
                records[-1].metadata if records else result.metadata,
            )
            _emit_event(
                ctx,
                "node_complete",
                {
                    "node": current.name,
                    "outcome": _classify_outcome(result.outcome),
                    "preview": records[-1].output_preview if records else "",
                    "is_exit": str(is_exit_node(current)),
                },
                seq,
            )

            if is_exit_node(current):
                ended_at_exit = True
                break

            try:
                if _para_jump_to is not None:
                    if is_exit_node(_para_jump_to):
                        ended_at_exit = True
                        break
                    gate_target = _goal_gate_target(graph, _para_jump_to, _para_result, ctx)
                    if gate_target is not None:
                        next_node = gate_target
                    else:
                        next_node = _pick_next(graph, _para_jump_to, _para_result, ctx)
                else:
                    gate_target = _goal_gate_target(graph, current, result, ctx)
                    if gate_target is not None:
                        next_node = gate_target
                    else:
                        outgoing = graph.outgoing(current.name)
                        join_edges = (
                            _parallel_join_edges(outgoing)
                            if current.attrs.get("parallel", False)
                            else []
                        )
                        if join_edges:
                            if _classify_outcome(result.outcome) == "success":
                                chosen = join_edges
                            else:
                                chosen = [
                                    edge
                                    for edge in outgoing
                                    if edge.condition and edge not in join_edges
                                ]
                        else:
                            chosen = outgoing
                        next_node = _pick_next_from_edges(graph, current, chosen, result, ctx)
            except Exception as exc:  # noqa: BLE001 — transition crash must be recorded, not fatal
                # Node already exited successfully above; skip the perf exit to avoid a
                # duplicate node_exit for the same enter/seq pair.
                next_node = _handle_node_exception(
                    graph, current, ctx, exc, history, checkpoint, cxdb, seq, log, visits,
                    skip_perf_exit=True,
                )
                seq += 1
                if next_node is None:
                    break
                current = next_node
                continue

            if next_node is None:
                _stuck_node = _para_jump_to if _para_jump_to is not None else current
                record = StepRecord(
                    node=_stuck_node.name,
                    outcome="stuck",
                    ts=time.time(),
                    output_preview="no matching outgoing edge",
                )
                seq = _append_record(
                    history, checkpoint, cxdb, ctx, seq, record, "no matching outgoing edge"
                )
                break
            _emit_event(
                ctx,
                "transition",
                {
                    "from": current.name,
                    "to": next_node.name,
                    "outcome": _classify_outcome(result.outcome),
                },
                seq,
            )
            perf_log.transition(
                getattr(ctx, "perf_run", None),
                from_node=current.name,
                to_node=next_node.name,
                outcome=result.outcome,
                seq=seq,
            )
            current = next_node
    finally:
        final_outcome = history[-1].outcome if history else "empty"
        if not ended_at_exit and _classify_outcome(final_outcome) == "success":
            final_outcome = "failure"
            if history:
                history[-1].outcome = final_outcome
        _emit_event(
            ctx,
            "run_end",
            {
                "pipeline": graph.name,
                "final_outcome": final_outcome,
                "ended_at_exit": str(ended_at_exit),
                "steps": str(len(history)),
            },
            seq,
        )
        _log(log, f"run end final={final_outcome!r} steps={len(history)}")
        success_count, failure_count, error_count = _outcome_counts(history)
        perf_log.close_run(
            getattr(ctx, "perf_run", None),
            final_outcome=final_outcome,
            steps=len(history),
            success_count=success_count,
            failure_count=failure_count,
            error_count=error_count,
        )
        if cxdb is not None and ctx.run_id is not None:
            try:
                cxdb.end_run(
                    ctx.run_id,
                    final=final_outcome,
                )
            except Exception:
                _emit_event(ctx, "run_end_failed", {"error": "cxdb_write_error"}, seq)
            finally:
                try:
                    cxdb.close()
                except Exception:
                    pass
        if log is not None:
            try:
                log.close()
            except OSError:
                pass

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
    outcome = _classify_outcome(result.outcome)
    if outcome == "success":
        return True
    allow_partial = node.attrs.get("allow_partial", False)
    if isinstance(allow_partial, bool):
        partial_allowed = allow_partial
    else:
        partial_allowed = str(allow_partial).strip().lower() in {"true", "1", "yes"}
    return partial_allowed and outcome == "partial"


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
        except Exception as exc:
            _emit_event(
                ctx,
                "checkpoint_write_failed",
                {
                    "node": record.node,
                    "seq": str(seq),
                    "error": f"{type(exc).__name__}: {exc}",
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
            "cxdb_record_failed",
            {
                "node": record.node,
                "seq": str(seq),
                "error": f"{type(exc).__name__}:{exc}",
            },
            seq,
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
