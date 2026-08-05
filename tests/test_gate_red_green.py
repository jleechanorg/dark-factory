"""Tests for the _gate_red and _gate_red/_gate_green handlers.

These handlers shell out to pytest and assert the inverse direction of the
red/green TDD discipline: gate_red passes when the test FAILS, gate_green
passes when the test PASSES.
"""

from __future__ import annotations

import pathlib
import sys
import textwrap

import tempfile
import pytest

ROOT = pathlib.Path(__file__).parent.parent

# Scratch workdir in the OS tempdir — using the repo root here leaked one
# branch_* mkdtemp per fan-out test into the working tree.
SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="gate_red_green_"))
sys.path.insert(0, str(ROOT))

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse, Node

from conftest import register_scratch_dir  # noqa: E402

register_scratch_dir(SCRATCH)


def _make_node(attrs: dict[str, str]) -> Node:
    """Build a minimal Node that the gate handlers can consume."""
    return Node(name="gate", attrs=dict(attrs))


def test_gate_red_passes_when_test_fails(tmp_path):
    """A failing pytest is what gate_red considers success."""
    from runner.handlers import _gate_red

    failing_test = tmp_path / "test_failing.py"
    failing_test.write_text(
        "def test_x():\n    assert 1 == 2\n"
    )
    node = _make_node({"test_path": str(failing_test)})
    ctx = Context(goal="g", workdir=tmp_path, backend="echo")
    result = _gate_red(node, ctx)
    assert result.outcome == "success", result.output
    assert "RED OK" in result.output


def test_gate_red_fails_when_test_passes(tmp_path):
    """A passing pytest is what gate_red considers a FAILURE (bug not reproduced)."""
    from runner.handlers import _gate_red

    passing_test = tmp_path / "test_passing.py"
    passing_test.write_text("def test_x():\n    assert 1 == 1\n")
    node = _make_node({"test_path": str(passing_test)})
    ctx = Context(goal="g", workdir=tmp_path, backend="echo")
    result = _gate_red(node, ctx)
    assert result.outcome == "failure", result.output
    assert "RED FAIL" in result.output


def test_gate_green_passes_when_test_passes(tmp_path):
    """A passing pytest is what gate_green considers success."""
    from runner.handlers import _gate_green

    passing_test = tmp_path / "test_passing.py"
    passing_test.write_text("def test_x():\n    assert 1 == 1\n")
    node = _make_node({"test_path": str(passing_test)})
    ctx = Context(goal="g", workdir=tmp_path, backend="echo")
    result = _gate_green(node, ctx)
    assert result.outcome == "success", result.output
    assert "GREEN OK" in result.output


def test_gate_green_fails_when_test_fails(tmp_path):
    """A still-failing pytest is what gate_green considers FAILURE (fix incomplete)."""
    from runner.handlers import _gate_green

    failing_test = tmp_path / "test_failing.py"
    failing_test.write_text("def test_x():\n    assert 1 == 2\n")
    node = _make_node({"test_path": str(failing_test)})
    ctx = Context(goal="g", workdir=tmp_path, backend="echo")
    result = _gate_green(node, ctx)
    assert result.outcome == "failure", result.output
    assert "GREEN FAIL" in result.output


def test_gate_red_uses_state_substitution(tmp_path):
    """When test_path contains ${state.bug_fix.test_path}, it is substituted from ctx.state."""
    from runner.handlers import _gate_red

    passing_test = tmp_path / "test_passing.py"
    passing_test.write_text("def test_x():\n    assert 1 == 1\n")
    node = _make_node({"test_path": "${state.bug_fix.test_path}"})
    ctx = Context(goal="g", workdir=tmp_path, backend="echo")
    ctx.state["bug_fix.test_path"] = str(passing_test)
    result = _gate_red(node, ctx)
    # test passes -> red gate fails (bug was not reproduced)
    assert result.outcome == "failure"
    assert "RED FAIL" in result.output


def test_gate_red_with_unresolved_state_path_fails():
    """When state substitution leaves a literal ${state.*} marker, gate_red fails cleanly."""
    from runner.handlers import _gate_red

    node = _make_node({"test_path": "${state.bug_fix.test_path}"})
    ctx = Context(goal="g", workdir=SCRATCH, backend="echo")
    # no state set
    result = _gate_red(node, ctx)
    assert result.outcome == "failure"
    assert "unresolved" in result.output.lower() or "test_path" in result.output
