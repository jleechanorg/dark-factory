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
import re
import stat
import subprocess
import threading
import time
import traceback
import uuid
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
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

_CONTROLLER_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
_CONTROLLER_SNAPSHOT_JOURNAL = "controller-snapshot-journal.json"
_RUN_ID_RE = re.compile(r"^[A-Za-z0-9_.-]+$")


def _set_controller_base_sha(ctx: Context, base_sha: str) -> None:
    """Set controller provenance through the runner-owned initialization path."""
    normalized = str(base_sha).strip().lower()
    if not _CONTROLLER_SHA_RE.fullmatch(normalized):
        raise ValueError("controller base SHA must be a full 40-hex revision")
    ctx._controller_base_sha = normalized
    ctx.state["_controller_base_sha"] = normalized


def _seed_controller_base_sha(ctx: Context, graph: Graph) -> None:
    """Bind a cold-review base to the selected target before any worker runs.

    The runner owns this initial provenance. It never asks the target checkout
    to resolve a mutable branch, and public state or graph attributes cannot
    narrow the controller's review range. AO worktrees supplied before
    execution are selected by ``_target_worktree`` and therefore receive the
    same immutable HEAD capture as the ordinary CLI worktree.

    Non-Git workdirs and unavailable HEADs are left unset so non-controller
    pipelines retain their existing behavior; a controller request then fails
    closed at its own validation boundary.
    """
    private_base = getattr(ctx, "_controller_base_sha", None)
    if isinstance(private_base, str) and _CONTROLLER_SHA_RE.fullmatch(private_base.strip().lower()):
        ctx.state["_controller_base_sha"] = private_base.strip().lower()
        return
    try:
        from .handler_core import _target_worktree
        from .handler_sandbox import _holdout_denied_paths
        from .review_controller import ReviewContractError, validate_workspace_path

        raw_target = _target_worktree(ctx)
        holdout_roots = tuple(
            str(pathlib.Path(root).resolve(strict=False))
            for root in _holdout_denied_paths()
        )
        # Validate the lexical path before any target-owned Git operation.
        target = validate_workspace_path(
            str(raw_target),
            holdout_roots=holdout_roots,
        )
        proc = subprocess.run(
            ["git", "-C", str(target), "rev-parse", "HEAD^{commit}"],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
    except (
        OSError,
        subprocess.SubprocessError,
        AttributeError,
        TypeError,
        ReviewContractError,
    ):
        return
    base_sha = proc.stdout.strip().lower() if proc.returncode == 0 else ""
    if _CONTROLLER_SHA_RE.fullmatch(base_sha):
        _set_controller_base_sha(ctx, base_sha)


def _is_controller_graph(graph: Graph) -> bool:
    """Identify graphs whose cold-review contract requires durable provenance."""
    if str(graph.attrs.get("review_contract", "")).strip() == "cold-review-v1":
        return True
    return any(
        str(node.attrs.get("review_contract", "")).strip() == "cold-review-v1"
        for node in graph.nodes.values()
    )


def _controller_snapshot_journal_path(ctx: Context) -> pathlib.Path | None:
    """Return the durable snapshot journal path for this run, if addressable."""
    run_id = str(
        ctx.state.get("_controller_snapshot_journal_run_id")
        or getattr(ctx, "run_id", "")
        or ""
    )
    if not _RUN_ID_RE.fullmatch(run_id):
        return None
    home = pathlib.Path.home()
    try:
        home_info = home.lstat()
    except OSError as exc:
        raise ValueError("controller snapshot journal home is unavailable") from exc
    if (
        not home.is_absolute()
        or stat.S_ISLNK(home_info.st_mode)
        or not stat.S_ISDIR(home_info.st_mode)
        or home_info.st_uid != os.getuid()
        or home_info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
    ):
        raise ValueError("controller snapshot journal home is not private")
    run_dir = home
    for component in (".dark-factory", "runs", run_id):
        run_dir /= component
        try:
            info = run_dir.lstat()
        except FileNotFoundError:
            try:
                run_dir.mkdir(mode=0o700)
            except FileExistsError:
                pass
            info = run_dir.lstat()
        if (
            stat.S_ISLNK(info.st_mode)
            or not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        ):
            raise ValueError(f"controller snapshot journal directory is not private: {run_dir}")
    return run_dir / _CONTROLLER_SNAPSHOT_JOURNAL


def _controller_snapshot_dir_flags() -> int:
    flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        flags |= os.O_DIRECTORY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    return flags


def _validate_controller_snapshot_dir_fd(fd: int, description: str) -> None:
    info = os.fstat(fd)
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid not in {0, os.getuid()}
        or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
    ):
        raise ValueError(f"{description} is not private")


