from __future__ import annotations

import pathlib
import sys

import runner.handlers as handlers_mod
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.engine import run
from runner.parser import parse

ROOT = pathlib.Path(__file__).parent.parent


def test_minimal_feature_factory_runs_with_deterministic_gates(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", lambda node, ctx: Result(outcome="success", output="ok"))
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    ctx = Context(goal="ship a tiny feature", workdir=ROOT, backend="echo")
    ctx.state["feature"] = "hello"
    ctx.state["slim.test_command"] = f"{sys.executable} -c \"print('tests ok')\""

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "plan",
        "implement",
        "test",
        "review",
        "holdout",
        "gate_es",
        "gate_er",
        "exit",
    ]


def test_minimal_pr_factory_runs_with_deterministic_gates(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_pr.dot")
    ctx = Context(goal="refactor a tiny thing in-flight", workdir=ROOT, backend="echo")
    ctx.state["slim.test_command"] = f"{sys.executable} -c \"print('tests ok')\""

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "plan",
        "implement",
        "test",
        "review",
        "gate_es",
        "gate_er",
        "exit",
    ]

