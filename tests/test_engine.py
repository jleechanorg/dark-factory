"""Smoke tests for parser + engine.

Run with: source .venv/bin/activate && python -m pytest tests/
"""

from __future__ import annotations

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
    assert "start" in g.nodes
    assert "exit" in g.nodes
    assert "holdout" in g.nodes
    # fix -> holdout edge exists
    assert any(e.src == "fix" and e.dst == "holdout" for e in g.edges)


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
