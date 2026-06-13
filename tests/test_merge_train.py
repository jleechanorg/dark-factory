"""Tests for runner/_merge_train.py — file-domain locking."""

from __future__ import annotations

import datetime as _dt
import json
import pathlib
import sys
import threading
import time

import pytest

from runner import _merge_train
from runner._merge_train import (
    DEFAULT_LOCK_DIR,
    STALE_AFTER,
    Lock,
    LockBusy,
    acquire,
    is_locked,
    release,
)


# ---------------------------------------------------------------------------
# 1. Lock file format + directory creation
# ---------------------------------------------------------------------------


def test_acquire_creates_lock_file_with_expected_json(tmp_path):
    """First acquire creates the parent dir and writes a parseable JSON payload."""
    target = "/Users/jleechan/projects/some-repo"
    lock_dir = tmp_path / "locks"
    # lock_dir does NOT exist yet — acquire must create it.
    assert not lock_dir.exists()

    lock = acquire(target, "run-abc", lock_dir=lock_dir)

    assert isinstance(lock, Lock)
    assert lock.target_repo == target
    assert lock.run_id == "run-abc"
    # acquired_at must be an ISO-8601 UTC string
    parsed = _dt.datetime.fromisoformat(lock.acquired_at)
    assert parsed.tzinfo is not None
    assert (lock_dir / "_Users_jleechan_projects_some-repo.lock").exists()

    on_disk = json.loads(lock.path.read_text(encoding="utf-8"))
    assert on_disk == {"run_id": "run-abc", "acquired_at": lock.acquired_at}


# ---------------------------------------------------------------------------
# 2. Concurrent acquisition (threading)
# ---------------------------------------------------------------------------


def test_concurrent_acquire_only_one_winner(tmp_path):
    """Two threads racing for the same target: exactly one acquires, the other
    raises :class:`LockBusy`."""
    lock_dir = tmp_path / "locks"
    barrier = threading.Barrier(2)
    results: list = []
    errors: list[BaseException] = []

    def worker(tag: str) -> None:
        try:
            barrier.wait(timeout=5)
            lock = acquire("/repos/race", f"run-{tag}", lock_dir=lock_dir)
            results.append((tag, lock))
        except BaseException as exc:  # noqa: BLE001 — surface in assertion
            errors.append((tag, exc))

    t1 = threading.Thread(target=worker, args=("A",))
    t2 = threading.Thread(target=worker, args=("B",))
    t1.start()
    t2.start()
    t1.join(timeout=10)
    t2.join(timeout=10)
    assert not t1.is_alive() and not t2.is_alive(), "thread hung"

    # Exactly one winner, exactly one LockBusy loser.
    assert len(results) == 1, f"expected 1 winner, got {len(results)}: {results!r}"
    assert len(errors) == 1, f"expected 1 busy, got {len(errors)}: {errors!r}"
    _, exc = errors[0]
    assert isinstance(exc, LockBusy)
    assert exc.target_repo == "/repos/race"
    # The reported holder should be the WINNER's run id.
    winner_tag, winner_lock = results[0]
    assert winner_lock.run_id == f"run-{winner_tag}"
    assert exc.held_by == winner_lock.run_id


# ---------------------------------------------------------------------------
# 3. Stale recovery: > 1h old lock is reclaimable
# ---------------------------------------------------------------------------


def test_stale_lock_is_reclaimed(tmp_path):
    """A lock whose acquired_at is older than STALE_AFTER is treated as expired
    and the next acquire succeeds (without raising LockBusy)."""
    lock_dir = tmp_path / "locks"
    # Pretend the original lock was created 2h ago. ``_merge_train.now`` is
    # overridable via the public ``now=`` kwarg (used here as the time
    # written into the JSON, so the second call's clock can show it as stale).
    ancient = _dt.datetime(2026, 1, 1, 12, 0, 0, tzinfo=_dt.timezone.utc)
    first = acquire("/repos/stale", "run-old", lock_dir=lock_dir, now=ancient)
    assert first.run_id == "run-old"

    # Now (real wall clock, well after the 1h window) acquire again.
    lock = acquire("/repos/stale", "run-new", lock_dir=lock_dir)
    assert lock.run_id == "run-new"

    on_disk = json.loads(lock.path.read_text(encoding="utf-8"))
    assert on_disk["run_id"] == "run-new"


