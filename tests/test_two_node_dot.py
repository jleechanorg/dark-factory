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
    assert g.nodes["cold_reviewer"].attrs.get("type") == "gate_er"
    assert g.nodes["cold_reviewer"].attrs.get("class") == "review"


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


def test_two_node_dot_cold_reviewer_uses_codex_priority_queue() -> None:
    """The cold_reviewer node must use the canonical adversarial priority queue
    `codex > minimax > agy > claude-sonnet` (or a strict superset) and pin
    prefer_adversarial=true so the reviewer is a different vendor from the
    worker whenever possible.

    This is the user-stated contract: "Default reviewer is codex with all the
    fallback CLI the repo already has."
    """
    g = parse(ROOT / _PIPELINE)
    reviewer = g.nodes["cold_reviewer"]
    priority = reviewer.attrs.get("backend_priority", "")
    # The list is comma-separated; we require codex to be first.
    entries = [p.strip() for p in str(priority).split(",") if p.strip()]
    assert entries, (
        "cold_reviewer must declare backend_priority (got empty)"
    )
    assert entries[0] == "codex", (
        f"cold_reviewer backend_priority must lead with codex; got {entries!r}"
    )
    assert "minimax" in entries, (
        f"cold_reviewer backend_priority must include minimax in the fallback "
        f"chain; got {entries!r}"
    )
    # prefer_adversarial is stored as a string by the parser; compare loosely.
    prefer = str(reviewer.attrs.get("prefer_adversarial", "")).lower()
    assert prefer in {"true", "1", "yes"}, (
        f"cold_reviewer must declare prefer_adversarial=true; got {prefer!r}"
    )


def test_two_node_dot_cold_reviewer_prompt_is_static() -> None:
    """The cold_reviewer must point at the static Codex cold-reviewer prompt.

    The user contract: "Cold reviewer is the static codex prompt we cannot
    change." This test pins the prompt reference so a future refactor that
    accidentally swaps it for `prompts/slim/review.md` (the heavier
    G4/G5 five-step procedure) is caught.
    """
    g = parse(ROOT / _PIPELINE)
    prompt = g.nodes["cold_reviewer"].attrs.get("prompt", "")
    assert prompt == "@prompts/slim/cold_reviewer.md", (
        f"cold_reviewer prompt must be @prompts/slim/cold_reviewer.md "
        f"(the static cold-reviewer contract); got {prompt!r}"
    )
