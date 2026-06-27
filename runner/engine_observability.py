"""Side-effect emission + outcome classification for the engine.

Splits out from `runner/engine.py` (see `docs/refactor/file-ownership-map.engine.md`).
Owns CXDB events, perf log, run log, heartbeat, transcript sidecar, and the
module-level outcome classifiers used throughout the runner.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import re
import subprocess
import sys
import threading
import time
from typing import Optional, TextIO

from . import perf_log
from ._classify import _classify_outcome
from .handlers import Context
from .parser import Node, is_exit_node, is_start_node

_VALIDATION_TYPES = {"holdout_eval", "gate_es", "gate_er", "gate_code_standards", "gate_audit"}

# Per-run runner logs land here so a crash always leaves a diagnosable
# traceback on disk even when no CXDB is attached. Monkeypatchable in tests.
_LOG_DIR = pathlib.Path.home() / ".dark-factory" / "logs"
_EVENT_DIR = pathlib.Path.home() / ".dark-factory" / "events"

_event_lock = threading.Lock()
_heartbeat_lock = threading.Lock()


def _handoff_refs(metadata: dict[str, str]) -> dict[str, str]:
    """Return path-like metadata keys that should be emitted on node_result.

    The contract keeps `output`/`output_preview` as the handoff source of truth,
    while still logging full path references for input/prompt/llm/shadow sidecars.

    Using suffixes keeps this robust as new path keys are introduced without
    requiring a central whitelist drift.
    """
    if not metadata:
        return {}
    refs: dict[str, str] = {}
    for key, value in metadata.items():
        if not isinstance(key, str):
            continue
        if not isinstance(value, str):
            value = str(value)
        if not value:
            continue
        if key.endswith("_path") or key.endswith("_sha256"):
            refs[key] = value
    return refs


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


def _write_heartbeat(
    ctx: Context,
    graph: Optional["object"] = None,
    node: Optional[Node] = None,
    is_complete: bool = False,
) -> None:
    run_id = ctx.run_id
    if not run_id:
        return
    with _heartbeat_lock:
        try:
            run_dir = pathlib.Path.home() / ".dark-factory" / "runs" / run_id
            run_dir.mkdir(parents=True, exist_ok=True)
            hb_path = run_dir / "heartbeat.json"

            existing = {}
            if hb_path.exists():
                try:
                    existing = json.loads(hb_path.read_text(encoding="utf-8"))
                except Exception:
                    pass

            pipeline = existing.get("pipeline", "")
            if graph is not None:
                pipeline = str(graph.pipeline_path) if getattr(graph, "pipeline_path", None) else graph.name

            goal = ctx.goal
            workdir = str(ctx.workdir)

            start_ts = existing.get("start_timestamp")
            if not is_complete and node is not None:
                start_ts = time.time()
            if start_ts is None:
                start_ts = time.time()

            elapsed_time = existing.get("elapsed_time")
            if is_complete and start_ts is not None:
                elapsed_time = time.time() - start_ts

            if node is not None:
                current_node = None if is_complete else node.name
                backend = _node_backend(node, ctx)
                timeout_raw = node.attrs.get("timeout")
                timeout = None
                if timeout_raw is not None:
                    try:
                        if isinstance(timeout_raw, (int, float)):
                            timeout = timeout_raw
                        else:
                            timeout = float(timeout_raw)
                    except (TypeError, ValueError):
                        pass
            else:
                current_node = existing.get("current_node")
                backend = existing.get("backend", ctx.backend)
                timeout = existing.get("timeout")

            hb_data = {
                "pipeline": pipeline,
                "goal": goal,
                "workdir": workdir,
                "current_node": current_node,
                "start_timestamp": start_ts,
                "backend": backend,
                "timeout": timeout,
                "last_completed_seq": getattr(ctx, "last_completed_seq", 0)
            }

            if is_complete or elapsed_time is not None:
                hb_data["elapsed_time"] = elapsed_time
                hb_data["timestamp"] = time.time()

            hb_path.write_text(json.dumps(hb_data, indent=2), encoding="utf-8")
        except Exception as exc:
            try:
                print(f"[runner:write_heartbeat:{type(exc).__name__}] {exc}", file=sys.stderr, flush=True)
            except Exception:
                pass


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
        record = {
            "ts": time.time(),
            "run_id": ctx.run_id,
            "event": event,
        }
        if seq is not None:
            record["seq"] = seq
        record.update(payload)
        payload_text = json.dumps(record, sort_keys=True)
        with _event_lock:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.open("a", encoding="utf-8").write(payload_text + "\n")
    except Exception as exc:
        try:
            print(f"[runner:emit_event:{type(exc).__name__}] {payload_text}", file=sys.stderr, flush=True)
        except Exception:
            pass


def _write_transcript_sidecar(
    ctx: Context,
    seq: int,
    node_name: str,
    attempt_index: int,
    output: str,
) -> tuple[Optional[str], Optional[str]]:
    """Write the full output (transcript) of a node to a sidecar file.

    If the node is a holdout, the output is redacted.
    Returns (absolute_path_str, sha256_hash).
    """
    if not output:
        return None, None

    run_id = getattr(ctx, "run_id", None)
    if not run_id:
        return None, None

    # Redact holdout outputs
    is_holdout = (
        node_name == "holdout"
        or node_name.startswith("holdout_")
        or "holdout" in node_name.lower()
    )
    if is_holdout:
        content = "<redacted holdout output>"
    else:
        content = output

    sha256 = hashlib.sha256(content.encode("utf-8")).hexdigest()

    try:
        run_dir = pathlib.Path.home() / ".dark-factory" / "runs" / run_id
        transcripts_dir = run_dir / "transcripts"
        transcripts_dir.mkdir(parents=True, exist_ok=True)

        file_name = f"{seq}_{node_name}_{attempt_index}.txt"
        file_path = transcripts_dir / file_name
        file_path.write_text(content, encoding="utf-8")

        return str(file_path), sha256
    except Exception as exc:
        # Observability errors should not fail the run
        try:
            print(f"[runner:write_transcript_sidecar:{type(exc).__name__}] {exc}", file=sys.stderr, flush=True)
        except Exception:
            pass
        return None, None


def _write_input_sidecar(
    ctx: Context,
    seq: int,
    node_name: str,
    attempt_index: int,
    content: str,
    kind: str = "input",
) -> tuple[Optional[str], Optional[str]]:
    """Write the full input for a node attempt to a sidecar file."""
    if not content:
        return None, None

    run_id = getattr(ctx, "run_id", None)
    if not run_id:
        return None, None

    sha256 = hashlib.sha256(content.encode("utf-8")).hexdigest()
    try:
        run_dir = pathlib.Path.home() / ".dark-factory" / "runs" / run_id
        inputs_dir = run_dir / "inputs"
        inputs_dir.mkdir(parents=True, exist_ok=True)

        safe_kind = re.sub(r"[^A-Za-z0-9_.-]+", "_", kind).strip("_") or "input"
        file_name = f"{seq}_{node_name}_{attempt_index}_{safe_kind}.txt"
        file_path = inputs_dir / file_name
        file_path.write_text(content, encoding="utf-8")

        return str(file_path), sha256
    except Exception as exc:
        try:
            print(f"[runner:write_input_sidecar:{type(exc).__name__}] {exc}", file=sys.stderr, flush=True)
        except Exception:
            pass
        return None, None


def _node_input_snapshot(node: Node, ctx: Context) -> str:
    """Return a human-readable snapshot of the input visible to a node."""
    payload = {
        "node": node.name,
        "node_type": _node_type(node),
        "backend": _node_backend(node, ctx),
        "goal": ctx.goal,
        "workdir": str(ctx.workdir),
        "attrs": {str(k): str(v) for k, v in node.attrs.items()},
        "state": {str(k): str(v) for k, v in ctx.state.items()},
    }
    parts = [
        "# Dark Factory node input",
        "",
        json.dumps(payload, indent=2, sort_keys=True),
    ]

    if node.prompt_ref:
        try:
            import runner.handlers as _handlers_shim

            parts.extend(
                [
                    "",
                    "## Rendered prompt",
                    "",
                    _handlers_shim._render_prompt(node, ctx),
                ]
            )
        except Exception as exc:
            parts.extend(
                [
                    "",
                    "## Rendered prompt",
                    "",
                    f"<prompt render failed: {type(exc).__name__}: {exc}>",
                ]
            )
    return "\n".join(parts)


def _write_node_input_sidecar(
    ctx: Context,
    seq: int,
    node: Node,
    attempt_index: int,
) -> dict[str, str]:
    """Persist the node's pre-execution input snapshot and emit an event."""
    content = _node_input_snapshot(node, ctx)
    path, sha256 = _write_input_sidecar(ctx, seq, node.name, attempt_index, content)
    if not path:
        return {}
    meta = {
        "input_path": path,
        "input_sha256": sha256 or "",
    }
    _emit_event(
        ctx,
        "node_input",
        {
            "node": node.name,
            "attempt": str(attempt_index),
            **meta,
        },
        seq,
    )
    return meta


