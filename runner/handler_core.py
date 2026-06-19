"""Public handler data types and the trivial start/exit builtins.

Owns:
  * `Result` — the public result shape returned by every handler.
  * `Context` — the public run-state shape passed to every handler.
  * `Handler` — the ``Callable[[Node, Context], Result]`` type alias.
  * `_TIMEOUT_MIN_SECONDS`, `_TIMEOUT_MAX_SECONDS` — policy envelope.
  * `_coerce_timeout` — parse + clamp a timeout attr to the envelope.
  * `_start`, `_exit` — the Mdiamond / Msquare builtin handlers.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Callable, Optional

from .parser import Node

if TYPE_CHECKING:
    from .perf_log import GitContext, PerfRun


@dataclass
class Result:
    outcome: str = "success"  # used by edge `condition="outcome=success"`
    output: str = ""
    metadata: dict[str, str] = field(default_factory=dict)
    preferred_label: str = ""
    suggested_next_ids: list[str] = field(default_factory=list)
    context_updates: dict[str, str] = field(default_factory=dict)


@dataclass
class Context:
    """Mutable run state passed to every handler."""

    goal: str
    workdir: "pathlib.Path"
    state: dict[str, str] = field(default_factory=dict)
    history: list[dict[str, str]] = field(default_factory=list)
    backend: str = "echo"  # echo | mock_llm | ao | claude | codex | agy
    cxdb_path: Optional["pathlib.Path"] = None
    run_id: Optional[str] = None
    event_log_path: Optional["pathlib.Path"] = None
    perf_log_root: Optional["pathlib.Path"] = None
    git_ctx: Optional["GitContext"] = None
    perf_run: Optional["PerfRun"] = None
    last_completed_seq: int = 0


_TIMEOUT_MIN_SECONDS = 5
_TIMEOUT_MAX_SECONDS = 3600


def _coerce_timeout(value: object, default: int, *, minimum: int = _TIMEOUT_MIN_SECONDS, maximum: int = _TIMEOUT_MAX_SECONDS) -> int:
    """Parse and clamp timeout values to the policy envelope.

    Invalid values fall back to `default`. Very small / very large values are
    clamped to prevent pathological zero-timeout runs or runaway hangs.
    """
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    if parsed < minimum:
        return minimum
    if parsed > maximum:
        return maximum
    return parsed


Handler = Callable[[Node, Context], Result]


def _start(node: Node, ctx: Context) -> Result:
    return Result(outcome="success", output=f"start: {ctx.goal!r}")


def _exit(node: Node, ctx: Context) -> Result:
    unresolved = ctx.state.get("_unresolved_failure")
    if unresolved:
        return Result(outcome=unresolved, output=f"exit after unresolved {unresolved}")
    if ctx.history:
        previous = ctx.history[-1].get("outcome", "success")
        if previous != "success":
            return Result(outcome=previous, output=f"exit after {previous}")
    return Result(outcome="success", output="exit")
