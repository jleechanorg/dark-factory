"""Prompt template resolution (``@path`` → rendered text).

Owns:
  * `_render_prompt` — resolve ``@path``, enforce workdir-relative, holdout-deny,
    substitute ``${goal}`` / ``${state.<key>}` / ``${diff}``. Mirrors
    ``runner.handlers._render_prompt`` so ``tests/test_prompt_pinning.py`` can
    pin the same resolution order.

Substitutions (in order):
  * ``${goal}``     → the run-level goal text
  * ``${state.<key>}`` → any ``ctx.state`` key, e.g. ``${state._last_output}``
  * ``${diff}``     → the most recent codergen's ``git diff`` (G4), captured
    automatically by ``_codergen`` on the success path. Defaults to
    ``"(no diff captured)"`` when no codergen has run yet (or when the
    capture silently failed because the workdir is not a git repo).
"""

from __future__ import annotations

import pathlib
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _substitute_placeholders(text: str, ctx: "Context") -> str:
    """Apply ``${goal}`` / ``${state.<key>}`` / ``${diff}`` substitutions.

    ``${diff}`` resolves to ``ctx.state["_last_diff"]`` if a successful
    codergen stashed one, else the placeholder ``"(no diff captured)"`` so
    reviewer prompts never render an empty cell where the diff should be.
    """
    text = text.replace("${goal}", ctx.goal)
    for k, v in ctx.state.items():
        # Skip non-string values entirely: the prompt template only expects
        # strings, and silently coercing opaque types (e.g. a ``Graph`` object
        # stashed by the ``type="dynamic"`` handler for graph-driven fallback
        # resolution) would inject a multi-KB debug dump via ``__str__`` into
        # the rendered prompt. Earlier ``str(v)`` coercion also caused crashes
        # with non-string types in some render paths; skipping is the
        # minimum-viable safe fix.
        if not isinstance(v, str):
            continue
        text = text.replace("${state." + k + "}", v)
    diff = ctx.state.get("_last_diff", "")
    if not diff:
        diff = "(no diff captured)"
    text = text.replace("${diff}", diff)
    return text


def _render_prompt(node: "Node", ctx: "Context") -> str:
    ref = node.prompt_ref
    if not ref:
        return f"# {node.name}\n\nGoal: {ctx.goal}"
    ref_path = pathlib.Path(ref)
    if ref_path.is_absolute():
        resolved_ref = ref_path
        try:
            resolved_ref = ref_path.resolve()
        except FileNotFoundError:
            return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"

        for deny in _handlers_shim._holdout_denied_paths():
            try:
                resolved_ref.relative_to(deny)
            except ValueError:
                pass
            else:
                return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"

        text_path = resolved_ref
        if not text_path.exists():
            return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"
        text = text_path.read_text()
        return _substitute_placeholders(text, ctx)
    from .paths import factory_home
    root = ctx.workdir.resolve()
    p = (root / ref_path).resolve()
    if not p.exists():
        home = factory_home()
        if home is not None:
            alt = (home / ref_path).resolve()
            if alt.exists():
                p = alt
    try:
        p.relative_to(root)
    except ValueError:
        home = factory_home()
        if home is not None:
            try:
                p.relative_to(home.resolve())
            except ValueError:
                return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
        else:
            return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
    if not p.exists():
        return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"
    text = p.read_text()
    return _substitute_placeholders(text, ctx)
