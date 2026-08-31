"""Regression guard: ``pipelines/slim/two_node.dot`` (the default `/f` graph)
declares a timeout on every subprocess-spawning node, AND parses cleanly
without an inherited `_base.dot`.

Companion to ``test_slim_pipelines_timeouts.py``. Two_node.dot is intentionally
NOT added to that test's `_PIPELINES_IN_THIS_PR` list because two_node.dot is
not new-slim-template sibling (it does not `@include=` the explore phase
subgraph). It is its own minimal graph: 2 productive nodes (worker +
cold_reviewer) plus start/exit and one fix loop, exactly the slim default
shape.

File-disjoint: new test file, only reads two_node.dot plus a parser import.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.parser import parse  # noqa: E402


# Every node type that can run a subprocess in dark-factory. Must stay
# in lock-step with the other timeout tests (e.g.
# tests/test_slim_pipelines_timeouts.py) — if a new node type is added
# that can spawn a subprocess, add it to BOTH frozensets in the same
# commit, or this test becomes a false negative.
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

_PIPELINE = "pipelines/slim/two_node.dot"
_WORKER_PROMPT = "prompts/slim/worker.md"
_EXPECTED_TIMEOUT_S = 600


def _normalise_timeout(value: object) -> int | None:
    """Coerce a DOT timeout attribute to an int, or None if missing/unparseable."""
    if value is None:
        return None
    try:
        return int(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return None


def test_two_node_dot_parses_and_has_expected_topology() -> None:
    """The default `/f` graph parses cleanly and exposes exactly two productive
    nodes (worker + cold_reviewer), plus start/exit anchors. No third fix node.

    This test pins the contract: any future PR that adds or removes nodes
    from two_node.dot must update this assertion deliberately.
    """
    g = parse(ROOT / _PIPELINE)
    names = set(g.nodes.keys())
    # Top-level anchor nodes + the two productive nodes.
    assert names == {"start", "worker", "cold_reviewer", "exit"}, (
        f"two_node.dot must have exactly start/worker/cold_reviewer/exit; "
        f"got {sorted(names)}"
    )
    # Worker and cold_reviewer are the only goal-producing nodes.
    assert g.nodes["worker"].attrs.get("type") == "codergen"
    assert g.nodes["worker"].attrs.get("class") == "worker"
    assert g.nodes["cold_reviewer"].attrs.get("type") == "codergen"
    assert g.nodes["cold_reviewer"].attrs.get("class") == "review"
    assert g.nodes["cold_reviewer"].attrs.get("backend") == "codex"
    assert g.nodes["cold_reviewer"].attrs.get("verdict_gate") == "true"
    assert "review_contract" not in g.nodes["cold_reviewer"].attrs


def test_two_node_dot_declares_timeout_on_every_subprocess_node() -> None:
    """Every subprocess-spawning node in two_node.dot has a timeout= attribute,
    and that timeout matches the canonical 600s used by factory/ siblings."""
    g = parse(ROOT / _PIPELINE)
    missing: list[tuple[str, str]] = []
    wrong_value: list[tuple[str, str, int | None]] = []
    for name, node in g.nodes.items():
        if name in {"start", "exit"}:
            continue
        node_type = node.attrs.get("type", "")
        if node_type not in _SUBPROCESS_NODE_TYPES:
            continue
        if "timeout" not in node.attrs:
            missing.append((name, node_type))
            continue
        actual = _normalise_timeout(node.attrs.get("timeout"))
        if actual != _EXPECTED_TIMEOUT_S:
            wrong_value.append((name, node_type, actual))
    assert not missing, (
        f"two_node.dot nodes must declare a timeout= to prevent indefinite "
        f"hangs. Missing: {missing}. Use timeout={_EXPECTED_TIMEOUT_S}."
    )
    assert not wrong_value, (
        f"two_node.dot timeouts must be {_EXPECTED_TIMEOUT_S}s. "
        f"Offenders: {wrong_value}."
    )


def test_two_node_dot_cold_reviewer_is_a_fresh_codex_session() -> None:
    """The default reviewer is a direct fresh Codex invocation, not a controller."""
    g = parse(ROOT / _PIPELINE)
    reviewer = g.nodes["cold_reviewer"]
    assert reviewer.attrs.get("backend") == "codex"
    assert reviewer.attrs.get("fresh_session") == "true"
    assert reviewer.attrs.get("prompt") == "@prompts/slim/fresh_review.md"


def test_two_node_dot_default_bypasses_controller_machinery() -> None:
    """The simple default path must not select the frozen controller stack."""
    g = parse(ROOT / _PIPELINE)
    reviewer = g.nodes["cold_reviewer"]
    for key in ("review_contract", "backend_priority", "receipt_required", "evidence_paths"):
        assert key not in reviewer.attrs


def test_worker_prompt_does_not_reference_deleted_controller_receipts() -> None:
    """The default worker prompt must describe only the slim feedback loop."""
    prompt = (ROOT / _WORKER_PROMPT).read_text(encoding="utf-8")
    assert "verification receipt" not in prompt.lower()


def test_two_node_dot_reviewer_prompt_is_short_and_docs_agree() -> None:
    """The default prompt states the goal directly without a controller packet."""
    reviewer = parse(ROOT / _PIPELINE).nodes["cold_reviewer"]
    assert reviewer.attrs.get("prompt") == "@prompts/slim/fresh_review.md"
    assert reviewer.attrs.get("type") == "codergen"
    skill = (ROOT / ".claude/skills/dark-factory/SKILL.md").read_text()
    assert "fresh Codex reviewer" in skill
    assert "static Codex cold reviewer" not in skill
    prompt = (ROOT / "prompts/slim/fresh_review.md").read_text()
    assert len([line for line in prompt.splitlines() if line.strip()]) <= 6
    assert "Use all available tools" in prompt
    assert "Verdict: PASS" in prompt and "Verdict: FAIL" in prompt


def test_worker_prompt_is_direct_and_receipt_free() -> None:
    """The worker receives the goal and copied review, without packet ceremony."""
    prompt = (ROOT / _WORKER_PROMPT).read_text()
    assert "${goal}" in prompt
    assert "${state._last_review_feedback}" in prompt
    assert "evidence/worker-verification.json" not in prompt
    assert "schema_version" not in prompt


def test_worker_prompt_renders_untrusted_reviewer_feedback_only_on_retry() -> None:
    from runner.handler_core import Context
    from runner.handler_render import _render_prompt

    worker = parse(ROOT / _PIPELINE).nodes["worker"]
    ctx = Context(goal="repair the controller", workdir=ROOT, backend="echo")
    first_attempt = _render_prompt(worker, ctx)
    assert "(no prior reviewer feedback)" in first_attempt

    ctx.state["_last_review_feedback"] = "Finding: bind the snapshot lineage."
    retry_attempt = _render_prompt(worker, ctx)
    assert "Finding: bind the snapshot lineage." in retry_attempt
    assert "${state._last_review_feedback}" not in retry_attempt


def test_two_node_loop_copies_exact_review_output_to_worker_retry(tmp_path, monkeypatch) -> None:
    from runner.engine import run
    from runner.handler_core import Context, Result
    from runner.handlers import TYPE_REGISTRY

    review = (
        "Blocking: src/value.py:7 mishandles zero.\n"
        + ("context " * 50)
        + "TAIL-MARKER\nVerdict: FAIL\n"
    )
    worker_feedback: list[str | None] = []
    review_visits = 0

    def fake_codergen(node, ctx):
        nonlocal review_visits
        if node.name == "worker":
            worker_feedback.append(ctx.state.get("_last_review_feedback"))
            return Result(outcome="success", output="worker done")
        review_visits += 1
        if review_visits == 1:
            return Result(outcome="failure", output=review)
        return Result(outcome="success", output="Verdict: PASS\n")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    history = run(
        parse(ROOT / _PIPELINE),
        Context(goal="fix zero handling", workdir=tmp_path, backend="echo"),
    )

    assert history[-1].outcome == "success"
    assert worker_feedback == [None, review]


def test_two_node_resume_copies_exact_review_output_to_worker_retry(
    tmp_path, monkeypatch
) -> None:
    """A process restart between review failure and retry must retain findings."""
    from runner.engine import run
    from runner.handler_core import Context, Result
    from runner.handlers import TYPE_REGISTRY

    review = (
        "Blocking: src/value.py:7 mishandles zero.\n"
        + ("context " * 50)
        + "TAIL-MARKER\nVerdict: FAIL\n"
    )
    worker_feedback: list[str | None] = []
    review_visits = 0

    def fake_codergen(node, ctx):
        nonlocal review_visits
        if node.name == "worker":
            worker_feedback.append(ctx.state.get("_last_review_feedback"))
            return Result(outcome="success", output="worker done")
        review_visits += 1
        if review_visits == 1:
            return Result(outcome="failure", output=review)
        return Result(outcome="success", output="Verdict: PASS\n")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    checkpoint = tmp_path / "checkpoint.json"
    graph = parse(ROOT / _PIPELINE)
    from runner import engine_persist

    append_record = engine_persist._append_record

    def interrupt_after_review(*args, **kwargs):
        seq = append_record(*args, **kwargs)
        record = args[5]
        if record.node == "cold_reviewer":
            raise KeyboardInterrupt("simulated process interruption")
        return seq

    monkeypatch.setattr(engine_persist, "_append_record", interrupt_after_review)
    import pytest

    with pytest.raises(KeyboardInterrupt, match="simulated process interruption"):
        run(
            graph,
            Context(goal="fix zero handling", workdir=ROOT, backend="echo"),
            checkpoint=checkpoint,
        )
    assert "TAIL-MARKER" in checkpoint.read_text(encoding="utf-8")
    monkeypatch.setattr(engine_persist, "_append_record", append_record)

    resumed = run(
        graph,
        Context(goal="fix zero handling", workdir=ROOT, backend="echo"),
        checkpoint=checkpoint,
        resume=checkpoint,
    )

    assert resumed[-1].outcome == "success"
    assert worker_feedback == [None, review]


def test_two_node_second_resume_retains_review_feedback_after_worker_failure(
    tmp_path, monkeypatch
) -> None:
    """A second restart after a failed worker retry must retain the review."""
    from runner.engine import run
    from runner.handler_core import Context, Result
    from runner.handlers import TYPE_REGISTRY

    review = (
        "Blocking: src/value.py:7 mishandles zero.\n"
        + ("context " * 50)
        + "TAIL-MARKER\nVerdict: FAIL\n"
    )
    worker_feedback: list[str | None] = []
    worker_visits = 0
    review_visits = 0

    def fake_codergen(node, ctx):
        nonlocal review_visits, worker_visits
        if node.name == "worker":
            worker_feedback.append(ctx.state.get("_last_review_feedback"))
            worker_visits += 1
            outcome = "failure" if worker_visits == 2 else "success"
            return Result(outcome=outcome, output=f"worker visit {worker_visits}")
        review_visits += 1
        if review_visits == 1:
            return Result(outcome="failure", output=review)
        return Result(outcome="success", output="Verdict: PASS\n")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    checkpoint = tmp_path / "checkpoint.json"
    graph = parse(ROOT / _PIPELINE)
    graph.nodes["worker"].attrs["max_retries"] = "0"
    from runner import engine_persist

    append_record = engine_persist._append_record

    def interrupt_after_review(*args, **kwargs):
        seq = append_record(*args, **kwargs)
        if args[5].node == "cold_reviewer":
            raise KeyboardInterrupt("interrupt after review")
        return seq

    monkeypatch.setattr(engine_persist, "_append_record", interrupt_after_review)
    import pytest

    with pytest.raises(KeyboardInterrupt, match="interrupt after review"):
        run(
            graph,
            Context(goal="fix zero handling", workdir=ROOT, backend="echo"),
            checkpoint=checkpoint,
        )

    def interrupt_after_failed_worker(*args, **kwargs):
        seq = append_record(*args, **kwargs)
        record = args[5]
        if record.node == "worker" and record.outcome == "failure":
            raise KeyboardInterrupt("interrupt after failed worker")
        return seq

    monkeypatch.setattr(
        engine_persist, "_append_record", interrupt_after_failed_worker
    )
    with pytest.raises(KeyboardInterrupt, match="interrupt after failed worker"):
        run(
            graph,
            Context(goal="fix zero handling", workdir=ROOT, backend="echo"),
            checkpoint=checkpoint,
            resume=checkpoint,
        )

    import json

    legacy_checkpoint = tmp_path / "legacy-checkpoint.json"
    legacy_payload = json.loads(checkpoint.read_text(encoding="utf-8"))
    for record in legacy_payload:
        if record["node"] == "cold_reviewer":
            record["metadata"].pop("_review_feedback", None)
    legacy_checkpoint.write_text(
        json.dumps(legacy_payload, indent=2), encoding="utf-8"
    )
    with pytest.raises(ValueError, match="missing full reviewer feedback"):
        run(
            graph,
            Context(goal="fix zero handling", workdir=ROOT, backend="echo"),
            checkpoint=legacy_checkpoint,
            resume=legacy_checkpoint,
        )

    monkeypatch.setattr(engine_persist, "_append_record", append_record)
    resumed = run(
        graph,
        Context(goal="fix zero handling", workdir=ROOT, backend="echo"),
        checkpoint=checkpoint,
        resume=checkpoint,
    )

    assert resumed[-1].outcome == "success"
    assert worker_feedback == [None, review, review]


def test_two_node_reviewer_error_is_terminal(tmp_path, monkeypatch) -> None:
    from runner.engine import run
    from runner.handler_core import Context, Result
    from runner.handlers import TYPE_REGISTRY

    worker_visits = 0

    def fake_codergen(node, ctx):
        nonlocal worker_visits
        if node.name == "worker":
            worker_visits += 1
            return Result(outcome="success", output="worker done")
        return Result(outcome="error", output="reviewer infrastructure failed")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    history = run(
        parse(ROOT / _PIPELINE),
        Context(goal="review without mutation", workdir=tmp_path, backend="echo"),
    )

    assert history[-1].outcome == "error"
    assert worker_visits == 1
