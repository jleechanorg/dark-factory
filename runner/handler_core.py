"""Public handler data types and the trivial start/exit builtins.

Owns:
  * `Result` — the public result shape returned by every handler.
  * `Context` — the public run-state shape passed to every handler.
  * `Handler` — the ``Callable[[Node, Context], Result]`` type alias.
  * `_TIMEOUT_MIN_SECONDS`, `_TIMEOUT_MAX_SECONDS` — policy envelope.
  * `_coerce_timeout` — parse + clamp a timeout attr to the envelope.
  * `_serialize_state_value` — the single repository-wide convention for
    rendering a ``ctx.state`` value into placeholder-substitution text.
    Shared by ``handler_render._substitute_placeholders`` (prompt templates)
    and ``handler_decision._substitute_state`` (tool/holdout/test_path node
    attrs) so both substitution paths serialize structured state (dict/list)
    identically instead of drifting into inconsistent representations
    (jleechan-7t92).
  * `_start`, `_exit` — the Mdiamond / Msquare builtin handlers.
"""

from __future__ import annotations

import json
import pathlib
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

# Rationale (jleechan-arr): the policy envelope here is the *clamp range*,
# not a default. Callers pass `default=...` to ``_coerce_timeout`` and that
# default is what shows up when a .dot node omits the ``timeout`` attribute.
# The roadmap at ``docs/plans/factory_improvement_analysis.md`` proposes
# per-class defaults of 60 / 180 / 300 seconds (tool / codergen / review).
# Those values are *not* enforced as global defaults because production
# evidence contradicts them: real codergen runs hit the 1800s ceiling on
# claude/codex backends (p99 = 1800s, max = 1800s); gate reviewer runs
# (gate_es / gate_er / gate_code_standards / gate_audit) have a p99 = 206s
# but the 1200s default still has headroom for very large diffs. The
# per-class defaults are co-located with the call sites that use them
# (``runner/handler_codergen.py`` codergen 1800s/600s, ``handler_audit.py``
# / ``handler_universal_prompts.py`` / ``handler_special_gates.py`` gates
# 1200s) — see ``TIMEOUT_DEFAULTS_RATIONALE`` in
# ``docs/plans/factory_improvement_analysis.implementation.md`` Pillar 4
# for the empirical distribution and the rationale.


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


def _serialize_state_value(key: str, value: object) -> str:
    """Deterministically render a ``ctx.state`` value for text substitution.

    Repository structured-data convention (established by ``runner/cxdb.py``'s
    event metadata column and ``handler_dispatch.py``'s
    ``resolved_backend_meta``): structured values (dict/list) serialize as
    sort-keyed JSON; scalars serialize via ``str()``. Every placeholder
    substitution site in the runner (prompt rendering, tool/holdout/test_path
    attrs) MUST go through this single function so a dict/list value renders
    identically everywhere instead of drifting between JSON here and Python
    ``repr()`` there.

    Serialization failure (e.g. a dict holding a non-JSON-serializable nested
    object, or a scalar whose ``__str__`` raises) must never crash the
    render. On failure this returns a placeholder that names the offending
    *key* and the value's *type* only — never the value's content — so a
    render failure can be diagnosed without risking a leak of sealed/holdout
    data that might be sitting in ``ctx.state``.
    """
    if isinstance(value, str):
        return value
    if isinstance(value, (dict, list)):
        try:
            return json.dumps(value, sort_keys=True)
        except (TypeError, ValueError):
            return f"<unserializable state value: key={key!r} type={type(value).__name__}>"
    try:
        return str(value)
    except Exception:
        return f"<unserializable state value: key={key!r} type={type(value).__name__}>"


Handler = Callable[[Node, Context], Result]


def _gate_strict_flag(node: "Node") -> bool:
    """Read the ``gate_strict`` node attribute as a bool.

    Accepts ``True`` / ``"true"`` / ``"1"`` (case-insensitive). Any other
    value is treated as False so unknown typos do not silently enable
    strict-mode and break existing graphs.

    See jleechan-9ia (F6 in 2026-06-22 /factory-evolve proposals). When
    True, the gate's `warn` verdict is normalized to `failure` instead of
    the legacy warn→success mapping.

    Lives in handler_core (not handler_dispatch) to avoid the circular
    import between handler_dispatch and runner.handlers.
    """
    raw = node.attrs.get("gate_strict")
    if raw is True:
        return True
    if isinstance(raw, str) and raw.strip().lower() in ("true", "1", "yes"):
        return True
    if isinstance(raw, int) and raw == 1:
        return True
    return False


def _target_worktree(ctx: "Context") -> pathlib.Path:
    """Return the validated target worktree for worker diff and controller review.

    Prefers `ctx.state["ao.worktree"]` when it is an absolute, non-traversing
    path to an existing directory; otherwise falls back to `ctx.workdir`.
    """
    if hasattr(ctx, "state") and isinstance(ctx.state, dict):
        ao_wt = ctx.state.get("ao.worktree")
        if ao_wt:
            ao_path = pathlib.Path(str(ao_wt))
            if (
                ao_path.is_absolute()
                and ".." not in ao_path.parts
                and ao_path.is_dir()
            ):
                return ao_path.resolve()
    try:
        return pathlib.Path(ctx.workdir).resolve()
    except (AttributeError, TypeError):
        return pathlib.Path(".").resolve()


def current_dispatch_meta(ctx: "Context", fallback_node: str = "") -> tuple[int, int, str]:
    """Engine-set dispatch coordinates (seq, attempt, node_name) for sidecars.

    engine_run sets `_df_current_*` on ctx before each node dispatch; handlers
    invoked outside the engine fall back to the last completed seq / attempt 1.
    """
    seq = int(getattr(ctx, "_df_current_seq", getattr(ctx, "last_completed_seq", 0)))
    attempt = int(getattr(ctx, "_df_current_attempt", 1))
    node_name = str(getattr(ctx, "_df_current_node", fallback_node))
    return seq, attempt, node_name


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
    
    # G8: re-pin HEAD SHA at exit node
    last_validated = ctx.state.get("_last_validated_head_sha")
    if last_validated:
        from .handler_verdict import _worktree_head_sha
        current_sha = _worktree_head_sha(ctx.workdir)
        if current_sha and current_sha.lower() != last_validated.lower():
            return Result(
                outcome="error",
                output=(
                    f"exit blocked: HEAD SHA changed since last validated review gate. "
                    f"Expected: {last_validated}, got: {current_sha}"
                ),
                metadata={"exit_sha_status": "mismatched"},
            )
    return Result(outcome="success", output="exit")
