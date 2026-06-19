"""Parallel fanout / join handlers.

Owns:
  * `_parallel_fanout` — fan-out record step (real branching is in engine).
  * `_join_handler` — join record step (real policy is in engine).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _parallel_fanout(node: "Node", ctx: "Context") -> "Result":
    """Fan-out handler — records the fan-out step; actual concurrent branching is in engine.py."""
    return Result(outcome="success", output=f"fanout: {node.name}", metadata={"role": "fanout"})


def _join_handler(node: "Node", ctx: "Context") -> "Result":
    """Join handler — signals the node type; policy evaluation is in engine.py.

    The engine's parallel block calls _apply_join_policy and builds the join
    StepRecord directly, so this handler is never invoked for join nodes that
    follow a type=parallel fan-out.  If a join node is reached via normal
    (non-parallel) traversal, there are no branches to aggregate and
    returning success is correct.
    """
    return Result(outcome="success", output=f"join: {node.name}", metadata={"role": "join"})
