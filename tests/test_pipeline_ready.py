import pytest
from pathlib import Path

from runner.parser import parse
from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY


def test_ready_pipeline_parses_and_has_required_nodes():
    pipeline_path = Path("pipelines/slim/ready.dot")
    assert pipeline_path.exists(), "pipelines/slim/ready.dot must exist"

    graph = parse(pipeline_path)
    node_names = set(graph.nodes.keys())

    assert "start" in node_names
    assert "exit" in node_names
    assert "round_begin" in node_names
    assert "round_end" in node_names
    assert "test" in node_names
    assert "gate_es" in node_names
    assert "gate_er" in node_names
    assert "gate_advice" in node_names
    assert "holdout" in node_names
    assert "fix" in node_names

    assert str(graph.attrs.get("validation_rounds")).lower() in ("true", "1", "yes")

    fix_node = graph.nodes["fix"]
    assert fix_node is not None
    assert fix_node.attrs.get("type") == "codergen"

    advice_node = graph.nodes["gate_advice"]
    assert advice_node is not None
    assert advice_node.attrs.get("type") == "gate_slash"
    assert advice_node.attrs.get("command") == "advice"


def test_ready_pipeline_green_execution(monkeypatch, tmp_path):
    pipeline_path = Path("pipelines/slim/ready.dot")
    graph = parse(pipeline_path)

    # Mock gates to succeed
    def fake_gate(node, ctx):
        return Result(outcome="success", output="fake success")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_gate)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_es", fake_gate)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_er", fake_gate)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_slash", fake_gate)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_gate)

    ctx = Context(
        goal="Drive PR to /ready",
        workdir=tmp_path,
        backend="echo",
        state={
            "slim.test_command": "echo 'tests passed'",
            "feature": "ready_feature",
        },
    )

    history = run(graph, ctx)
    executed_nodes = [step.node for step in history]

    assert "start" in executed_nodes
    assert "round_begin" in executed_nodes
    assert "test" in executed_nodes
    assert "gate_es" in executed_nodes
    assert "gate_er" in executed_nodes
    assert "gate_advice" in executed_nodes
    assert "holdout" in executed_nodes
    assert "round_end" in executed_nodes
    assert "exit" in executed_nodes
    assert "fix" not in executed_nodes


def test_ready_pipeline_iteration_and_fix_loop(monkeypatch, tmp_path):
    pipeline_path = Path("pipelines/slim/ready.dot")
    graph = parse(pipeline_path)

    calls = {"gate_er": 0, "fix": 0}

    def fake_er(node, ctx):
        calls["gate_er"] += 1
        return Result(outcome="failure", output="er failure")

    def fake_success(node, ctx):
        return Result(outcome="success", output="success")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_es", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_er", fake_er)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_slash", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_success)

    ctx = Context(
        goal="Drive PR to /ready with fix iteration",
        workdir=tmp_path,
        backend="echo",
        state={
            "slim.test_command": "echo 'tests passed'",
            "feature": "ready_feature",
        },
    )

    history = run(graph, ctx, max_steps=50)
    executed_nodes = [step.node for step in history]

    assert "fix" in executed_nodes
    assert history[-1].outcome == "exhausted"
    assert history[-1].node == "round_end"
    assert calls["gate_er"] == 3