@contextmanager
def _controller_snapshot_journal_directory(
    ctx: Context,
) -> Iterator[tuple[pathlib.Path, int] | None]:
    """Descend to the journal directory using validated, no-follow dirfds."""
    run_id = str(
        ctx.state.get("_controller_snapshot_journal_run_id")
        or getattr(ctx, "run_id", "")
        or ""
    )
    if not _RUN_ID_RE.fullmatch(run_id):
        yield None
        return
    home = pathlib.Path.home()
    try:
        home_info = home.lstat()
    except OSError as exc:
        raise ValueError("controller snapshot journal home is unavailable") from exc
    if (
        not home.is_absolute()
        or stat.S_ISLNK(home_info.st_mode)
        or not stat.S_ISDIR(home_info.st_mode)
        or home_info.st_uid not in {0, os.getuid()}
        or home_info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
    ):
        raise ValueError("controller snapshot journal home is not private")

    directory_fd: int | None = None
    directory_path = home
    try:
        directory_fd = os.open(home, _controller_snapshot_dir_flags())
        _validate_controller_snapshot_dir_fd(directory_fd, "controller snapshot journal home")
        for component in (".dark-factory", "runs", run_id):
            try:
                next_fd = os.open(
                    component,
                    _controller_snapshot_dir_flags(),
                    dir_fd=directory_fd,
                )
            except FileNotFoundError:
                try:
                    os.mkdir(component, 0o700, dir_fd=directory_fd)
                except FileExistsError:
                    pass
                try:
                    next_fd = os.open(
                        component,
                        _controller_snapshot_dir_flags(),
                        dir_fd=directory_fd,
                    )
                except OSError as exc:
                    raise ValueError(
                        f"controller snapshot journal directory {component} is unsafe "
                        "(symlink or non-private)"
                    ) from exc
            except OSError as exc:
                raise ValueError(
                    f"controller snapshot journal directory {component} is unsafe "
                    "(symlink or non-private)"
                ) from exc
            try:
                _validate_controller_snapshot_dir_fd(
                    next_fd, f"controller snapshot journal directory {component}"
                )
            except Exception:
                os.close(next_fd)
                raise
            os.close(directory_fd)
            directory_fd = next_fd
            directory_path /= component
        yield directory_path / _CONTROLLER_SNAPSHOT_JOURNAL, directory_fd
    finally:
        if directory_fd is not None:
            os.close(directory_fd)


def _read_controller_private_json(
    path: pathlib.Path, *, dir_fd: int | None = None
) -> object | None:
    """Read a private JSON file through a validated no-follow descriptor."""
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        if dir_fd is None:
            fd = os.open(path, flags)
        else:
            fd = os.open(path.name, flags, dir_fd=dir_fd)
    except FileNotFoundError:
        return None
    try:
        info = os.fstat(fd)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid not in {0, os.getuid()}
            or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        ):
            raise ValueError("controller snapshot journal is not private")
        chunks: list[bytes] = []
        while True:
            chunk = os.read(fd, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
    finally:
        os.close(fd)
    try:
        return json.loads(b"".join(chunks).decode("utf-8"))
    except (UnicodeError, TypeError, json.JSONDecodeError) as exc:
        raise ValueError("controller private JSON is malformed") from exc


def _read_controller_snapshot_journal(
    path: pathlib.Path, *, dir_fd: int | None = None
) -> list:
    """Read a journal through a no-follow descriptor after validating its file."""
    entries = _read_controller_private_json(path, dir_fd=dir_fd)
    if entries is None:
        return []
    if not isinstance(entries, list):
        raise TypeError("controller snapshot journal is malformed")
    return entries


@contextmanager
def _controller_snapshot_journal_lock(
    ctx: Context,
) -> Iterator[tuple[pathlib.Path, int] | None]:
    """Serialize journal updates with an owner-validated per-run lock."""
    import fcntl

    with _controller_snapshot_journal_directory(ctx) as location:
        if location is None:
            yield None
            return
        path, directory_fd = location
        lock_name = f".{path.name}.lock"
        flags = os.O_RDWR | os.O_CREAT
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        fd = os.open(lock_name, flags, 0o600, dir_fd=directory_fd)
        try:
            info = os.fstat(fd)
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_uid not in {0, os.getuid()}
                or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
            ):
                raise ValueError("controller snapshot journal lock is not private")
            fcntl.flock(fd, fcntl.LOCK_EX)
            yield location
        finally:
            try:
                fcntl.flock(fd, fcntl.LOCK_UN)
            finally:
                os.close(fd)


