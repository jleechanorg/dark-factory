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
    assert "test" in node_names
    assert "gate_es" in node_names
    assert "gate_er" in node_names
    assert "gate_advice" in node_names
    assert "holdout" in node_names
    assert "fix" in node_names

    fix_node = graph.nodes["fix"]
    assert fix_node is not None
    assert fix_node.attrs.get("max_visits") == 3, "fix node must enforce max_visits=3"
    assert fix_node.attrs.get("type") == "codergen"

    advice_node = graph.nodes["gate_advice"]
    assert advice_node is not None
    assert advice_node.attrs.get("type") == "gate_slash"
    assert advice_node.attrs.get("command") == "advice"


def test_ready_pipeline_gate_advice_is_gate_strict():
    """gate_advice must set gate_strict="true" so a Codex `verdict: warn`
    (real disagreement, e.g. NOT APPROVED buried under an AGY-synthesized
    warn) fails the gate instead of the legacy warn->success mapping
    silently treating disagreement as approval (2026-08-29 false-positive
    /factory --pipeline ready run)."""
    from runner.handler_core import _gate_strict_flag

    graph = parse(Path("pipelines/slim/ready.dot"))
    advice_node = graph.nodes["gate_advice"]
    assert _gate_strict_flag(advice_node) is True, (
        "gate_advice missing gate_strict='true'; a warn verdict from /advice "
        "would be normalized to success and mask reviewer disagreement."
    )


def test_ready_pipeline_advice_warn_fails_gate(monkeypatch, tmp_path):
    """End-to-end: a gate_advice lane that returns `verdict: warn` (the
    AGY-synthesized outcome when its inner Codex review disagrees) must
    route to `fix`, not `exit`, now that gate_strict is set.

    The fake gate_slash handler below re-runs the real
    `_gate_strict_flag` + `_parse_verdict` chain that `_gate_slash` uses
    internally (instead of hardcoding outcome="success"), so this test
    actually exercises the DOT attribute -> normalization wiring rather
    than just asserting on a canned Result."""
    from runner.handler_core import _gate_strict_flag
    from runner.handler_verdict import _parse_verdict

    pipeline_path = Path("pipelines/slim/ready.dot")
    graph = parse(pipeline_path)

    def fake_success(node, ctx):
        return Result(outcome="success", output="verdict: pass")

    def fake_advice_warn(node, ctx):
        raw, outcome = _parse_verdict(
            "verdict: warn", gate_strict=_gate_strict_flag(node)
        )
        return Result(outcome=outcome, output=f"verdict: {raw}")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_es", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_er", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_slash", fake_advice_warn)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_success)

    ctx = Context(
        goal="Drive PR to /ready",
        workdir=tmp_path,
        backend="echo",
        state={
            "slim.test_command": "echo 'tests passed'",
            "feature": "ready_feature",
        },
    )

    history = run(graph, ctx, max_steps=50)
    executed_nodes = [step.node for step in history]

    assert "exit" not in executed_nodes, (
        "gate_advice returned a warn verdict but the pipeline reached exit; "
        "gate_strict is not being enforced on /advice."
    )
    assert "fix" in executed_nodes


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
    assert "test" in executed_nodes
    assert "gate_es" in executed_nodes
    assert "gate_er" in executed_nodes
    assert "gate_advice" in executed_nodes
    assert "holdout" in executed_nodes
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
    assert history[-1].node == "fix"
    assert calls["gate_er"] >= 2
