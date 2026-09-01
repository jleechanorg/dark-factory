from __future__ import annotations

import pathlib
import sys
import tempfile

import runner.handlers as handlers_mod
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.engine import run
from runner.parser import parse

ROOT = pathlib.Path(__file__).parent.parent

# Scratch workdir in the OS tempdir — using the repo root here leaks one
# branch_* mkdtemp per fan-out test into the working tree.
SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="test_slim_"))

from conftest import init_git_repo, register_scratch_dir  # noqa: E402

register_scratch_dir(SCRATCH)
init_git_repo(SCRATCH)


def test_slim_stylesheet_routes_roles_by_class():
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    # explore is now a 4-way fanout from _base.dot; pick the concept sub-agent
    explore = graph.nodes["explore_concept"].attrs
    plan = graph.nodes["plan"].attrs
    implement = graph.nodes["implement"].attrs
    fix = graph.nodes["fix"].attrs
    review = graph.nodes["review"].attrs

    assert explore.get("class") == "explore"
    assert "backend" not in explore
    assert "model_name" not in explore

    assert plan.get("class") == "plan"
    assert plan["backend"] == "claude"
    assert plan["model_name"] == "claude-opus-4-6"

    assert implement.get("class") == "implement"
    assert "backend" not in implement
    assert "model_name" not in implement

    assert fix.get("class") == "fix"
    assert "backend" not in fix
    assert "model_name" not in fix

    assert review.get("class") == "review"
    assert review["backend"] == "codex"


def test_slim_generic_review_nodes_use_fresh_codex_contract():
    """Every generic slim review node uses the bounded fresh reviewer contract."""
    pipelines = (
        "minimal_feature.dot",
        "bugfix_noholdout.dot",
        "brownfield_delete_first.dot",
        "minimal_pr.dot",
        "minimal_feature_cs.dot",
        "review_pr.dot",
    )
    for filename in pipelines:
        graph = parse(ROOT / "pipelines" / "slim" / filename)
        review = graph.nodes["review"]
        assert review.attrs.get("type") == "codergen", filename
        assert review.attrs.get("class") == "review", filename
        assert review.attrs.get("backend") == "codex", filename
        assert review.attrs.get("prompt") == "@prompts/slim/fresh_review.md", filename
        assert review.attrs.get("fresh_session") == "true", filename
        assert review.attrs.get("verdict_gate") == "true", filename


def test_minimal_feature_factory_runs_with_deterministic_gates(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", lambda node, ctx: Result(outcome="success", output="ok"))
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    # production routes plan→claude opus and review→codex; pin echo for offline determinism
    graph.nodes["plan"].attrs["backend"] = "echo"
    graph.nodes["review"].attrs["backend"] = "echo"
    ctx = Context(goal="ship a tiny feature", workdir=SCRATCH, backend="echo")
    ctx.state["feature"] = "hello"
    ctx.state["slim.test_command"] = f"{sys.executable} -c \"print('tests ok')\""

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "explore_in",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_stitch",
        "explore_out",
        "plan",
        "implement",
        "test",
        "review",
        "holdout",
        "gate_es",
        "gate_er",
        "exit",
    ]


def test_minimal_pr_has_research_node_on_coder_tier():
    """minimal_pr.dot wires explore_out -> research -> plan; research is
    classless so it honors the run-level --backend (coder tier)."""
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_pr.dot")
    research = graph.nodes.get("research")
    assert research is not None, "minimal_pr.dot is missing the research node"
    assert research.attrs.get("type") == "codergen"
    assert "class" not in research.attrs, "research must ride the coder tier"
    assert research.attrs.get("prompt") == "@prompts/slim/research.md"


def test_minimal_pr_factory_runs_with_deterministic_gates(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", lambda node, ctx: Result(outcome="success", output="ok"))
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_pr.dot")
    # production routes plan→claude opus and review→codex; pin echo for offline determinism
    graph.nodes["plan"].attrs["backend"] = "echo"
    graph.nodes["review"].attrs["backend"] = "echo"
    ctx = Context(goal="refactor a tiny thing in-flight", workdir=SCRATCH, backend="echo")
    ctx.state["feature"] = "hello"
    ctx.state["slim.test_command"] = f"{sys.executable} -c \"print('tests ok')\""

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "explore_in",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_stitch",
        "explore_out",
        "research",
        "plan",
        "implement",
        "test",
        "review",
        "holdout",
        "gate_es",
        "gate_er",
        "web_advice",
        "exit",
    ]


def test_minimal_research_factory_runs_with_deterministic_gates(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    graph = parse(ROOT / "pipelines" / "slim" / "minimal_research.dot")
    ctx = Context(goal="research some code", workdir=SCRATCH, backend="echo")
    # gate_er is echo-short-circuited by --backend echo; pre-seed a success
    # verdict so the lane exits deterministically without invoking a real
    # reviewer (per factory-evolve G1, research must pass a reviewer).
    ctx.state["gate_er.outcome"] = "success"

    history = run(graph, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "explore_in",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_stitch",
        "explore_out",
        "research",
        "gate_er",
        "exit",
    ]