def _write_controller_private_json(
    path: pathlib.Path, entries: object, *, dir_fd: int | None = None
) -> None:
    """Atomically replace private JSON and flush file plus directory metadata."""
    if dir_fd is None:
        if path.is_symlink():
            raise ValueError("controller snapshot journal is a symlink")
    else:
        try:
            if stat.S_ISLNK(os.stat(path.name, dir_fd=dir_fd, follow_symlinks=False).st_mode):
                raise ValueError("controller snapshot journal is a symlink")
        except FileNotFoundError:
            pass
    payload = (json.dumps(entries, sort_keys=True, separators=(",", ":")) + "\n").encode(
        "utf-8"
    )
    owns_directory_fd = dir_fd is None
    directory_fd = (
        os.open(path.parent, _controller_snapshot_dir_flags()) if owns_directory_fd else dir_fd
    )
    temporary_name = f".{path.name}.{uuid.uuid4().hex}.tmp"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd: int | None = None
    try:
        try:
            fd = os.open(temporary_name, flags, 0o600, dir_fd=directory_fd)
            view = memoryview(payload)
            while view:
                written = os.write(fd, view)
                view = view[written:]
            os.fsync(fd)
        finally:
            if fd is not None:
                os.close(fd)
        os.replace(
            temporary_name,
            path.name,
            src_dir_fd=directory_fd,
            dst_dir_fd=directory_fd,
        )
        os.fsync(directory_fd)
    except Exception:
        try:
            os.unlink(temporary_name, dir_fd=directory_fd)
        except OSError:
            pass
        raise
    finally:
        if owns_directory_fd:
            os.close(directory_fd)


def _write_controller_snapshot_journal(
    path: pathlib.Path, entries: list, *, dir_fd: int | None = None
) -> None:
    """Atomically replace a journal and flush both file and directory metadata."""
    _write_controller_private_json(path, entries, dir_fd=dir_fd)


def _persist_controller_snapshot_journal(
    ctx: Context, *, remove_entry: dict | None = None
) -> None:
    """Merge and persist the controller snapshot journal under its run lock."""
    raw = ctx.state.get("_controller_review_snapshots", "[]")
    try:
        entries = json.loads(raw)
    except (TypeError, json.JSONDecodeError) as exc:
        raise ValueError("controller snapshot state is malformed") from exc
    if not isinstance(entries, list):
        raise TypeError("controller snapshot state is malformed")
    with _controller_snapshot_journal_lock(ctx) as location:
        if location is None:
            return
        path, dir_fd = location
        merged = _read_controller_snapshot_journal(path, dir_fd=dir_fd)
        for entry in entries:
            if entry not in merged:
                merged.append(entry)
        if remove_entry is not None:
            merged = [entry for entry in merged if entry != remove_entry]
        ctx.state["_controller_review_snapshots"] = json.dumps(
            merged, sort_keys=True, separators=(",", ":")
        )
        _write_controller_snapshot_journal(path, merged, dir_fd=dir_fd)


