"""Regression guard: every airbnb-clone codergen node declares a timeout.

Companion to ``test_gates_dot_timeouts.py`` for the
``benchmarks/airbnb-clone/pipelines/`` family. Same pinned contract:
a codergen node that runs a subprocess without a ``timeout`` attribute
can hang indefinitely (the ``claude --print`` / ``agy --print`` /
``codex exec`` / etc. subprocess has no upper bound). Pinned to
``timeout=600`` for parity with
``pipelines/factory/{gates,pr_gates}.dot`` and the new
``pipelines/slim/{minimal_feature_cs,levelup_pra_validate}.dot``.

Scope note: this test only covers the 4 airbnb-clone pipelines
themselves. The ``holdout_eval`` verify nodes already declare
``timeout="600"`` (S1/S2) or ``timeout="900"`` (S3, larger
frontend surface) — those are separate values and out of scope for
this codergen contract.

File-disjoint: new file, only reads the .dot pipelines in
``benchmarks/airbnb-clone/pipelines/`` and a parser import. No WIP
file touched.
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
# tests/test_gates_dot_timeouts.py and tests/test_slim_pipelines_timeouts.py
# — if a new node type is added that can spawn a subprocess, add it to
# ALL THREE frozensets in the same commit, or this test becomes a
# false negative.
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

# Codergen-specific subset (the focus of this PR). The holdout_eval
# verify nodes already declare their own timeouts (600/900); we don't
# overwrite those, and we don't add new holds to them. The contract
# here is for the LLM-driven plan/implement/fix nodes only.
_CODERGEN_TYPES = frozenset({"codergen"})

# Pipelines in the airbnb-clone benchmark family. The master pipeline
# composes 3 sprints; each sprint also has its own composition.
_AIRBNB_CLONE_PIPELINES = (
    "benchmarks/airbnb-clone/pipelines/airbnb-clone.dot",
    "benchmarks/airbnb-clone/pipelines/sprint-1-data.dot",
    "benchmarks/airbnb-clone/pipelines/sprint-2-backend.dot",
    "benchmarks/airbnb-clone/pipelines/sprint-3-frontend.dot",
)

# Timeout value pinned for parity with the factory/ and slim/ siblings.
_EXPECTED_TIMEOUT_S = 600


def _normalise_timeout(value: object) -> int | None:
    """Coerce a DOT timeout attribute to an int, or None if missing/unparseable."""
    if value is None:
        return None
    try:
        return int(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return None


def _load(relative_path: str) -> object:
    """Parse a pipeline at ``relative_path`` (relative to repo root)."""
    return parse(ROOT / relative_path)


def test_every_airbnb_clone_codergen_node_declares_a_timeout() -> None:
    """Every codergen node in airbnb-clone/pipelines/ has a timeout.

    Iterates every node in every airbnb-clone pipeline and asserts that
    any node whose type is in the codergen allow-list has a ``timeout``
    attribute. The holdout_eval verify nodes are NOT checked here
    (they already declare their own timeouts — 600 for S1/S2, 900 for
    S3 — and adding this PR's contract to them would be scope creep).
    """
    missing: list[tuple[str, str, str]] = []
    for rel_path in _AIRBNB_CLONE_PIPELINES:
        g = _load(rel_path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            node_type = node.attrs.get("type", "")
            if node_type in _CODERGEN_TYPES:
                if "timeout" not in node.attrs:
                    missing.append((rel_path, name, node_type))
    assert not missing, (
        "airbnb-clone codergen nodes must declare a timeout= to prevent "
        f"indefinite hangs. Missing: {missing}. Use the same timeout=600 "
        "as the factory/ and slim/ siblings."
    )


def test_airbnb_clone_codergen_nodes_use_canonical_600_second_timeout() -> None:
    """The airbnb-clone codergen timeouts must match the factory/ and slim/ siblings.

    Three pipeline families that compose the same code-gen chain should
    not silently diverge on the per-node timeout. Pinned to ``600``
    because that is the value used in
    ``pipelines/factory/{gates,pr_gates}.dot`` and
    ``pipelines/slim/{minimal_feature_cs,levelup_pra_validate}.dot``.
    """
    offenders: list[tuple[str, str, str]] = []
    for rel_path in _AIRBNB_CLONE_PIPELINES:
        g = _load(rel_path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            node_type = node.attrs.get("type", "")
            if node_type in _CODERGEN_TYPES:
                actual = _normalise_timeout(node.attrs.get("timeout"))
                if actual != _EXPECTED_TIMEOUT_S:
                    offenders.append(
                        (rel_path, name, f"{actual!r} != {_EXPECTED_TIMEOUT_S!r}")
                    )
    assert not offenders, (
        f"airbnb-clone codergen timeouts must be {_EXPECTED_TIMEOUT_S}s "
        f"(parity with factory/ and slim/ siblings). Offenders: {offenders}."
    )


def test_airbnb_clone_codergen_count_is_stable() -> None:
    """Pin the codergen node count for the airbnb-clone family.

    A future maintainer who adds a sprint should also add a test
    update. The master ``airbnb-clone.dot`` has 9 codergen nodes
    (plan/implement/fix × 3 sprints); each per-sprint pipeline has 3.
    If this test breaks, the pinned contract above (timeout attrs)
    is at risk of silently missing the new sprint.
    """
    g = _load("benchmarks/airbnb-clone/pipelines/airbnb-clone.dot")
    codergen_count = sum(
        1
        for name, node in g.nodes.items()
        if name not in {"start", "exit"}
        and node.attrs.get("type", "") in _CODERGEN_TYPES
    )
    assert codergen_count == 9, (
        f"airbnb-clone master pipeline should have 9 codergen nodes "
        f"(plan/implement/fix × 3 sprints), got {codergen_count}. "
        "If you added a sprint, update the timeout contract test too."
    )

    for rel_path, expected in (
        ("benchmarks/airbnb-clone/pipelines/sprint-1-data.dot", 3),
        ("benchmarks/airbnb-clone/pipelines/sprint-2-backend.dot", 3),
        ("benchmarks/airbnb-clone/pipelines/sprint-3-frontend.dot", 3),
    ):
        g = _load(rel_path)
        n = sum(
            1
            for name, node in g.nodes.items()
            if name not in {"start", "exit"}
            and node.attrs.get("type", "") in _CODERGEN_TYPES
        )
        assert n == expected, (
            f"{rel_path} should have {expected} codergen nodes, got {n}."
        )