def test_fresh_lock_raises_lockbusy_with_holder(tmp_path):
    """A non-stale lock blocks a second acquire and reports the holder."""
    lock_dir = tmp_path / "locks"
    acquire("/repos/live", "run-holder", lock_dir=lock_dir)

    with pytest.raises(LockBusy) as excinfo:
        acquire("/repos/live", "run-attempt", lock_dir=lock_dir)
    assert excinfo.value.target_repo == "/repos/live"
    assert excinfo.value.held_by == "run-holder"


def test_empty_lock_file_blocks_steal(tmp_path):
    """An empty (zero-byte) or unparseable lock file is NOT auto-stolen.

    The acquire path cannot tell a crashed-corrupt lock from a
    concurrent writer's transient empty state, so the conservative
    choice is to refuse the steal and raise :class:`LockBusy`. The
    operator can remove the file manually if a crash is confirmed.
    """
    lock_dir = tmp_path / "locks"
    lock_dir.mkdir(parents=True, exist_ok=True)
    # Manually drop a zero-byte file in the lock path the next acquire
    # would touch. The acquire must NOT treat this as "stale" and steal.
    target = "/repos/ambiguous"
    path = lock_dir / "_repos_ambiguous.lock"
    path.write_bytes(b"")

    with pytest.raises(LockBusy) as excinfo:
        acquire(target, "run-new", lock_dir=lock_dir)
    # held_by is None because the file is unparseable.
    assert excinfo.value.held_by is None
    # The empty file is still there — we did not touch it.
    assert path.read_bytes() == b""


def test_garbage_lock_file_blocks_steal(tmp_path):
    """A lock file with non-JSON content blocks the steal, same as empty."""
    lock_dir = tmp_path / "locks"
    lock_dir.mkdir(parents=True, exist_ok=True)
    target = "/repos/garbage"
    path = lock_dir / "_repos_garbage.lock"
    path.write_text("not json at all", encoding="utf-8")

    with pytest.raises(LockBusy) as excinfo:
        acquire(target, "run-new", lock_dir=lock_dir)
    assert excinfo.value.held_by is None
    assert path.read_text(encoding="utf-8") == "not json at all"


# ---------------------------------------------------------------------------
# 4. Idempotent release + run_id mismatch safety
# ---------------------------------------------------------------------------


def test_release_is_idempotent_and_removes_file(tmp_path):
    lock_dir = tmp_path / "locks"
    lock = acquire("/repos/rel", "run-x", lock_dir=lock_dir)
    assert lock.path.exists()

    release(lock)
    assert not lock.path.exists()

    # Second release on the same Lock object must not raise.
    release(lock)
    assert not lock.path.exists()


def test_release_does_not_touch_lock_held_by_another_run(tmp_path):
    """If the on-disk lock's run_id no longer matches, release() leaves it
    alone — a stale reclaim should not be clobbered by a late release."""
    lock_dir = tmp_path / "locks"
    ancient = _dt.datetime(2026, 1, 1, 12, 0, 0, tzinfo=_dt.timezone.utc)
    old = acquire("/repos/clobber", "run-old", lock_dir=lock_dir, now=ancient)

    # Reclaim with a fresh run id (real wall clock → stale).
    new = acquire("/repos/clobber", "run-new", lock_dir=lock_dir)
    assert new.run_id == "run-new"
    assert new.path.exists()

    # The old holder finally calls release() — it must NOT delete new's lock.
    release(old)
    assert new.path.exists()
    on_disk = json.loads(new.path.read_text(encoding="utf-8"))
    assert on_disk["run_id"] == "run-new"


# ---------------------------------------------------------------------------
# 5. is_locked() reflects current state
# ---------------------------------------------------------------------------


def test_is_locked_false_when_no_file(tmp_path):
    assert is_locked("/repos/none", lock_dir=tmp_path / "locks") is False


