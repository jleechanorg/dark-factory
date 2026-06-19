"""Prompt template resolution (``@path`` → rendered text).

Owns:
  * `_render_prompt` — resolve ``@path``, enforce workdir-relative, holdout-deny,
    substitute ``${goal}`` / ``${state.<key>}``. Mirrors
    ``runner.handlers._render_prompt`` so ``tests/test_prompt_pinning.py`` can
    pin the same resolution order.
"""

from __future__ import annotations

import pathlib
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


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
        text = text.replace("${goal}", ctx.goal)
        for k, v in ctx.state.items():
            text = text.replace("${state." + k + "}", v)
        return text
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
    text = text.replace("${goal}", ctx.goal)
    for k, v in ctx.state.items():
        text = text.replace("${state." + k + "}", v)
    return text
