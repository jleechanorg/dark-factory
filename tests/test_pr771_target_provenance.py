"""Regression tests for controller-owned target and base provenance."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from runner.handler_core import Context, _target_worktree
from runner.handler_parallel_reviewer import _controller_review_request
from runner.parser import Node


def _git(cwd: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout.strip()


def _repo(tmp_path: Path) -> tuple[Path, str, str]:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.name", "test")
    _git(repo, "config", "user.email", "test@example.invalid")
    (repo / "value.txt").write_text("base\n")
    _git(repo, "add", "value.txt")
    _git(repo, "commit", "-q", "-m", "base")
    base = _git(repo, "rev-parse", "HEAD")
    (repo / "value.txt").write_text("worker change\n")
    _git(repo, "commit", "-qam", "worker change")
    head = _git(repo, "rev-parse", "HEAD")
    return repo, base, head


def _cleanup_snapshots(repo: Path, ctx: Context) -> None:
    for entry in json.loads(ctx.state.get("_controller_review_snapshots", "[]")):
        _git(repo, "worktree", "remove", "--force", entry["snapshot_path"])


def test_controller_base_does_not_follow_mutable_origin_main(tmp_path: Path) -> None:
    """A worker-controlled origin/main ref cannot collapse the reviewed range."""
    repo, base, head = _repo(tmp_path)
    _git(repo, "update-ref", "refs/remotes/origin/main", head)
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={"base_sha": base},
        run_id="base-provenance",
    )

    try:
        request = _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
        envelope = json.loads(request.envelope_json)
        assert envelope["target"]["base_sha"] == base
        assert envelope["target"]["base_sha"] != envelope["target"]["head_sha"]
        assert envelope["snapshots"]["changed_files"] == ["value.txt"]
    finally:
        _cleanup_snapshots(repo, ctx)


def test_controller_review_fails_closed_without_authenticated_base(tmp_path: Path) -> None:
    """No target-owned ref is an acceptable implicit controller base."""
    repo, _base, head = _repo(tmp_path)
    _git(repo, "update-ref", "refs/remotes/origin/main", head)
    ctx = Context(goal="review worker change", workdir=repo, run_id="missing-base")

    with pytest.raises(ValueError, match="base SHA|target head"):
        _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)


def test_controller_preserves_lexical_symlink_parent_until_validation(tmp_path: Path) -> None:
    """The earliest target resolver must not erase a symlinked parent."""
    repo, _base, head = _repo(tmp_path)
    alias_parent = tmp_path / "alias"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    lexical_repo = alias_parent / repo.name
    ctx = Context(goal="review", workdir=lexical_repo)

    assert _target_worktree(ctx) == lexical_repo
    with pytest.raises(ValueError, match="symlink"):
        _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)


def test_controller_preserves_lexical_ao_worktree_until_validation(tmp_path: Path) -> None:
    """AO worktree aliases receive the same lexical validation boundary."""
    repo, _base, head = _repo(tmp_path)
    alias_parent = tmp_path / "ao-alias"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    lexical_repo = alias_parent / repo.name
    ctx = Context(
        goal="review",
        workdir=repo,
        state={"ao.worktree": str(lexical_repo)},
    )

    assert _target_worktree(ctx) == lexical_repo
    with pytest.raises(ValueError, match="symlink"):
        _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
