"""Tests for graph-level max_visits loop-bound hygiene (bead jleechan-bt3).

The `max_visits` attribute is a per-node per-pipeline cap on how many times
the engine may visit a single node within one run. When `visits[name] >
max_visits`, the engine emits a synthetic `exhausted` step and terminates.
This is distinct from handler-level `max_retries`, which controls
per-codergen attempt re-execution on a single visit.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402

from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.parser import parse  # noqa: E402


def test_max_visits_attribute_parsed(tmp_path):
    """Parser must read `max_visits="N"` as an integer attribute on a node."""
    dot = tmp_path / "bounded.dot"
    dot.write_text(
        'digraph bounded {\n'
        '  graph [goal="bounded loop" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  fix [type="codergen", label="Fix", max_visits="3"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> fix\n'
        '  fix -> exit\n'
        '}\n'
    )
    g = parse(dot)
    fix_node = g.nodes["fix"]
    assert int(fix_node.attrs["max_visits"]) == 3


def test_engine_emits_exhausted_after_max_visits(tmp_path, monkeypatch):
    """When a node is visited more than max_visits times, the engine emits
    an `exhausted` step on that node and terminates the run."""
    dot = tmp_path / "loop.dot"
    dot.write_text(
        'digraph loop {\n'
        '  graph [goal="loop" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  ping [type="codergen", label="Ping", max_visits="2"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> ping\n'
        '  ping -> ping  [condition="outcome=success"]\n'
        '  ping -> exit\n'
        '}\n'
    )

    def fake_codergen(node, ctx):
        return Result(outcome="success", output="ping ok")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)

    g = parse(dot)
    ctx = Context(goal="loop test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    assert history[-1].node == "ping"
    assert history[-1].outcome == "exhausted"
    assert "max_visits=2" in history[-1].output_preview
    ping_visits = sum(1 for r in history if r.node == "ping")
    assert ping_visits >= 3


def test_hello_dot_has_max_visits_on_fix():
    """Regression guard: hello.dot's `fix` node must declare max_visits."""
    g = parse(_pipeline("hello.dot"))
    assert "fix" in g.nodes
    fix_attrs = g.nodes["fix"].attrs
    assert "max_visits" in fix_attrs, "hello.dot fix node missing max_visits bound"
    assert int(fix_attrs["max_visits"]) == 3


def test_hello_dot_has_exhausted_routing_edge():
    """Regression guard: hello.dot must include a `fix -> exit` edge gated
    on `outcome=exhausted` as a safety net."""
    g = parse(_pipeline("hello.dot"))
    exhausted_edges = [
        e
        for e in g.edges
        if e.src == "fix" and e.dst == "exit" and e.condition == "outcome=exhausted"
    ]
    assert exhausted_edges, "hello.dot missing fix -> exit [outcome=exhausted] edge"


def test_slim_pipelines_have_max_visits_on_fix():
    """Regression guard: slim feature/PR pipelines must bound their fix loops."""
    for pipeline_name in ("minimal_feature.dot", "minimal_pr.dot"):
        path = ROOT / "pipelines" / "slim" / pipeline_name
        g = parse(path)
        fix_attrs = g.nodes["fix"].attrs
        assert "max_visits" in fix_attrs, (
            f"{pipeline_name} fix node missing max_visits bound"
        )
        assert int(fix_attrs["max_visits"]) == 3
