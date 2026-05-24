from __future__ import annotations

import pathlib
import sys

import runner.handlers as handlers_mod
from runner.engine import run
from runner.handlers import Context
from runner.parser import parse

ROOT = pathlib.Path(__file__).parent.parent


def test_minimal_feature_factory_runs_with_deterministic_gates(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    ctx = Context(goal="ship a tiny feature", workdir=ROOT, backend="echo")
    ctx.state["slim.test_command"] = f"{sys.executable} -c \"print('tests ok')\""
    ctx.state["slim.holdout_command"] = f"{sys.executable} -c \"print('holdouts ok')\""

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "plan",
        "implement",
        "test",
        "review",
        "holdout",
        "exit",
    ]
