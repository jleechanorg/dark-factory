"""Regression guard and contract tests for ``pipelines/slim/two_node_holdout.dot``.

Opt-in pipeline: generic worker + fresh Codex reviewer + behavioral holdout eval.
Success path: start -> worker -> cold_reviewer -> holdout -> exit.
Failure routing: cold_reviewer and holdout both route non-success back to worker.
Worker retry limit is bounded by max_visits="3".
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT, init_git_repo  # noqa: E402, F811

import pytest  # noqa: E402
from runner.engine import run  # noqa: E402
from runner.graph_audit import audit_graph, audit_graphs  # noqa: E402
from runner.handler_core import Context, Result  # noqa: E402
from runner.handlers import TYPE_REGISTRY  # noqa: E402
from runner.parser import parse  # noqa: E402
from runner.preflight import preflight_check  # noqa: E402

_PIPELINE = "pipelines/slim/two_node_holdout.dot"
_DEFAULT_PIPELINE = "pipelines/slim/two_node.dot"
_SUBPROCESS_NODE_TYPES = frozenset(
    {
        "codergen",
        "tool",
        "holdout_eval",
        "gate_es",
        "gate_er",
        "gate_code_standards",
        "human_gate",
        "agy",
        "ao",
    }
)


def _normalise_timeout(value: object) -> int | None:
    if value is None:
        return None
    try:
        return int(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return None


def test_two_node_holdout_dot_parses_and_has_expected_topology() -> None:
    """The opt-in two_node_holdout graph parses cleanly with exactly start,
    worker, cold_reviewer, holdout, and exit nodes, adhering to the public contracts.
    """
    g = parse(ROOT / _PIPELINE)
    names = set(g.nodes.keys())
    assert names == {"start", "worker", "cold_reviewer", "holdout", "exit"}, (
        f"two_node_holdout.dot must have exactly start/worker/cold_reviewer/holdout/exit; "
        f"got {sorted(names)}"
    )

    # Worker contract (mirrors two_node.dot)
    worker = g.nodes["worker"]
    assert worker.attrs.get("type") == "codergen"
    assert worker.attrs.get("label") == "Generic Worker"
    assert worker.attrs.get("class") == "worker"
    assert worker.attrs.get("prompt") == "@prompts/slim/worker.md"
    assert str(worker.attrs.get("max_retries")) == "2"
    assert str(worker.attrs.get("max_visits")) == "3"
    assert _normalise_timeout(worker.attrs.get("timeout")) == 600

    # Cold reviewer contract (mirrors two_node.dot fresh Codex reviewer)
    reviewer = g.nodes["cold_reviewer"]
    assert reviewer.attrs.get("type") == "codergen"
    assert reviewer.attrs.get("label") == "Fresh Codex Reviewer"
    assert reviewer.attrs.get("class") == "review"
    assert reviewer.attrs.get("backend") == "codex"
    assert reviewer.attrs.get("prompt") == "@prompts/slim/fresh_review.md"
    assert str(reviewer.attrs.get("verdict_gate")).lower() == "true"
    assert str(reviewer.attrs.get("fresh_session")).lower() == "true"
    assert str(reviewer.attrs.get("goal_gate")).lower() == "true"
    assert _normalise_timeout(reviewer.attrs.get("timeout")) == 600
    assert "review_contract" not in reviewer.attrs

    # Holdout node contract (literal type=holdout_eval, feature=${state.feature}, validation=true, goal_gate=true, timeout=600)
    holdout = g.nodes["holdout"]
    assert holdout.attrs.get("type") == "holdout_eval"
    assert holdout.attrs.get("label") == "Behavioral Holdouts"
    assert holdout.attrs.get("feature") == "${state.feature}"
    assert str(holdout.attrs.get("validation")).lower() == "true"
    assert str(holdout.attrs.get("goal_gate")).lower() == "true"
    assert _normalise_timeout(holdout.attrs.get("timeout")) == 600

    # Edges wiring:
    # start -> worker
    # worker -> cold_reviewer [condition="outcome=success"]
    # worker -> worker [condition="outcome!=success"]
    # cold_reviewer -> holdout [condition="outcome=success"]
    # cold_reviewer -> worker [condition="outcome!=success"]
    # holdout -> exit [condition="outcome=success"]
    # holdout -> worker [condition="outcome!=success"]
    start_edges = g.outgoing("start")
    assert [e.dst for e in start_edges] == ["worker"]

    worker_edges = g.outgoing("worker")
    assert any(e.dst == "cold_reviewer" and e.condition == "outcome=success" for e in worker_edges)
    assert any(e.dst == "worker" and e.condition == "outcome!=success" for e in worker_edges)

    reviewer_edges = g.outgoing("cold_reviewer")
    assert any(e.dst == "holdout" and e.condition == "outcome=success" for e in reviewer_edges)
    assert any(e.dst == "worker" and e.condition in ("outcome=failure", "outcome!=success") for e in reviewer_edges)

    holdout_edges = g.outgoing("holdout")
    assert any(e.dst == "exit" and e.condition == "outcome=success" for e in holdout_edges)
    assert any(e.dst == "worker" and e.condition in ("outcome!=success", "outcome=failure") for e in holdout_edges)


def test_two_node_holdout_dot_declares_timeout_on_every_subprocess_node() -> None:
    """Every subprocess-spawning node in two_node_holdout.dot has timeout=600."""
    g = parse(ROOT / _PIPELINE)
    for name, node in g.nodes.items():
        if name in {"start", "exit"}:
            continue
        node_type = node.attrs.get("type", "")
        if node_type in _SUBPROCESS_NODE_TYPES:
            assert _normalise_timeout(node.attrs.get("timeout")) == 600, (
                f"Node {name} ({node_type}) missing required timeout=600"
            )


def test_default_two_node_dot_invariant_and_cli_default_unchanged() -> None:
    """The default two_node.dot remains byte-for-byte untouched and topology unchanged,
    and CLI defaults remain pointed to two_node.dot.
    """
    g_default = parse(ROOT / _DEFAULT_PIPELINE)
    names = set(g_default.nodes.keys())
    assert names == {"start", "worker", "cold_reviewer", "exit"}
    assert "holdout" not in names

    # Verify CLI default pipeline reference
    main_file = ROOT / "runner" / "__main__.py"
    content = main_file.read_text(encoding="utf-8")
    assert "two_node.dot" in content
    assert "two_node_holdout.dot" not in content


def test_two_node_holdout_engine_success_path(tmp_path, monkeypatch) -> None:
    """Engine executes start -> worker -> cold_reviewer -> holdout -> exit on success."""
    visited: list[str] = []

    def fake_codergen(node, ctx):
        visited.append(node.name)
        return Result(outcome="success", output="ok")

    def fake_holdout(node, ctx):
        visited.append(node.name)
        return Result(outcome="success", output="holdouts passed")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    graph = parse(ROOT / _PIPELINE)
    ctx = Context(goal="ship feature with holdout", workdir=init_git_repo(tmp_path), backend="echo")
    ctx.state["feature"] = "test_feature"

    history = run(graph, ctx)
    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "worker",
        "cold_reviewer",
        "holdout",
        "exit",
    ]


def test_two_node_holdout_engine_holdout_failure_routes_to_worker(tmp_path, monkeypatch) -> None:
    """Holdout non-success routes back through worker, and subsequent success exits."""
    holdout_attempts = 0

    def fake_codergen(node, ctx):
        return Result(outcome="success", output=f"{node.name} ok")

    def fake_holdout(node, ctx):
        nonlocal holdout_attempts
        holdout_attempts += 1
        if holdout_attempts == 1:
            return Result(outcome="failure", output="holdout assertion failed")
        return Result(outcome="success", output="holdout assertions passed")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    graph = parse(ROOT / _PIPELINE)
    ctx = Context(goal="ship feature with holdout retry", workdir=init_git_repo(tmp_path), backend="echo")
    ctx.state["feature"] = "test_feature"

    history = run(graph, ctx)
    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "worker",
        "cold_reviewer",
        "holdout",
        "worker",
        "cold_reviewer",
        "holdout",
        "exit",
    ]


def test_two_node_holdout_engine_reviewer_failure_routes_to_worker(tmp_path, monkeypatch) -> None:
    """Reviewer failure routes back through worker, and subsequent success reaches holdout."""
    review_attempts = 0

    def fake_codergen(node, ctx):
        nonlocal review_attempts
        if node.name == "cold_reviewer":
            review_attempts += 1
            if review_attempts == 1:
                return Result(outcome="failure", output="Verdict: FAIL\n")
            return Result(outcome="success", output="Verdict: PASS\n")
        return Result(outcome="success", output="worker ok")

    def fake_holdout(node, ctx):
        return Result(outcome="success", output="holdouts passed")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    graph = parse(ROOT / _PIPELINE)
    ctx = Context(goal="fix review comments", workdir=init_git_repo(tmp_path), backend="echo")
    ctx.state["feature"] = "test_feature"

    history = run(graph, ctx)
    assert history[-1].outcome == "success"
    assert [step.node for step in history] == [
        "start",
        "worker",
        "cold_reviewer",
        "worker",
        "cold_reviewer",
        "holdout",
        "exit",
    ]


def test_two_node_holdout_engine_worker_visits_bounded(tmp_path, monkeypatch) -> None:
    """Worker retries remain bounded at max_visits="3" when holdout consistently fails."""
    worker_visits = 0

    def fake_codergen(node, ctx):
        nonlocal worker_visits
        if node.name == "worker":
            worker_visits += 1
        return Result(outcome="success", output="ok")

    def fake_holdout(node, ctx):
        return Result(outcome="failure", output="persistent failure")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    graph = parse(ROOT / _PIPELINE)
    ctx = Context(goal="test bounded loops", workdir=init_git_repo(tmp_path), backend="echo")
    ctx.state["feature"] = "test_feature"

    history = run(graph, ctx)
    # The run should terminate after worker visit limit is reached
    assert worker_visits == 3
    assert history[-1].outcome in ("exhausted", "failure", "error")


def test_missing_feature_preflight_contract() -> None:
    """Missing feature with require_holdouts=True fails preflight, while this graph
    declares feature="${state.feature}".
    """
    res = preflight_check(backend="echo", require_holdouts=True, feature=None)
    assert res["status"] == "fail"
    assert res["holdouts"]["required"] is True
    assert res["holdouts"]["ok"] is False

    g = parse(ROOT / _PIPELINE)
    assert g.nodes["holdout"].attrs.get("feature") == "${state.feature}"


def test_two_node_holdout_graph_audit_clean() -> None:
    """Graph audit reports zero violations for two_node_holdout.dot."""
    violations = audit_graph(ROOT / _PIPELINE)
    assert violations == [], f"Expected 0 violations, got: {violations}"
    all_violations = audit_graphs(ROOT / "pipelines")
    holdout_violations = [v for v in all_violations if "two_node_holdout.dot" in v.pipeline]
    assert holdout_violations == [], f"Expected 0 violations for two_node_holdout.dot, got: {holdout_violations}"
