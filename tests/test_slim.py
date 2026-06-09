from __future__ import annotations

import pathlib
import sys

import runner.handlers as handlers_mod
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.engine import run
from runner.parser import parse

ROOT = pathlib.Path(__file__).parent.parent


def test_slim_stylesheet_routes_roles_by_class():
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    # explore is now a 4-way fanout from _base.dot; pick the concept sub-agent
    explore = graph.nodes["explore_concept"].attrs
    plan = graph.nodes["plan"].attrs
    implement = graph.nodes["implement"].attrs
    fix = graph.nodes["fix"].attrs
    review = graph.nodes["review"].attrs

    assert explore.get("class") == "explore"
    assert "backend" not in explore
    assert "model_name" not in explore

    assert plan.get("class") == "plan"
    assert plan["backend"] == "claude"
    assert plan["model_name"] == "claude-opus-4-6"

    assert implement.get("class") == "implement"
    assert "backend" not in implement
    assert "model_name" not in implement

    assert fix.get("class") == "fix"
    assert "backend" not in fix
    assert "model_name" not in fix

    assert review.get("class") == "review"
    assert review["backend"] == "agy"


def test_minimal_feature_factory_runs_with_deterministic_gates(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", lambda node, ctx: Result(outcome="success", output="ok"))
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    # production routes plan→claude opus and review→agy; pin echo for offline determinism
    graph.nodes["plan"].attrs["backend"] = "echo"
    graph.nodes["review"].attrs["backend"] = "echo"
    ctx = Context(goal="ship a tiny feature", workdir=ROOT, backend="echo")
    ctx.state["feature"] = "hello"
    ctx.state["slim.test_command"] = f"{sys.executable} -c \"print('tests ok')\""

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "explore_in",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_stitch",
        "explore_out",
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
    # production routes plan→claude opus and review→agy; pin echo for offline determinism
    graph.nodes["plan"].attrs["backend"] = "echo"
    graph.nodes["review"].attrs["backend"] = "echo"
    ctx = Context(goal="refactor a tiny thing in-flight", workdir=ROOT, backend="echo")
    ctx.state["slim.test_command"] = f"{sys.executable} -c \"print('tests ok')\""

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "explore_in",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_stitch",
        "explore_out",
        "plan",
        "implement",
        "test",
        "review",
        "gate_es",
        "gate_er",
        "exit",
    ]

