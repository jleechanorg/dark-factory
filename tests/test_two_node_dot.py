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
        "parallel_reviewer",
        "agy",
        "ao",
    }
)

_PIPELINE = "pipelines/slim/two_node.dot"
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


def test_two_node_dot_cold_reviewer_is_fixed_to_codex() -> None:
    """The default reviewer is Codex, without backend shopping or fallback."""
    g = parse(ROOT / _PIPELINE)
    reviewer = g.nodes["cold_reviewer"]
    assert reviewer.attrs.get("backend") == "codex"
    assert "backend_priority" not in reviewer.attrs
    assert "prefer_adversarial" not in reviewer.attrs


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
