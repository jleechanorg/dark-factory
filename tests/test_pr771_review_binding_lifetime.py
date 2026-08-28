"""Regression coverage for replacing the current controller source binding."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from runner.handler_core import Context
from runner.handler_parallel_reviewer import (
    _controller_review_request,
    _verify_controller_workspace,
)
from runner.parser import Node


def _git(cwd: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def _repo(tmp_path: Path) -> tuple[Path, str, str]:
    repo = tmp_path / "repo"
    repo.mkdir(mode=0o700)
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.name", "test")
    _git(repo, "config", "user.email", "test@example.invalid")
    (repo / "value.txt").write_text("base\n")
    _git(repo, "add", "value.txt")
    _git(repo, "commit", "-q", "-m", "base")
    base = _git(repo, "rev-parse", "HEAD")
    (repo / "value.txt").write_text("worker change\n")
    _git(repo, "commit", "-qam", "worker change")
    return repo, base, _git(repo, "rev-parse", "HEAD")


def _trusted_context(repo: Path, base: str) -> Context:
    from runner.engine_run import _set_controller_base_sha

    context = Context(goal="review worker output", workdir=repo, run_id="binding-lifetime")
    _set_controller_base_sha(context, base)
    return context


def test_second_review_uses_current_source_binding_after_worker_mutation(
    tmp_path: Path, monkeypatch
) -> None:
    """A later review must not revalidate a stale source checkout fingerprint."""
    repo, base, head = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    home.chmod(0o700)
    monkeypatch.setenv("HOME", str(home))
    worker_output = repo / "worker-output.txt"
    worker_output.write_text("first review\n")
    context = _trusted_context(repo, base)
    node = Node(name="cold_reviewer", attrs={})

    try:
        first_request = _controller_review_request(node, context, head)
        _verify_controller_workspace(context, first_request)
        first_bindings = json.loads(
            context.state["_controller_review_source_bindings"]
        )
        assert len(first_bindings) == 1

        # Simulate the worker updating its output before a second reviewer visit.
        worker_output.write_text("second review\n")
        second_request = _controller_review_request(node, context, head)
        _verify_controller_workspace(context, second_request)

        current_bindings = json.loads(
            context.state["_controller_review_source_bindings"]
        )
        assert len(current_bindings) == 1
        assert current_bindings[0]["fingerprint"] != first_bindings[0]["fingerprint"]
        snapshots = json.loads(context.state["_controller_review_snapshots"])
        assert len(snapshots) == 2
        assert snapshots[0]["snapshot_path"] != snapshots[1]["snapshot_path"]
    finally:
        from runner.engine_run import _cleanup_controller_snapshot

        _cleanup_controller_snapshot(context)

    assert json.loads(context.state["_controller_review_snapshots"]) == []
