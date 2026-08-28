"""Regression tests for durable controller-base provenance across resume."""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import asdict
from pathlib import Path

from runner import engine_run
from runner.engine_run import (
    _CONTROLLER_BASE_PROVENANCE,
    _load_controller_base_provenance,
    _persist_controller_base_provenance,
    _seed_controller_base_sha,
)
from runner.handlers import Context, Result
from runner.parser import Edge, Graph, Node


def _git(cwd: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout.strip()


def _repo(tmp_path: Path) -> tuple[Path, str]:
    repo = tmp_path / "repo"
    repo.mkdir(mode=0o700)
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.name", "test")
    _git(repo, "config", "user.email", "test@example.invalid")
    (repo / "value.txt").write_text("base\n")
    _git(repo, "add", "value.txt")
    _git(repo, "commit", "-q", "-m", "base")
    base = _git(repo, "rev-parse", "HEAD")
    return repo, base


def _advance_worker(repo: Path) -> str:
    (repo / "value.txt").write_text("worker change\n")
    _git(repo, "commit", "-qam", "worker change")
    return _git(repo, "rev-parse", "HEAD")


def _graph() -> Graph:
    return Graph(
        name="resume-base",
        goal="",
        nodes={name: Node(name=name) for name in ("start", "worker", "reviewer", "exit")},
        edges=[
            Edge(src="start", dst="worker"),
            Edge(src="worker", dst="reviewer"),
            Edge(src="reviewer", dst="exit"),
        ],
    )


def test_resume_restores_original_base_before_reseed_after_worker_advances_head(
    tmp_path: Path, monkeypatch
) -> None:
    repo, base = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    home.chmod(0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)

    calls: list[str] = []

    def handler(node: Node, ctx: Context) -> Result:
        calls.append(node.name)
        if node.name == "worker":
            worker_head = _advance_worker(repo)
            ctx.state["worker_head"] = worker_head
        if node.name == "reviewer":
            assert ctx._controller_base_sha == base
        return Result()

    monkeypatch.setattr(engine_run, "resolve", lambda node: handler)
    checkpoint = home / ".dark-factory" / "runs" / "resume-base" / "checkpoint.json"
    first = Context(goal="review", workdir=repo, run_id="resume-base")
    first_history = engine_run.run(_graph(), first, checkpoint=checkpoint, max_steps=2)
    assert calls == ["start", "worker"]
    assert first._controller_base_sha == base
    worker_head = _git(repo, "rev-parse", "HEAD")

    # The bounded first run records an engine exhaustion marker for the next
    # node. Remove that marker to model a process interruption after worker;
    # the durable checkpoint still contains the completed worker step.
    checkpoint.write_text(
        json.dumps([asdict(step) for step in first_history[:2]]), encoding="utf-8"
    )

    resumed = Context(
        goal="review",
        workdir=repo,
        run_id="resume-base",
        # This is deliberately attacker-controlled/public state. It must not
        # authenticate the controller review range on a fresh Context.
        state={"_controller_base_sha": worker_head},
    )
    assert resumed._controller_base_sha is None
    resumed_history = engine_run.run(
        _graph(), resumed, checkpoint=checkpoint, resume=checkpoint, max_steps=10
    )

    assert resumed._controller_base_sha == base
    assert resumed.state["_controller_base_sha"] == base
    assert resumed._controller_base_sha != worker_head
    assert calls == ["start", "worker", "reviewer", "exit"]
    assert [step.node for step in resumed_history] == ["start", "worker", "reviewer", "exit"]

    sidecar = home / ".dark-factory" / "runs" / "resume-base" / _CONTROLLER_BASE_PROVENANCE
    payload = json.loads(sidecar.read_text(encoding="utf-8"))
    assert payload == {"controller_base_sha": base}
    assert sidecar.stat().st_mode & 0o077 == 0
    assert sidecar.stat().st_uid == os.getuid()


def test_public_state_alone_cannot_restore_private_controller_base(
    tmp_path: Path, monkeypatch
) -> None:
    repo, base = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    home.chmod(0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    worker_head = _advance_worker(repo)
    resumed = Context(
        goal="review",
        workdir=repo,
        run_id="without-sidecar",
        state={"_controller_base_sha": base},
    )

    assert _load_controller_base_provenance(resumed) is False
    assert resumed._controller_base_sha is None
    _seed_controller_base_sha(resumed, _graph())

    # The mutable checkout's current HEAD is the only value the runner may
    # capture when no durable provenance exists; public state did not help.
    assert resumed._controller_base_sha == worker_head
    assert resumed.state["_controller_base_sha"] == worker_head
