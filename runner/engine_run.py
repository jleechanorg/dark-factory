"""Main `run()` loop + single-node driver for the engine.

Splits out from `runner/engine.py` (see `docs/refactor/file-ownership-map.engine.md`).
Owns `run` (the public graph-traversal entry point) and `_run_single_node` (the
retries-normalize-state helper that produces the per-step Result + StepRecord
pair consumed by the main loop).
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import subprocess
import threading
import time
import traceback
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed
from typing import Optional

from . import engine_branches as _branches
from . import engine_edges as _edges
from . import engine_exceptions as _exc
from . import engine_observability as _obs
from . import engine_parallel as _parallel
from . import engine_persist as _persist
from . import perf_log
from ._classify import _classify_outcome
from .cxdb import CXDB
from .handlers import Context, Result, resolve
from .parser import Graph, Node, is_exit_node


# Cross-run exhaustion circuit breaker (v4 hardening).
#
# When the last N prior runs of the same pipeline ALL ended with
# `final='exhausted'`, skip this run entirely instead of burning LLM
# budget on a guaranteed-to-fail attempt. Prevents the 16+ WIP-exhausted
# commit stack seen on test-merged (memory 2026-06-27).
#
# Proof state (per root-cause-first):
#   - Server-owned invariant: CXDB stores cross-run state that the agent
#     cannot see. Engine owns run lifecycle.
#   - Prompt-insufficient (proven): the fix prompt has no signal about
#     prior-run exhaustion.
#
# Set DARK_FACTORY_CROSS_RUN_CIRCUIT_THRESHOLD=0 to disable.
CB_THRESHOLD = int(os.environ.get("DARK_FACTORY_CROSS_RUN_CIRCUIT_THRESHOLD", "3"))


def _auto_wip_commit_on_exhaustion(ctx: "Context", reason: str) -> None:
    """If workdir is a git repo with uncommitted changes, commit as WIP.

    Only fires at exhaustion (not on success paths). Prevents the
    2026-06-22 PR-B'' work-loss false alarm pattern (run 7aa7695b1cf6)
    where dark-factory exhausted without committing and the parent agent
    declared work LOST — recoverable in fact, but only by luck because
    the worktree was never reset.

    Guards:
      - workdir missing → noop
      - workdir not a git repo → noop
      - worktree clean (`git status --porcelain` empty) → noop
      - subprocess failures → silently swallowed (best-effort)
    """
    try:
        workdir = pathlib.Path(getattr(ctx, "workdir", "") or "")
        if not workdir or not workdir.exists():
            return
        if not (workdir / ".git").exists():
            return
        try:
            status = subprocess.run(
                ["git", "status", "--porcelain"],
                cwd=str(workdir),
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
            )
        except Exception:
            return
        if status.returncode != 0 or not (status.stdout or "").strip():
            return

        run_id = getattr(ctx, "run_id", None) or "unknown"
        head_sha = "unknown"
        try:
            head = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=str(workdir),
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
            )
            if head.returncode == 0 and (head.stdout or "").strip():
                head_sha = head.stdout.strip()[:12]
        except Exception:
            pass

        msg = (
            f"WIP: dark-factory exhausted at {run_id} {head_sha}\n\n"
            f"Auto-recovery commit. {reason}"
        )
        try:
            subprocess.run(
                ["git", "add", "-A"],
                cwd=str(workdir),
                check=False,
                timeout=30,
            )
            subprocess.run(
                ["git", "commit", "-m", msg],
                cwd=str(workdir),
                check=False,
                timeout=30,
            )
        except Exception:
            pass
    except Exception:
        # Best-effort: never let the WIP commit path raise into the run loop.
        return


def _run_single_node(
    node: Node,
    ctx: Context,
    graph: Graph,
) -> tuple[list[Result], list]:
    _obs._write_heartbeat(ctx, graph, node)
    try:
        handler = resolve(node)
        results = _persist._run_with_retries(handler, node, ctx, graph)
        normalized_results: list[Result] = []
        records: list = []
        for attempt in results:
            attempt = _obs._normalized_result(attempt)
            ctx.state.update(attempt.context_updates)
            ctx.state["_last_node"] = node.name
            ctx.state["_last_outcome"] = attempt.outcome
            ctx.state["_last_output"] = attempt.output[:4000]
            normalized_results.append(attempt)
            records.append(
                _persist.StepRecord(
                    node=node.name,
                    outcome=attempt.outcome,
                    ts=time.time(),
                    output_preview=attempt.output[:280],
                    metadata=attempt.metadata,
                )
            )
            _persist._update_failure_state(node, ctx, attempt)
            ctx.history.append({"node": node.name, "outcome": attempt.outcome})
        return normalized_results, records
    finally:
        _obs._write_heartbeat(ctx, graph, node, is_complete=True)


def run(
    graph: Graph,
    ctx: Context,
    checkpoint: Optional[pathlib.Path] = None,
    resume: Optional[pathlib.Path] = None,
    max_steps: int = 100,
) -> list:
    """Execute the graph starting at 'start' until 'exit' or max_steps.

    If `resume` is provided, execution restarts from the successor of the
    checkpointed last step.
    """
    history: list = []
    visits: dict[str, int] = {}
    # Per-node ring of recent output hashes for the no_progress detector
    # (D3 in feedback 2026-06-22). When a node produces the same output
    # hash on consecutive visits, the engine short-circuits to "exhausted"
    # rather than burning LLM budget on a stuck fix loop. Opt-in via the
    # node's `no_progress_max="N"` attribute (default 0 = disabled).
    _no_progress_history: dict[str, list[str]] = {}
    current = _persist._start_node(graph)
    seq = 0  # CXDB sequence — independent of history length so refactors can't desync.
    ctx.last_completed_seq = seq
    _resumed_overhead = 0

    if resume is not None:
        resumed = _persist._load_checkpoint(resume)
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
            synthetic = _obs._normalized_result(Result(outcome=last.outcome))
            # Detect incomplete parallel fan-out: the fan-out step was checkpointed
            # but branches never ran (job was interrupted between the fan-out record
            # write and the ThreadPoolExecutor completing).  Re-run from the parallel
            # node so all branches execute; remove the incomplete record first to
            # avoid a duplicate fan-out step in the history.
            if _parallel._is_parallel_node(last_node) and last.metadata.get("role") == "fanout":
                history.pop()
                visits[last_node.name] = max(0, visits.get(last_node.name, 1) - 1)
                current = last_node
            else:
                goal_gate_node = _persist._goal_gate_target(graph, last_node, synthetic, ctx)
                next_node = goal_gate_node or _edges._pick_next(graph, last_node, synthetic, ctx)
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
            _obs._emit_event(
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
        ctx.event_log_path = pathlib.Path.home() / ".dark-factory" / "runs" / ctx.run_id / "events.jsonl"

    if ctx.run_id is not None:
        run_dir = pathlib.Path.home() / ".dark-factory" / "runs" / ctx.run_id
        run_dir.mkdir(parents=True, exist_ok=True)
        manifest_path = run_dir / "manifest.json"
        pipeline_val = str(graph.pipeline_path) if getattr(graph, "pipeline_path", None) else graph.name
        manifest_data = {
            "pipeline": pipeline_val,
            "goal": ctx.goal,
            "backend": ctx.backend,
            "workdir": str(ctx.workdir) if ctx.workdir else "",
            "state": ctx.state,
        }
        try:
            manifest_path.write_text(json.dumps(manifest_data, indent=2), encoding="utf-8")
        except Exception:
            pass

    if checkpoint is None and ctx.run_id is not None:
        checkpoint = pathlib.Path.home() / ".dark-factory" / "runs" / ctx.run_id / "checkpoint.json"

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

    log = _obs._open_run_log(ctx.run_id)
    _obs._log(log, f"run start pipeline={graph.name!r} goal={ctx.goal!r} backend={ctx.backend!r}")
    if ctx.run_id is not None:
        _obs._emit_event(
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
            # Cross-run exhaustion circuit breaker (v4 hardening):
            # When the last CB_THRESHOLD prior runs of this pipeline ALL
            # ended with `final='exhausted'`, emit a synthetic exhausted
            # record at run start and break. This fires before any node
            # handler runs, so no LLM budget is consumed on a stuck pattern.
            # The synthetic node name `__cross_run_circuit__` is reserved
            # (collision-free with real .dot node names) so the Healer can
            # cluster it distinctly from real exhaustion.
            if cxdb is not None and CB_THRESHOLD > 0:
                _prior_finals = cxdb.recent_run_finals(graph.name, CB_THRESHOLD)
                if (
                    len(_prior_finals) >= CB_THRESHOLD
                    and all(f == "exhausted" for f in _prior_finals)
                ):
                    _cb_record = _persist.StepRecord(
                        node="__cross_run_circuit__",
                        outcome="exhausted",
                        ts=time.time(),
                        output_preview=(
                            f"cross_run_circuit_breaker: last {CB_THRESHOLD} "
                            f"runs of pipeline {graph.name!r} all ended "
                            f"exhausted; skipping run"
                        ),
                        metadata={
                            "cross_run_circuit_breaker": "true",
                            "threshold": str(CB_THRESHOLD),
                            "prior_finals": json.dumps(_prior_finals),
                        },
                    )
                    seq = _persist._append_record(
                        history, checkpoint, cxdb, ctx, seq, _cb_record, "",
                    )
                    break

            if len(history) - _parallel_overhead >= max_steps:
                record = _persist.StepRecord(
                    node=current.name,
                    outcome="exhausted",
                    ts=time.time(),
                    output_preview=f"max_steps={max_steps} reached before exit",
                )
                seq = _persist._append_record(history, checkpoint, cxdb, ctx, seq, record, "")
                _auto_wip_commit_on_exhaustion(ctx, f"max_steps={max_steps} reached")
                break


            visits[current.name] = visits.get(current.name, 0) + 1
            max_visits = _edges._attr_int(current, "max_visits", 0)
            if max_visits and visits[current.name] > max_visits:
                record = _persist.StepRecord(
                    node=current.name,
                    outcome="exhausted",
                    ts=time.time(),
                    output_preview=f"max_visits={max_visits} exceeded",
                )
                seq = _persist._append_record(history, checkpoint, cxdb, ctx, seq, record, "")
                _auto_wip_commit_on_exhaustion(
                    ctx, f"max_visits={max_visits} exceeded on node {current.name!r}"
                )
                break

            try:
                _obs._emit_event(
                    ctx,
                    "node_start",
                    {
                        "node": current.name,
                        "attempt": "1",
                    },
                    seq,
                )
                enter_seq = seq  # capture before _append_record increments seq
                _obs._perf_node_enter(ctx, current, enter_seq, visits[current.name])
                results, records = _run_single_node(current, ctx, graph)
            except Exception as exc:  # noqa: BLE001 — any node crash must be recorded, not fatal
                next_node = _exc._handle_node_exception(
                    graph, current, ctx, exc, history, checkpoint, cxdb, seq, log, visits
                )
                seq += 1
                ctx.last_completed_seq = seq
                if next_node is None:
                    break
                current = next_node
                continue

            if records:
                result = results[-1]
            else:
                result = _obs._normalized_result(Result(outcome="success"))

            # D3 (feedback 2026-06-22): semantic loop bound. When a node
            # has `no_progress_max="N"` set, track the last N output hashes
            # for that node. If all N match, the node is stuck — short-
            # circuit to "exhausted" with reason="no_progress" instead of
            # waiting for `max_visits` to fire. This catches the pattern
            # where a fix-node returns `success` blindly (e.g. blind prompt
            # with no test output) but the upstream failure never resolves.
            no_progress_max = _edges._attr_int(current, "no_progress_max", 0)
            if no_progress_max and result.output is not None:
                head = (result.output or "")[:1024]
                node_hash = hashlib.sha256(head.encode("utf-8")).hexdigest()
                history_for_node = _no_progress_history.setdefault(current.name, [])
                history_for_node.append(node_hash)
                # Trim to the no_progress_max window
                if len(history_for_node) > no_progress_max:
                    history_for_node.pop(0)
                if (
                    len(history_for_node) == no_progress_max
                    and len(set(history_for_node)) == 1
                ):
                    record = _persist.StepRecord(
                        node=current.name,
                        outcome="exhausted",
                        ts=time.time(),
                        output_preview=(
                            f"no_progress_max={no_progress_max} reached — "
                            f"output hash unchanged across last "
                            f"{no_progress_max} visits"
                        ),
                        metadata={
                            "no_progress": "true",
                            "no_progress_max": str(no_progress_max),
                            "stuck_hash": node_hash,
                        },
                    )
                    seq = _persist._append_record(history, checkpoint, cxdb, ctx, seq, record, "")
                    _auto_wip_commit_on_exhaustion(
                        ctx, f"no_progress_max={no_progress_max} reached on node {current.name!r}"
                    )
                    break

            branch_records: list[tuple] = []
            if current.attrs.get("parallel", False) and not _parallel._is_parallel_node(current):
                branch_edges = _parallel._parallel_branches(graph, current, result, ctx)
                branch_results: list[Result] = []
                b_seq = seq + len(results)
                for edge in branch_edges:
                    target = graph.nodes.get(edge.dst)
                    if target is None:
                        continue
                    branch_visit = 1
                    target_start_seq = b_seq
                    _obs._emit_event(
                        ctx,
                        "node_start",
                        {
                            "node": target.name,
                            "attempt": "1",
                            "parallel": "true",
                        },
                        b_seq,
                    )
                    _obs._perf_node_enter(ctx, target, b_seq, branch_visit)
                    try:
                        b_results, b_records = _run_single_node(target, _branches._clone_context(ctx), graph)
                    except Exception as exc:  # noqa: BLE001
                        b_tb = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
                        summary_line = b_tb.strip().splitlines()[-1] if b_tb.strip() else ""
                        b_record = _persist.StepRecord(
                            node=target.name,
                            outcome="error",
                            ts=time.time(),
                            output_preview=(f"{type(exc).__name__}: {exc} | {summary_line}")[:280],
                            metadata={"exception": type(exc).__name__, "parallel": "true"},
                        )
                        branch_records.append((b_record, b_tb, {"exception": type(exc).__name__, "parallel": "true"}))
                        branch_results.append(Result(outcome="error", output=b_tb, metadata={"exception": type(exc).__name__}))
                        _obs._perf_node_exit(
                            ctx,
                            target.name,
                            target_start_seq,
                            "error",
                            branch_visit,
                            {"exception": type(exc).__name__, "parallel": "true"},
                        )
                        _obs._log(log, f"parallel branch {target.name!r} crashed: {type(exc).__name__}: {exc}")
                        _obs._emit_event(
                            ctx,
                            "branch_exception",
                            {
                                "node": target.name,
                                "error_type": type(exc).__name__,
                                "message": str(exc),
                            },
                            b_seq,
                        )
                        transcript_path, transcript_sha256 = _obs._write_transcript_sidecar(
                            ctx, b_seq, target.name, 1, b_tb
                        )
                        payload = {
                            "node": target.name,
                            "outcome": "error",
                            "attempt": "1",
                            "max_retries": "0",
                            "parallel": "true",
                        }
                        if transcript_path:
                            payload["transcript_path"] = transcript_path
                            payload["transcript_sha256"] = transcript_sha256
                        _obs._emit_event(ctx, "node_result", payload, b_seq)

                        _obs._emit_event(
                            ctx,
                            "node_complete",
                            {
                                "node": target.name,
                                "outcome": "error",
                                "parallel": "true",
                            },
                            b_seq,
                        )
                        b_seq += 1
                        continue

                    if b_records:
                        for branch_index, b_result in enumerate(b_results):
                            b_record = b_records[branch_index]
                            branch_records.append((b_record, b_result.output, b_record.metadata))

                            if branch_index > 0:
                                _obs._emit_event(
                                    ctx,
                                    "retry",
                                    {
                                        "node": target.name,
                                        "attempt": str(branch_index + 1),
                                        "max_retries": b_record.metadata.get("max_retries", "0"),
                                        "previous_outcome": b_results[branch_index - 1].outcome,
                                        "parallel": "true",
                                    },
                                    b_seq,
                                )
                                _obs._emit_event(
                                    ctx,
                                    "node_start",
                                    {
                                        "node": target.name,
                                        "attempt": str(branch_index + 1),
                                        "parallel": "true",
                                    },
                                    b_seq,
                                )

                            transcript_path, transcript_sha256 = _obs._write_transcript_sidecar(
                                ctx, b_seq, target.name, branch_index + 1, b_result.output
                            )
                            payload = {
                                "node": target.name,
                                "outcome": _classify_outcome(b_record.outcome),
                                "attempt": str(branch_index + 1),
                                "max_retries": b_record.metadata.get("max_retries", "0"),
                                "parallel": "true",
                            }
                            if transcript_path:
                                payload["transcript_path"] = transcript_path
                                payload["transcript_sha256"] = transcript_sha256
                            _obs._emit_event(ctx, "node_result", payload, b_seq)
                            b_seq += 1

                        final_b_record = b_records[-1]
                        _obs._perf_node_exit(
                            ctx,
                            target.name,
                            target_start_seq,
                            b_results[-1].outcome,
                            branch_visit,
                            final_b_record.metadata,
                        )
                        branch_results.append(b_results[-1])
                        _obs._emit_event(
                            ctx,
                            "node_complete",
                            {
                                "node": target.name,
                                "outcome": _classify_outcome(b_results[-1].outcome),
                                "parallel": "true",
                            },
                            b_seq - 1,
                        )

                if branch_records:
                    join_outcome = _parallel._parallel_join_outcome(current, branch_results, _edges._allow_partial(current))
                    join_metadata = {
                        "parallel_branches": str(len(branch_results)),
                        "parallel_successes": str(
                            sum(1 for branch in branch_results if _obs._is_success_result(branch.outcome))
                        ),
                        "join_quorum": str(_edges._attr_int(current, "join_quorum", len(branch_results))),
                        "parallel": "true",
                    }
                    result = _obs._normalized_result(
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
                if index > 0:
                    _obs._emit_event(
                        ctx,
                        "retry",
                        {
                            "node": current.name,
                            "attempt": str(index + 1),
                            "max_retries": record.metadata.get("max_retries", "0"),
                            "previous_outcome": results[index - 1].outcome,
                        },
                        seq,
                    )
                    _obs._emit_event(
                        ctx,
                        "node_start",
                        {
                            "node": current.name,
                            "attempt": str(index + 1),
                        },
                        seq,
                    )

                transcript_path, transcript_sha256 = _obs._write_transcript_sidecar(
                    ctx, seq, current.name, index + 1, attempt.output
                )
                payload = {
                    "node": current.name,
                    "outcome": _classify_outcome(record.outcome),
                    "attempt": str(index + 1),
                    "max_retries": record.metadata.get("max_retries", "0"),
                }
                if transcript_path:
                    payload["transcript_path"] = transcript_path
                    payload["transcript_sha256"] = transcript_sha256
                _obs._emit_event(ctx, "node_result", payload, seq)

                seq = _persist._append_record(
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
                seq = _persist._append_record(
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

            if _parallel._is_parallel_node(current):
                _jn = _parallel._find_join_node(graph, current)
                if _jn is None:
                    # No join node reachable — miswired graph; report failure and stop.
                    _err_msg = f"parallel node '{current.name}' has no reachable join node"
                    _err_rec = _persist.StepRecord(
                        node=current.name,
                        outcome="failure",
                        ts=time.time(),
                        output_preview=_err_msg,
                        metadata={"error": "no_join_node"},
                    )
                    seq = _persist._append_record(
                        history, checkpoint, cxdb, ctx, seq, _err_rec, _err_msg,
                        {"error": "no_join_node"},
                    )
                    ctx.state["_last_node"] = current.name
                    ctx.state["_last_outcome"] = "failure"
                    ctx.state[current.name + ".outcome"] = "failure"
                    _persist._update_failure_state(
                        current, ctx, Result(outcome="failure", output=_err_msg)
                    )
                    _obs._perf_node_exit(
                        ctx, current.name, enter_seq, "failure",
                        visits[current.name], {"error": "no_join_node"},
                    )
                    _obs._emit_event(
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
                    # Pre-check join max_visits before running branches.
                    # If already exceeded, skip branch workers entirely — prevents
                    # spurious join-success records followed immediately by exhausted.
                    _jn_max = _edges._attr_int(_jn, "max_visits", 0)
                    _jn_visit_next = visits.get(_jn.name, 0) + 1
                    if _jn_max and _jn_visit_next > _jn_max:
                        visits[_jn.name] = _jn_visit_next
                        _ex_rec = _persist.StepRecord(
                            node=_jn.name,
                            outcome="exhausted",
                            ts=time.time(),
                            output_preview=f"max_visits={_jn_max} exceeded",
                        )
                        seq = _persist._append_record(history, checkpoint, cxdb, ctx, seq, _ex_rec, "")
                        ctx.state["_last_node"] = _jn.name
                        ctx.state["_last_outcome"] = "exhausted"
                        ctx.state[_jn.name + ".outcome"] = "exhausted"
                        _persist._update_failure_state(_jn, ctx, Result(outcome="exhausted", output=_ex_rec.output_preview))
                        _obs._perf_node_exit(
                            ctx, current.name, enter_seq, "exhausted",
                            visits[current.name], {},
                        )
                        _obs._emit_event(
                            ctx, "node_complete",
                            {
                                "node": current.name,
                                "outcome": "exhausted",
                                "preview": _ex_rec.output_preview,
                                "is_exit": str(is_exit_node(current)),
                            },
                            seq,
                        )
                        _auto_wip_commit_on_exhaustion(
                            ctx, f"join max_visits={_jn_max} exceeded on join {_jn.name!r}"
                        )
                        break
                    # Filter by edge conditions; deduplicate by node name so multiple
                    # edges to the same target don't launch duplicate branch workers.
                    _seen_branch_names: set[str] = set()
                    _branch_starts = []
                    for _e in graph.outgoing(current.name):
                        _bn = graph.nodes.get(_e.dst)
                        if (
                            _bn is not None
                            and not _parallel._is_join_node(_bn)
                            and _edges._edge_matches(_e, result, ctx, current)
                            and _bn.name not in _seen_branch_names
                        ):
                            _seen_branch_names.add(_bn.name)
                            _branch_starts.append(_bn)

                    _branch_results_list: list[Result] = []
                    _branch_flat_records: list = []

                    if _branch_starts:
                        _seq_ref: list[int] = [seq]
                        _seq_lock = threading.Lock()
                        _name_to_br: dict[str, tuple[list, Result]] = {}

                        _cxdb_path = cxdb.path if cxdb is not None else None
                        with ThreadPoolExecutor(max_workers=len(_branch_starts)) as _executor:
                            _futures = {
                                _executor.submit(
                                    _parallel._run_branch_until_join,
                                    graph, _bs,
                                    _branches._branch_context(
                                        ctx, _bs.name,
                                        str(_bs.attrs.get("type", "")),
                                    ),
                                    _jn,
                                    _seq_ref, _seq_lock, _cxdb_path,
                                    max_steps,  # pass outer limit to prevent branch hangs
                                ): _bs.name
                                for _bs in _branch_starts
                            }
                            for _f in as_completed(_futures):
                                try:
                                    _name_to_br[_futures[_f]] = _f.result()
                                except Exception as _br_exc:
                                    _name_to_br[_futures[_f]] = (
                                        [],
                                        Result(outcome="failure", output=f"branch exception: {_br_exc}"),
                                    )

                        seq = _seq_ref[0]
                        ctx.last_completed_seq = seq

                        for _bs in _branch_starts:
                            _b_recs, _b_res = _name_to_br.get(_bs.name, ([], Result(outcome="failure")))
                            _branch_flat_records.extend(_b_recs)
                            _branch_results_list.append(_b_res)

                    # Apply join policy; fall back to join_quorum on fanout node
                    # if the join node has no explicit policy (legacy compat).
                    _fanout_quorum = _edges._attr_int(current, "join_quorum", 0)
                    _jn_policy = str(_jn.attrs.get("policy", "")).strip().lower()
                    if _fanout_quorum and not _jn_policy and _branch_results_list:
                        _n_b = len(_branch_results_list)
                        _n_ok = sum(1 for _r in _branch_results_list if _obs._is_success_result(_r.outcome))
                        _join_outcome = "success" if _n_ok >= _fanout_quorum else "failure"
                    else:
                        _join_outcome = _parallel._apply_join_policy(_jn, _branch_results_list)
                    _join_meta: dict[str, str] = {
                        "policy": str(_jn.attrs.get("policy", "wait_all")),
                        "branches": str(len(_branch_results_list)),
                        "successes": str(
                            sum(1 for _r in _branch_results_list if _obs._is_success_result(_r.outcome))
                        ),
                    }
                    _join_rec = _persist.StepRecord(
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
                    seq = _persist._append_record(
                        history, checkpoint, cxdb, ctx, seq,
                        _join_rec, _join_rec.output_preview, _join_meta,
                    )
                    result = _para_result
                    _para_jump_to = _jn
                    # Branch records are internal overhead; don't count them
                    # against the main pipeline's max_steps budget.
                    _parallel_overhead += len(_branch_flat_records)

                    # Record this join visit (pre-check already ran above; if we
                    # reach here, the limit was not yet exceeded on this cycle).
                    visits[_jn.name] = _jn_visit_next
            # --- end parallel fan-out/fan-in ---

            if records:
                # Fix: attribute failure state to join node (not fanout) when parallel ran
                _failure_node = _para_jump_to if _para_jump_to is not None else current
                _persist._update_failure_state(_failure_node, ctx, result)

            _obs._perf_node_exit(
                ctx,
                current.name,
                enter_seq,  # use enter_seq so key matches node_enter_ts entry
                result.outcome,
                visits[current.name],
                records[-1].metadata if records else result.metadata,
            )
            _obs._emit_event(
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
                    gate_target = _persist._goal_gate_target(graph, _para_jump_to, _para_result, ctx)
                    if gate_target is not None:
                        next_node = gate_target
                    else:
                        next_node = _edges._pick_next(graph, _para_jump_to, _para_result, ctx)
                else:
                    gate_target = _persist._goal_gate_target(graph, current, result, ctx)
                    if gate_target is not None:
                        next_node = gate_target
                    else:
                        outgoing = graph.outgoing(current.name)
                        join_edges = (
                            _parallel._parallel_join_edges(outgoing)
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
                        next_node = _edges._pick_next_from_edges(graph, current, chosen, result, ctx)
            except Exception as exc:  # noqa: BLE001 — transition crash must be recorded, not fatal
                # Node already exited successfully above; skip the perf exit to avoid a
                # duplicate node_exit for the same enter/seq pair.
                next_node = _exc._handle_node_exception(
                    graph, current, ctx, exc, history, checkpoint, cxdb, seq, log, visits,
                    skip_perf_exit=True,
                )
                seq += 1
                ctx.last_completed_seq = seq
                if next_node is None:
                    break
                current = next_node
                continue

            if next_node is None:
                _stuck_node = _para_jump_to if _para_jump_to is not None else current
                record = _persist.StepRecord(
                    node=_stuck_node.name,
                    outcome="stuck",
                    ts=time.time(),
                    output_preview="no matching outgoing edge",
                )
                seq = _persist._append_record(
                    history, checkpoint, cxdb, ctx, seq, record, "no matching outgoing edge"
                )
                break
            _obs._emit_event(
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
        # P-B: snapshot working-tree state so the operator can tell at a glance
        # whether the run exhausted with no work, or with N files of uncommitted
        # work sitting in the worktree. Surfaced into BOTH the run_end event
        # payload (CXDB record) AND the human-readable log line.
        uncommitted = _obs._collect_uncommitted_state(getattr(ctx, "workdir", None))
        _obs._emit_event(
            ctx,
            "run_end",
            {
                "pipeline": graph.name,
                "final_outcome": final_outcome,
                "ended_at_exit": str(ended_at_exit),
                "steps": str(len(history)),
                **uncommitted,
            },
            seq,
        )
        uncommitted_log = _obs._format_uncommitted_for_log(uncommitted)
        log_line = f"run end final={final_outcome!r} steps={len(history)}"
        if uncommitted_log:
            log_line = f"{log_line} {uncommitted_log}"
        _obs._log(log, log_line)
        success_count, failure_count, error_count = _obs._outcome_counts(history)
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
                # P-B: append a synthetic terminal step carrying the
                # uncommitted state so the Healer can sub-cluster
                # `exhausted_with_uncommitted_work` vs `exhausted_clean`.
                # `node="__run_end__"` is reserved and never produced by a
                # real .dot node, so the cluster key is collision-free.
                try:
                    cxdb.record_step(
                        ctx.run_id,
                        seq + 1,
                        node="__run_end__",
                        outcome=final_outcome,
                        ts=time.time(),
                        output=json.dumps(
                            {
                                "final_outcome": final_outcome,
                                "ended_at_exit": bool(ended_at_exit),
                                "steps": len(history),
                                **uncommitted,
                            },
                            sort_keys=True,
                        ),
                        metadata={
                            "final_outcome": final_outcome,
                            "ended_at_exit": str(ended_at_exit).lower(),
                            "steps": str(len(history)),
                            **uncommitted,
                        },
                    )
                except Exception:
                    # CXDB write is best-effort; never fail the run on
                    # a synthetic step that the operator can recover from.
                    pass
                cxdb.end_run(
                    ctx.run_id,
                    final=final_outcome,
                )
            except Exception:
                _obs._emit_event(ctx, "run_end_failed", {"error": "cxdb_write_error"}, seq)
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
