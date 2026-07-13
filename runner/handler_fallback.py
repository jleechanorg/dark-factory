"""Fallback agent lifecycle handlers for dark-factory."""

from __future__ import annotations

import os
import subprocess
import time
import urllib.request
import json
from datetime import datetime, timezone
from typing import TYPE_CHECKING

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context

DEFAULT_RATE_LIMIT_KEYWORDS = [
    "weekly limit",
    "rate limit exceeded",
    "rate_limit_error",
    "overloaded_error",
    "429",
    "too many requests",
    "quota exceeded",
    "resource_exhausted",
    "ratelimitexceeded",
    "insufficient_quota",
    "rate limit",
    "over your limit",
    "you've hit your",
    "usage limit",
    "try again later",
]

def _parse_agent_chain(chain_str: str) -> list[tuple[str, str]]:
    result = []
    for entry in chain_str.split(","):
        entry = entry.strip()
        if not entry:
            continue
        if ":" in entry:
            agent, model = entry.split(":", 1)
            result.append((agent.strip(), model.strip()))
        else:
            result.append((entry, ""))
    return result

def _agent_spawn(node: Node, ctx: Context) -> Result:
    """Prepares environment for spawning the first agent in the chain."""
    chain_str = node.attrs.get("agent_chain")
    if not chain_str:
        return Result(outcome="failure", output="agent_spawn node requires agent_chain attribute")

    chain = _parse_agent_chain(chain_str)
    if not chain:
        return Result(outcome="failure", output="Empty or invalid agent_chain")

    ctx.state["fallback_index"] = "0"
    ctx.state["agent_chain"] = chain_str

    agent, model = chain[0]
    ctx.state["ao.agent"] = agent
    if agent in ("antigravity", "agy") and model:
        os.environ["ANTIGRAVITY_MODEL"] = model
    elif agent == "minimax" and model:
        os.environ["MINIMAX_MODEL"] = model

    return Result(
        outcome="success",
        output=f"Prepared to spawn agent={agent} model={model}",
        context_updates={
            "ao.agent": agent,
            "fallback_index": "0",
            "agent_chain": chain_str,
        }
    )

def _agent_watchdog(node: Node, ctx: Context) -> Result:
    """Checks if the running agent has been rate-limited or stuck."""
    session = ctx.state.get("ao.session")
    if not session:
        return Result(outcome="success", output="No active ao.session stashed")

    # 1. Find tmux session
    tmux_session = None
    try:
        r = subprocess.run(
            ["tmux", "list-sessions", "-F", "#{session_name}"],
            capture_output=True, text=True, timeout=5,
        )
        if r.returncode == 0:
            for line in r.stdout.splitlines():
                if line.endswith(f"-{session}") or line == session:
                    tmux_session = line
                    break
    except Exception as exc:
        print(f"[watchdog] tmux list failed: {exc}")

    # 2. Capture pane and check rate limits
    if tmux_session:
        try:
            r = subprocess.run(
                ["tmux", "capture-pane", "-t", tmux_session, "-p", "-S", "-150"],
                capture_output=True, text=True, timeout=5,
            )
            if r.returncode == 0:
                pane_text = r.stdout
                low = pane_text.lower()
                matched = [kw for kw in DEFAULT_RATE_LIMIT_KEYWORDS if kw in low]
                if matched:
                    return Result(
                        # Map stuck_rate_limited -> partial
                        outcome="partial",
                        output=f"Rate limit detected: {matched}",
                    )
        except Exception as exc:
            print(f"[watchdog] tmux capture failed: {exc}")

    # 3. Query AO API for session activity/status
    ao_port = int(node.attrs.get("ao_port", "3001"))
    url = f"http://127.0.0.1:{ao_port}/api/v1/sessions/{session}"
    try:
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=5) as resp:
            data = json.loads(resp.read())
            status = data.get("status") or data.get("activity") or ""
            if status in ("exited", "dead", "killed", "error"):
                # Map exhausted -> error
                return Result(outcome="error", output=f"AO session status={status}")
    except Exception as exc:
        print(f"[watchdog] AO API check failed: {exc}")

    # Map active -> success
    return Result(outcome="success", output="Agent active and healthy")


def _agent_swap(node: Node, ctx: Context) -> Result:
    """Kills current agent session and prepares spawning of the next agent."""
    session = ctx.state.get("ao.session")
    chain_str = ctx.state.get("agent_chain")
    idx_str = ctx.state.get("fallback_index", "0")

    if not chain_str:
        return Result(outcome="exhausted", output="No agent_chain found in state")

    chain = _parse_agent_chain(chain_str)
    next_idx = int(idx_str) + 1
    max_swaps = int(node.attrs.get("max_swaps", "2"))

    # Kill current session if active
    if session:
        ao_bin = os.path.expanduser(node.attrs.get("ao_bin", "~/bin/ao"))
        try:
            print(f"[agent_swap] killing session {session}")
            subprocess.run([ao_bin, "session", "kill", session], capture_output=True, timeout=30)
        except Exception as exc:
            print(f"[agent_swap] kill session failed: {exc}")

    # Clean up state so codergen node re-spawns
    ctx.state.pop("ao.session", None)
    ctx.state.pop("ao.worktree", None)

    if next_idx >= len(chain) or next_idx > max_swaps:
        return Result(outcome="exhausted", output=f"All {len(chain)} agents exhausted")

    agent, model = chain[next_idx]
    ctx.state["ao.agent"] = agent
    ctx.state["fallback_index"] = str(next_idx)

    # Set model in env
    if agent in ("antigravity", "agy") and model:
        os.environ["ANTIGRAVITY_MODEL"] = model
        os.environ.pop("MINIMAX_MODEL", None)
    elif agent == "minimax" and model:
        os.environ["MINIMAX_MODEL"] = model
        os.environ.pop("ANTIGRAVITY_MODEL", None)
    else:
        os.environ.pop("ANTIGRAVITY_MODEL", None)
        os.environ.pop("MINIMAX_MODEL", None)

    return Result(
        outcome="success",
        output=f"Swapped to agent={agent} model={model} idx={next_idx}",
        context_updates={
            "ao.agent": agent,
            "fallback_index": str(next_idx),
        }
    )
