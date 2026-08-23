"""Tests for folding web_advice fail-open advisory node into:
1. pipelines/factory/pr_gates.dot (min_diff_lines="5")
2. pipelines/factory/gates.dot (min_diff_lines="5")
3. pipelines/slim/minimal_pr.dot (min_diff_lines="20")

Acceptance criteria verification:
- Each target pipeline contains a web_advice node after strict gates.
- min_diff_lines is correctly configured ("5" for factory gates, "20" for slim minimal_pr).
- Each web_advice node has a single unconditional outgoing edge to downstream (exit).
- Handler runner/handler_web_advice.py and prompt prompts/web_advice.txt exist and are reused.
- Graph audit passes with 0 violations across the pipeline corpus.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner import graph_audit
from runner.handlers import TYPE_REGISTRY
from runner.parser import parse


def test_handler_and_prompt_reused():
    """Verify handler and prompt files exist and are registered."""
    handler_path = ROOT / "runner" / "handler_web_advice.py"
    prompt_path = ROOT / "prompts" / "web_advice.txt"

    assert handler_path.exists(), f"Handler missing at {handler_path}"
    assert prompt_path.exists(), f"Prompt missing at {prompt_path}"
    assert "web_advice" in TYPE_REGISTRY, "web_advice must be registered in TYPE_REGISTRY"


def test_pr_gates_dot_web_advice_integration():
    """pr_gates.dot contains web_advice after strict gates with min_diff_lines=5 and unconditional exit edge."""
    pipeline_path = ROOT / "pipelines" / "factory" / "pr_gates.dot"
    g = parse(pipeline_path)

    assert "web_advice" in g.nodes, "pr_gates.dot missing web_advice node"
    node = g.nodes["web_advice"]
    assert node.attrs.get("type") == "web_advice"
    assert node.attrs.get("min_diff_lines") == "5"
    assert node.attrs.get("prompt") == "@prompts/web_advice.txt"
    assert node.attrs.get("timeout") == "900" or int(node.attrs.get("timeout", 0)) == 900

    # Inflow: gate_cs -> web_advice on outcome=success
    in_edges = [e for e in g.edges if e.dst == "web_advice"]
    assert len(in_edges) == 1
    assert in_edges[0].src == "gate_cs"
    assert in_edges[0].condition == "outcome=success"

    # Outflow: single unconditional edge to exit (fail-open)
    out_edges = [e for e in g.edges if e.src == "web_advice"]
    assert len(out_edges) == 1
    assert out_edges[0].dst == "exit"
    assert out_edges[0].condition is None


def test_gates_dot_web_advice_integration():
    """gates.dot contains web_advice after strict gates with min_diff_lines=5 and unconditional exit edge."""
    pipeline_path = ROOT / "pipelines" / "factory" / "gates.dot"
    g = parse(pipeline_path)

    assert "web_advice" in g.nodes, "gates.dot missing web_advice node"
    node = g.nodes["web_advice"]
    assert node.attrs.get("type") == "web_advice"
    assert node.attrs.get("min_diff_lines") == "5"
    assert node.attrs.get("prompt") == "@prompts/web_advice.txt"
    assert node.attrs.get("timeout") == "900" or int(node.attrs.get("timeout", 0)) == 900

    # Inflow: gate_cs -> web_advice on outcome=success
    in_edges = [e for e in g.edges if e.dst == "web_advice"]
    assert len(in_edges) == 1
    assert in_edges[0].src == "gate_cs"
    assert in_edges[0].condition == "outcome=success"

    # Outflow: single unconditional edge to exit (fail-open)
    out_edges = [e for e in g.edges if e.src == "web_advice"]
    assert len(out_edges) == 1
    assert out_edges[0].dst == "exit"
    assert out_edges[0].condition is None


def test_minimal_pr_dot_web_advice_integration():
    """minimal_pr.dot contains web_advice after strict gates with min_diff_lines=20 and unconditional exit edge."""
    pipeline_path = ROOT / "pipelines" / "slim" / "minimal_pr.dot"
    g = parse(pipeline_path)

    assert "web_advice" in g.nodes, "minimal_pr.dot missing web_advice node"
    node = g.nodes["web_advice"]
    assert node.attrs.get("type") == "web_advice"
    assert node.attrs.get("min_diff_lines") == "20"
    assert node.attrs.get("prompt") == "@prompts/web_advice.txt"
    assert node.attrs.get("timeout") == "900" or int(node.attrs.get("timeout", 0)) == 900

    # Inflow: gate_er -> web_advice on outcome=success
    in_edges = [e for e in g.edges if e.dst == "web_advice"]
    assert len(in_edges) == 1
    assert in_edges[0].src == "gate_er"
    assert in_edges[0].condition == "outcome=success"

    # Outflow: single unconditional edge to exit (fail-open)
    out_edges = [e for e in g.edges if e.src == "web_advice"]
    assert len(out_edges) == 1
    assert out_edges[0].dst == "exit"
    assert out_edges[0].condition is None


def test_pipeline_graph_audit_clean():
    """Verify graph_audit on the entire pipelines directory reports 0 violations."""
    violations = graph_audit.audit_graphs(ROOT / "pipelines")
    assert violations == [], f"Expected 0 violations, got: {violations}"
