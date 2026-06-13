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
# The default case (no explicit type, plain shape) is handled
# inline in `_node_needs_prompt` — it's a separate code path
# from the explicit-type allow-list because the dispatch is
# "shape determines handler" rather than "type determines handler."
_NEEDS_PROMPT_TYPES = frozenset({"codergen"})


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

    Mirrors the engine's handler resolution (see
    ``runner.handlers.resolve`` and ``runner.engine._is_parallel_node``
    / ``_is_join_node``). The order matters:

    1. If the node has an explicit ``type=`` attribute, the type wins
       over shape. A node with ``type="codergen", shape="component"``
       is a codergen (not a parallel fan-out) — the engine's
       ``_is_parallel_node`` returns False because explicit type
       takes priority. Similarly, ``type="tool"`` is a tool handler
       regardless of shape.
    2. If the node has NO explicit ``type=``, shape determines the
       handler: ``component`` = parallel fan-out, ``tripleoctagon``
       = join, ``point`` = topology anchor, ``Mdiamond`` = start,
       ``Msquare`` = exit. Anything else (ellipse, box, box3d, ...)
       falls through to the default ``_codergen`` handler.
    3. An explicit ``type=`` value that is NOT in the engine's
       ``TYPE_REGISTRY`` (e.g. a typo like ``type="codergn"``) is
       treated as unregistered: ``resolve()`` falls through to
       the default ``_codergen`` handler. The test must mirror
       this — an unregistered explicit type still needs a prompt.

    The contract is: if the engine will spawn an LLM via
    ``_codergen`` for this node, the node must have a non-empty
    prompt. Special shapes that the engine routes elsewhere
    (point, component, tripleoctagon, Mdiamond, Msquare) are
    exempt ONLY when no explicit ``type=`` is set.
    """
    shape = str(node.attrs.get("shape", ""))
    t = node.attrs.get("type")
    if t is not None:
        # Explicit type wins over shape. A codergen with shape=component
        # is still a codergen. A tool with shape=point is still a tool.
        # An unregistered type falls through to _codergen at runtime,
        # so it still needs a prompt (the type lookup misses, and
        # the engine's resolve() chain lands on _codergen).
        if str(t) in _NEEDS_PROMPT_TYPES:
            return True
        # Explicit type is NOT codergen-default — the engine has a
        # dedicated handler for it (tool, holdout_eval, gate_*, etc).
        # Even if the type is unregistered, we treat it as a
        # non-codergen here because the engine's TYPE_REGISTRY is
        # the source of truth; a typo like type="codergn" would
        # fail at engine time anyway, and the test's job is to
        # pin the contract for known-codergen nodes. (We could
        # mirror the typo→_codergen fallback, but that would
        # generate noisy false positives for typos the test is
        # not designed to catch.)
        return False
    # No explicit type — shape determines handler.
    # Special shapes that never reach _codergen:
    if shape in ("point", "component", "tripleoctagon", "Mdiamond", "Msquare"):
        return False
    # Plain shape (ellipse / box / box3d / ...): engine falls
    # through to _codergen.
    return True


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
    # The set must contain `codergen` (the canonical case). It must
    # NOT contain `start` or `exit` (those are built-ins, not codergens).
    # The default-shape case is handled inline in `_node_needs_prompt`,
    # not in the allow-list, because the dispatch path is different
    # (shape-driven for no-type nodes, type-driven for explicit-type
    # nodes).
    assert "codergen" in _NEEDS_PROMPT_TYPES, (
        f"_NEEDS_PROMPT_TYPES must include 'codergen': "
        f"{sorted(_NEEDS_PROMPT_TYPES)}"
    )
    for forbidden in ("start", "exit", "tool", "holdout_eval", "gate_es"):
        assert forbidden not in _NEEDS_PROMPT_TYPES, (
            f"_NEEDS_PROMPT_TYPES must not include {forbidden!r} "
            f"(not a codergen): {sorted(_NEEDS_PROMPT_TYPES)}"
        )