def _open_run_log(run_id: str) -> Optional[TextIO]:
    """Open ~/.dark-factory/logs/<run_id>.log for append-mode tee logging.

    Failures to open the log must never take down a run, so this swallows
    filesystem errors and returns None (logging becomes a no-op).

    The log directory is resolved through the `runner.engine` shim at call
    time so tests that monkeypatch `runner.engine._LOG_DIR` keep working
    (the test contract predates the engine split).
    """
    try:
        import runner.engine as _engine_mod  # local import to dodge cycles
        log_dir = _engine_mod._LOG_DIR
        log_dir.mkdir(parents=True, exist_ok=True)
        return (log_dir / f"{run_id}.log").open("a", encoding="utf-8")
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


def _outcome_counts(history: list["object"]) -> tuple[int, int, int]:
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


# git diff --shortstat prints: "<N> files changed, <N> insertions(+), <N> deletions(-)"
# but only when there ARE changes. The shape can also drop a section when
# only insertions or only deletions occurred. Pre-compile so we don't pay
# the re-compile cost on every run_end.
_DIFF_SHORTSTAT_RE = re.compile(
    r"(?P<files>\d+) files? changed"
    r"(?:, (?P<ins>\d+) insertions?\(\+\))?"
    r"(?:, (?P<del>\d+) deletions?\(-\))?"
)


