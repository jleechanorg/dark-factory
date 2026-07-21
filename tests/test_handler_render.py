"""Unit tests for ``runner.handler_render`` prompt-substitution guard.

Lane A1 fix: ``_substitute_placeholders`` previously called
``text.replace("${state." + k + "}", v)`` with a raw value, which raised
``TypeError: replace() argument 2 must be str, not Graph`` when the
``type="dynamic"`` handler stashed the live ``Graph`` object into
``ctx.state["_graph"]`` for fallback resolution against
``default="<node_name>"``.

A later revision coerced non-string values via ``str(v)``. That coercion
silently injected multi-KB ``__str__`` dumps (Graph's repr is ~150 chars for
a tiny graph and grows with node/edge count) into the rendered prompt.

The minimum-viable safe behavior is: **skip non-string values entirely**.
The prompt template only ever references string keys, and any opaque value
present in ``ctx.state`` is a side-effect of an internal handler — not a
prompt input the user authored.

These tests pin:

1. **Skip non-string values** — passing a ``Graph``-like object (anything
   non-string) must not crash and must leave the placeholder in place.
2. **Strings still substitute** — regression test: pure-string values still
   get substituted exactly as before.
3. **Mixed dict** — when some state values are strings and others are not,
   only the strings are substituted; non-strings are skipped without
   affecting adjacent substitutions.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT as _ROOT  # noqa: E402, F811

# ``runner.handler_render`` imports ``runner.handlers`` at module load time
# (line 23: ``import runner.handlers as _handlers_shim``), and ``handlers``
# back-imports ``_render_prompt`` from ``handler_render``. Under a clean
# ``python -m pytest tests/test_handler_render.py`` invocation the
# ``handler_render`` import fires first and the back-import fails with
# ``partially initialized module``. Loading ``runner.handlers`` here
# populates ``sys.modules`` and lets the cross-reference resolve — the
# same trick pytest's collection uses implicitly when other tests have
# already imported the runner package.
import runner.handlers as _handlers_preload  # noqa: E402, F401
from runner.handler_core import Context  # noqa: E402
from runner.handler_render import _substitute_placeholders  # noqa: E402
from runner.parser import Graph, Node  # noqa: E402


class _OpaqueNonString:
    """Stand-in for an opaque object stashed in ctx.state.

    Mirrors the ``type="dynamic"`` fallback use case: a real ``Graph``
    instance lives in ``ctx.state["_graph"]`` so the dynamic handler can
    resolve ``default="<node_name>"`` against the parsed DOT graph. We
    don't pull a real ``Graph`` into the unit test because constructing
    one requires name/goal/nodes/edges — the contract is "any non-string
    value," and a small stub is the cleanest way to assert it.
    """

    def __init__(self, marker: str = "OPAQUE") -> None:
        self.marker = marker

    def __str__(self) -> str:  # pragma: no cover - never reached when guard works
        return f"<{self.marker}-debug-dump>"


def _make_ctx(tmp_path: pathlib.Path, goal: str = "g") -> Context:
    return Context(goal=goal, workdir=tmp_path, backend="echo")


def test_substitute_placeholders_skips_non_string_values(tmp_path: pathlib.Path) -> None:
    """A non-string ``ctx.state`` value (Graph-like opaque) must not crash.

    Regression for Lane A's ``type="dynamic"`` fallback: with the Graph
    stashed in ``ctx.state["_graph"]``, the old code crashed at
    ``text.replace("...", graph_object)`` with a ``TypeError``. The new
    guard skips non-string values and leaves the placeholder text intact,
    so the prompt renders without injecting a debug dump.
    """
    ctx = _make_ctx(tmp_path)
    ctx.state["_graph"] = _OpaqueNonString(marker="Graph")

    text = "Implementing against graph: ${state._graph}\n"
    rendered = _substitute_placeholders(text, ctx)

    # Must not raise (the regression we're guarding against).
    assert isinstance(rendered, str)
    # The placeholder must be left alone — the engine did not coerce the
    # opaque value into the prompt; downstream codergen logic decides
    # what to do with _graph.
    assert "${state._graph}" in rendered
    # Critically: NO debug dump from __str__ must appear in the prompt.
    assert "Graph-debug-dump" not in rendered
    assert "<OPAQUE-debug-dump>" not in rendered


def test_substitute_placeholders_still_substitutes_strings(tmp_path: pathlib.Path) -> None:
    """Regression: pure-string state values still substitute exactly.

    The guard must not change behavior for the 99% case: a state value
    that IS a string must still be substituted into the template via
    ``str.replace``. We assert the exact substituted text (not just
    absence-of-placeholder) so a future refactor that breaks the
    substitution path gets caught here, not at the integration layer.
    """
    ctx = _make_ctx(tmp_path)
    ctx.state["feature_name"] = "ratelimit-hardening"
    ctx.state["last_output"] = "Reviewed 3 files; no blockers."

    text = "Feature: ${state.feature_name}\nNotes: ${state.last_output}\n"
    rendered = _substitute_placeholders(text, ctx)

    assert rendered == (
        "Feature: ratelimit-hardening\n"
        "Notes: Reviewed 3 files; no blockers.\n"
    )
    assert "${state.feature_name}" not in rendered
    assert "${state.last_output}" not in rendered


def test_substitute_placeholders_mixed_string_and_non_string(tmp_path: pathlib.Path) -> None:
    """Mixed dict: only strings substitute, non-strings are skipped.

    Verifies the guard does not skip the entire iteration on a single
    non-string value (a naive ``any(isinstance(...)) -> break`` would
    regress this). The opaque value's placeholder must remain, while
    adjacent string placeholders must still be substituted.
    """
    ctx = _make_ctx(tmp_path)
    ctx.state["_graph"] = _OpaqueNonString(marker="Graph")
    ctx.state["feature_name"] = "dynamic-fallback"

    text = (
        "feature=${state.feature_name}\n"
        "graph=${state._graph}\n"
        "feature_again=${state.feature_name}\n"
    )
    rendered = _substitute_placeholders(text, ctx)

    # String substitution applied on both sides.
    assert "feature=dynamic-fallback" in rendered
    assert "feature_again=dynamic-fallback" in rendered
    # Non-string placeholder was left in place.
    assert "graph=${state._graph}" in rendered
    # No debug dump from __str__ of the opaque value.
    assert "Graph-debug-dump" not in rendered


def test_substitute_placeholders_empty_state(tmp_path: pathlib.Path) -> None:
    """Empty state (no keys) leaves the text untouched (apart from ${goal})."""
    ctx = _make_ctx(tmp_path, goal="ship the feature")
    text = "Goal: ${goal}\nNo state references here.\n"
    rendered = _substitute_placeholders(text, ctx)

    assert rendered == "Goal: ship the feature\nNo state references here.\n"


def test_substitute_placeholders_goal_substitution_still_works(tmp_path: pathlib.Path) -> None:
    """The ${goal} substitution path is not affected by the state guard.

    Sanity: the guard sits inside the ``for k, v in ctx.state.items()``
    loop; the ${goal} line above it must still substitute. If a future
    refactor accidentally moves the guard above the ${goal} line, this
    test catches it.
    """
    ctx = _make_ctx(tmp_path, goal="make tests green")
    text = "${goal} — ${state.missing_key}\n"
    rendered = _substitute_placeholders(text, ctx)

    assert rendered.startswith("make tests green — ")
    # The missing-key placeholder is left alone (no string value present).
    assert "${state.missing_key}" in rendered