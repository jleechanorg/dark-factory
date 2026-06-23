"""Tests for the `gate_strict="true"` DOT attribute opt-in (F6, jleechan-9ia).

Verifies the per-node bool-attr parser behaviour plus the
`runner/handler_dispatch.py::_gate_strict_flag(node)` helper. The downstream
verdict-override behaviour is covered in `tests/test_verdict_parsing.py::test_gate_strict_overrides_warn_to_failure`.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.handler_core import _gate_strict_flag  # noqa: E402
from runner.parser import Node, parse  # noqa: E402


def _make_node(attrs):
    return Node(name="test_gate", attrs=attrs)


def test_gate_strict_flag_accepts_canonical_forms():
    """The bool-attr parser must accept True / "true" / "1" (case-insensitive)."""
    for value in (True, "true", "True", "TRUE", "1", "yes", "YES"):
        assert _gate_strict_flag(_make_node({"gate_strict": value})) is True, f"expected True for {value!r}"


def test_gate_strict_flag_rejects_unknown_values():
    """Unknown / typo'd values must default to False (no silent strict-mode enable)."""
    for value in (False, "false", "False", "0", "no", "", None, "warn", "2"):
        assert _gate_strict_flag(_make_node({"gate_strict": value})) is False, f"expected False for {value!r}"


def test_gate_strict_flag_missing_attr_defaults_to_false():
    """No `gate_strict` attr at all → False (legacy behaviour preserved)."""
    assert _gate_strict_flag(_make_node({})) is False
    assert _gate_strict_flag(_make_node({"other_attr": "x"})) is False


def test_pilot_graphs_have_gate_strict_true_on_gate_er():
    """Count-pinning: the F6 pilot graphs (review_pr, bugfix_noholdout,
    brownfield_delete_first) must all set `gate_strict="true"` on the
    `gate_er` node that does evidence review. If a future refactor drops
    the attribute, this test fails fast so the warn→success regression
    is caught at CI time, not at the next cold review."""
    pilot_graphs = [
        ROOT / "pipelines" / "slim" / "review_pr.dot",
        ROOT / "pipelines" / "slim" / "bugfix_noholdout.dot",
        ROOT / "pipelines" / "slim" / "brownfield_delete_first.dot",
    ]
    for path in pilot_graphs:
        graph = parse(path)
        # Find the node whose type is gate_er (the node NAME varies across
        # graphs: `evidence` in review_pr.dot vs `gate_er` in the other two).
        gate_er_nodes = [n for n in graph.nodes.values() if n.attrs.get("type") == "gate_er"]
        assert len(gate_er_nodes) == 1, (
            f"{path.name}: expected 1 gate_er-type node, got {len(gate_er_nodes)}"
        )
        node = gate_er_nodes[0]
        assert _gate_strict_flag(node) is True, (
            f"{path.name}: {node.name} (type=gate_er) missing gate_strict='true'; "
            f"would regress to legacy warn→success and miss the F6 fix."
        )
