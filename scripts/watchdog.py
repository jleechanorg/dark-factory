#!/usr/bin/env python3
"""
scripts/watchdog.py — Agent Watchdog for dark-factory fallback pipeline.

Polls a running AO session (e.g. "wa-3111") and classifies its state.
Prints a single outcome line on stdout that drives DOT pipeline edges:

    outcome=active              agent is running fine
    outcome=stuck_normal        stuck but NOT rate-limited (logic/CI issue)
    outcome=stuck_rate_limited  hit a usage cap → swap to next agent
    outcome=exhausted           session/tmux pane is dead

Usage (as a dark-factory tool node):
    python3 scripts/watchdog.py <ao_session> [options]

Options:
    --ao-port INT               AO daemon port (default: 3001)
    --stuck-threshold INT       seconds before calling "stuck" (default: 600)
    --rate-limit-keywords STR   comma-separated extra phrases to match
    --cxdb PATH                 write a watchdog event row to CXDB SQLite
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import urllib.request
from datetime import datetime, timezone

DEFAULT_RATE_LIMIT_KEYWORDS: list[str] = [
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


def tmux_capture(tmux_session: str, lines: int = 150) -> str:
    try:
        r = subprocess.run(
            ["tmux", "capture-pane", "-t", tmux_session, "-p", "-S", f"-{lines}"],
            capture_output=True, text=True, timeout=10,
        )
        return r.stdout
    except Exception:
        return ""


def find_tmux_session(ao_session: str) -> str | None:
    try:
        r = subprocess.run(
            ["tmux", "list-sessions", "-F", "#{session_name}"],
            capture_output=True, text=True, timeout=10,
        )
        for line in r.stdout.splitlines():
            if line.endswith(f"-{ao_session}") or line == ao_session:
                return line
        return None
    except Exception:
        return None


def ao_api_session(ao_session: str, port: int) -> dict:
    url = f"http://127.0.0.1:{port}/api/v1/sessions/{ao_session}"
    try:
        req = urllib.request.Request(url, headers={"Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=8) as resp:
            return json.loads(resp.read())
    except Exception:
        return {}


def seconds_since(iso_ts: str | None) -> float | None:
    if not iso_ts:
        return None
    try:
        ts = datetime.fromisoformat(iso_ts.replace("Z", "+00:00"))
        return (datetime.now(timezone.utc) - ts).total_seconds()
    except Exception:
        return None


def contains_rate_limit(text: str, keywords: list[str]) -> bool:
    low = text.lower()
    return any(kw.lower() in low for kw in keywords)


def classify(ao_session: str, port: int, stuck_threshold: int, keywords: list[str]) -> tuple[str, str]:
    tmux_name = find_tmux_session(ao_session)
    if tmux_name is None:
        return "exhausted", f"tmux session not found for {ao_session}"

    pane_text = tmux_capture(tmux_name)

    if contains_rate_limit(pane_text, keywords):
        matched = [kw for kw in keywords if kw.lower() in pane_text.lower()]
        return "stuck_rate_limited", f"rate-limit keyword(s): {matched[:3]}"

    session_data = ao_api_session(ao_session, port)
    status = session_data.get("status") or session_data.get("activity") or ""
    last_act = session_data.get("lastActivityAt") or session_data.get("updatedAt")
    idle_sec = seconds_since(last_act)

    if status in ("exited", "dead", "killed", "error"):
        return "exhausted", f"AO session status={status}"

    if not pane_text.strip():
        return "exhausted", "tmux pane is empty"

    if idle_sec is not None and idle_sec > stuck_threshold:
        return "stuck_normal", f"idle {idle_sec:.0f}s > threshold {stuck_threshold}s"

    detail = f"status={status}"
    if idle_sec is not None:
        detail += f" idle={idle_sec:.0f}s"
    return "active", detail


def write_cxdb_event(cxdb_path: str, ao_session: str, outcome: str, detail: str) -> None:
    try:
        import sqlite3
        con = sqlite3.connect(cxdb_path, timeout=5)
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("""CREATE TABLE IF NOT EXISTS watchdog_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL, ao_session TEXT NOT NULL,
            outcome TEXT NOT NULL, detail TEXT)""")
        con.execute(
            "INSERT INTO watchdog_events (ts, ao_session, outcome, detail) VALUES (?,?,?,?)",
            (datetime.now(timezone.utc).isoformat(), ao_session, outcome, detail),
        )
        con.commit()
        con.close()
    except Exception as exc:
        print(f"[watchdog] cxdb write failed: {exc}", file=sys.stderr)


def main() -> None:
    parser = argparse.ArgumentParser(description="dark-factory agent watchdog")
    parser.add_argument("ao_session", help="AO session ID, e.g. wa-3111")
    parser.add_argument("--ao-port", type=int, default=3001)
    parser.add_argument("--stuck-threshold", type=int, default=600)
    parser.add_argument("--rate-limit-keywords", default="")
    parser.add_argument("--cxdb", default="")
    args = parser.parse_args()

    keywords = list(DEFAULT_RATE_LIMIT_KEYWORDS)
    if args.rate_limit_keywords:
        keywords.extend(k.strip() for k in args.rate_limit_keywords.split(",") if k.strip())

    outcome, detail = classify(
        ao_session=args.ao_session,
        port=args.ao_port,
        stuck_threshold=args.stuck_threshold,
        keywords=keywords,
    )

    if args.cxdb:
        write_cxdb_event(args.cxdb, args.ao_session, outcome, detail)

    print(f"outcome={outcome}")
    print(f"detail={detail}", file=sys.stderr)
    sys.exit(0)


if __name__ == "__main__":
    main()
