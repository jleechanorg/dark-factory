#!/usr/bin/env python3
"""
scripts/agent_swap.py — Agent swap for dark-factory fallback pipeline.

Kills the current AO session and spawns the next agent in the fallback chain.
Prints context_updates lines on stdout for the factory engine to consume:

    ao.session=wa-NNNN
    current_agent=antigravity
    current_model=gemini-2.5-flash
    fallback_index=2
    outcome=success          (or outcome=exhausted)

Usage:
    python3 scripts/agent_swap.py <ao_session> <agent_chain> <fallback_index> [options]

Arguments:
    ao_session      Current AO session to kill (e.g. wa-3111)
    agent_chain     Comma-separated "agent:model" pairs
                    e.g. "minimax:MiniMax-M3,antigravity:claude-sonnet-4-5,antigravity:gemini-2.5-flash"
    fallback_index  Current index in the chain (0-based integer)

Options:
    --max-swaps INT         Max total swaps allowed (default: 2)
    --ao-bin PATH           Path to ao CLI binary (default: ~/bin/ao)
    --project STR           AO project ID (e.g. worldarchitect)
    --pr INT                PR number to claim on the new session
    --goal STR              Task goal forwarded to the new session
    --cxdb PATH             Write a swap event row to CXDB SQLite
    --dry-run               Print what would happen without executing
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone


AO_BIN_DEFAULT = os.path.expanduser("~/bin/ao")


def parse_chain(chain_str: str) -> list[tuple[str, str]]:
    """Parse "agent:model,agent:model" into list of (agent, model) tuples."""
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


def ao_kill(ao_bin: str, session_id: str, dry_run: bool) -> bool:
    """Kill an AO session. Returns True on success."""
    cmd = [ao_bin, "session", "kill", session_id]
    print(f"[agent_swap] kill: {' '.join(cmd)}", file=sys.stderr)
    if dry_run:
        return True
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        if r.returncode != 0:
            print(f"[agent_swap] kill failed (rc={r.returncode}): {r.stderr.strip()}", file=sys.stderr)
        return r.returncode == 0
    except Exception as exc:
        print(f"[agent_swap] kill exception: {exc}", file=sys.stderr)
        return False


def ao_spawn(ao_bin: str, agent: str, model: str, project: str,
             pr: str, goal: str, dry_run: bool) -> str | None:
    """Spawn a new AO session. Returns session ID on success, None on failure."""
    cmd = [ao_bin, "spawn", "--project", project, "--agent", agent]
    if pr:
        cmd += ["--claim-pr", pr]
    if goal:
        cmd.append(goal)

    env = dict(os.environ)
    # Forward model as ANTIGRAVITY_MODEL / MINIMAX_MODEL based on agent
    if agent in ("antigravity", "agy") and model:
        env["ANTIGRAVITY_MODEL"] = model
    elif agent == "minimax" and model:
        env["MINIMAX_MODEL"] = model

    print(f"[agent_swap] spawn: {' '.join(cmd)}", file=sys.stderr)
    if dry_run:
        return "wa-DRYRUN"

    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=60, env=env)
        output = r.stdout + r.stderr
        # Parse session ID from "Session wa-NNNN created" or "SESSION=wa-NNNN"
        for pattern in [r"SESSION=(\S+)", r"Session\s+(wa-\d+)\s+created", r"\b(wa-\d+)\b"]:
            m = re.search(pattern, output)
            if m:
                return m.group(1)
        print(f"[agent_swap] spawn output (no session ID found):\n{output[:400]}", file=sys.stderr)
        return None
    except Exception as exc:
        print(f"[agent_swap] spawn exception: {exc}", file=sys.stderr)
        return None


def write_cxdb_event(cxdb_path: str, old_session: str, new_session: str | None,
                     agent: str, model: str, idx: int, outcome: str) -> None:
    try:
        import sqlite3
        con = sqlite3.connect(cxdb_path, timeout=5)
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("""CREATE TABLE IF NOT EXISTS agent_swap_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL, old_session TEXT, new_session TEXT,
            agent TEXT, model TEXT, fallback_index INTEGER, outcome TEXT)""")
        con.execute(
            "INSERT INTO agent_swap_events VALUES (NULL,?,?,?,?,?,?,?)",
            (datetime.now(timezone.utc).isoformat(), old_session, new_session,
             agent, model, idx, outcome),
        )
        con.commit()
        con.close()
    except Exception as exc:
        print(f"[agent_swap] cxdb write failed: {exc}", file=sys.stderr)


def main() -> None:
    parser = argparse.ArgumentParser(description="dark-factory agent swap")
    parser.add_argument("ao_session",     help="Current AO session ID to kill")
    parser.add_argument("agent_chain",    help='e.g. "minimax:MiniMax-M3,antigravity:claude-sonnet-4-5"')
    parser.add_argument("fallback_index", type=int, help="Current chain index (0-based)")
    parser.add_argument("--max-swaps",    type=int, default=2)
    parser.add_argument("--ao-bin",       default=AO_BIN_DEFAULT)
    parser.add_argument("--project",      default="worldarchitect")
    parser.add_argument("--pr",           default="")
    parser.add_argument("--goal",         default="")
    parser.add_argument("--cxdb",         default="")
    parser.add_argument("--dry-run",      action="store_true")
    args = parser.parse_args()

    chain = parse_chain(args.agent_chain)
    next_idx = args.fallback_index + 1

    if next_idx >= len(chain) or args.fallback_index >= args.max_swaps:
        print(f"[agent_swap] chain exhausted (next_idx={next_idx} len={len(chain)} max={args.max_swaps})",
              file=sys.stderr)
        ao_kill(args.ao_bin, args.ao_session, args.dry_run)
        if args.cxdb:
            write_cxdb_event(args.cxdb, args.ao_session, None, "", "", next_idx, "exhausted")
        print("outcome=exhausted")
        sys.exit(0)

    agent, model = chain[next_idx]
    print(f"[agent_swap] swapping {args.ao_session} (idx={args.fallback_index}) "
          f"→ {agent}:{model} (idx={next_idx})", file=sys.stderr)

    ao_kill(args.ao_bin, args.ao_session, args.dry_run)

    new_session = ao_spawn(
        ao_bin=args.ao_bin,
        agent=agent,
        model=model,
        project=args.project,
        pr=args.pr,
        goal=args.goal,
        dry_run=args.dry_run,
    )

    if not new_session:
        print("[agent_swap] spawn failed — treating as exhausted", file=sys.stderr)
        if args.cxdb:
            write_cxdb_event(args.cxdb, args.ao_session, None, agent, model, next_idx, "spawn_failed")
        print("outcome=exhausted")
        sys.exit(0)

    if args.cxdb:
        write_cxdb_event(args.cxdb, args.ao_session, new_session, agent, model, next_idx, "success")

    # Output context_updates for factory engine (one key=value per line)
    print(f"outcome=success")
    print(f"ao.session={new_session}")
    print(f"current_agent={agent}")
    print(f"current_model={model}")
    print(f"fallback_index={next_idx}")
    sys.exit(0)


if __name__ == "__main__":
    main()
