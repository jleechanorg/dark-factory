"""Tests for pipelines/slim/spec_gen.dot and its prompts (jleechan-6a6 / #34)."""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

import runner.handlers as handlers_mod
from runner.engine import run
from runner.handlers import Context, TYPE_REGISTRY
from runner.parser import parse

SPEC_GEN = ROOT / "pipelines" / "slim" / "spec_gen.dot"


# ---------------------------------------------------------------------------
# Graph structure / parse tests
# ---------------------------------------------------------------------------


def test_spec_gen_dot_parses_successfully():
    """spec_gen.dot must parse without errors."""
    g = parse(SPEC_GEN)
    assert g is not None
    assert g.name == "SpecGenFactory"


def test_spec_gen_has_required_nodes():
    g = parse(SPEC_GEN)
    for node_name in ("start", "exit", "plan", "spec_review", "fix_spec"):
        assert node_name in g.nodes, f"missing node: {node_name}"


def test_spec_gen_has_no_implement_node():
    """spec_gen is spec-only; it must never contain an implement node."""
    g = parse(SPEC_GEN)
    assert "implement" not in g.nodes, (
        "spec_gen.dot must not have an implement node — "
        "it is a spec-only lane (design doc §1)"
    )


def test_spec_gen_includes_base_dot():
    """spec_gen inherits the 4-way explore fanout from _base.dot."""
    g = parse(SPEC_GEN)
    for name in ("explore_in", "explore_out", "explore_fanout",
                 "explore_concept", "explore_auth", "explore_reuse",
                 "explore_risks", "explore_join", "explore_stitch"):
        assert name in g.nodes, f"_base.dot node missing: {name}"


def test_spec_gen_topology_start_to_explore_in():
    g = parse(SPEC_GEN)
    assert any(e.src == "start" and e.dst == "explore_in" for e in g.edges), (
        "start must wire to explore_in"
    )


def test_spec_gen_topology_explore_out_to_plan():
    g = parse(SPEC_GEN)
    assert any(e.src == "explore_out" and e.dst == "plan" for e in g.edges), (
        "explore_out must wire to plan"
    )


def test_spec_gen_topology_plan_to_spec_review():
    g = parse(SPEC_GEN)
    assert any(e.src == "plan" and e.dst == "spec_review" for e in g.edges), (
        "plan must wire to spec_review"
    )


def test_spec_gen_topology_spec_review_success_to_exit():
    g = parse(SPEC_GEN)
    success_edges = [
        e for e in g.edges
        if e.src == "spec_review" and e.dst == "exit"
    ]
    assert len(success_edges) == 1, (
        "spec_review must have exactly one success edge to exit"
    )


def test_spec_gen_topology_spec_review_failure_to_fix_spec():
    g = parse(SPEC_GEN)
    fail_edges = [
        e for e in g.edges
        if e.src == "spec_review" and e.dst == "fix_spec"
    ]
    assert len(fail_edges) == 1, (
        "spec_review must have exactly one failure edge to fix_spec"
    )


def test_spec_gen_topology_fix_spec_back_to_spec_review():
    g = parse(SPEC_GEN)
    assert any(e.src == "fix_spec" and e.dst == "spec_review" for e in g.edges), (
        "fix_spec must loop back to spec_review"
    )


# ---------------------------------------------------------------------------
# Node attribute / contract tests
# ---------------------------------------------------------------------------


def test_spec_gen_plan_is_plan_class():
    g = parse(SPEC_GEN)
    assert g.nodes["plan"].attrs.get("class") == "plan"


def test_spec_gen_plan_prompt_is_plan_md():
    g = parse(SPEC_GEN)
    assert g.nodes["plan"].attrs.get("prompt") == "@prompts/slim/plan.md"


def test_spec_gen_spec_review_is_gate_er():
    """spec_review must be type=gate_er for cold adversarial review."""
    g = parse(SPEC_GEN)
    assert g.nodes["spec_review"].attrs.get("type") == "gate_er", (
        "spec_review must be type=gate_er to use adversarial priority queue"
    )


