"""Performance logging — repo/branch-organized enter/exit observability under /tmp.

Best-effort I/O only; failures never abort a pipeline run.
"""

from __future__ import annotations

import json
import pathlib
import re
import subprocess
import time
from dataclasses import dataclass, field
from typing import Optional, TextIO


def _safe_slug(name: str) -> str:
    """Filesystem-safe slug for repo/branch directory names."""
    return re.sub(r"[^A-Za-z0-9._-]+", "_", name)[:64] or "unknown"


def _classify_outcome(raw: str) -> str:
    """Normalize diverse outcomes to engine taxonomy (duplicated to avoid circular imports)."""
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


def _is_success_outcome(raw: str) -> bool:
    return _classify_outcome(raw) == "success"


@dataclass
class GitContext:
    repo_slug: str
    branch_slug: str
    head_sha: Optional[str]
    workdir: pathlib.Path


@dataclass
class PerfRun:
    run_id: str
    git_ctx: GitContext
    run_dir: pathlib.Path
    jsonl_path: pathlib.Path
    log_path: pathlib.Path
    jsonl_handle: Optional[TextIO] = None
    log_handle: Optional[TextIO] = None
    node_enter_ts: dict[str, float] = field(default_factory=dict)
    pipeline: str = ""
    goal: str = ""
    backend: str = "echo"
    run_start_mono: float = 0.0
    run_start_ts: float = 0.0


