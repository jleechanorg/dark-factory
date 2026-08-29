"""Sequential validation round handlers (round_begin, round_end).

Owns:
  * `_round_begin` — increments round count, resets stale per-member state,
    and initializes a clean per-round member ledger.
  * `_round_end` — aggregates member outcomes from the per-round ledger.
    Exits early on all-success; routes to fix on failure below budget;
    emits terminal exhausted when round reaches max_rounds budget.
"""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

from ._classify import _classify_outcome
from .handler_core import Result

if TYPE_CHECKING:
    from .handler_core import Context
    from .parser import Graph, Node


def _record_round_ledger(
    node: Node,
    ctx: Context,
    outcome: str,
    output: str | None = None,
    metadata: dict[str, str] | None = None,
) -> None:
    """Record member execution outcome to the current round's ledger."""
    if "rounds.current" not in ctx.state or node.attrs.get("type") in ("round_begin", "round_end"):
        return
    raw_ledger = ctx.state.get("rounds.ledger", "{}")
    if isinstance(raw_ledger, str):
        try:
            ledger = json.loads(raw_ledger)
        except Exception:
            ledger = {}
    elif isinstance(raw_ledger, dict):
        ledger = raw_ledger
    else:
        ledger = {}
    ledger[node.name] = {
        "outcome": outcome,
        "output": (output or "")[:500],
        "metadata": metadata or {},
    }
    ctx.state["rounds.ledger"] = json.dumps(ledger)
    ctx.state[f"{node.name}.outcome"] = outcome


def _reconstruct_round_state_on_resume(
    graph: Graph,
    ctx: Context,
    history: list,
) -> None:
    """Reconstruct validation rounds state and ledger from checkpoint history."""
    if not history:
        return
    last_rb_idx = -1
    for idx, step in enumerate(history):
        s_node_name = getattr(step, "node", "")
        if s_node_name == "round_begin" or (
            graph.nodes.get(s_node_name)
            and graph.nodes[s_node_name].attrs.get("type") == "round_begin"
        ):
            last_rb_idx = idx

    if last_rb_idx == -1:
        return

    rb_step = history[last_rb_idx]
    rb_node = graph.nodes.get(getattr(rb_step, "node", ""))
    round_meta = getattr(rb_step, "metadata", {}) or {}

    round_num = int(round_meta.get("round", 1))
    max_rounds = int(round_meta.get("max_rounds", ctx.state.get("rounds.requested", 3)))
    members_raw = round_meta.get("members", "")
    if members_raw:
        members = [m.strip() for m in members_raw.split(",") if m.strip()]
    elif rb_node:
        members = _parse_members(rb_node, ctx)
    else:
        members = []

    ctx.state["rounds.current"] = round_num
    ctx.state["rounds.effective"] = round_num
    ctx.state["rounds.requested"] = max_rounds
    ctx.state["rounds.max"] = max_rounds
    ctx.state["rounds.members"] = json.dumps(members)

    has_round_end = any(
        (
            getattr(step, "node", None) == "round_end"
            or (
                graph.nodes.get(getattr(step, "node", ""))
                and graph.nodes[step.node].attrs.get("type") == "round_end"
            )
        )
        for step in history[last_rb_idx + 1 :]
    )

    if not has_round_end:
        ledger: dict[str, dict] = {}
        for step in history[last_rb_idx + 1 :]:
            s_node = getattr(step, "node", "")
            if s_node in members:
                s_outcome = getattr(step, "outcome", "unknown")
                ledger[s_node] = {
                    "outcome": s_outcome,
                    "output": getattr(step, "output_preview", "")[:500],
                    "metadata": getattr(step, "metadata", {}) or {},
                }
                ctx.state[f"{s_node}.outcome"] = s_outcome
        ctx.state["rounds.ledger"] = json.dumps(ledger)


def _parse_members(node: Node, ctx: Context) -> list[str]:
    raw = node.attrs.get("members") or ctx.state.get("rounds.members") or ""
    if isinstance(raw, list):
        return [str(x).strip() for x in raw if str(x).strip()]
    if isinstance(raw, str) and raw.startswith("["):
        try:
            parsed = json.loads(raw)
            if isinstance(parsed, list):
                return [str(x).strip() for x in parsed if str(x).strip()]
        except Exception:
            pass
    return [m.strip() for m in str(raw).split(",") if m.strip()]


