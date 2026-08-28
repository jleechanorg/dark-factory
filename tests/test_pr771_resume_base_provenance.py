"""Regression tests for durable controller-base provenance across resume."""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import asdict
from pathlib import Path

import pytest

from runner import engine_run
from runner.engine_run import (
    _CONTROLLER_BASE_PROVENANCE,
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


def _nonancestor_sha(repo: Path) -> str:
    _git(repo, "checkout", "-q", "-b", "side")
    (repo / "side.txt").write_text("side\n")
    _git(repo, "add", "side.txt")
    _git(repo, "commit", "-q", "-m", "side")
    side_sha = _git(repo, "rev-parse", "HEAD")
    _git(repo, "checkout", "-q", "main")
    (repo / "main.txt").write_text("main\n")
    _git(repo, "add", "main.txt")
    _git(repo, "commit", "-q", "-m", "main")
    return side_sha


def _graph(*, controller: bool = True) -> Graph:
    reviewer_attrs = {"review_contract": "cold-review-v1"} if controller else {}
    return Graph(
        name="resume-base",
        goal="",
        nodes={
            name: Node(name=name, attrs=reviewer_attrs if name == "reviewer" else {})
            for name in ("start", "worker", "reviewer", "exit")
        },
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


@pytest.mark.parametrize(
    "sidecar_kind",
    ["missing", "malformed", "nonobject", "missing_field", "bad_sha", "symlink", "unsafe"],
)
def test_controller_resume_rejects_lost_or_invalid_sidecar_before_execution(
    tmp_path: Path, monkeypatch, sidecar_kind: str
) -> None:
    repo, base = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    home.chmod(0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    run_dir = home / ".dark-factory" / "runs" / "without-sidecar"
    run_dir.mkdir(parents=True, mode=0o700)
    sidecar = run_dir / _CONTROLLER_BASE_PROVENANCE
    if sidecar_kind == "malformed":
        sidecar.write_text("{", encoding="utf-8")
    elif sidecar_kind == "nonobject":
        sidecar.write_text("[]", encoding="utf-8")
    elif sidecar_kind == "missing_field":
        sidecar.write_text("{}", encoding="utf-8")
    elif sidecar_kind == "bad_sha":
        sidecar.write_text(
            json.dumps({"controller_base_sha": "not-a-sha"}), encoding="utf-8"
        )
    elif sidecar_kind == "symlink":
        target = tmp_path / "sidecar-target"
        target.write_text(json.dumps({"controller_base_sha": base}), encoding="utf-8")
        sidecar.symlink_to(target)
    elif sidecar_kind == "unsafe":
        sidecar.write_text(json.dumps({"controller_base_sha": base}), encoding="utf-8")
        sidecar.chmod(0o666)

    checkpoint = run_dir / "checkpoint.json"
    checkpoint.write_text("[]", encoding="utf-8")
    worker_head = _advance_worker(repo)
    calls: list[str] = []

    def handler(node: Node, _ctx: Context) -> Result:
        calls.append(node.name)
        return Result()

    monkeypatch.setattr(engine_run, "resolve", lambda node: handler)
    monkeypatch.setattr(
        engine_run,
        "_seed_controller_base_sha",
        lambda *_args: pytest.fail("controller resume reseeded after sidecar loss"),
    )
    monkeypatch.setattr(
        engine_run._persist,
        "_load_checkpoint",
        lambda *_args: pytest.fail("controller resume loaded checkpoint after sidecar loss"),
    )
    resumed = Context(
        goal="review worker change",
        workdir=repo,
        run_id="without-sidecar",
        state={"_controller_base_sha": base},
    )

    with pytest.raises(ValueError, match="controller base provenance is unavailable"):
        engine_run.run(_graph(), resumed, checkpoint=checkpoint, resume=checkpoint, max_steps=10)

    assert resumed._controller_base_sha is None
    assert calls == []
    assert worker_head != base


@pytest.mark.parametrize("sidecar_kind", ["unknown", "noncommit", "nonancestor"])
def test_controller_resume_rejects_semantically_invalid_sidecar_before_checkpoint(
    tmp_path: Path, monkeypatch, sidecar_kind: str
) -> None:
    repo, base = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    home.chmod(0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    run_dir = home / ".dark-factory" / "runs" / "semantic-sidecar"
    run_dir.mkdir(parents=True, mode=0o700)
    if sidecar_kind == "unknown":
        sidecar_sha = "0" * 40
    elif sidecar_kind == "noncommit":
        (repo / "blob.txt").write_text("blob\n")
        sidecar_sha = _git(repo, "hash-object", "-w", "blob.txt")
    else:
        sidecar_sha = _nonancestor_sha(repo)
    sidecar = run_dir / _CONTROLLER_BASE_PROVENANCE
    sidecar.write_text(json.dumps({"controller_base_sha": sidecar_sha}), encoding="utf-8")
    sidecar.chmod(0o600)
    checkpoint = run_dir / "checkpoint.json"
    checkpoint.write_text("[]", encoding="utf-8")
    calls: list[str] = []

    def handler(node: Node, _ctx: Context) -> Result:
        calls.append(node.name)
        return Result()

    monkeypatch.setattr(engine_run, "resolve", lambda node: handler)
    monkeypatch.setattr(
        engine_run._persist,
        "_load_checkpoint",
        lambda *_args: pytest.fail("semantic sidecar loaded checkpoint before validation"),
    )
    resumed = Context(
        goal="review",
        workdir=repo,
        run_id="semantic-sidecar",
        state={"_controller_base_sha": base},
    )

    with pytest.raises(ValueError, match="controller base provenance is unavailable"):
        engine_run.run(_graph(), resumed, checkpoint=checkpoint, resume=checkpoint, max_steps=10)

    assert resumed._controller_base_sha == sidecar_sha
    assert calls == []


def test_non_controller_resume_without_sidecar_still_reseeds(
    tmp_path: Path, monkeypatch
) -> None:
    repo, base = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    home.chmod(0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)

    calls: list[str] = []

    def handler(node: Node, _ctx: Context) -> Result:
        calls.append(node.name)
        if node.name == "worker":
            _advance_worker(repo)
        return Result()

    monkeypatch.setattr(engine_run, "resolve", lambda node: handler)
    checkpoint = home / ".dark-factory" / "runs" / "non-controller" / "checkpoint.json"
    first = Context(goal="review", workdir=repo, run_id="non-controller")
    first_history = engine_run.run(
        _graph(controller=False), first, checkpoint=checkpoint, max_steps=2
    )
    sidecar = checkpoint.parent / _CONTROLLER_BASE_PROVENANCE
    sidecar.unlink()
    checkpoint.write_text(
        json.dumps([asdict(step) for step in first_history[:2]]), encoding="utf-8"
    )

    resumed = Context(goal="review", workdir=repo, run_id="non-controller")
    history = engine_run.run(
        _graph(controller=False), resumed, checkpoint=checkpoint, resume=checkpoint, max_steps=10
    )

    assert resumed._controller_base_sha == _git(repo, "rev-parse", "HEAD")
    assert resumed._controller_base_sha != base
    assert [step.node for step in history] == ["start", "worker", "reviewer", "exit"]
    assert calls == ["start", "worker", "reviewer", "exit"]
