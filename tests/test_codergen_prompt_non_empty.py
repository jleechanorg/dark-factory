"""Regression guard: every ``codergen`` node in any .dot pipeline has
a non-empty ``prompt`` attribute.

Companion to the 4 contract-test families:

- ``test_gates_dot_timeouts.py`` (factory/) — per-family timeout-attr presence
- ``test_slim_pipelines_timeouts.py`` (slim/) — per-family timeout-attr presence
- ``test_airbnb_clone_pipelines_timeouts.py`` (airbnb-clone/) — per-family
- ``test_amazon_clone_pipelines_timeouts.py`` (amazon-clone/) — per-family
- ``test_remaining_pipelines_timeouts.py`` (3 more families) — per-family
- ``test_timeout_value_pinning.py`` — cross-family timeout value range
- ``test_prompt_pinning.py`` — every ``prompt=@...`` reference resolves

The contract-test pattern has 4 dimensions so far: per-family timeout
presence, per-family timeout value, cross-family timeout range, and
prompt resolution. This test adds a 5th dimension: **prompt
non-empty on codergen nodes**.

The contract: if a node is a codergen (or a default-shape node that
the engine resolves to ``_codergen``), the engine's ``_render_prompt``
will fall back to a goal-only stub (``"# <node>\n\nGoal: <goal>"``)
when ``prompt`` is missing or empty. That stub is degraded: it
cannot drive a real coding agent because it carries no task
description, only the goal. The right contract is: **every codergen
node has an explicit prompt file or string**, so the engine never
silently degrades to the goal-only stub.

A regression that drops a ``prompt=`` attr (a copy-paste error, a
merge conflict resolution that left the attr on the wrong side, a
``sed`` typo) would currently ship silently and only surface when
the agent's run produced a confusingly-vague prompt. This test
catches the regression at unit-test time.

Catches a different class of bug than ``test_prompt_pinning.py``:

- ``test_prompt_pinning.py`` asserts every ``prompt=@...`` REFERENCE
  RESOLVES to an existing file. This catches broken @-references
  (a typo, a renamed prompt, a deleted file).
- This test asserts every ``codergen`` node HAS a non-empty
  ``prompt=`` attribute. This catches missing-prompt codergens
  (a copy-paste error, a merge conflict that lost the attr).

The two tests are complementary: a codergen with ``prompt=@bad.md``
would pass this test (the attr is non-empty) but fail the prompt
resolution test; a codergen with no ``prompt=`` attr at all would
pass the prompt resolution test (no reference to resolve) but fail
this test.

**File-disjoint**: this test is a new file, only reads .dot
pipelines through the existing parser. Does not touch any
WIP-touched file. The 7 sibling test files are NOT imported — this
test inlines its own walk to survive WIP rebases.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.parser import parse  # noqa: E402


# Node types that the engine treats as "needs an explicit prompt" —
# i.e. the handler renders the node's prompt template into the
# agent's invocation. Nodes that read their own template (e.g.
# holdout_eval, gate_*) are exempt: the prompt is internal to the
# handler and not a user-facing template.
#
# `default` covers the implicit-codergen case: nodes without a
# `type="..."` attr and without a special shape resolve to the
# `_codergen` handler via the engine's lookup chain. The contract
# is: "if the engine will spawn an LLM here, it must have a prompt."
_NEEDS_PROMPT_TYPES = frozenset({"codergen", "default"})


def _all_dot_files() -> list[pathlib.Path]:
    """Return every .dot file in the repo, excluding worktree copies and include-only fragments."""
    out: list[pathlib.Path] = []
    for path in ROOT.rglob("*.dot"):
        parts = path.parts
        if ".claude" in parts and "worktrees" in parts:
            continue
        if path.stem.startswith("_"):
            continue
        out.append(path)
    return sorted(out)


def _node_needs_prompt(node) -> bool:
    """Return True if the engine will render this node's prompt at runtime.

    Mirrors the engine's handler resolution: a node with
    ``type="codergen"`` is explicit, a node with no ``type=`` and
    no special shape defaults to ``_codergen`` (the "default" case).
    Nodes with other ``type=`` values (``tool``, ``holdout_eval``,
    ``gate_*``, ``human_gate``, ``conditional``) are NOT codergens
    and are exempt from the contract.

    Special-shape exemptions (mirroring ``runner.engine._is_parallel_node``
    / ``_is_join_node``):

    - ``shape=point`` — graph topology anchors (e.g. ``_base.dot``'s
      ``explore_in`` / ``explore_out``); they exist purely as zero-width
      routing nodes and the engine never instantiates a handler for them.
    - ``shape=component`` (no explicit ``type=``) — fan-out node, handled
      by the parallel branch in the engine, not by ``_codergen``.
    - ``shape=tripleoctagon`` (no explicit ``type=``) — fan-in join node,
      handled by the parallel branch, not by ``_codergen``.

    These exemptions match the engine's own dispatch logic — a
    node the engine routes to ``_codergen`` is the only kind that
    needs a prompt, and these shapes are routed elsewhere.
    """
    # Special-shape exemptions first — these never reach `_codergen`.
    shape = str(node.attrs.get("shape", ""))
    if shape in ("point", "component", "tripleoctagon", "Mdiamond", "Msquare"):
        # point=anchor, component=fanout, tripleoctagon=join,
        # Mdiamond=start, Msquare=exit (also caught by name skip).
        return False
    t = node.attrs.get("type")
    if t is None:
        # No explicit type — engine falls back to _codergen if shape
        # is also default (ellipse / box / box3d etc). The shape
        # exemptions above already removed the special shapes.
        return True
    return str(t) in _NEEDS_PROMPT_TYPES


def test_every_codergen_node_has_a_non_empty_prompt() -> None:
    """Every ``codergen`` (or default-shape implicit-codergen) node has a non-empty ``prompt``.

    Iterates every .dot file in the repo and asserts that any
    codergen-class node has a non-empty ``prompt`` attribute. The
    engine's ``_render_prompt`` falls back to a goal-only stub
    (``"# <node>\n\nGoal: <goal>"``) when ``prompt`` is missing or
    empty, which is degraded: a real coding agent gets a vague
    invocation. The contract is: every codergen node has an
    explicit prompt template so the engine never silently
    degrades.

    Catches copy-paste errors, merge conflicts that lost the
    ``prompt=`` attr, and ``sed`` typos. A bug class that would
    otherwise surface only when the agent's run produced a
    confusingly-vague prompt.
    """
    offenders: list[tuple[str, str, str]] = []
    for path in _all_dot_files():
        rel = str(path.relative_to(ROOT))
        g = parse(path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            if not _node_needs_prompt(node):
                continue
            ref = node.prompt_ref
            if not ref or not ref.strip():
                t = node.attrs.get("type", "(default)")
                offenders.append((rel, name, str(t)))
    assert not offenders, (
        f"every codergen node must have a non-empty prompt. "
        f"The engine's _render_prompt falls back to a goal-only stub "
        f"when prompt is missing — a degraded mode that produces a "
        f"vague agent invocation. Offenders: {offenders}."
    )


def test_needs_prompt_types_allowlist_is_sane() -> None:
    """The ``_NEEDS_PROMPT_TYPES`` allow-list is reviewable and bounded.

    A future maintainer adding new codergen-like types to the
    engine (e.g. ``type="long_codergen"``) would silently bypass
    this test. The allow-list guard surfaces that change: if
    ``_NEEDS_PROMPT_TYPES`` grows, the maintainer is forced to
    consider whether the new type also needs a prompt contract.
    """
    # The set must contain at least `codergen` (the canonical case)
    # and `default` (the implicit-fallback case). It must NOT
    # contain `start` or `exit` (those are built-ins, not codergens).
    assert "codergen" in _NEEDS_PROMPT_TYPES, (
        f"_NEEDS_PROMPT_TYPES must include 'codergen': "
        f"{sorted(_NEEDS_PROMPT_TYPES)}"
    )
    assert "default" in _NEEDS_PROMPT_TYPES, (
        f"_NEEDS_PROMPT_TYPES must include 'default' (implicit "
        f"codergen via no-type): {sorted(_NEEDS_PROMPT_TYPES)}"
    )
    for forbidden in ("start", "exit", "tool", "holdout_eval", "gate_es"):
        assert forbidden not in _NEEDS_PROMPT_TYPES, (
            f"_NEEDS_PROMPT_TYPES must not include {forbidden!r} "
            f"(not a codergen): {sorted(_NEEDS_PROMPT_TYPES)}"
        )