def _load_controller_snapshot_journal(ctx: Context) -> None:
    """Restore pending snapshots from disk before resume or early return."""
    try:
        with _controller_snapshot_journal_lock(ctx) as location:
            if location is None:
                return
            path, dir_fd = location
            disk_entries = _read_controller_snapshot_journal(path, dir_fd=dir_fd)
    except (OSError, TypeError, ValueError):
        return
    raw = ctx.state.get("_controller_review_snapshots", "[]")
    try:
        local_entries = json.loads(raw) if raw not in (None, "") else []
    except (TypeError, json.JSONDecodeError):
        local_entries = []
    if not isinstance(local_entries, list):
        local_entries = []
    entries = list(disk_entries)
    for entry in local_entries:
        if entry not in entries:
            entries.append(entry)
    if entries:
        ctx.state["_controller_review_snapshots"] = json.dumps(
            entries, sort_keys=True, separators=(",", ":")
        )


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

# Time decay (rev-vl3zr): a streak of N exhausted runs caused by a
# transient condition (e.g. upstream LLM quota exhaustion) looks
# identical in CXDB to N genuinely-stuck runs. Without decay, the breaker
# stays tripped forever even after the quota resets. Every
# CB_DECAY_HALF_LIFE_SECS of idle time since the most recent exhausted
# run, the effective streak count is halved — so a long-enough gap
# (default 30 min) drops the effective streak below CB_THRESHOLD and lets
# the next dispatch proceed. Set DARK_FACTORY_CROSS_RUN_CIRCUIT_DECAY_HALF_LIFE_SECS=0
# to disable decay (streak never decays, matching pre-rev-vl3zr behavior).
CB_DECAY_HALF_LIFE_SECS = float(
    os.environ.get("DARK_FACTORY_CROSS_RUN_CIRCUIT_DECAY_HALF_LIFE_SECS", "1800")
)