def _git_cmd(workdir: pathlib.Path, *args: str) -> Optional[str]:
    try:
        proc = subprocess.run(
            ["git", "-C", str(workdir), *args],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    value = proc.stdout.strip()
    return value or None


def _parse_repo_name(remote_url: str) -> str:
    url = remote_url.rstrip("/")
    if url.endswith(".git"):
        url = url[:-4]
    if ":" in url and "@" in url.split(":")[0]:
        # scp-style: git@github.com:org/repo
        return url.split(":")[-1].split("/")[-1]
    return url.split("/")[-1] or "unknown"


def resolve_git_context(
    workdir: pathlib.Path,
    state: Optional[dict[str, str]] = None,
) -> GitContext:
    """Resolve repo/branch identity from workdir, preferring ao.worktree when set."""
    state = state or {}
    target = workdir
    ao_worktree = state.get("ao.worktree", "").strip()
    if ao_worktree:
        target = pathlib.Path(ao_worktree).expanduser().resolve()

    repo_slug = _safe_slug(target.name)
    branch_slug = "unknown"
    head_sha: Optional[str] = None

    branch_raw = _git_cmd(target, "rev-parse", "--abbrev-ref", "HEAD")
    if branch_raw:
        branch_slug = _safe_slug(branch_raw)

    remote_url = _git_cmd(target, "remote", "get-url", "origin")
    if remote_url:
        repo_slug = _safe_slug(_parse_repo_name(remote_url))

    sha_raw = _git_cmd(target, "rev-parse", "HEAD")
    if sha_raw and len(sha_raw) == 40 and all(c in "0123456789abcdef" for c in sha_raw.lower()):
        head_sha = sha_raw.lower()

    return GitContext(
        repo_slug=repo_slug,
        branch_slug=branch_slug,
        head_sha=head_sha,
        workdir=target,
    )


def run_dir(root: pathlib.Path, git_ctx: GitContext) -> pathlib.Path:
    return root / git_ctx.repo_slug / git_ctx.branch_slug


def _write_json(perf: PerfRun, record: dict[str, str]) -> None:
    if perf.jsonl_handle is None:
        return
    try:
        base = {
            "ts": str(time.time()),
            "run_id": perf.run_id,
            "repo": perf.git_ctx.repo_slug,
            "branch": perf.git_ctx.branch_slug,
            "head_sha": perf.git_ctx.head_sha or "",
            "pipeline": perf.pipeline,
            "backend": perf.backend,
        }
        base.update(record)
        perf.jsonl_handle.write(json.dumps(base, sort_keys=True) + "\n")
        perf.jsonl_handle.flush()
    except (OSError, ValueError):
        pass


def _log_line(perf: PerfRun, message: str) -> None:
    if perf.log_handle is None:
        return
    try:
        perf.log_handle.write(f"[{time.time():.3f}] {message}\n")
        perf.log_handle.flush()
    except (OSError, ValueError):
        pass


def _update_symlink(link_path: pathlib.Path, target: pathlib.Path) -> None:
    try:
        if link_path.is_symlink() or link_path.exists():
            link_path.unlink()
        link_path.symlink_to(target.name)
    except OSError:
        pass


def open_run(
    root: pathlib.Path,
    git_ctx: GitContext,
    run_id: str,
    *,
    pipeline: str,
    goal: str,
    backend: str,
) -> PerfRun:
    """Open per-run JSONL + text log under root/<repo>/<branch>/."""
    directory = run_dir(root, git_ctx)
    directory.mkdir(parents=True, exist_ok=True)

    jsonl_path = directory / f"{run_id}.jsonl"
    log_path = directory / f"{run_id}.log"

    perf = PerfRun(
        run_id=run_id,
        git_ctx=git_ctx,
        run_dir=directory,
        jsonl_path=jsonl_path,
        log_path=log_path,
        pipeline=pipeline,
        goal=goal,
        backend=backend,
        run_start_mono=time.monotonic(),
        run_start_ts=time.time(),
    )

    try:
        perf.jsonl_handle = jsonl_path.open("a", encoding="utf-8")
    except OSError:
        perf.jsonl_handle = None

    try:
        perf.log_handle = log_path.open("a", encoding="utf-8")
    except OSError:
        perf.log_handle = None

    _update_symlink(directory / "latest.jsonl", jsonl_path)
    _update_symlink(directory / "latest.log", log_path)

    _write_json(perf, {
        "event": "run_start",
        "goal": goal,
    })
    _log_line(
        perf,
        f"RUN_START run_id={run_id} pipeline={pipeline!r} goal={goal!r} "
        f"repo={git_ctx.repo_slug} branch={git_ctx.branch_slug} backend={backend}",
    )
    return perf


def node_enter(
    perf: Optional[PerfRun],
    *,
    node: str,
    seq: int,
    node_type: str = "",
    visit: int = 1,
    backend: str = "",
) -> None:
    if perf is None:
        return
    key = f"{node}:{seq}:{visit}"
    perf.node_enter_ts[key] = time.monotonic()
    effective_backend = backend or perf.backend
    _write_json(perf, {
        "event": "node_enter",
        "node": node,
        "seq": str(seq),
        "node_type": node_type,
        "visit": str(visit),
        "backend": effective_backend,
    })
    type_part = f" type={node_type}" if node_type else ""
    _log_line(
        perf,
        f"ENTER node={node!r} seq={seq} visit={visit} backend={effective_backend}{type_part}",
    )


def node_exit(
    perf: Optional[PerfRun],
    *,
    node: str,
    seq: int,
    raw_outcome: str,
    visit: int = 1,
    metadata: Optional[dict[str, str]] = None,
) -> None:
    if perf is None:
        return
    key = f"{node}:{seq}:{visit}"
    enter_mono = perf.node_enter_ts.pop(key, None)
    duration_ms = ""
    if enter_mono is not None:
        duration_ms = str(int((time.monotonic() - enter_mono) * 1000))

    classified = _classify_outcome(raw_outcome)
    meta = metadata or {}
    payload: dict[str, str] = {
        "event": "node_exit",
        "node": node,
        "seq": str(seq),
        "outcome": classified,
        "raw_outcome": raw_outcome,
        "success": "true" if _is_success_outcome(raw_outcome) else "false",
        "visit": str(visit),
    }
    if duration_ms:
        payload["duration_ms"] = duration_ms
    for k, v in meta.items():
        payload[f"meta_{k}"] = str(v)

    _write_json(perf, payload)
    dur_part = f" duration_ms={duration_ms}" if duration_ms else ""
    _log_line(
        perf,
        f"EXIT node={node!r} seq={seq} outcome={classified} raw={raw_outcome!r}{dur_part}",
    )


def transition(
    perf: Optional[PerfRun],
    *,
    from_node: str,
    to_node: str,
    outcome: str,
    seq: int,
) -> None:
    if perf is None:
        return
    classified = _classify_outcome(outcome)
    _write_json(perf, {
        "event": "transition",
        "from": from_node,
        "to": to_node,
        "outcome": classified,
        "seq": str(seq),
    })
    _log_line(
        perf,
        f"TRANSITION from={from_node!r} to={to_node!r} outcome={classified} seq={seq}",
    )


def close_run(
    perf: Optional[PerfRun],
    *,
    final_outcome: str,
    steps: int,
    success_count: int,
    failure_count: int,
    error_count: int,
) -> None:
    if perf is None:
        return

    total_duration_ms = str(int((time.monotonic() - perf.run_start_mono) * 1000))
    classified = _classify_outcome(final_outcome)

    _write_json(perf, {
        "event": "run_end",
        "final_outcome": classified,
        "raw_final_outcome": final_outcome,
        "total_duration_ms": total_duration_ms,
        "steps": str(steps),
        "success_count": str(success_count),
        "failure_count": str(failure_count),
        "error_count": str(error_count),
    })
    _log_line(
        perf,
        f"RUN_END final={classified} steps={steps} duration_ms={total_duration_ms} "
        f"success={success_count} failure={failure_count} error={error_count}",
    )

    index_path = perf.run_dir / "runs.index.jsonl"
    try:
        summary = {
            "ts": time.time(),
            "run_id": perf.run_id,
            "repo": perf.git_ctx.repo_slug,
            "branch": perf.git_ctx.branch_slug,
            "head_sha": perf.git_ctx.head_sha or "",
            "pipeline": perf.pipeline,
            "goal": perf.goal,
            "backend": perf.backend,
            "final_outcome": classified,
            "raw_final_outcome": final_outcome,
            "duration_ms": int(total_duration_ms) if total_duration_ms else 0,
            "steps": steps,
            "success": success_count,
            "failure": failure_count,
            "error": error_count,
        }
        index_path.open("a", encoding="utf-8").write(json.dumps(summary, sort_keys=True) + "\n")
    except OSError:
        pass

    for handle in (perf.jsonl_handle, perf.log_handle):
        if handle is not None:
            try:
                handle.close()
            except OSError:
                pass
    perf.jsonl_handle = None
    perf.log_handle = None