def test_spec_gen_spec_review_has_backend_priority():
    g = parse(SPEC_GEN)
    bp = g.nodes["spec_review"].attrs.get("backend_priority", "")
    assert bp, "spec_review must declare backend_priority for adversarial routing"
    priority = [p.strip() for p in bp.split(",")]
    assert "codex" in priority, "codex must be in backend_priority"
    assert "agy" in priority, "agy must be in backend_priority"


def test_spec_gen_spec_review_prefer_adversarial():
    g = parse(SPEC_GEN)
    prefer = g.nodes["spec_review"].attrs.get("prefer_adversarial")
    assert str(prefer).lower() in ("true", "1", "yes"), (
        "spec_review must set prefer_adversarial=true"
    )


def test_spec_gen_spec_review_goal_gate_true():
    g = parse(SPEC_GEN)
    goal_gate = g.nodes["spec_review"].attrs.get("goal_gate")
    assert str(goal_gate).lower() in ("true", "1", "yes"), (
        "spec_review must set goal_gate=true"
    )


def test_spec_gen_spec_review_retry_target_fix_spec():
    g = parse(SPEC_GEN)
    assert g.nodes["spec_review"].attrs.get("retry_target") == "fix_spec", (
        "spec_review retry_target must be fix_spec"
    )


def test_spec_gen_spec_review_prompt_is_spec_review_md():
    g = parse(SPEC_GEN)
    assert g.nodes["spec_review"].attrs.get("prompt") == "@prompts/slim/spec_review.md", (
        "spec_review must use @prompts/slim/spec_review.md"
    )


def test_spec_gen_fix_spec_max_retries():
    g = parse(SPEC_GEN)
    mr = g.nodes["fix_spec"].attrs.get("max_retries")
    assert str(mr) == "2", "fix_spec max_retries must be 2"


def test_spec_gen_fix_spec_prompt_is_fix_spec_md():
    g = parse(SPEC_GEN)
    assert g.nodes["fix_spec"].attrs.get("prompt") == "@prompts/slim/fix_spec.md"


def test_spec_gen_fix_spec_class_is_fix():
    g = parse(SPEC_GEN)
    assert g.nodes["fix_spec"].attrs.get("class") == "fix"


# ---------------------------------------------------------------------------
# End-to-end engine run (echo backend — deterministic offline)
# ---------------------------------------------------------------------------


def test_spec_gen_happy_path_explore_plan_review_exit(monkeypatch, tmp_path):
    """Happy path: spec_review succeeds on first attempt → exit."""
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)

    g = parse(SPEC_GEN)
    # Pin plan to echo; gate_er in echo mode reads outcome from ctx.state.
    g.nodes["plan"].attrs["backend"] = "echo"

    ctx = Context(goal="define a reviewed spec for a tiny utility", workdir=ROOT, backend="echo")
    ctx.state["spec_review.outcome"] = "success"

    history = run(g, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    node_names = [step.node for step in history]

    # Must visit the full explore fanout from _base.dot.
    assert "explore_in" in node_names
    assert "explore_fanout" in node_names
    assert "explore_stitch" in node_names
    assert "explore_out" in node_names

    # Core spec-gen nodes must appear.
    assert "plan" in node_names
    assert "spec_review" in node_names
    assert "exit" in node_names

    # implement must NOT appear.
    assert "implement" not in node_names
    assert "fix" not in node_names  # fix from other lanes also absent

    # fix_spec must NOT appear on a happy path (no spec failures).
    assert "fix_spec" not in node_names


def test_spec_gen_fix_loop_on_review_failure(monkeypatch, tmp_path):
    """Failure path: spec_review fails → fix_spec → spec_review succeeds."""
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)

    g = parse(SPEC_GEN)
    g.nodes["plan"].attrs["backend"] = "echo"

    ctx = Context(goal="bad spec needs one fix", workdir=ROOT, backend="echo")

    # First review visit fails; second succeeds (fix_spec runs once in between).
    call_count = {"n": 0}
    original_handler = handlers_mod._gate_er  # noqa: SLF001

    def patched_gate_er(node, _ctx):
        if node.name == "spec_review":
            call_count["n"] += 1
            if call_count["n"] == 1:
                from runner.handlers import Result
                return Result(outcome="failure", output="spec missing non-goals")
            else:
                from runner.handlers import Result
                return Result(outcome="success", output="spec approved after fix")
        return original_handler(node, _ctx)

    monkeypatch.setitem(TYPE_REGISTRY, "gate_er", patched_gate_er)

    history = run(g, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    node_names = [step.node for step in history]
    assert "fix_spec" in node_names
    # spec_review visited twice: once failing, once succeeding.
    assert node_names.count("spec_review") == 2


def test_spec_gen_full_node_sequence_happy_path(monkeypatch, tmp_path):
    """Full deterministic node order for the happy path."""
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)

    g = parse(SPEC_GEN)
    g.nodes["plan"].attrs["backend"] = "echo"

    ctx = Context(goal="spec-gen happy path ordering", workdir=ROOT, backend="echo")
    ctx.state["spec_review.outcome"] = "success"

    history = run(g, ctx, checkpoint=tmp_path / "checkpoint.json")

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
        "spec_review",
        "exit",
    ]


