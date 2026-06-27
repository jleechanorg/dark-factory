"""Tests for the Level-5 (G3 closure) pipeline migrations.

Lane C deliverable. Covers:

  * The reference `pipelines/factory/level5_feature.dot` parses cleanly
    and contains all the locked-design shape (4 default codergen nodes
    for explore / plan / implement / fix, hard-tier reviewers,
    soft-tier evaluators, `graph [level5="true"]`).
  * The migrated `pipelines/factory/gates.dot` and `pr_gates.dot` carry
    the level5 reviewer additions (`gate_skeptic`, `adversarial_reviewer`,
    `level5="true"` graph attr) and still satisfy the start/exit parser
    invariants.
  * The default-node structure: every coding node is `type="codergen"`
    (the engine default) — no `type="dynamic"`, no `default=` fallback,
    no separate static nodes. The Claude Workflow orchestrator is
    responsible for dispatching each as a separate .dot run.

Design doc: project_2026-06-22_g3_closure_dynamic_node_design.md
Bead: jleechan-0qy
Lane: C
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402
from runner.parser import parse  # noqa: E402


# ---------------------------------------------------------------------------
# 1. Reference Level-5 pipeline parses and has the locked shape
# ---------------------------------------------------------------------------


def test_level5_feature_dot_exists():
    """The reference Level-5 pipeline must exist at the documented path."""
    path = ROOT / "pipelines" / "factory" / "level5_feature.dot"
    assert path.exists(), (
        f"reference Level-5 pipeline must exist at {path} (Lane C deliverable)"
    )


def test_level5_feature_dot_parses_cleanly():
    """The reference .dot parses without errors and carries the level5 attr."""
    path = ROOT / "pipelines" / "factory" / "level5_feature.dot"
    g = parse(path)
    assert "start" in g.nodes
    assert "exit" in g.nodes
    # Locked design: `graph [level5="true"]` forces the rule even outside
    # `pipelines/factory/`.
    assert str(g.attrs.get("level5", "")).strip().lower() == "true"


def test_level5_feature_dot_has_four_default_codergen_nodes():
    """The reference .dot must contain exactly the 4 default coding nodes
    (explore / plan / implement / fix) — the locked G3 surface.

    These are `type="codergen"` nodes (the engine default) — Lane A's
    `type="dynamic"` was scrapped per the architecture pivot to
    default .dot nodes + Claude Workflow orchestration.
    """
    g = parse(ROOT / "pipelines" / "factory" / "level5_feature.dot")
    default_names = {"explore", "plan", "implement", "fix"}
    missing = [name for name in default_names if name not in g.nodes]
    assert not missing, (
        f"reference .dot must have default codergen nodes {sorted(default_names)}, "
        f"missing: {sorted(missing)}"
    )
    for name in default_names:
        node = g.nodes[name]
        node_type = str(node.attrs.get("type", "codergen")).strip().lower()
        assert node_type != "dynamic", (
            f"node {name!r} must not use type=\"dynamic\" (Lane A scrapped; "
            f"architecture pivot: default codergen nodes + Claude Workflow orchestrator)"
        )
        # Each must reference a prompt template (the single-phase orchestration unit)
        assert node.attrs.get("prompt"), (
            f"default codergen node {name!r} is missing prompt= attribute "
            f"(Claude Workflow orchestrator needs explicit prompt paths)"
        )
    # No `default=` attributes should remain — the static-fallback pattern
    # is removed entirely under the pivot.
    for node in g.nodes.values():
        assert "default" not in node.attrs, (
            f"node {node.name!r} has default= attribute; the static-fallback "
            f"pattern was removed in the architecture pivot"
        )


def test_level5_feature_dot_has_no_static_fallback_nodes():
    """Under the architecture pivot, the dynamic+static fallback pair
    (`default="<static>"` plus a separate `<static>` node) is gone.
    Each coding phase is a single default codergen node."""
    g = parse(ROOT / "pipelines" / "factory" / "level5_feature.dot")
    static_names = {"explore_static", "plan_static", "implement_static", "fix_static"}
    found = static_names & set(g.nodes)
    assert not found, (
        f"reference .dot must not contain static fallback nodes "
        f"{sorted(static_names)} (architecture pivot removed them), "
        f"found: {sorted(found)}"
    )


def test_level5_feature_dot_has_hard_tier_reviewers():
    """The 4 hard-tier reviewers must be present in the reference .dot
    (CXDB is built-in instrumentation, not a node)."""
    g = parse(ROOT / "pipelines" / "factory" / "level5_feature.dot")
    node_names = set(g.nodes)
    node_types = {
        str(n.attrs.get("type", "")).strip().lower()
        for n in g.nodes.values()
    }
    # gate_er: type OR name-based match (Lane B allows both).
    assert (
        "gate_er" in node_types
        or any(
            str(n.attrs.get("type", "")).strip().lower() in {"gate_er", "gate_evidence_audit"}
            for n in g.nodes.values()
        )
    ), "gate_er node required"
    # gate_skeptic: name-based match (Lane B allows both name and type).
    assert "gate_skeptic" in node_names, (
        f"reference .dot must contain a gate_skeptic node (got nodes: {sorted(node_names)})"
    )
    # adversarial_reviewer: name-based check. The node is implemented by the
    # parallel reviewer handler so the primary and shadow Codex reviews are
    # one audited gate instead of a raw serial shell tool.
    assert "adversarial_reviewer" in node_names, (
        "reference .dot must contain an adversarial_reviewer node"
    )


def test_level5_feature_dot_has_parallel_adversarial_reviewer():
    """The adversarial_reviewer must use the parallel reviewer node type."""
    g = parse(ROOT / "pipelines" / "factory" / "level5_feature.dot")
    rev = g.nodes["adversarial_reviewer"]
    assert rev.attrs.get("type") == "parallel_reviewer"
    assert "codex" in str(rev.attrs.get("backend_priority", ""))
    assert str(rev.attrs.get("prefer_adversarial", "")).lower() == "true"


def test_level5_feature_dot_has_soft_tier_nodes():
    """The reference .dot should also carry the 3 soft-tier nodes
    (holdout_eval / healer / spec_validation) by default; they may be
    opted out via skip flags on iteration lanes."""
    g = parse(ROOT / "pipelines" / "factory" / "level5_feature.dot")
    node_names = set(g.nodes)
    assert "holdout_eval" in node_names, (
        "reference .dot must contain a holdout_eval node (no skip flag set)"
    )
    assert "healer" in node_names, "reference .dot must contain a healer node"
    assert "spec_validation" in node_names, (
        "reference .dot must contain a spec_validation node (no skip flag set)"
    )


# ---------------------------------------------------------------------------
# 2. Migrated gates.dot / pr_gates.dot have the level5 reviewer additions
# ---------------------------------------------------------------------------


def test_gates_dot_has_level5_graph_attribute():
    """Migrated gates.dot must declare `graph [level5="true"]`."""
    g = parse(_pipeline("gates.dot"))
    assert str(g.attrs.get("level5", "")).strip().lower() == "true", (
        "migrated gates.dot must have graph [level5=\"true\"]"
    )


def test_gates_dot_has_gate_skeptic_and_adversarial_reviewer():
    """Migrated gates.dot must contain gate_skeptic + adversarial_reviewer
    nodes (the additions Lane B's level5 rule requires)."""
    g = parse(_pipeline("gates.dot"))
    assert "gate_skeptic" in g.nodes, (
        "migrated gates.dot must contain a gate_skeptic node"
    )
    assert "adversarial_reviewer" in g.nodes, (
        "migrated gates.dot must contain an adversarial_reviewer node"
    )


def test_gates_dot_adversarial_reviewer_is_parallel():
    """Migrated gates.dot's adversarial_reviewer must be parallelized."""
    g = parse(_pipeline("gates.dot"))
    rev = g.nodes["adversarial_reviewer"]
    assert rev.attrs.get("type") == "parallel_reviewer"
    assert "codex" in str(rev.attrs.get("backend_priority", ""))
    assert str(rev.attrs.get("prefer_adversarial", "")).lower() == "true"


def test_pr_gates_dot_has_level5_graph_attribute():
    """Migrated pr_gates.dot must declare `graph [level5="true"]`."""
    g = parse(_pipeline("pr_gates.dot"))
    assert str(g.attrs.get("level5", "")).strip().lower() == "true", (
        "migrated pr_gates.dot must have graph [level5=\"true\"]"
    )


def test_pr_gates_dot_has_gate_skeptic_and_adversarial_reviewer():
    """Migrated pr_gates.dot must contain gate_skeptic + adversarial_reviewer
    nodes."""
    g = parse(_pipeline("pr_gates.dot"))
    assert "gate_skeptic" in g.nodes, (
        "migrated pr_gates.dot must contain a gate_skeptic node"
    )
    assert "adversarial_reviewer" in g.nodes, (
        "migrated pr_gates.dot must contain an adversarial_reviewer node"
    )


def test_pr_gates_dot_adversarial_reviewer_is_parallel():
    """Migrated pr_gates.dot's adversarial_reviewer must be parallelized."""
    g = parse(_pipeline("pr_gates.dot"))
    rev = g.nodes["adversarial_reviewer"]
    assert rev.attrs.get("type") == "parallel_reviewer"
    assert "codex" in str(rev.attrs.get("backend_priority", ""))
    assert str(rev.attrs.get("prefer_adversarial", "")).lower() == "true"


def test_migrated_gates_still_have_fix_loop():
    """Sanity: the fix loop contract (per factory-evolve G2) is preserved
    after the level5 migration — `outcome!=success` still routes to fix."""
    g = parse(_pipeline("gates.dot"))
    fix_loop_edges = [
        e for e in g.edges
        if "outcome!=success" in str(e.condition or "")
        and e.dst == "fix"
    ]
    assert len(fix_loop_edges) >= 1, (
        "migrated gates.dot must preserve the fix-loop contract"
    )


def test_migrated_dot_files_end_with_newline():
    """Both migrated .dot files must end with a trailing newline
    (was missing in the original Lane C output)."""
    for name in ("gates.dot", "pr_gates.dot"):
        path = ROOT / "pipelines" / "factory" / name
        raw = path.read_bytes()
        assert raw.endswith(b"\n"), (
            f"{name} must end with trailing newline (was: ...{raw[-5:]!r})"
        )
