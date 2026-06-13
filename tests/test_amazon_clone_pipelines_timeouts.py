"""Regression guard: every amazon-clone codergen + gate node declares a timeout.

Companion to ``test_gates_dot_timeouts.py`` and
``test_airbnb_clone_pipelines_timeouts.py`` for the
``benchmarks/amazon-clone/pipelines/`` family. Same pinned contract: a
subprocess-spawning node without a ``timeout`` attribute can hang
indefinitely. Pinned to ``timeout=600`` for parity with the factory/,
slim/, and airbnb-clone siblings.

The amazon-clone family is structurally different from airbnb-clone:
- 5 pipelines have codergen + gate nodes (dark_factory, kilroy,
  mammoth, smasher, tracker) — these are what this test guards.
- 5 pipelines (slim + 4 slices_*) already declare codergen timeouts
  as STRING values (e.g., ``timeout="600"``); pydot returns them as
  strings, not ints. The test normalises via ``int(value)`` so both
  forms pass.

Scope note: this test covers the 5 pipelines where the F6c contract
gap was present. The pre-existing timeouts on slim + 4 slices_* are
verified by the same test (parity-checked) but the test does NOT
assert they were added in this PR — the attr-presence test runs
against all 10 pipelines.

File-disjoint: new file, only reads the .dot pipelines in
``benchmarks/amazon-clone/pipelines/`` and a parser import. No WIP
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
# tests/test_gates_dot_timeouts.py, tests/test_slim_pipelines_timeouts.py,
# and tests/test_airbnb_clone_pipelines_timeouts.py — if a new node
# type is added that can spawn a subprocess, add it to ALL FOUR
# frozensets in the same commit, or this test becomes a false
# negative.
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

# All 10 amazon-clone pipelines. 5 of them had no timeouts on
# codergen + gate nodes before this PR (the focus); 5 already
# declared them (smoke-checked for parity).
_AMAZON_CLONE_PIPELINES = tuple(
    f"benchmarks/amazon-clone/pipelines/{name}.dot"
    for name in (
        "dark_factory",
        "kilroy",
        "mammoth",
        "slim",
        "smasher",
        "tracker",
        "slices_cart",
        "slices_catalog",
        "slices_checkout",
        "slices_foundation",
    )
)

# Timeout value pinned for parity with the factory/, slim/, and
# airbnb-clone siblings.
_EXPECTED_TIMEOUT_S = 600

# Codergen nodes that must be exactly ``_EXPECTED_TIMEOUT_S`` (600s).
# The pre-existing ``fix`` codergen nodes on amazon-clone slim + 4
# slices_* use a tighter 300s budget (the fix loop should be quick —
# the main flow is the 600s window), so those are explicitly excluded
# from the 600s contract. Listing them explicitly keeps the contract
# reviewable.
_CODERGEN_600_EXPECTED = {
    "dark_factory": {"spec_review", "architect", "data_model", "backend", "frontend",
                     "firestore_rules", "seed_data", "validation", "fix"},
    "kilroy": {"spec", "build", "review"},
    "mammoth": {"specify", "build", "repair"},
    "slim": {"implement"},  # ``fix`` is 300 (intentional)
    "smasher": {"plan", "implement", "fix"},
    "tracker": {"analyze", "create", "refine"},
    # slices_*'s ``implement`` is 600, ``fix`` is 300 (intentional)
}


def _normalise_timeout(value: object) -> int | None:
    """Coerce a DOT timeout attribute to an int, or None if missing/unparseable.

    DOT allows ``timeout=600`` (int) or ``timeout="600"`` (string).
    pydot returns whatever was written, so both forms reach us. The
    amazon-clone benchmark uses BOTH forms: pre-existing timeouts on
    slim + 4 slices_* are strings, new ones added in this PR are ints.
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


def test_every_amazon_clone_subprocess_node_declares_a_timeout() -> None:
    """Every subprocess-spawning node in amazon-clone/ declares a timeout.

    Iterates every node in every amazon-clone pipeline and asserts that
    any node whose type is in the canonical subprocess allow-list has
    a ``timeout`` attribute. Covers codergen + tool + holdout_eval +
    gate_es + gate_er + gate_code_standards + human_gate + agy + ao.
    """
    missing: list[tuple[str, str, str]] = []
    for rel_path in _AMAZON_CLONE_PIPELINES:
        g = _load(rel_path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            node_type = node.attrs.get("type", "")
            if node_type in _SUBPROCESS_NODE_TYPES:
                if "timeout" not in node.attrs:
                    missing.append((rel_path, name, node_type))
    assert not missing, (
        "amazon-clone subprocess-spawning nodes must declare a timeout= "
        f"to prevent indefinite hangs. Missing: {missing}."
    )


def test_amazon_clone_codergen_nodes_use_canonical_600_second_timeout() -> None:
    """The amazon-clone codergen timeouts must match the factory/, slim/, airbnb-clone siblings.

    Four pipeline families that compose the same code-gen chain should
    not silently diverge on the per-node timeout. Pinned to ``600``
    because that is the value used in
    ``pipelines/factory/{gates,pr_gates}.dot``,
    ``pipelines/slim/{minimal_feature_cs,levelup_pra_validate}.dot``,
    and the airbnb-clone master + 3 sprint pipelines.

    Scope: this test only enforces 600 on the codergen nodes
    enumerated in ``_CODERGEN_600_EXPECTED``. The pre-existing
    ``fix`` codergen nodes on amazon-clone slim + 4 slices_* use
    a tighter 300s budget (the fix loop is the rapid-iteration
    path, not the long-running main flow), so those are explicitly
    excluded from the 600s contract.
    """
    offenders: list[tuple[str, str, str]] = []
    for pipeline_stem, expected_nodes in _CODERGEN_600_EXPECTED.items():
        rel_path = f"benchmarks/amazon-clone/pipelines/{pipeline_stem}.dot"
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
        f"amazon-clone codergen timeouts must be {_EXPECTED_TIMEOUT_S}s "
        f"(parity with factory/, slim/, and airbnb-clone siblings). "
        f"Offenders: {offenders}."
    )


def test_amazon_clone_pipeline_count_is_stable() -> None:
    """Pin the amazon-clone pipeline family composition.

    10 pipelines total: 4 attribution-lanes (kilroy, mammoth, smasher,
    tracker) + 1 master (dark_factory) + 1 slim + 4 slices
    (cart, catalog, checkout, foundation). If a future maintainer
    adds an 11th pipeline, this test breaks and forces an explicit
    decision to add it to the allow-list above.
    """
    amazon_dir = ROOT / "benchmarks" / "amazon-clone" / "pipelines"
    actual = sorted(p.stem for p in amazon_dir.glob("*.dot"))
    assert actual == [
        "dark_factory",
        "kilroy",
        "mammoth",
        "slices_cart",
        "slices_catalog",
        "slices_checkout",
        "slices_foundation",
        "slim",
        "smasher",
        "tracker",
    ], (
        f"amazon-clone pipeline family composition changed. "
        f"Expected 10 pipelines (4 attribution-lanes + master + slim + "
        f"4 slices), got {actual}. Update the allow-list + count test."
    )
