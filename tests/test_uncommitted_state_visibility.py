"""Tests for the P-B uncommitted-state visibility helpers.

P-B surfaces `git status` / `git diff --shortstat` snapshots at RUN_END
so the operator can tell whether an exhausted run produced zero work or
N files of uncommitted work sitting in the worktree. The Healer (P-C)
then clusters the two cases separately.

All git operations are against `tmp_path` only — never real
``~/.dark-factory/`` or ``worldarchitect.ai``.
"""

from __future__ import annotations

import json
import pathlib
import subprocess

import pytest

from runner.cxdb import CXDB
from runner.engine import _collect_uncommitted_state, _format_uncommitted_for_log
from runner.engine_observability import _collect_uncommitted_state as _direct


# ---------------------------------------------------------------------------
# _collect_uncommitted_state — direct unit tests
# ---------------------------------------------------------------------------


def _init_git_repo(path: pathlib.Path) -> None:
    """Init a fresh git repo at ``path`` with a single empty initial commit.

    Uses the operator's canonical git identity so the workspace pre-commit
    guard (``HERMES_SKIP_EXAMPLE_COM_GUARD``) does not block the placeholder
    email pattern. The repo lives under ``tmp_path`` only and the commits
    never leave pytest's temp directory.
    """
    subprocess.run(
        ["git", "-C", str(path), "init", "-q"],
        check=True,
        timeout=10,
    )
    # Local identity so the initial commit doesn't fail on machines where
    # the operator hasn't set global git config. We deliberately use the
    # operator's identity (NOT a placeholder) to avoid tripping the
    # pre-commit guard.
    subprocess.run(
        ["git", "-C", str(path), "config", "user.email", "jleechan2015@users.noreply.github.com"],
        check=True,
        timeout=5,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "jleechan2015"],
        check=True,
        timeout=5,
    )
    # An empty initial commit gives us a HEAD without requiring staged files.
    subprocess.run(
        ["git", "-C", str(path), "commit", "--allow-empty", "-m", "init", "-q"],
        check=True,
        timeout=10,
    )


def test_uncommitted_state_returns_correct_counts(tmp_path: pathlib.Path) -> None:
    """Three new untracked files => files=3, staged=3, empty diff shortstat."""
    _init_git_repo(tmp_path)
    (tmp_path / "a.py").write_text("print('a')\n")
    (tmp_path / "b.py").write_text("print('b')\n")
    (tmp_path / "c.py").write_text("print('c')\n")

    state = _collect_uncommitted_state(tmp_path)

    assert state["uncommitted_files"] == "3"
    assert state["uncommitted_staged_files"] == "3"
    # No `git add` was performed, so the index diff is empty even though the
    # worktree has untracked files. Insertions/deletions on `--shortstat` only
    # reflect the index-vs-HEAD delta.
    assert state["uncommitted_insertions"] == ""
    assert state["uncommitted_deletions"] == ""


def test_uncommitted_state_handles_staged_modifications(tmp_path: pathlib.Path) -> None:
    """Staged additions produce non-empty insertions and a non-empty files count."""
    _init_git_repo(tmp_path)
    target = tmp_path / "feature.py"
    target.write_text("print('original')\n")
    subprocess.run(
        ["git", "-C", str(tmp_path), "add", "feature.py"],
        check=True,
        timeout=5,
    )
    subprocess.run(
        ["git", "-C", str(tmp_path), "commit", "-m", "baseline", "-q"],
        check=True,
        timeout=10,
    )

    # Now create 2 new files and stage them so the index-vs-HEAD diff picks
    # up real insertions.
    (tmp_path / "new1.py").write_text("a\nb\nc\n")
    (tmp_path / "new2.py").write_text("d\n")
    subprocess.run(
        ["git", "-C", str(tmp_path), "add", "new1.py", "new2.py"],
        check=True,
        timeout=5,
    )

    state = _collect_uncommitted_state(tmp_path)

    # `git status --porcelain` reports staged-but-uncommitted entries with
    # leading letters (e.g. "A  new1.py"). `uncommitted_files` counts all
    # non-empty porcelain lines; `uncommitted_staged_files` is untracked-only.
    assert state["uncommitted_files"] == "2"
    assert state["uncommitted_staged_files"] == "0"
    assert state["uncommitted_insertions"] == "4"
    assert state["uncommitted_deletions"] == ""


