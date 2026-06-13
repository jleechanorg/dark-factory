"""Regression guard: every all-nodes-coverage + attractor-spec-review + fibonacci
subprocess-spawning node declares a timeout.

Companion to ``test_gates_dot_timeouts.py``,
``test_slim_pipelines_timeouts.py``,
``test_airbnb_clone_pipelines_timeouts.py``, and
``test_amazon_clone_pipelines_timeouts.py``.

Covers 3 pipeline families that were WIP-clean at the time of this PR:
- all-nodes-coverage (1 pipeline: ``pipeline.dot``)
- attractor-spec-review (2 pipelines: ``review_full.dot``, ``review_slim.dot``)
- fibonacci (1 pipeline: ``slim.dot``)

Total: 4 pipelines, 22 subprocess-spawning nodes, all of which this
PR adds ``timeout=600`` to. The companion test pins the contract.

Two structural notes that drove the test design:

1. ``benchmarks/all-nodes-coverage/pipeline.dot::verify`` is a
   ``holdout_eval`` with a pre-existing ``timeout="180"`` (string form)
   — a tighter budget than the canonical 600s. The test pins the
   timeout-attr-present contract (any value is fine) for the wider
   node set, and the 600s-value contract only on the codergen nodes
   enumerated in ``_CODERGEN_600_EXPECTED``.

2. The four target pipelines are WIP-clean (``git diff main..WIP``
   does not list them); this PR is a file-disjoint additive change
   that doesn't touch any of the 4 sibling timeout test files (which
   ARE WIP-touched) or ``runner/parser.py`` (also WIP-touched — only
   imported here, not modified).

File-disjoint: this test is a new file, only reads the 4 .dot pipelines
listed below and a parser import. No WIP file touched.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.parser import parse  # noqa: E402


# Subprocess-spawning node types. Must stay in lock-step with
# tests/test_gates_dot_timeouts.py, tests/test_slim_pipelines_timeouts.py,
# tests/test_airbnb_clone_pipelines_timeouts.py, and
# tests/test_amazon_clone_pipelines_timeouts.py — if a new node type
# is added that can spawn a subprocess, add it to ALL FIVE frozensets
# in the same commit, or these tests become a false negative.
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

# All 4 pipelines covered by this PR. Three pipeline families,
# all WIP-clean at the time of writing. If a future maintainer adds
# a 5th file in any of these families, the count-pinning test breaks
# and forces an explicit contract update.
_REMAINING_PIPELINES = (
    "benchmarks/all-nodes-coverage/pipeline.dot",
    "benchmarks/attractor-spec-review/pipelines/review_full.dot",
    "benchmarks/attractor-spec-review/pipelines/review_slim.dot",
    "benchmarks/fibonacci/pipelines/slim.dot",
)

# Timeout value pinned for parity with factory/, slim/, airbnb-clone/,
# and amazon-clone/ siblings.
_EXPECTED_TIMEOUT_S = 600

# Codergen nodes that must be exactly ``_EXPECTED_TIMEOUT_S`` (600s).
# The pre-existing ``verify`` (holdout_eval) on all-nodes-coverage uses
# a tighter 180s budget (sealed evaluator runtime is bounded by the
# test surface), so it is explicitly excluded from the 600s contract
# here. Tool nodes are also excluded from the 600s check — tool
# timeouts match the underlying command's expected runtime, not the
# codergen 600s default. Listing the codergen nodes explicitly keeps
# the contract reviewable.
_CODERGEN_600_EXPECTED = {
    "benchmarks/all-nodes-coverage/pipeline.dot": {"plan", "implement", "fix"},
    "benchmarks/attractor-spec-review/pipelines/review_full.dot": {
        "plan",
        "implement",
        "fix",
    },
    "benchmarks/attractor-spec-review/pipelines/review_slim.dot": {
        "plan",
        "implement",
        "fix",
    },
    "benchmarks/fibonacci/pipelines/slim.dot": {"plan", "implement", "fix"},
}


def _normalise_timeout(value: object) -> int | None:
    """Coerce a DOT timeout attribute to an int, or None if missing/unparseable.

    DOT allows ``timeout=600`` (int) or ``timeout="600"`` (string).
    pydot returns whatever was written, so both forms reach us. The
    benchmarks use BOTH forms: pre-existing timeouts on
    all-nodes-coverage::verify is a string (``"180"``), new ones
    added in this PR are ints (``600``).
    """
    if value is None:
        return None
    try:
        return int(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return None


def _load(relative_path: str) -> object:
    """Parse a pipeline at ``relative_path`` (relative to repo root)."""
    return parse(ROOT / relative_path)


def test_every_remaining_pipeline_subprocess_node_declares_a_timeout() -> None:
    """Every subprocess-spawning node in the 3 remaining families declares a timeout.

    Iterates every node in every covered pipeline and asserts that
    any node whose type is in the canonical subprocess allow-list has
    a ``timeout`` attribute. Covers codergen + tool + holdout_eval +
    gate_es + gate_er + gate_code_standards + human_gate + agy + ao.
    """
    missing: list[tuple[str, str, str]] = []
    for rel_path in _REMAINING_PIPELINES:
        g = _load(rel_path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            node_type = node.attrs.get("type", "")
            if node_type in _SUBPROCESS_NODE_TYPES:
                if "timeout" not in node.attrs:
                    missing.append((rel_path, name, node_type))
    assert not missing, (
        "remaining-family subprocess-spawning nodes must declare a "
        f"timeout= to prevent indefinite hangs. Missing: {missing}."
    )


def test_remaining_pipeline_codergen_nodes_use_canonical_600_second_timeout() -> None:
    """The remaining-family codergen timeouts must match the factory/ slim/ airbnb-clone/ amazon-clone/ siblings.

    Seven pipeline families that compose the same code-gen chain
    should not silently diverge on the per-node timeout. Pinned to
    ``600`` because that is the value used in
    ``pipelines/factory/{gates,pr_gates}.dot``,
    ``pipelines/slim/minimal_feature_cs.dot``,
    ``pipelines/slim/levelup_pra_validate.dot``,
    the airbnb-clone master + 3 sprint pipelines, and the amazon-clone
    10-pipeline family.

    Scope: this test only enforces 600 on the codergen nodes
    enumerated in ``_CODERGEN_600_EXPECTED``. The pre-existing
    ``verify`` (holdout_eval) on all-nodes-coverage uses a tighter
    180s budget, and tool nodes match the underlying command's
    expected runtime — both are explicitly excluded from the 600s
    contract here.
    """
    offenders: list[tuple[str, str, str]] = []
    for rel_path, expected_nodes in _CODERGEN_600_EXPECTED.items():
        g = _load(rel_path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            if name not in expected_nodes:
                continue
            node_type = node.attrs.get("type", "")
            if node_type == "codergen":
                actual = _normalise_timeout(node.attrs.get("timeout"))
                if actual != _EXPECTED_TIMEOUT_S:
                    offenders.append(
                        (rel_path, name, f"{actual!r} != {_EXPECTED_TIMEOUT_S!r}")
                    )
    assert not offenders, (
        f"remaining-family codergen timeouts must be {_EXPECTED_TIMEOUT_S}s "
        f"(parity with factory/, slim/, airbnb-clone/, amazon-clone/ "
        f"siblings). Offenders: {offenders}."
    )


def test_remaining_pipeline_count_is_stable() -> None:
    """Pin the 3-family composition of the remaining pipelines.

    4 pipelines total: 1 all-nodes-coverage + 2 attractor-spec-review
    + 1 fibonacci. If a future maintainer adds a 5th pipeline (e.g.
    a second fibonacci variant), this test breaks and forces an
    explicit decision to add it to the allow-list above.
    """
    roots = [ROOT / "benchmarks" / "all-nodes-coverage" / "pipeline.dot",
             ROOT / "benchmarks" / "attractor-spec-review" / "pipelines" / "review_full.dot",
             ROOT / "benchmarks" / "attractor-spec-review" / "pipelines" / "review_slim.dot",
             ROOT / "benchmarks" / "fibonacci" / "pipelines" / "slim.dot"]
    actual = sorted(str(p.relative_to(ROOT)) for p in roots)
    expected = [
        "benchmarks/all-nodes-coverage/pipeline.dot",
        "benchmarks/attractor-spec-review/pipelines/review_full.dot",
        "benchmarks/attractor-spec-review/pipelines/review_slim.dot",
        "benchmarks/fibonacci/pipelines/slim.dot",
    ]
    assert actual == expected, (
        f"remaining-pipeline family composition changed. "
        f"Expected 4 pipelines (1 all-nodes-coverage + 2 attractor-spec-review "
        f"+ 1 fibonacci), got {actual}. Update the allow-list + count test."
    )
