"""File-domain locking for Dark Factory runs that mutate a target repo.

Goal
----
Prevent two ``bin/dark-factory`` invocations from editing the same
target repo at the same time. The lock is a sentinel file under
``~/.dark-factory/merge_train/`` whose JSON contents identify the
holding ``run_id`` and the ``acquired_at`` timestamp.

API
---
- :func:`acquire`  — atomically create a lock; raise :class:`LockBusy`
  if another run holds a non-stale lock.
- :func:`release`  — remove the lock; idempotent; verifies the caller
  still owns it (matches the recorded ``run_id``).
- :func:`is_locked` — report whether a target repo is currently held.

Stale-lock recovery
-------------------
A lock whose ``acquired_at`` is older than :data:`STALE_AFTER` (default
1 hour) is treated as expired and is silently re-acquired. This is the
only way a run can "steal" a lock — useful when the previous holder
crashed without releasing.

Layout
------
Lock files live at ``<lock_dir>/<sanitized_target_repo>.lock`` where
``sanitized_target_repo`` is the target repo string with every
non-``[A-Za-z0-9_-]`` character replaced by ``_``. The file's contents
are a JSON object: ``{"run_id": ..., "acquired_at": ...}``.

The module is also runnable as ``python -m runner._merge_train`` so the
bash wrappers in ``bin/`` can call acquire / release without importing
Python from the runner itself.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import pathlib
import re
import sys
import time
from dataclasses import dataclass
from typing import Optional

# Default lock directory under the user's home. Tests override per-call
# via the ``lock_dir`` argument so they never touch the real directory.
DEFAULT_LOCK_DIR = pathlib.Path.home() / ".dark-factory" / "merge_train"

# Locks older than this are considered stale and may be reclaimed.
STALE_AFTER = _dt.timedelta(hours=1)

# Characters that are safe inside a lock filename. Anything else is
# collapsed to '_' so absolute paths, spaces, and shell metacharacters
# from the target_repo string never reach the filesystem unchecked.
_SAFE_NAME_RE = re.compile(r"[^A-Za-z0-9_-]")


class LockBusy(RuntimeError):
    """Raised by :func:`acquire` when another run already holds the lock.

    Attributes
    ----------
    target_repo:
        The target repo string that could not be locked.
    held_by:
        The ``run_id`` recorded in the conflicting lock file (may be
        ``None`` if the file's contents could not be parsed).
    """

    def __init__(self, target_repo: str, held_by: Optional[str]) -> None:
        self.target_repo = target_repo
        self.held_by = held_by
        super().__init__(
            f"target_repo {target_repo!r} is locked by run_id={held_by!r}"
        )


@dataclass(frozen=True)
class Lock:
    """A successfully-acquired lock.

    The :class:`dataclass` is frozen so a caller cannot mutate the
    recorded ``run_id`` and accidentally release a lock they no longer
    own. ``path`` is the absolute filesystem path of the lock file.
    """

    target_repo: str
    run_id: str
    acquired_at: str
    path: pathlib.Path


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------


def _sanitize(target_repo: str) -> str:
    """Return a filesystem-safe filename for ``target_repo``.

    Examples
    --------
    >>> _sanitize("/Users/foo/bar")
    '_Users_foo_bar'
    >>> _sanitize("../etc/passwd")
    '____etc_passwd'
    >>> _sanitize("my-repo_v2")
    'my-repo_v2'
    """
    return _SAFE_NAME_RE.sub("_", target_repo)


def _lock_path(target_repo: str, lock_dir: pathlib.Path) -> pathlib.Path:
    return lock_dir / f"{_sanitize(target_repo)}.lock"


def _reserve_tmp_path(final_path: pathlib.Path) -> pathlib.Path:
    """Return a unique sibling temp path for the atomic write-then-link.

    The temp file lives in the same directory as the final lock so
    ``os.link`` is a same-filesystem operation (POSIX only guarantees
    link(2) atomicity on the same filesystem). Uniqueness comes from
    pid + a thread-safe monotonic counter; collisions are resolved by
    the O_EXCL retry in :func:`acquire`.
    """
    import threading as _threading

    state = getattr(_reserve_tmp_path, "_state", None)
    if state is None:
        state = (_threading.Lock(), 0)
        _reserve_tmp_path._state = state
    lock, n = state
    with lock:
        n = state[1] + 1
        _reserve_tmp_path._state = (lock, n)
    suffix = f".{os.getpid()}.{n}.tmp"
    return final_path.with_name(final_path.name + suffix)


def _now_utc() -> _dt.datetime:
    """Return the current UTC time as a timezone-aware datetime."""
    return _dt.datetime.now(_dt.timezone.utc)


def _parse_timestamp(value: str) -> Optional[_dt.datetime]:
    """Parse an ISO-8601 UTC timestamp; return ``None`` on failure.

    Accepts the ``+00:00`` offset form produced by
    :meth:`datetime.isoformat` for ``timezone.utc`` datetimes. Naive
    datetimes are treated as UTC for forward compatibility with callers
    that wrote a bare ``Z``-suffixed value.
    """
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = _dt.datetime.fromisoformat(value)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=_dt.timezone.utc)
    return parsed


def _read_existing(path: pathlib.Path) -> Optional[dict]:
    """Best-effort JSON read of ``path``; return ``None`` on any error.

    A corrupt or partially-written lock file is treated the same as a
    missing file for staleness decisions — :func:`acquire` will then
    fall through to the reclaim path. The error is swallowed here
    because the only way a corrupt file could have landed is via a
    previous crashed writer, and the reclaim path is the correct
    recovery.
    """
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    except OSError:
        return None
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return None
    if not isinstance(data, dict):
        return None
    return data


def _is_stale(data: Optional[dict], now: _dt.datetime) -> bool:
    """Return True iff ``data`` is a fresh-and-valid lock older than the cutoff.

    Conservative by design: a missing dict, a missing ``acquired_at``, or
    an unparseable timestamp is treated as NOT-stale. The reasoning is
    the in-flight race observed under heavy contention — a concurrent
    writer may briefly present the file as empty / unparseable while
    its ``os.write`` is in flight, and a "stale-or-not" decision made
    against that empty state would let a second writer steal the lock.
    We avoid that by requiring a parseable timestamp; a truly crashed
    lock (no acquired_at, or garbage) blocks the next acquire until
    the operator manually removes the file, which is the correct
    fail-safe for an ambiguous state.
    """
    if not data:
        return False
    acquired_at = _parse_timestamp(data.get("acquired_at", ""))
    if acquired_at is None:
        return False
    return (now - acquired_at) >= STALE_AFTER


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------


def acquire(
    target_repo: str,
    run_id: str,
    lock_dir: Optional[pathlib.Path] = None,
    *,
    now: Optional[_dt.datetime] = None,
) -> Lock:
    """Atomically acquire the lock for ``target_repo``.

    Creates ``<lock_dir>/<sanitized>.lock`` with JSON contents
    ``{"run_id": run_id, "acquired_at": <iso8601-utc>}``. If the file
    already exists and its ``acquired_at`` is within the stale window,
    raises :class:`LockBusy` with the recorded holder's run id. If the
    existing file is stale (or its contents are unparsable), it is
    removed and a fresh lock is taken.

    Parameters
    ----------
    target_repo:
        Opaque string identifying the target repo (typically the
        ``--workdir`` the operator passed to ``bin/dark-factory``).
        Sanitized for use as a filename.
    run_id:
        Unique identifier for this run. ``bin/dark-factory`` generates
        one per invocation; tests pass any string.
    lock_dir:
        Override the lock directory. Defaults to
        :data:`DEFAULT_LOCK_DIR`. Tests should pass a ``tmp_path``-
        rooted directory to avoid touching the real filesystem.
    now:
        Override the current time. Tests pass a controlled value to
        exercise the stale-reclaim path without sleeping.

    Returns
    -------
    :class:`Lock` describing the held lock.

    Raises
    ------
    LockBusy
        Another non-stale run holds the lock.
    OSError
        Filesystem failure that is not the expected ``EEXIST`` race
        (e.g. permission denied on the parent directory).
    """
    base = pathlib.Path(lock_dir) if lock_dir is not None else DEFAULT_LOCK_DIR
    base.mkdir(parents=True, exist_ok=True)
    path = _lock_path(target_repo, base)
    timestamp = (now or _now_utc()).astimezone(_dt.timezone.utc)
    payload = {"run_id": run_id, "acquired_at": timestamp.isoformat()}
    payload_bytes = json.dumps(payload, sort_keys=True).encode("utf-8")

    # Atomicity strategy: write the JSON to a sibling temp file, fsync
    # it, then use ``os.link(tmp, final)`` as the "publish or fail"
    # step. ``os.link`` is POSIX-guaranteed to fail with
    # ``FileExistsError`` if the final path exists, and to create a
    # hard link otherwise — both atomically. This is the key invariant:
    # a reader who sees ``final`` existing will see a fully-written
    # payload, never an empty file. The previous ``O_EXCL`` probe
    # approach created an empty file that was visible to readers for
    # the duration of the temp-write + fsync + close window, which
    # broke the loser-thread reads under contention (the Skeptic CI
    # gate caught this: ``exc.held_by == None`` in the concurrent test).
    tmp_path = _reserve_tmp_path(path)
    for _attempt in range(3):
        try:
            tmp_fd = os.open(str(tmp_path), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
            break
        except FileExistsError:
            tmp_path = _reserve_tmp_path(path)
    else:
        raise OSError(f"could not reserve temp lock path near {path}")
    try:
        os.write(tmp_fd, payload_bytes)
        os.fsync(tmp_fd)
    except BaseException:
        os.close(tmp_fd)
        try:
            tmp_path.unlink()
        except OSError:
            pass
        raise
    os.close(tmp_fd)

    try:
        os.link(tmp_path, path)
    except FileExistsError:
        # Another live holder has the lock. Reclaim-stale decides
        # between "fresh holder, raise LockBusy" and "stale, take
        # over". The temp file is now an orphan since we never
        # published.
        try:
            tmp_path.unlink()
        except OSError:
            pass
        return _reclaim_stale(path, target_repo, run_id, payload, now=now)
    except OSError:
        try:
            tmp_path.unlink()
        except OSError:
            pass
        raise

    # Link succeeded: ``path`` and ``tmp_path`` are now two paths to
    # the same inode. Drop the temp link so only ``path`` remains.
    try:
        tmp_path.unlink()
    except OSError:
        pass

    return Lock(
        target_repo=target_repo,
        run_id=run_id,
        acquired_at=payload["acquired_at"],
        path=path,
    )


def _reclaim_stale(
    path: pathlib.Path,
    target_repo: str,
    run_id: str,
    payload: dict,
    *,
    now: Optional[_dt.datetime] = None,
) -> Lock:
    """Handle the ``FileExistsError`` branch of :func:`acquire`.

    Reads the existing file, decides whether it is stale, and either
    raises :class:`LockBusy` (live holder) or replaces the file with a
    fresh lock for the new caller.
    """
    moment = now or _now_utc()
    existing = _read_existing(path)
    if not _is_stale(existing, moment):
        held_by = existing.get("run_id") if existing else None
        raise LockBusy(target_repo, held_by)

    # Stale or unparsable — best-effort unlink then retry. If the
    # unlink fails with ENOENT (someone else just released), the
    # retry's O_EXCL handles the race. If it fails for any other
    # reason, propagate.
    try:
        path.unlink()
    except FileNotFoundError:
        pass

    try:
        fd = os.open(str(path), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
    except FileExistsError:
        # Another racer reclaimed the stale lock between our unlink
        # and re-open. Treat them as the new holder.
        existing = _read_existing(path)
        held_by = existing.get("run_id") if existing else None
        raise LockBusy(target_repo, held_by) from None

    try:
        os.write(fd, json.dumps(payload, sort_keys=True).encode("utf-8"))
    except:
        os.close(fd)
        path.unlink(missing_ok=True)
        raise
    else:
        os.close(fd)

    return Lock(
        target_repo=target_repo,
        run_id=run_id,
        acquired_at=payload["acquired_at"],
        path=path,
    )


def release(lock: Lock) -> None:
    """Release a lock previously returned by :func:`acquire`.

    Idempotent: releasing a lock whose file is already gone is a no-op.
    If the file exists but its recorded ``run_id`` does not match the
    lock's ``run_id``, the call refuses to delete it (returns silently)
    so a stale reclaim by another run does not get clobbered by a
    late-arriving release from the previous holder.
    """
    path = lock.path
    try:
        existing = _read_existing(path)
    except OSError:
        return
    if existing is None:
        return  # already released
    if existing.get("run_id") != lock.run_id:
        return  # someone else owns it now; do not touch

    try:
        path.unlink()
    except FileNotFoundError:
        return  # raced with another releaser; idempotent


def is_locked(
    target_repo: str,
    lock_dir: Optional[pathlib.Path] = None,
    *,
    now: Optional[_dt.datetime] = None,
) -> bool:
    """Return True if ``target_repo`` is held by a non-stale lock."""
    base = pathlib.Path(lock_dir) if lock_dir is not None else DEFAULT_LOCK_DIR
    path = _lock_path(target_repo, base)
    moment = now or _now_utc()
    if not path.exists():
        return False
    return not _is_stale(_read_existing(path), moment)


# ---------------------------------------------------------------------------
# CLI: ``python -m runner._merge_train {acquire,release,is-locked} ...``
# ---------------------------------------------------------------------------


def main(argv: Optional[list[str]] = None) -> int:
    """CLI entry point used by ``bin/dark-factory``.

    Subcommands
    -----------
    ``acquire``   — create a lock; exit 0 on success, 3 on busy,
                   1 on usage / filesystem error.
    ``release``   — release a previously-acquired lock; always exits 0
                   (idempotent, swallows errors so the EXIT trap stays
                   a fire-and-forget cleanup).
    ``is-locked`` — exit 0 if locked, 1 if not, 2 on usage error.
    """
    parser = argparse.ArgumentParser(
        prog="runner._merge_train",
        description="File-domain lock for dark-factory target repos.",
    )
    parser.add_argument(
        "--lock-dir",
        type=pathlib.Path,
        default=None,
        help=f"Override lock dir (default: {DEFAULT_LOCK_DIR})",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    acq = sub.add_parser("acquire", help="Acquire a lock for a target repo")
    acq.add_argument("--target", required=True, help="Target repo identifier")
    acq.add_argument("--run-id", required=True, help="Unique run identifier")

    rel = sub.add_parser("release", help="Release a previously-acquired lock")
    rel.add_argument("--target", required=True, help="Target repo identifier")
    rel.add_argument("--run-id", required=True, help="Run id that holds the lock")

    chk = sub.add_parser("is-locked", help="Check whether a target repo is locked")
    chk.add_argument("--target", required=True, help="Target repo identifier")

    args = parser.parse_args(argv)
    lock_dir = args.lock_dir

    if args.command == "acquire":
        try:
            lock = acquire(args.target, args.run_id, lock_dir=lock_dir)
        except LockBusy as exc:
            print(
                f"merge_train: {exc.target_repo!r} is held by run_id={exc.held_by!r}",
                file=sys.stderr,
            )
            return 3
        # Machine-friendly line for the bash wrapper to extract. Print
        # path + run_id so a future verbose mode can use it; today
        # the wrapper ignores stdout and only inspects the exit code.
        print(f"acquired target={lock.target_repo} run_id={lock.run_id} path={lock.path}")
        return 0

    if args.command == "release":
        # Reconstruct a Lock so the public ``release`` API is the only
        # release path. We don't have acquired_at available; release()
        # does not need it (it only checks run_id and unlinks).
        base = pathlib.Path(lock_dir) if lock_dir is not None else DEFAULT_LOCK_DIR
        path = _lock_path(args.target, base)
        lock = Lock(
            target_repo=args.target,
            run_id=args.run_id,
            acquired_at="",
            path=path,
        )
        release(lock)
        return 0

    if args.command == "is-locked":
        return 0 if is_locked(args.target, lock_dir=lock_dir) else 1

    parser.error(f"unknown command: {args.command}")
    return 2  # unreachable: parser.error calls SystemExit


if __name__ == "__main__":
    raise SystemExit(main())
