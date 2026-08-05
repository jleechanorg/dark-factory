"""Tests for Attractor-compatible edge-selection semantics.

Verifies:
1. Hexagon decision nodes evaluating conditions against ctx.state.
2. Support for key=value and key!=value expressions.
3. Priority of conditional edges over unconditional edges.
"""

from __future__ import annotations

import pathlib
import sys

import tempfile
import pytest

ROOT = pathlib.Path(__file__).parent.parent

# Scratch workdir in the OS tempdir — using the repo root here leaked one
# branch_* mkdtemp per fan-out test into the working tree.
SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="attractor_semantics_"))
sys.path.insert(0, str(ROOT))

from runner.engine import run, _edge_matches
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import Edge, Graph, Node

from conftest import register_scratch_dir  # noqa: E402

register_scratch_dir(SCRATCH)


def test_hexagon_node_decision_key_from_state():
    """Verify that a hexagon node (with shape='hexagon' or type='conditional')
    evaluates outgoing edge conditions against the context state,
    and supports both key=value and key!=value.
    """
    nodes = {
        "start": Node(name="start"),
        "decide": Node(name="decide", attrs={"shape": "hexagon", "decision_key": "choice"}),
        "yes_branch": Node(name="yes_branch"),
        "no_branch": Node(name="no_branch"),
        "other_branch": Node(name="other_branch"),
        "exit": Node(name="exit"),
    }
    # decide -> yes_branch if choice=yes
    # decide -> no_branch if choice=no
    # decide -> other_branch if choice!=yes and choice is maybe
    edges = [
        Edge(src="start", dst="decide"),
        Edge(src="decide", dst="yes_branch", attrs={"condition": "choice=yes"}),
        Edge(src="decide", dst="no_branch", attrs={"condition": "choice=no"}),
        Edge(src="decide", dst="other_branch", attrs={"condition": "choice!=yes"}),
        Edge(src="yes_branch", dst="exit"),
        Edge(src="no_branch", dst="exit"),
        Edge(src="other_branch", dst="exit"),
    ]
    graph = Graph(name="hex_test", goal="test hexagon", nodes=nodes, edges=edges)

    # Choice = yes -> should go to yes_branch (and other_branch should not be visited because choice!=yes is false)
    ctx = Context(goal="test", workdir=SCRATCH, backend="echo")
    ctx.state["choice"] = "yes"
    history = run(graph, ctx, max_steps=10)
    visited = [r.node for r in history]
    assert "yes_branch" in visited
    assert "no_branch" not in visited
    assert "other_branch" not in visited

    # Choice = maybe -> should go to other_branch (since choice!=yes is true, and choice=yes/no are false)
    ctx3 = Context(goal="test", workdir=SCRATCH, backend="echo")
    ctx3.state["choice"] = "maybe"
    history3 = run(graph, ctx3, max_steps=10)
    visited3 = [r.node for r in history3]
    assert "other_branch" in visited3
    assert "yes_branch" not in visited3
    assert "no_branch" not in visited3


def test_hexagon_node_default_decision_key():
    """If no decision_key is specified on a hexagon node, it should default to the node name."""
    nodes = {
        "start": Node(name="start"),
        "decide_default": Node(name="decide_default", attrs={"shape": "hexagon"}),
        "yes_branch": Node(name="yes_branch"),
        "exit": Node(name="exit"),
    }
    edges = [
        Edge(src="start", dst="decide_default"),
        Edge(src="decide_default", dst="yes_branch", attrs={"condition": "decide_default=yes"}),
        Edge(src="yes_branch", dst="exit"),
    ]
    graph = Graph(name="hex_default_test", goal="test hexagon default key", nodes=nodes, edges=edges)

    ctx = Context(goal="test", workdir=SCRATCH, backend="echo")
    ctx.state["decide_default"] = "yes"
    history = run(graph, ctx, max_steps=10)
    visited = [r.node for r in history]
    assert "yes_branch" in visited


def test_conditional_edges_priority_over_unconditional():
    """Verify that conditional edges take priority over unconditional edges."""
    nodes = {
        "start": Node(name="start"),
        "check": Node(name="check", attrs={"shape": "ellipse"}),
        "cond_branch": Node(name="cond_branch"),
        "uncond_branch": Node(name="uncond_branch"),
        "exit": Node(name="exit"),
    }
    # From 'check', we have a conditional edge to 'cond_branch' if status=ok,
    # and an unconditional edge to 'uncond_branch'.
    edges = [
        Edge(src="start", dst="check"),
        Edge(src="check", dst="cond_branch", attrs={"condition": "status=ok"}),
        Edge(src="check", dst="uncond_branch"),
        Edge(src="cond_branch", dst="exit"),
        Edge(src="uncond_branch", dst="exit"),
    ]
    graph = Graph(name="priority_test", goal="test priority", nodes=nodes, edges=edges)

    # If status is "ok", the conditional edge matches, so it MUST go to cond_branch.
    ctx = Context(goal="test", workdir=SCRATCH, backend="echo")

    def custom_check_ok(node, ctx):
        return Result(outcome="success", metadata={"status": "ok"})

    TYPE_REGISTRY["custom_check_ok"] = custom_check_ok
    nodes["check"].attrs["type"] = "custom_check_ok"

    history = run(graph, ctx, max_steps=10)
    visited = [r.node for r in history]
    assert "cond_branch" in visited
    assert "uncond_branch" not in visited

    # If status is "bad", the conditional edge does NOT match, so it MUST fall back to uncond_branch.
    ctx2 = Context(goal="test", workdir=SCRATCH, backend="echo")

    def custom_check_bad(node, ctx):
        return Result(outcome="success", metadata={"status": "bad"})

    TYPE_REGISTRY["custom_check_bad"] = custom_check_bad
    nodes["check"].attrs["type"] = "custom_check_bad"

    history2 = run(graph, ctx2, max_steps=10)
    visited2 = [r.node for r in history2]
    assert "uncond_branch" in visited2
    assert "cond_branch" not in visited2
