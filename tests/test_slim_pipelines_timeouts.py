"""Regression guard: nodes defined in the new slim pipelines declare a timeout.

Companion to ``test_gates_dot_timeouts.py`` for the two slim/ pipelines
landed alongside this test. Same pinned contract: a node that runs a
subprocess without a ``timeout`` attribute can hang indefinitely (the
``claude --print`` / ``agy --print`` / ``codex exec`` / etc. subprocess
has no upper bound), which is the same "stuck run" failure mode the
factory/ sibling test catches. Pinned to ``timeout=600`` for parity
with ``pipelines/factory/{gates,pr_gates}.dot``.

Scope note: this test only checks the nodes *defined* in the new
pipelines (top-level ``node [...]`` statements), not nodes inherited
from ``@include="@pipelines/_base.dot"``. The ``_base.dot`` include
fragment is WIP-touched and is the right home for an eventual
explore-phase timeout sweep; that fix lives with the WIP branch, not
this PR.

File-disjoint: new file, only reads the .dot pipelines in
``pipelines/slim/`` and a parser import. No WIP file touched.
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
# in lock-step with tests/test_gates_dot_timeouts.py — if a new node
# type is added that can spawn a subprocess, add it to BOTH frozensets
# in the same commit, or this test becomes a false negative.
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

# Pipelines this PR adds. A future slim/ pipeline landed in a new PR
# should either be added here (with the timeout contract verified) or
# get its own test file. Pinning the set makes a brand-new pipeline a
# deliberate addition — the new pipeline will fail this test until it
# is added to this list AND its timeouts are checked.
_PIPELINES_IN_THIS_PR = (
    "pipelines/slim/minimal_feature_cs.dot",
    "pipelines/slim/levelup_pra_validate.dot",
)

# Nodes inherited from pipelines/_base.dot (via `@include=`). These
# are WIP-touched and out of scope for this PR. Listing them here
# documents the boundary explicitly.
_INHERITED_FROM_BASE_DOT = frozenset(
    {
        "explore_in",
        "explore_out",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_stitch",
    }
)

# Timeout value pinned for parity with the factory/ siblings
# (pipelines/factory/{gates,pr_gates}.dot all use ``timeout=600``).
_EXPECTED_TIMEOUT_S = 600


def _normalise_timeout(value: object) -> int | None:
    """Coerce a DOT timeout attribute to an int, or None if missing/unparseable.

    DOT allows ``timeout=600`` (int) or ``timeout="600"`` (string).
    pydot returns whatever was written, so both forms reach us.
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


def test_every_node_defined_in_new_slim_pipelines_declares_a_timeout() -> None:
    """Every subprocess-spawning node defined in the new slim/ pipelines has a timeout.

    Iterates every node *defined* in the top-level body of each new
    pipeline (skipping ``start``/``exit`` markers and nodes inherited
    via ``@include``) and asserts that any node whose type can run a
    subprocess has a ``timeout`` attribute. Inherited nodes from
    ``@pipelines/_base.dot`` are WIP-touched and live in a follow-up.
    """
    missing: list[tuple[str, str, str]] = []
    for rel_path in _PIPELINES_IN_THIS_PR:
        g = _load(rel_path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            if name in _INHERITED_FROM_BASE_DOT:
                continue
            node_type = node.attrs.get("type", "")
            if node_type in _SUBPROCESS_NODE_TYPES:
                if "timeout" not in node.attrs:
                    missing.append((rel_path, name, node_type))
    assert not missing, (
        "new slim/ nodes must declare a timeout= to prevent indefinite "
        f"hangs. Missing: {missing}. Use the same timeout=600 as the "
        "factory/ siblings."
    )


def test_new_slim_pipelines_use_canonical_600_second_timeout() -> None:
    """The new slim/ timeouts must match the factory/ siblings.

    Two pipeline families that compose the same gate chain should not
    silently diverge on the per-node timeout — a future maintainer
    debugging "why does the same gate hang in slim/ and not in
    factory/" will lose an hour to that drift. Pinned to
    ``600`` because that is the value used in
    ``pipelines/factory/{gates,pr_gates}.dot``.
    """
    offenders: list[tuple[str, str, str]] = []
    for rel_path in _PIPELINES_IN_THIS_PR:
        g = _load(rel_path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            if name in _INHERITED_FROM_BASE_DOT:
                continue
            node_type = node.attrs.get("type", "")
            if node_type in _SUBPROCESS_NODE_TYPES:
                actual = _normalise_timeout(node.attrs.get("timeout"))
                if actual != _EXPECTED_TIMEOUT_S:
                    offenders.append(
                        (rel_path, name, f"{actual!r} != {_EXPECTED_TIMEOUT_S!r}")
                    )
    assert not offenders, (
        f"new slim/ pipeline timeouts must be {_EXPECTED_TIMEOUT_S}s "
        f"(parity with factory/ siblings). Offenders: {offenders}."
    )
