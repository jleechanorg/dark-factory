"""Regression tests for controller-owned target and base provenance."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from runner.handler_core import Context, _target_worktree
from runner.handler_parallel_reviewer import (
    _controller_review_request,
    _controller_snapshot_root,
    _verify_controller_workspace,
)
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


def test_controller_snapshot_root_rejects_symlink_before_git_mutation(
    tmp_path: Path, monkeypatch
) -> None:
    """A symlinked controller-snapshots root cannot redirect Git worktrees."""
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    outside = tmp_path / "outside"
    outside.mkdir(mode=0o700)
    (home / ".dark-factory").mkdir(mode=0o700)
    (home / ".dark-factory" / "controller-snapshots").symlink_to(
        outside, target_is_directory=True
    )
    monkeypatch.setenv("HOME", str(home))

    with pytest.raises(ValueError, match="symlink"):
        _controller_snapshot_root()
    assert not list(outside.iterdir())


def test_controller_post_review_revalidates_mutable_source_checkout(tmp_path: Path, monkeypatch):
    """A source/index change after snapshot creation invalidates a pass."""
    repo, base, head = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setenv("HOME", str(home))
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={"base_sha": base},
        run_id="source-revalidation",
    )
    try:
        request = _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
        (repo / "value.txt").write_text("mutated after snapshot\n")
        with pytest.raises(ValueError, match="source checkout changed"):
            _verify_controller_workspace(ctx, request)
    finally:
        _cleanup_snapshots(repo, ctx)


def test_default_two_node_seeds_base_before_worker_mutation(tmp_path: Path, monkeypatch) -> None:
    """The production default graph binds its base before creating a review snapshot."""
    import runner.handler_parallel_reviewer as reviewer
    from runner import handlers
    from runner.__main__ import main
    from runner.handler_core import Result

    repo, base, _head = _repo(tmp_path)
    _git(repo, "reset", "--hard", base)
    (repo / "evidence").mkdir()
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))

    def worker(node, ctx):
        (repo / "worker.txt").write_text("worker mutation\n")
        (repo / "evidence" / "worker-verification.json").write_text(
            '{"status":"pass"}\n'
        )
        return Result(outcome="success", output="worker complete")

    monkeypatch.setitem(handlers.TYPE_REGISTRY, "codergen", worker)
    captured: dict[str, object] = {}
    original_request = reviewer._controller_review_request

    def capture_request(node, ctx, expected_sha):
        request = original_request(node, ctx, expected_sha)
        captured["request"] = request
        captured["base_state"] = ctx.state.get("_controller_base_sha")
        return request

    monkeypatch.setattr(reviewer, "_controller_review_request", capture_request)
    monkeypatch.setattr(
        reviewer,
        "_run_primary_review",
        lambda *args, **kwargs: Result(
            outcome="success",
            output=json.dumps(
                {
                    "verdict": "pass",
                    "findings": [],
                    "evidence_checked": ["worker-verification.json"],
                    "commands_executed": ["verification command"],
                    "caveats": [],
                }
            ),
            metadata={
                "returncode": "0",
                "_controller_command_receipts": [
                    {
                        "command": "verification command",
                        "exit_code": 0,
                        "output_sha256": "1" * 64,
                    }
                ],
            },
        ),
    )

    result = main(
        [
            "--goal",
            "review worker mutation",
            "--workdir",
            str(repo),
            "--ao-worktree",
            str(repo),
            "--backend",
            "echo",
            "--no-perf-log",
            "--max-steps",
            "10",
        ]
    )

    request = captured["request"]
    envelope = json.loads(request.envelope_json)
    assert result == 0
    assert captured["base_state"] == base
    assert envelope["target"]["base_sha"] == base
    assert envelope["target"]["base_sha"] != envelope["target"]["head_sha"]
    assert "worker.txt" in envelope["snapshots"]["changed_files"]


def test_cli_ao_worktree_symlink_parent_rejected_before_snapshot(
    tmp_path: Path, monkeypatch
) -> None:
    """CLI AO worktree ingestion must retain aliases for the lexical guard."""
    from runner import handlers
    from runner.__main__ import main
    from runner.handler_core import Result

    repo, _base, _head = _repo(tmp_path)
    alias_parent = tmp_path / "ao-alias"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    lexical_worktree = alias_parent / repo.name
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    head_lookup_attempted = False

    def worker(node, ctx):
        return Result(outcome="success", output="worker complete")

    def unexpected_head(*args, **kwargs):
        nonlocal head_lookup_attempted
        head_lookup_attempted = True
        raise AssertionError("symlinked AO worktree reached a Git HEAD lookup")

    monkeypatch.setitem(handlers.TYPE_REGISTRY, "codergen", worker)
    monkeypatch.setattr(handlers, "_worktree_head_sha", unexpected_head)
    result = main(
        [
            "--goal",
            "reject symlinked AO worktree",
            "--workdir",
            str(repo),
            "--ao-worktree",
            str(lexical_worktree),
            "--backend",
            "echo",
            "--no-perf-log",
            "--max-steps",
            "10",
        ]
    )

    assert result != 0
    assert head_lookup_attempted is False
