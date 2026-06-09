"""End-to-end tests for pipelines/bug_fix.dot — the red/green bug-fix lane."""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse

BUG_FIX = ROOT / "pipelines" / "bug_fix.dot"


def test_bug_fix_runs_red_to_green_to_exit(tmp_path, monkeypatch):
    """Successful flow: red gate passes (test fails), fix runs, green gate passes (test passes).

    We mock both gates (gate_red and gate_green) to return success so the pipeline
    walks the full path: reproduce -> gate_red -> fix -> gate_green -> exit.
    """
    test_path = tmp_path / "tests" / "test_repro.py"
    test_path.parent.mkdir(parents=True, exist_ok=True)
    test_path.write_text("def test_x():\n    assert 1 == 2\n")  # never read; gates are mocked

    def fake_codergen(node, ctx):
        if node.name == "reproduce":
            ctx.state["bug_fix.test_path"] = str(test_path)
        return Result(outcome="success", output=f"fake {node.name}")

    def fake_gate_red(node, ctx):
        return Result(outcome="success", output="RED OK (mocked)")

    def fake_gate_green(node, ctx):
        return Result(outcome="success", output="GREEN OK (mocked)")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_red", fake_gate_red)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_green", fake_gate_green)

    g = parse(BUG_FIX)
    ctx = Context(goal="fix the bug", workdir=tmp_path, backend="echo")
    ctx.state["bug_fix.test_path"] = str(test_path)
    history = run(g, ctx, max_steps=80)

    nodes = [r.node for r in history]
    assert history[-1].outcome == "success", f"final history: {history}"
    # expected trajectory: start, explore_*, plan, reproduce, gate_red, fix,
    # gate_green, evidence (adversarial review), exit
    expected_post_explore = [
        "plan", "reproduce", "gate_red", "fix", "gate_green", "evidence", "exit",
    ]
    assert nodes[-len(expected_post_explore):] == expected_post_explore
    assert nodes[0] == "start"


def test_bug_fix_exits_when_red_gate_cant_reproduce(tmp_path, monkeypatch):
    """If the red gate fails (test passes — bug not reproduced), pipeline exits at gate_red."""
    passing = tmp_path / "tests" / "test_passes.py"
    passing.parent.mkdir(parents=True, exist_ok=True)
    passing.write_text("def test_x():\n    assert 1 == 1\n")

    def fake_codergen(node, ctx):
        if node.name == "reproduce":
            ctx.state["bug_fix.test_path"] = str(passing)
        return Result(outcome="success", output=f"fake {node.name}")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)

    g = parse(BUG_FIX)
    ctx = Context(goal="fix the bug", workdir=tmp_path, backend="echo")
    ctx.state["bug_fix.test_path"] = str(passing)
    history = run(g, ctx, max_steps=80)

    nodes = [r.node for r in history]
    # We should hit gate_red and exit there (fix must NOT run when bug not reproduced)
    assert "gate_red" in nodes
    assert "fix" not in nodes, (
        f"fix ran even though red gate failed; full path: {nodes}"
    )
    assert history[-1].node == "exit"


def test_bug_fix_fix_loops_back_when_green_fails(tmp_path, monkeypatch):
    """If green gate fails, fix runs again (bounded by max_visits=3)."""
    # Failing test that never becomes passing → green always fails → fix loops
    never_passes = tmp_path / "tests" / "test_never.py"
    never_passes.parent.mkdir(parents=True, exist_ok=True)
    never_passes.write_text("def test_x():\n    assert 1 == 2\n")

    fix_visits = {"n": 0}

    def fake_codergen(node, ctx):
        if node.name == "reproduce":
            ctx.state["bug_fix.test_path"] = str(never_passes)
        if node.name == "fix":
            fix_visits["n"] += 1
        return Result(outcome="success", output=f"fake {node.name}")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)

    g = parse(BUG_FIX)
    ctx = Context(goal="fix the bug", workdir=tmp_path, backend="echo")
    ctx.state["bug_fix.test_path"] = str(never_passes)
    history = run(g, ctx, max_steps=80)

    # fix should have run at least once and bounded by max_visits=3
    assert fix_visits["n"] >= 1
    # green gate always fails → the last visited fix hits max_visits and is exhausted
    assert history[-1].outcome in ("exhausted", "failure", "error")


def test_bug_fix_evidence_fail_terminates_as_failure(tmp_path, monkeypatch):
    """A real adversarial-review fail must surface as the run's terminal
    outcome — reviewed-and-rejected code never exits silently green."""
    test_path = tmp_path / "tests" / "test_repro.py"
    test_path.parent.mkdir(parents=True, exist_ok=True)
    test_path.write_text("def test_x():\n    assert 1 == 2\n")

    def fake_codergen(node, ctx):
        if node.name == "reproduce":
            ctx.state["bug_fix.test_path"] = str(test_path)
        return Result(outcome="success", output=f"fake {node.name}")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    monkeypatch.setitem(
        TYPE_REGISTRY, "gate_red",
        lambda node, ctx: Result(outcome="success", output="RED OK (mocked)"))
    monkeypatch.setitem(
        TYPE_REGISTRY, "gate_green",
        lambda node, ctx: Result(outcome="success", output="GREEN OK (mocked)"))

    g = parse(BUG_FIX)
    ctx = Context(goal="fix the bug", workdir=tmp_path, backend="echo")
    ctx.state["bug_fix.test_path"] = str(test_path)
    ctx.state["evidence.outcome"] = "failure"  # echo gate_er pre-seed
    history = run(g, ctx, max_steps=80)

    nodes = [r.node for r in history]
    assert "evidence" in nodes
    assert history[-1].node == "exit"
    assert history[-1].outcome == "failure", (
        f"evidence fail must propagate to the terminal outcome; history: {history}"
    )


def test_bug_fix_has_adversarial_evidence_node():
    """bug_fix.dot must carry a gate_er reviewer with prefer_adversarial=true
    — the lane never ships unreviewed code (jleechan-7je / issue #28)."""
    g = parse(BUG_FIX)
    evidence = g.nodes.get("evidence")
    assert evidence is not None, "bug_fix.dot is missing the evidence reviewer node"
    assert evidence.attrs.get("type") == "gate_er"
    assert str(evidence.attrs.get("prefer_adversarial")).lower() == "true"
    assert "codex" in str(evidence.attrs.get("backend_priority", ""))


def test_bug_fix_max_visits_is_three():
    """The fix node in bug_fix.dot carries max_visits=3 (red discipline)."""
    g = parse(BUG_FIX)
    # AttrValue is str | int | bool; the parser stores unquoted "3" as int 3
    assert str(g.nodes["fix"].attrs.get("max_visits")) == "3"
