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
import tempfile

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

# Scratch workdir in the OS tempdir — using the repo root here leaked one
# branch_* mkdtemp per fan-out test into the working tree.
SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="test_two_node_dot_"))

from runner.parser import parse  # noqa: E402

from conftest import register_scratch_dir  # noqa: E402

register_scratch_dir(SCRATCH)


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
        "parallel_reviewer",
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
    assert g.nodes["cold_reviewer"].attrs.get("type") == "parallel_reviewer"
    assert g.nodes["cold_reviewer"].attrs.get("class") == "review"
    assert g.nodes["cold_reviewer"].attrs.get("review_contract") == "cold-review-v1"


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


def test_two_node_dot_cold_reviewer_uses_only_its_supported_transport() -> None:
    """Cold-review-v1 advertises only Codex, its sole receipt-capable transport."""
    g = parse(ROOT / _PIPELINE)
    reviewer = g.nodes["cold_reviewer"]
    priority = reviewer.attrs.get("backend_priority", "")
    entries = [p.strip() for p in str(priority).split(",") if p.strip()]
    assert entries == ["codex"], (
        "cold-review-v1 must not advertise minimax/agy/claude fallbacks: "
        "the controller has no compatible receipt transport for them"
    )


def test_two_node_dot_cold_reviewer_controller_binding() -> None:
    """The cold_reviewer must bind to the SHA-pinned controller cold-review-v1 prompt contract.

    This pins the requirement: use the existing SHA-pinned cold-review-v1
    controller execution path (prompts/catalog/controller_cold_review_v1.md)
    rather than any divergent prompt copy.
    """
    g = parse(ROOT / _PIPELINE)
    contract = g.nodes["cold_reviewer"].attrs.get("review_contract", "")
    assert contract == "cold-review-v1", (
        f"cold_reviewer review_contract must be cold-review-v1; got {contract!r}"
    )
    from runner.review_controller import PROMPT_ID, _TEMPLATE_PATH
    assert PROMPT_ID == "controller-cold-review-v1"
    assert _TEMPLATE_PATH.exists()


def test_two_node_dot_reviewer_has_no_target_authored_prompt_and_docs_agree() -> None:
    """The slim graph cannot override its controller-owned reviewer contract."""
    reviewer = parse(ROOT / _PIPELINE).nodes["cold_reviewer"]
    assert "prompt" not in reviewer.attrs
    assert reviewer.attrs.get("type") == "parallel_reviewer"
    assert reviewer.attrs.get("review_contract") == "cold-review-v1"
    assert reviewer.attrs.get("backend_priority") == "codex"
    skill = (ROOT / ".claude/skills/dark-factory/SKILL.md").read_text()
    assert "controller-owned `cold-review-v1`" in skill
    assert '`type="parallel_reviewer"`' in skill
    assert '`backend_priority="codex"`' in skill
    assert not (ROOT / "prompts/slim/cold_reviewer.md").exists()


def test_two_node_dot_binds_the_worker_verification_receipt() -> None:
    """The default cold reviewer receives the worker's declared evidence file."""
    reviewer = parse(ROOT / _PIPELINE).nodes["cold_reviewer"]
    assert reviewer.attrs.get("evidence_paths") == "evidence/worker-verification.json"


def test_worker_prompt_requires_a_bounded_structured_verification_receipt() -> None:
    """Every default worker must provide reproducible, non-fabricated review data."""
    prompt = (ROOT / _WORKER_PROMPT).read_text()
    assert "evidence/worker-verification.json" in prompt
    assert "1 MiB" in prompt
    for field in (
        "schema_version",
        "target_head_sha",
        "goal",
        "changed_files",
        "commands",
        "not_applicable",
        "cwd",
        "exit_code",
        "stdout",
        "stderr",
    ):
        assert field in prompt
    assert "Do not fabricate" in prompt


def test_worker_prompt_renders_untrusted_reviewer_feedback_only_on_retry() -> None:
    from runner.handler_core import Context
    from runner.handler_render import _render_prompt

    worker = parse(ROOT / _PIPELINE).nodes["worker"]
    ctx = Context(goal="repair the controller", workdir=SCRATCH, backend="echo")
    first_attempt = _render_prompt(worker, ctx)
    assert "(no prior reviewer feedback)" in first_attempt

    ctx.state["_last_review_feedback"] = "Finding: bind the snapshot lineage."
    retry_attempt = _render_prompt(worker, ctx)
    assert "Finding: bind the snapshot lineage." in retry_attempt
    assert "${state._last_review_feedback}" not in retry_attempt