def _round_begin(node: Node, ctx: Context) -> Result:
    """Initialize a validation round, reset stale state, and open member ledger."""
    members = _parse_members(node, ctx)
    max_rounds = int(ctx.state.get("rounds.requested", 3))
    current_round = int(ctx.state.get("rounds.current", 0)) + 1

    ctx.state["rounds.current"] = current_round
    ctx.state["rounds.effective"] = current_round
    ctx.state["rounds.requested"] = max_rounds
    ctx.state["rounds.max"] = max_rounds
    ctx.state["rounds.members"] = json.dumps(members)

    # Reset stale per-member state so earlier round outcomes never bleed through
    for member in members:
        ctx.state.pop(f"{member}.outcome", None)
        ctx.state.pop(f"{member}.output", None)
        ctx.state.pop(f"{member}.verdict", None)
        ctx.state.pop(f"{member}.metadata", None)
        ctx.state.pop(f"{member}.returncode", None)

    # Clear prior failure and review state
    ctx.state.pop("last_test_output", None)
    ctx.state.pop("last_test_rc", None)
    ctx.state.pop("last_test_command", None)
    ctx.state.pop("_unresolved_failure", None)
    ctx.state.pop("_unresolved_failure_node", None)
    ctx.state.pop("_last_review_feedback", None)
    ctx.state.pop("_last_coder_handoff", None)
    ctx.state.pop("_last_verdict", None)

    # Clean ledger for the new round
    ctx.state["rounds.ledger"] = json.dumps({})

    meta = {
        "round": str(current_round),
        "max_rounds": str(max_rounds),
        "rounds_requested": str(max_rounds),
        "rounds_effective": str(current_round),
        "members": ",".join(members),
    }
    return Result(
        outcome="success",
        output=f"Validation round {current_round}/{max_rounds} begin ({len(members)} members: {", ".join(members)})",
        metadata=meta,
    )


def _round_end(node: Node, ctx: Context) -> Result:
    """Aggregate member outcomes for the round and decide success, fix, or exhaustion."""
    current_round = int(ctx.state.get("rounds.current", 1))
    max_rounds = int(ctx.state.get("rounds.requested", 3))
    members = _parse_members(node, ctx)

    raw_ledger = ctx.state.get("rounds.ledger", "{}")
    if isinstance(raw_ledger, str):
        try:
            ledger = json.loads(raw_ledger)
        except Exception:
            ledger = {}
    elif isinstance(raw_ledger, dict):
        ledger = raw_ledger
    else:
        ledger = {}

    member_outcomes: dict[str, str] = {}
    for m in members:
        entry = ledger.get(m)
        if isinstance(entry, dict) and "outcome" in entry:
            member_outcomes[m] = entry["outcome"]
        elif f"{m}.outcome" in ctx.state:
            member_outcomes[m] = str(ctx.state[f"{m}.outcome"])
        else:
            member_outcomes[m] = "unknown"

    failed_members = [
        m for m, o in member_outcomes.items() if _classify_outcome(o) != "success"
    ]
    error_members = [
        m for m, o in member_outcomes.items() if _classify_outcome(o) == "error"
    ]

    meta = {
        "round": str(current_round),
        "max_rounds": str(max_rounds),
        "rounds_requested": str(max_rounds),
        "rounds_effective": str(current_round),
        "members": ",".join(members),
        "member_outcomes": json.dumps(member_outcomes),
    }

    if not failed_members:
        meta["aggregate"] = "success"
        return Result(
            outcome="success",
            output=f"Round {current_round}/{max_rounds} succeeded: all {len(members)} members passed",
            metadata=meta,
        )

    meta["failed_members"] = ",".join(failed_members)
    primary_outcome = "error" if error_members else "failure"
    meta["aggregate"] = primary_outcome

    if current_round < max_rounds:
        return Result(
            outcome=primary_outcome,
            output=(
                f"Round {current_round}/{max_rounds} failed "
                f"({len(failed_members)}/{len(members)} members failed: {", ".join(failed_members)})"
            ),
            metadata=meta,
        )

    # Terminal exhaustion: reached max_rounds bound
    meta["aggregate"] = "exhausted"
    meta["exhausted"] = "true"
    return Result(
        outcome="exhausted",
        output=(
            f"Round {current_round}/{max_rounds} failed; max rounds reached "
            f"({len(failed_members)}/{len(members)} members failed: {", ".join(failed_members)})"
        ),
        metadata=meta,
    )