def test_uncommitted_state_handles_clean_repo(tmp_path: pathlib.Path) -> None:
    """Clean tree => all counts zero, no insertion/deletion markers."""
    _init_git_repo(tmp_path)
    committed = tmp_path / "committed.py"
    committed.write_text("hello\n")
    subprocess.run(
        ["git", "-C", str(tmp_path), "add", "committed.py"],
        check=True,
        timeout=5,
    )
    subprocess.run(
        ["git", "-C", str(tmp_path), "commit", "-m", "add committed.py", "-q"],
        check=True,
        timeout=10,
    )

    state = _collect_uncommitted_state(tmp_path)

    assert state["uncommitted_files"] == "0"
    assert state["uncommitted_staged_files"] == "0"
    assert state["uncommitted_insertions"] == ""
    assert state["uncommitted_deletions"] == ""


def test_uncommitted_state_handles_non_git_dir(tmp_path: pathlib.Path) -> None:
    """Non-git directory => empty strings across the board, no exception."""
    # tmp_path is created by pytest but is not a git repo
    state = _collect_uncommitted_state(tmp_path)
    assert state == {
        "uncommitted_files": "",
        "uncommitted_insertions": "",
        "uncommitted_deletions": "",
        "uncommitted_staged_files": "",
    }


def test_uncommitted_state_handles_none_workdir() -> None:
    """`workdir=None` is a permitted smoke-run shape — must return empty dict."""
    state = _collect_uncommitted_state(None)
    assert all(v == "" for v in state.values())


def test_uncommitted_state_handles_missing_dir(tmp_path: pathlib.Path) -> None:
    """A workdir that doesn't exist must not raise — return empty dict."""
    ghost = tmp_path / "does-not-exist"
    state = _collect_uncommitted_state(ghost)
    assert all(v == "" for v in state.values())


def test_uncommitted_state_survives_tracked_modifications(tmp_path: pathlib.Path) -> None:
    """Modifying a tracked file shows up as +N/-M even without staging."""
    _init_git_repo(tmp_path)
    target = tmp_path / "feature.py"
    target.write_text("line1\nline2\nline3\nline4\n")
    subprocess.run(
        ["git", "-C", str(tmp_path), "add", "feature.py"],
        check=True,
        timeout=5,
    )
    subprocess.run(
        ["git", "-C", str(tmp_path), "commit", "-m", "baseline", "-q"],
        check=True,
        timeout=10,
    )

    # Overwrite with 6 lines (4 new) and 0 deletions.
    target.write_text("line1\nline2\nline3\nline4\nline5\nline6\n")

    state = _collect_uncommitted_state(tmp_path)

    # `git status --porcelain` reports the unstaged modification.
    assert state["uncommitted_files"] == "1"
    # `uncommitted_staged_files` is untracked-only, so 0 here.
    assert state["uncommitted_staged_files"] == "0"
    # The diff is from the worktree vs the index, so we see the 2 new lines.
    # (Some git versions include the staged diff as well; we only assert
    # non-empty insertion count.)
    assert state["uncommitted_insertions"] != ""
    assert int(state["uncommitted_insertions"]) >= 2


def test_format_uncommitted_for_log_empty() -> None:
    """Empty / zero state => empty string so the caller can skip appending."""
    assert _format_uncommitted_for_log({}) == ""
    assert _format_uncommitted_for_log(
        {
            "uncommitted_files": "0",
            "uncommitted_insertions": "",
            "uncommitted_deletions": "",
            "uncommitted_staged_files": "0",
        }
    ) == ""


def test_format_uncommitted_for_log_with_work() -> None:
    """Non-zero counts => compact human-readable fragment for the RUN_END line."""
    fragment = _format_uncommitted_for_log(
        {
            "uncommitted_files": "3",
            "uncommitted_insertions": "47",
            "uncommitted_deletions": "12",
            "uncommitted_staged_files": "2",
        }
    )
    assert "uncommitted=3" in fragment
    assert "+47" in fragment
    assert "-12" in fragment
    assert "staged=2" in fragment


