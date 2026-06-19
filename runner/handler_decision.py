"""Decision node + ``${state.<key>}`` substitution helpers.

Owns:
  * `_conditional` — the hexagon decision handler (outcome comes from state).
  * `_substitute_state` — replace ``${state.<key>}`` markers in text from
    ``ctx.state``. Unresolved markers are left intact so a downstream
    subprocess will see them and typically fail visibly rather than silently
    substituting "".
  * `_path_attr` — resolve a node attribute holding a filesystem path with
    substitution + holdout-deny fallback.
  * `_has_unresolved_state_placeholder` — predicate for the marker.
"""

from __future__ import annotations

import pathlib
from typing import TYPE_CHECKING

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _conditional(node: "Node", ctx: "Context"):
    """A `shape=hexagon` decision node. The outcome comes from state."""
    key = node.attrs.get("decision_key", node.name)
    outcome = ctx.state.get(key, "success")
    return Result(outcome=outcome, output=f"decision({key})={outcome}")


def _substitute_state(text: str, ctx: "Context") -> str:
    """Replace `${state.<key>}` markers in `text` from ctx.state.

    Unresolved markers are left intact so a downstream subprocess will see
    them (and typically fail visibly) rather than silently substituting "".
    """
    if "${state." not in text:
        return text
    for k, v in ctx.state.items():
        text = text.replace("${state." + k + "}", str(v))
    return text


def _path_attr(node: "Node", ctx: "Context", key: str, default: pathlib.Path) -> pathlib.Path:
    raw = node.attrs.get(key)
    if not raw:
        return default
    raw = _substitute_state(raw, ctx)
    if "${state." in raw:
        return default
    path = pathlib.Path(raw).expanduser()
    if not path.is_absolute():
        path = (ctx.workdir / path).resolve()
    return path


def _has_unresolved_state_placeholder(value: str) -> bool:
    return "${state." in value