def _decayed_exhausted_streak(
    streak_count: int,
    most_recent_ended_ts: Optional[float],
    now: Optional[float] = None,
) -> float:
    """Apply idle-time decay to a cross-run exhausted streak.

    Returns ``streak_count`` unchanged when there's nothing to decay
    against (no streak, no timestamp, decay disabled, or less than one
    full half-life of idle time has elapsed). Otherwise halves the
    effective streak for every *complete* ``CB_DECAY_HALF_LIFE_SECS`` of
    idle time elapsed since ``most_recent_ended_ts``.

    Decay is stepped (not continuous) so that ordinary sub-second
    scheduling jitter between ``end_run`` and the next dispatch's
    breaker check never nudges a genuine same-instant streak below the
    integer threshold — the original v4 protection must still fire for
    back-to-back exhaustion with no meaningful idle gap.
    """
    if streak_count <= 0 or not most_recent_ended_ts or CB_DECAY_HALF_LIFE_SECS <= 0:
        return float(streak_count)
    now = time.time() if now is None else now
    idle_secs = max(0.0, now - most_recent_ended_ts)
    half_lives_elapsed = int(idle_secs // CB_DECAY_HALF_LIFE_SECS)
    if half_lives_elapsed <= 0:
        return float(streak_count)
    return streak_count / (2.0**half_lives_elapsed)


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
    # Skip auto-WIP commit if running under pytest / test mode to avoid polluting the repo
    import sys
    import os
    if "pytest" in sys.modules or "PYTEST_CURRENT_TEST" in os.environ or os.environ.get("DARK_FACTORY_TESTING") == "1":
        current_test = os.environ.get("PYTEST_CURRENT_TEST", "")
        # Allow specific tests in test_pre_exhaustion_wip_commit to run WIP commits
        if "test_pre_exhaustion_wip_commit" not in current_test and os.environ.get("DARK_FACTORY_ALLOW_WIP_TEST") != "1":
            return
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


def _cleanup_controller_snapshot(ctx: Context) -> None:
    """Remove every exact controller snapshot after the engine owns run end.

    Controller snapshots are detached worktrees created beneath the dedicated
    snapshot root. Keep them alive through review and exit re-pin, then remove
    only the exact validated paths recorded by the controller handler.
    """
    raw_snapshots = ctx.state.get("_controller_review_snapshots")
    if not isinstance(raw_snapshots, str) or not raw_snapshots:
        return
    try:
        entries = json.loads(raw_snapshots)
    except json.JSONDecodeError:
        return
    if not isinstance(entries, list):
        return

    # Use the same no-follow, owner/mode-checked root validator as snapshot
    # creation. If an operator or another process replaced any parent with a
    # symlink, skip cleanup rather than handing that path to Git.
    try:
        from .handler_parallel_reviewer import _controller_snapshot_root
        from .handler_sandbox import _holdout_denied_paths
        from .review_controller import validate_workspace_path

        root = _controller_snapshot_root()
    except (OSError, RuntimeError, ValueError):
        return
    seen: set[tuple[str, str]] = set()
    for entry in entries:
        if (
            not isinstance(entry, dict)
            or set(entry) != {"snapshot_path", "source_worktree"}
            or not isinstance(entry["snapshot_path"], str)
            or not isinstance(entry["source_worktree"], str)
        ):
            continue
        snapshot = pathlib.Path(entry["snapshot_path"])
        # Preserve the submitted spelling separately from any validated,
        # canonical path. The lexical path must be revalidated before each
        # independent Git mutation so an ancestor swap cannot redirect prune.
        source_lexical = pathlib.Path(entry["source_worktree"])
        source = source_lexical
        key = (str(snapshot), str(source))
        if key in seen:
            continue
        seen.add(key)
        if (
            not snapshot.is_absolute()
            or snapshot.parent != root
            or not snapshot.name.startswith("snapshot-")
            or snapshot.is_symlink()
            or not source.is_absolute()
            or source.is_symlink()
            or not source.is_dir()
        ):
            continue
        try:
            if snapshot.resolve(strict=False).parent != root.resolve(strict=False):
                continue
        except OSError:
            continue
        try:
            # Validate the complete lexical source path immediately before
            # every Git operation. Checking only ``source.is_symlink()`` would
            # miss an ancestor being swapped to a symlink.
            source = validate_workspace_path(
                str(source),
                holdout_roots=tuple(str(path) for path in _holdout_denied_paths()),
            )
            remove_result = subprocess.run(
                [
                    "git",
                    "-C",
                    str(source),
                    "worktree",
                    "remove",
                    "--force",
                    str(snapshot),
                ],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            if remove_result.returncode != 0 and snapshot.exists():
                continue
            if snapshot.exists():
                continue
            source_for_prune = validate_workspace_path(
                str(source_lexical),
                holdout_roots=tuple(str(path) for path in _holdout_denied_paths()),
            )
            prune_result = subprocess.run(
                [
                    "git",
                    "-C",
                    str(source_for_prune),
                    "worktree",
                    "prune",
                    "--expire",
                    "now",
                ],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            if prune_result.returncode != 0:
                continue
            _persist_controller_snapshot_journal(ctx, remove_entry=entry)
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError):
            # Cleanup is best-effort and must never mask the terminal result.
            continue


def _extract_coder_handoff(text: str) -> str:
    """Extract ## Coder Handoff section from reviewer output."""
    if not text:
        return ""
    import re
    # Match ## Coder Handoff (case-insensitive) and extract until next markdown header (e.g. ## or #) or end of string.
    pattern = re.compile(
        r"##\s*Coder\s+Handoff\b(.*?)(?=^(?:#+|\s*##)\s|\Z)",
        re.IGNORECASE | re.DOTALL | re.MULTILINE
    )
    match = pattern.search(text)
    if match:
        return match.group(1).strip()
    return ""


def _run_single_node(
    node: Node,
    ctx: Context,
    graph: Graph,
    seq_base: int = 0,
) -> tuple[list[Result], list]:
    _obs._write_heartbeat(ctx, graph, node)
    try:
        handler = resolve(node)

        def _log_input(attempt_index: int) -> dict[str, str]:
            seq = seq_base + attempt_index - 1
            setattr(ctx, "_df_current_seq", seq)
            setattr(ctx, "_df_current_attempt", attempt_index)
            setattr(ctx, "_df_current_node", node.name)
            return _obs._write_node_input_sidecar(ctx, seq, node, attempt_index)

        results = _persist._run_with_retries(
            handler, node, ctx, graph, input_logger=_log_input
        )
        normalized_results: list[Result] = []
        records: list = []
        for attempt in results:
            attempt = _obs._normalized_result(attempt)
            ctx.state.update(attempt.context_updates)
            ctx.state["_last_node"] = node.name
            ctx.state["_last_outcome"] = attempt.outcome
            ctx.state["_last_output"] = attempt.output
            if str(node.attrs.get("class", "")).strip().lower() == "review":
                ctx.state["_last_review_feedback"] = attempt.output
            
            # Surface Coder Handoff section + verdict token (P5)
            verdict = attempt.metadata.get("verdict")
            if verdict:
                ctx.state["_last_verdict"] = verdict
            else:
                ctx.state.pop("_last_verdict", None)

            handoff = _extract_coder_handoff(attempt.output)
            if handoff:
                ctx.state["_last_coder_handoff"] = handoff
            else:
                ctx.state.pop("_last_coder_handoff", None)

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
    controller_graph = _is_controller_graph(graph)
    if resume is not None and controller_graph:
        raise ValueError("resume is not supported for cold-review-v1 graphs")

    # Resume uses the checkpoint's run directory as the durable journal owner.
    # Establish that identity before any terminal resume fast path can return.
    if resume is not None:
        candidate_run_id = pathlib.Path(resume).expanduser().parent.name
        if _RUN_ID_RE.fullmatch(candidate_run_id):
            ctx.state["_controller_snapshot_journal_run_id"] = candidate_run_id
            if ctx.run_id is None:
                ctx.run_id = candidate_run_id
    if not ctx.run_id:
        ctx.run_id = uuid.uuid4().hex[:12]
    if controller_graph or resume is not None:
        _load_controller_snapshot_journal(ctx)

    # Capture the controller-owned base before the first worker visit.  This
    # runs after CLI/AO state has been assembled, so an explicitly selected AO
    # worktree is the target whose immutable HEAD is bound.
    if controller_graph:
        _seed_controller_base_sha(ctx, graph)
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
                _cleanup_controller_snapshot(ctx)
                return history
            if len(history) - _resumed_overhead >= max_steps:
                _cleanup_controller_snapshot(ctx)
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
                    _cleanup_controller_snapshot(ctx)
                    return history
                current = next_node

    # Always have an addressable run_id so diagnostics are locatable even when
    # no CXDB is attached (ad-hoc smoke runs, echo backend, etc.).
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
        run_dir.mkdir(parents=True, mode=0o700, exist_ok=True)
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
                _prior_runs = cxdb.recent_run_finals_with_ts(graph.name, CB_THRESHOLD)
                _prior_finals = [f for f, _ts in _prior_runs]
                if (
                    len(_prior_finals) >= CB_THRESHOLD
                    and all(f == "exhausted" for f in _prior_finals)
                ):
                    # Time decay (rev-vl3zr): a streak that looks stuck can
                    # actually be N transient failures (e.g. upstream quota
                    # exhaustion) separated by idle time. Halve the
                    # effective streak per CB_DECAY_HALF_LIFE_SECS of idle
                    # time since the most recent exhausted run — once it
                    # decays below CB_THRESHOLD, let this run proceed
                    # instead of short-circuiting forever.
                    _most_recent_ts = _prior_runs[0][1]
                    _effective_streak = _decayed_exhausted_streak(
                        len(_prior_finals), _most_recent_ts
                    )
                    if _effective_streak >= CB_THRESHOLD:
                        _idle_secs = (
                            max(0.0, time.time() - _most_recent_ts)
                            if _most_recent_ts
                            else 0.0
                        )
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
                                "effective_streak": f"{_effective_streak:.4f}",
                                "idle_secs": f"{_idle_secs:.1f}",
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
                results, records = _run_single_node(current, ctx, graph, seq)
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
                        b_results, b_records = _run_single_node(
                            target, _branches._clone_context(ctx), graph, b_seq
                        )
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
                        payload.update(_obs._handoff_refs(b_record.metadata))
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
                            payload.update(_obs._handoff_refs(b_record.metadata))
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
                payload.update(_obs._handoff_refs(record.metadata))
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
                if _classify_outcome(result.outcome) == "error":
                    break
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
        _cleanup_controller_snapshot(ctx)

    return history