# ---------------------------------------------------------------------------
# Prompt file existence checks
# ---------------------------------------------------------------------------


def test_spec_review_prompt_exists():
    p = ROOT / "prompts" / "slim" / "spec_review.md"
    assert p.exists(), f"spec_review.md missing at {p}"


def test_fix_spec_prompt_exists():
    p = ROOT / "prompts" / "slim" / "fix_spec.md"
    assert p.exists(), f"fix_spec.md missing at {p}"


def test_spec_review_prompt_contains_verdict_contract():
    content = (ROOT / "prompts" / "slim" / "spec_review.md").read_text()
    # The contract must use the runner-parseable tokens (verdict: pass|fail).
    assert "verdict: pass" in content, (
        "spec_review.md must instruct concluding with 'verdict: pass'"
    )
    assert "verdict: fail" in content, (
        "spec_review.md must instruct concluding with 'verdict: fail'"
    )
    # The old contract tokens are unparseable by _parse_verdict — 'success'
    # is not a valid verdict token and 'failure' fails the fail\b boundary.
    assert "VERDICT: success" not in content, (
        "spec_review.md must not use the unparseable 'VERDICT: success' token"
    )
    assert "VERDICT: failure" not in content, (
        "spec_review.md must not use the unparseable 'VERDICT: failure' token"
    )


def test_spec_review_prompt_mentions_file_ownership_matrix():
    content = (ROOT / "prompts" / "slim" / "spec_review.md").read_text()
    assert "file-ownership" in content or "file ownership" in content.lower(), (
        "spec_review.md must mention the file-ownership matrix requirement"
    )
    assert "parallel" in content.lower(), (
        "spec_review.md must address parallel lanes"
    )


def test_spec_review_prompt_checks_acceptance_criteria():
    content = (ROOT / "prompts" / "slim" / "spec_review.md").read_text()
    assert "acceptance" in content.lower(), (
        "spec_review.md must check acceptance criteria testability"
    )


def test_spec_review_prompt_checks_non_goals():
    content = (ROOT / "prompts" / "slim" / "spec_review.md").read_text()
    assert "non-goal" in content.lower() or "non_goal" in content.lower(), (
        "spec_review.md must check for non-goals"
    )


def test_fix_spec_prompt_prohibits_implementation():
    content = (ROOT / "prompts" / "slim" / "fix_spec.md").read_text()
    assert "do not implement" in content.lower() or "not implement" in content.lower(), (
        "fix_spec.md must prohibit implementation"
    )


# ---------------------------------------------------------------------------
# pipeline-selection.md row
# ---------------------------------------------------------------------------


def test_pipeline_selection_has_spec_gen_row():
    ps = ROOT / "docs" / "pipeline-selection.md"
    content = ps.read_text()
    assert "spec_gen.dot" in content, (
        "docs/pipeline-selection.md must have a row for spec_gen.dot"
    )
    assert "no implement" in content.lower() or "without implementing" in content.lower(), (
        "pipeline-selection.md spec_gen row must note 'no implement'"
    )