# ---------------------------------------------------------------------------
# engine.py re-export — P-B surface area
# ---------------------------------------------------------------------------


def test_engine_module_reexports_helper() -> None:
    """The shim re-exports must work so call sites in `engine_run.py`
    can `from runner.engine import _collect_uncommitted_state`."""
    assert _collect_uncommitted_state is _direct


# ---------------------------------------------------------------------------
# CXDB write + Healer clustering — end-to-end on the uncommitted_* fields
# ---------------------------------------------------------------------------


def test_synthetic_run_end_step_carries_uncommitted_metadata(tmp_path: pathlib.Path) -> None:
    """End-to-end: a CXDB-backed run that finishes with N uncommitted files
    in its workdir ends up with a `__run_end__` step row whose metadata_json
    contains the `uncommitted_*` fields. The Healer (P-C) reads from
    metadata_json, so this is the contract P-C depends on."""
    _init_git_repo(tmp_path)
    (tmp_path / "new.py").write_text("print('new')\n")

    # Snapshot the uncommitted state BEFORE creating the CXDB — the CXDB
    # itself creates 3 untracked files (sqlite, sqlite-shm, sqlite-wal) that
    # would pollute the count. The runner snapshot in `engine_run.py` happens
    # before any new file is written into the workdir, so the test mirrors
    # that ordering.
    state = _collect_uncommitted_state(tmp_path)

    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        run_id = db.start_run(pipeline="test_pipeline", goal="verify uncommitted metadata")
        db.record_step(
            run_id=run_id,
            seq=0,
            node="implement",
            outcome="exhausted",
            ts=0.0,
            output="fix loop hit max_visits",
            metadata={"max_visits": "3"},
        )

        # Now simulate the synthetic run_end step that engine_run.py writes
        # after P-B landed. We inline the contract here so a regression in
        # engine_run.py is caught by reading this test's expectations.
        db.record_step(
            run_id=run_id,
            seq=1,
            node="__run_end__",
            outcome="exhausted",
            ts=1.0,
            output="",
            metadata={
                "final_outcome": "exhausted",
                "ended_at_exit": "true",
                "steps": "1",
                **state,
            },
        )
        db.end_run(run_id, "exhausted")
    finally:
        db.close()

    # Reopen and read back.
    db2 = CXDB(db_path)
    try:
        cur = db2._conn.cursor()
        cur.execute(
            "SELECT node, outcome, metadata_json FROM steps WHERE node='__run_end__'"
        )
        rows = cur.fetchall()
        assert len(rows) == 1
        node, outcome, meta_json = rows[0]
        assert node == "__run_end__"
        assert outcome == "exhausted"
        meta = json.loads(meta_json)
        assert meta["uncommitted_files"] == "1"
        assert meta["uncommitted_staged_files"] == "1"
    finally:
        db2.close()


# ---------------------------------------------------------------------------
# runner.healer — sub-cluster key
# ---------------------------------------------------------------------------


def test_healer_exhausted_with_uncommitted_subcluster_key() -> None:
    """The P-C sub-cluster key folds uncommitted state into the outcome."""
    from runner.healer import _exhausted_with_uncommitted

    # Non-exhaustion outcomes pass through untouched.
    assert _exhausted_with_uncommitted("failure", None) == "failure"
    assert _exhausted_with_uncommitted("error", {"uncommitted_files": "5"}) == "error"

    # Exhausted + uncommitted work => refined label.
    assert (
        _exhausted_with_uncommitted("exhausted", {"uncommitted_files": "5"})
        == "exhausted_with_uncommitted_work"
    )

    # Exhausted + clean tree => stays as raw exhausted.
    assert _exhausted_with_uncommitted("exhausted", {"uncommitted_files": "0"}) == "exhausted"
    assert _exhausted_with_uncommitted("exhausted", {"uncommitted_files": ""}) == "exhausted"
    assert _exhausted_with_uncommitted("exhausted", None) == "exhausted"
