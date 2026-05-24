"""Smoke tests for parser + engine.

Run with: source .venv/bin/activate && python -m pytest tests/
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


def test_parser_round_trip():
    g = parse(_pipeline("hello.dot"))
    assert g.name == "hello"
    assert g.goal == "Minimal smoke pipeline — plan, implement, holdout-eval, exit."
    assert "start" in g.nodes
    assert "exit" in g.nodes
    assert "holdout" in g.nodes
    # fix -> holdout edge exists
    assert any(e.src == "fix" and e.dst == "holdout" for e in g.edges)


def test_model_stylesheet_sets_node_backend_attributes(tmp_path):
    style = tmp_path / "minimal.model.css"
    style.write_text('* { backend: "echo" }\n.hot { backend: "mock_llm" }\n')
    dot = tmp_path / "styled.dot"
    dot.write_text(
        'digraph styled {\n'
        '  graph [goal="styled" model_stylesheet="minimal.model.css"]\n'
        '  start [shape=Mdiamond]\n'
        '  plan [type="codergen"]\n'
        '  implement [type="codergen", class="hot"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> plan -> implement -> exit\n'
        '}\n'
    )
    g = parse(dot)
    assert g.nodes["plan"].attrs["backend"] == "echo"
    assert g.nodes["implement"].attrs["backend"] == "mock_llm"


def test_echo_backend_loops_on_failed_holdout(monkeypatch, tmp_path):
    """If holdout always fails, fix loop should be bounded by max_visits."""
    calls = {"holdout": 0}

    def fake_holdout(node, ctx):
        calls["holdout"] += 1
        return Result(outcome="fail", output="fake fail")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    # Should terminate via fix-node max_visits=3 (4th visit triggers exhausted).
    assert history[-1].outcome == "exhausted"
    assert history[-1].node == "fix"
    assert calls["holdout"] >= 3


def test_echo_backend_green_path(monkeypatch):
    """Holdout succeeds → straight to exit."""
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="fake pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    nodes = [r.node for r in history]
    assert nodes == ["start", "plan", "implement", "holdout", "exit"]
    assert history[-1].outcome == "success"


def test_successful_validation_clears_prior_failure(monkeypatch):
    """A fix loop can recover if a later validation node succeeds."""
    calls = {"holdout": 0}

    def fake_holdout(node, ctx):
        calls["holdout"] += 1
        if calls["holdout"] == 1:
            return Result(outcome="fail", output="first fail")
        return Result(outcome="success", output="recovered")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    assert [r.node for r in history][-3:] == ["fix", "holdout", "exit"]
    assert history[-1].outcome == "success"


def test_cli_invocation_green():
    """End-to-end: run the CLI with the real holdout evaluator against the impl tree."""
    proc = subprocess.run(
        [
            sys.executable, "-m", "runner",
            "--pipeline", str(_pipeline("hello.dot")),
            "--goal", "smoke",
            "--backend", "echo",
            "--feature", "hello",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    # The greet.py impl ships with the repo so the holdout should pass.
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert '"final_outcome": "success"' in proc.stdout


def test_cli_feature_flag_overrides_dot_feature():
    proc = subprocess.run(
        [
            sys.executable, "-m", "runner",
            "--pipeline", str(_pipeline("hello.dot")),
            "--goal", "smoke",
            "--backend", "echo",
            "--feature", "does-not-exist",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert proc.returncode == 1
    assert '"final_outcome": "success"' not in proc.stdout


def test_max_steps_before_exit_is_failure():
    """A run that stops before reaching exit must not report success."""
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=1)

    assert history[-1].outcome == "exhausted"
    assert "max_steps=1" in history[-1].output_preview


def test_engine_resume_from_checkpoint(tmp_path):
    dot = tmp_path / "resume.dot"
    dot.write_text(
        'digraph resume {\n'
        '  graph [goal="Resume test" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  one [type="codergen", label="One"]\n'
        '  two [type="codergen", label="Two"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> one -> two -> exit\n'
        '}\n'
    )
    checkpoint = tmp_path / "checkpoint.json"
    checkpoint.write_text(
        json.dumps(
            [
                {
                    "node": "start",
                    "outcome": "success",
                    "ts": 0.0,
                    "output_preview": "start",
                    "metadata": {},
                },
                {
                    "node": "one",
                    "outcome": "success",
                    "ts": 0.0,
                    "output_preview": "one",
                    "metadata": {},
                },
            ]
        )
    )
    graph = parse(dot)
    ctx = Context(goal="resume", workdir=ROOT, backend="echo")
    history = run(graph, ctx, resume=checkpoint, max_steps=10)

    assert [step.node for step in history] == ["start", "one", "two", "exit"]


def test_engine_parallel_fanout_with_join_quorum_and_allow_partial(tmp_path, monkeypatch):
    dot = tmp_path / "parallel.dot"
    dot.write_text(
        'digraph parallel {\n'
        '  graph [goal="Parallel gate test" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  fan [type="codergen", label="Fanout", parallel="true", allow_partial="true", join_quorum=2]\n'
        '  branch_pass [type="branch_pass", label="Branch pass"]\n'
        '  branch_partial [type="branch_partial", label="Branch partial"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> fan\n'
        '  fan -> branch_pass\n'
        '  fan -> branch_partial\n'
        '  fan -> exit [join=true]\n'
        '}\n'
    )
    monkeypatch.setitem(TYPE_REGISTRY, "branch_pass", lambda node, ctx: Result(outcome="success", output="ok"))
    monkeypatch.setitem(TYPE_REGISTRY, "branch_partial", lambda node, ctx: Result(outcome="partial", output="ok"))

    graph = parse(dot)
    ctx = Context(goal="parallel", workdir=ROOT, backend="echo")
    history = run(graph, ctx, max_steps=20)

    assert [step.node for step in history] == [
        "start",
        "fan",
        "branch_pass",
        "branch_partial",
        "exit",
    ]
    assert history[-1].outcome == "success"