def _collect_uncommitted_state(workdir: Optional[pathlib.Path]) -> dict[str, str]:
    """Snapshot working-tree state at run end for CXDB + log visibility.

    Returns dict with keys ``uncommitted_files``, ``uncommitted_insertions``,
    ``uncommitted_deletions``, ``uncommitted_staged_files``. Empty strings
    when ``workdir`` is falsy, when the directory is not a git repo, or when
    the git subprocess is unavailable.

    The "staged" count is intentionally the count of `??`-prefixed lines
    in ``git status --porcelain`` (untracked files). Truly-staged files
    (in the index but not committed) are also part of "uncommitted work"
    in the operator's mental model, and ``git status --porcelain`` reports
    them with leading letters; we count all non-empty porcelain lines as
    "uncommitted files" and split untracked out as the staged count for
    backwards compat with existing operator workflows.

    Failures are silent — observability helpers must never break a run.
    """
    empty: dict[str, str] = {
        "uncommitted_files": "",
        "uncommitted_insertions": "",
        "uncommitted_deletions": "",
        "uncommitted_staged_files": "",
    }
    if not workdir:
        return empty
    try:
        wd = str(workdir)
        # Is it a git repo? `git -C <wd> rev-parse --is-inside-work-tree`
        # exits 0 + prints "true" only inside a work tree.
        try:
            repo_check = subprocess.run(
                ["git", "-C", wd, "rev-parse", "--is-inside-work-tree"],
                capture_output=True,
                text=True,
                timeout=5,
                check=False,
                stdin=subprocess.DEVNULL,
            )
        except (OSError, subprocess.TimeoutExpired):
            return empty
        if repo_check.returncode != 0 or repo_check.stdout.strip() != "true":
            return empty

        # Untracked file count via porcelain (lines starting with "??")
        try:
            status_proc = subprocess.run(
                ["git", "-C", wd, "status", "--porcelain"],
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
                stdin=subprocess.DEVNULL,
            )
        except (OSError, subprocess.TimeoutExpired):
            return {**empty, "uncommitted_files": "0", "uncommitted_staged_files": "0"}
        if status_proc.returncode != 0:
            return {**empty, "uncommitted_files": "0", "uncommitted_staged_files": "0"}
        status_lines = [ln for ln in status_proc.stdout.splitlines() if ln.strip()]
        staged_files = sum(1 for ln in status_lines if ln.startswith("??"))

        # Insertion / deletion shortstat — capture BOTH unstaged (worktree
        # vs index) AND staged (index vs HEAD) changes. A coder who has
        # `git add`'d work but not committed has real, recoverable work
        # sitting in the worktree; counting only unstaged diffs would
        # silently understate the uncommitted LOC.
        insertions_total = 0
        deletions_total = 0
        diff_succeeded = True
        for diff_args in (["diff"], ["diff", "--cached"]):
            try:
                diff_proc = subprocess.run(
                    ["git", "-C", wd, *diff_args, "--shortstat"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                    stdin=subprocess.DEVNULL,
                )
            except (OSError, subprocess.TimeoutExpired):
                diff_succeeded = False
                continue
            if diff_proc.returncode != 0:
                diff_succeeded = False
                continue
            if not diff_proc.stdout.strip():
                continue
            m = _DIFF_SHORTSTAT_RE.search(diff_proc.stdout)
            if not m:
                continue
            if m.group("ins"):
                try:
                    insertions_total += int(m.group("ins"))
                except (TypeError, ValueError):
                    pass
            if m.group("del"):
                try:
                    deletions_total += int(m.group("del"))
                except (TypeError, ValueError):
                    pass
        if not diff_succeeded and not status_lines:
            # We at least got a status call back; surface that.
            return {
                "uncommitted_files": str(len(status_lines)),
                "uncommitted_insertions": "",
                "uncommitted_deletions": "",
                "uncommitted_staged_files": str(staged_files),
            }
        return {
            "uncommitted_files": str(len(status_lines)),
            "uncommitted_insertions": str(insertions_total) if insertions_total else "",
            "uncommitted_deletions": str(deletions_total) if deletions_total else "",
            "uncommitted_staged_files": str(staged_files),
        }
    except Exception:
        # Belt + braces: a stray AttributeError or unicode issue must
        # never reach the run loop.
        return empty


def _format_uncommitted_for_log(state: dict[str, str]) -> str:
    """Build a compact human-readable fragment for the RUN_END log line.

    Returns the empty string when there is no uncommitted work, so the
    caller can choose whether to append anything at all.
    """
    files = state.get("uncommitted_files", "") or "0"
    if files == "" or files == "0":
        return ""
    ins = state.get("uncommitted_insertions", "") or "0"
    dele = state.get("uncommitted_deletions", "") or "0"
    staged = state.get("uncommitted_staged_files", "") or "0"
    return f"uncommitted={files} files +{ins}/-{dele} staged={staged}"


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


def _normalize_outcome_only(value: str) -> str:
    return _classify_outcome(value)


def _classify_records(records: list["object"]) -> tuple[int, int, int]:
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


def _normalized_result(result) -> "object":
    """Idempotently normalize Result.outcome via the classifier.

    Imported lazily here because `Result` is a sibling runtime symbol whose
    shape we only need to mutate.
    """
    outcome = _normalize_outcome_only(result.outcome)
    if outcome == result.outcome:
        return result
    result.outcome = outcome
    return result
