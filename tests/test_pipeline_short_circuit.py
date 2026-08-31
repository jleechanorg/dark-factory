"""Pipeline control flow: gate-failure short-circuit, rc!=0 spoof guard.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import stat
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline, install_adversarial_reviewer_stub  # noqa: E402

from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.parser import parse  # noqa: E402


@pytest.fixture(autouse=True)
def _stub_fail_open_web_advice(monkeypatch):
    """Keep graph-control tests off the host's live /web-advice transport.

    These tests assert gate ordering and short-circuit behavior.  The
    fail-open advisory node is deliberately an external integration, so let
    the graph reach its exit without invoking ``gh``/browser transports or
    depending on operator credentials.
    """
    install_adversarial_reviewer_stub(monkeypatch)

    def fake_web_advice(node, ctx):
        return Result(
            outcome="success",
            output="web_advice fixture: fail-open advisory skipped",
            metadata={"web_advice_outcome": "fixture_skipped"},
        )

    monkeypatch.setitem(TYPE_REGISTRY, "web_advice", fake_web_advice)


def test_pr_gates_runs_holdout_before_evidence_gates(monkeypatch):
    """Holdout-always policy: pr_gates.dot runs sealed holdouts before the
    three adversarial gates, mirroring gates.dot."""
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("pr_gates.dot"))
    assert g.nodes["holdout"].attrs.get("type") == "holdout_eval"

    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state["gate_skeptic.outcome"] = "success"
    ctx.state["_df_controller_fixture"] = "cold-review-v1"
    ctx.state["adversarial_reviewer.outcome"] = "success"
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "success"
    ctx.state["gate_cs.outcome"] = "success"

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    assert nodes == [
        "start",
        "holdout",
        "gate_skeptic",
        "adversarial_reviewer",
        "gate_es",
        "gate_er",
        "gate_cs",
        "web_advice",
        "exit",
    ]
    assert history[-1].outcome == "success"


def test_pr_gates_holdout_failure_short_circuits(monkeypatch):
    """A holdout failure in pr_gates routes to the fix loop (per factory-evolve
    G2) instead of exiting. Evidence gates do not run on a holdout failure."""
    def fake_holdout(node, ctx):
        return Result(outcome="failure", output="holdout FAIL")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("pr_gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    assert "gate_es" not in nodes
    # The fix node loops back to holdout (bounded by max_visits). The pipeline
    # terminates once the fix loop exhausts (history[-1].outcome != success).
    assert "fix" in nodes
    assert history[-1].outcome in ("failure", "exhausted")


def test_gate_failure_short_circuits(monkeypatch):
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state["_df_controller_fixture"] = "cold-review-v1"
    ctx.state["gate_skeptic.outcome"] = "success"
    ctx.state["adversarial_reviewer.outcome"] = "success"
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "failure"  # fail at /er
    ctx.state["gate_cs.outcome"] = "success"

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    # gate_er failure now routes to fix (which loops back to holdout), not exit.
    # The run terminates when fix's max_visits is exhausted.
    assert "gate_er" in nodes
    assert "fix" in nodes
    assert history[-1].outcome in ("failure", "exhausted")


def test_gate_nonzero_returncode_cannot_spoof_pass(monkeypatch, tmp_path):
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    # gate_skeptic has no registered handler; default _codergen would call
    # claude --print /gate_skeptic (no such command). Mock it so the test
    # can focus on the gate_es rc!=0 spoof scenario.
    def fake_skeptic(node, ctx):
        pre = ctx.state.get(f"{node.name}.outcome")
        return Result(outcome=pre or "success", output=f"fake_skeptic({node.name})")
    monkeypatch.setitem(TYPE_REGISTRY, "gate_skeptic", fake_skeptic)
    install_adversarial_reviewer_stub(monkeypatch)

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    claude = bin_dir / "claude"
    claude.write_text("#!/bin/sh\nprintf 'VERDICT: PASS\\n'\nexit 19\n")
    claude.chmod(claude.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("PATH", f"{bin_dir}:{pathlib.Path('/usr/bin')}")

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="claude")
    ctx.state["gate_skeptic.outcome"] = "success"
    ctx.state["adversarial_reviewer.outcome"] = "success"

    history = run(g, ctx, max_steps=20)

    assert history[-1].outcome != "success"
    assert any(r.node == "gate_es" and r.outcome == "error" for r in history)
