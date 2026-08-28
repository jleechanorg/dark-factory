"""Regression tests for the controller resume contract."""

from __future__ import annotations

import json
import subprocess
from dataclasses import asdict
from pathlib import Path

import pytest

from runner import engine_run
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
    return repo, _git(repo, "rev-parse", "HEAD")


def _advance_worker(repo: Path) -> str:
    (repo / "value.txt").write_text("worker change\n")
    _git(repo, "commit", "-qam", "worker change")
    return _git(repo, "rev-parse", "HEAD")


def _graph(*, graph_contract: bool = False, node_contract: bool = False) -> Graph:
    graph_attrs = {"review_contract": "cold-review-v1"} if graph_contract else {}
    reviewer_attrs = {"review_contract": "cold-review-v1"} if node_contract else {}
    return Graph(
        name="resume-contract",
        goal="",
        attrs=graph_attrs,
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


def _assert_resume_rejected_before_side_effects(
    graph: Graph, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Controller resume rejection must be the first executable run action."""
    checkpoint = tmp_path / "checkpoint.json"
    ctx = Context(goal="review", workdir=tmp_path)

    def unexpected(label: str):
        def fail(*_args, **_kwargs):
            pytest.fail(f"{label} ran before controller resume rejection")

        return fail

    monkeypatch.setattr(engine_run, "_load_controller_snapshot_journal", unexpected("journal"))
    monkeypatch.setattr(engine_run, "_seed_controller_base_sha", unexpected("target"))
    monkeypatch.setattr(engine_run._persist, "_load_checkpoint", unexpected("checkpoint"))
    monkeypatch.setattr(engine_run, "resolve", unexpected("resolve"))
    monkeypatch.setattr(engine_run, "CXDB", unexpected("cxdb"))
    monkeypatch.setattr(engine_run.uuid, "uuid4", unexpected("run-id"))

    with pytest.raises(ValueError, match="^resume is not supported for cold-review-v1 graphs$"):
        engine_run.run(graph, ctx, checkpoint=checkpoint, resume=checkpoint)

    assert ctx.run_id is None
    assert ctx.event_log_path is None
    assert ctx._controller_base_sha is None
    assert not checkpoint.exists()


def test_graph_level_cold_review_resume_is_rejected_before_side_effects(tmp_path, monkeypatch):
    _assert_resume_rejected_before_side_effects(
        _graph(graph_contract=True), tmp_path, monkeypatch
    )


def test_node_level_cold_review_resume_is_rejected_before_side_effects(tmp_path, monkeypatch):
    _assert_resume_rejected_before_side_effects(
        _graph(node_contract=True), tmp_path, monkeypatch
    )


def test_fresh_controller_loop_still_seeds_and_runs_in_process(tmp_path, monkeypatch):
    repo, base = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    calls: list[str] = []

    def handler(node: Node, _ctx: Context) -> Result:
        calls.append(node.name)
        return Result()

    monkeypatch.setattr(engine_run, "resolve", lambda _node: handler)
    ctx = Context(goal="review", workdir=repo, backend="echo", run_id="fresh-controller")
    history = engine_run.run(_graph(node_contract=True), ctx, max_steps=10)

    assert calls == ["start", "worker", "reviewer", "exit"]
    assert [step.node for step in history] == calls
    assert ctx._controller_base_sha == base
    assert ctx.state["_controller_base_sha"] == base


def test_noncontroller_resume_still_reseeds_current_target(tmp_path, monkeypatch):
    repo, base = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    calls: list[str] = []

    def handler(node: Node, _ctx: Context) -> Result:
        calls.append(node.name)
        if node.name == "worker":
            _advance_worker(repo)
        return Result()

    monkeypatch.setattr(engine_run, "resolve", lambda _node: handler)
    checkpoint = home / ".dark-factory" / "runs" / "non-controller" / "checkpoint.json"
    first = Context(goal="review", workdir=repo, run_id="non-controller")
    first_history = engine_run.run(_graph(), first, checkpoint=checkpoint, max_steps=2)
    checkpoint.write_text(
        json.dumps([asdict(step) for step in first_history[:2]]), encoding="utf-8"
    )
    worker_head = _git(repo, "rev-parse", "HEAD")

    resumed = Context(goal="review", workdir=repo, run_id="non-controller")
    history = engine_run.run(
        _graph(), resumed, checkpoint=checkpoint, resume=checkpoint, max_steps=10
    )

    assert calls == ["start", "worker", "reviewer", "exit"]
    assert [step.node for step in history] == ["start", "worker", "reviewer", "exit"]
    assert resumed._controller_base_sha == worker_head
    assert resumed._controller_base_sha != base
    assert not checkpoint.parent.joinpath("controller-base-provenance.json").exists()