def test_is_locked_true_for_fresh_lock_and_false_for_stale(tmp_path):
    lock_dir = tmp_path / "locks"
    ancient = _dt.datetime(2026, 1, 1, 12, 0, 0, tzinfo=_dt.timezone.utc)
    acquire("/repos/check", "run-x", lock_dir=lock_dir, now=ancient)
    # Real wall clock is in 2026, so a Jan-1 2026 lock is already stale
    # at test time (since STALE_AFTER is 1h). Test the freshness window
    # by acquiring a NEW lock and asserting it is locked; then we let it
    # sit until stale via the ``now=`` override on is_locked.
    fresh = acquire("/repos/check2", "run-y", lock_dir=lock_dir)
    assert is_locked("/repos/check2", lock_dir=lock_dir) is True

    # The same target, observed with a "now" far in the future, must
    # be reported as not-locked (stale).
    far_future = _dt.datetime(2099, 1, 1, tzinfo=_dt.timezone.utc)
    assert is_locked("/repos/check2", lock_dir=lock_dir, now=far_future) is False

    # The original target (acquired in 2026 with an ancient timestamp) is
    # also stale by real wall clock; release() on a now-irrelevant path
    # is a clean teardown.
    release(fresh)


# ---------------------------------------------------------------------------
# 6. Target repo sanitization
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "raw, expected_filename",
    [
        ("/Users/foo/bar", "_Users_foo_bar"),
        # `..` → `__`, `/` → `_`, `etc`, `/` → `_`, `passwd`
        ("../etc/passwd", "___etc_passwd"),
        ("my-repo_v2", "my-repo_v2"),
        ("repo with spaces", "repo_with_spaces"),
        ("", ""),  # empty target still produces a .lock file (callers shouldn't do this)
    ],
)
def test_target_repo_sanitized_for_filename(tmp_path, raw, expected_filename):
    lock_dir = tmp_path / "locks"
    lock = acquire(raw, "run-s", lock_dir=lock_dir)
    assert lock.path.name == f"{expected_filename}.lock"


# ---------------------------------------------------------------------------
# 7. CLI subcommand entry point (used by bin/dark-factory)
# ---------------------------------------------------------------------------


def test_cli_acquire_and_release_roundtrip(tmp_path, monkeypatch, capsys):
    """Smoke: invoking ``python -m runner._merge_train acquire`` and
    ``release`` as subprocesses works end-to-end against a tmp lock dir."""
    # Use a lock dir under tmp_path so we never touch the real home.
    lock_dir = tmp_path / "cli-locks"
    target = "/cli/test"

    proj = pathlib.Path(__file__).resolve().parent.parent
    py = sys.executable

    acq_rc = _run(py, proj, [
        "-m", "runner._merge_train",
        "--lock-dir", str(lock_dir),
        "acquire", "--target", target, "--run-id", "cli-run",
    ])
    assert acq_rc.returncode == 0, acq_rc.stderr
    assert (lock_dir / "_cli_test.lock").exists()

    # Second acquire must fail with rc=3 and print the holder on stderr.
    busy_rc = _run(py, proj, [
        "-m", "runner._merge_train",
        "--lock-dir", str(lock_dir),
        "acquire", "--target", target, "--run-id", "cli-other",
    ])
    assert busy_rc.returncode == 3, busy_rc.stderr
    assert "cli-run" in busy_rc.stderr

    # Release → file gone; second release is still rc=0.
    rel_rc = _run(py, proj, [
        "-m", "runner._merge_train",
        "--lock-dir", str(lock_dir),
        "release", "--target", target, "--run-id", "cli-run",
    ])
    assert rel_rc.returncode == 0, rel_rc.stderr
    assert not (lock_dir / "_cli_test.lock").exists()

    rel2_rc = _run(py, proj, [
        "-m", "runner._merge_train",
        "--lock-dir", str(lock_dir),
        "release", "--target", target, "--run-id", "cli-run",
    ])
    assert rel2_rc.returncode == 0, rel2_rc.stderr

    # After release, is-locked returns rc=1 (not locked).
    chk_rc = _run(py, proj, [
        "-m", "runner._merge_train",
        "--lock-dir", str(lock_dir),
        "is-locked", "--target", target,
    ])
    assert chk_rc.returncode == 1, chk_rc.stderr


def _run(py: str, cwd: pathlib.Path, args: list[str]):
    import subprocess
    return subprocess.run(
        [py, *args],
        cwd=str(cwd),
        capture_output=True,
        text=True,
        timeout=30,
    )
