"""Agent Orchestrator session polling helpers.

Owns:
  * `_ao_parse_status` — parse ``ao status --json`` output, return activity.
  * `_ao_wait_idle` — poll ``ao status --json`` until N consecutive idle reads
    (default 3), tolerating retry-loop "ready" bounces.

The polling loop calls ``_sanitized_env`` via the shim namespace so tests that
``monkeypatch.setattr("runner.handlers._sanitized_env", ...)`` see the
patched version.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import time
from typing import Optional

import runner.handlers as _handlers_shim


def _ao_parse_status(stdout: str, session: str) -> str:
    """Pull a session's `activity` from `ao status --json` output.

    `ao status` prepends notifier noise lines before the JSON array; strip
    everything before the first `[`.
    """
    idx = stdout.find("[")
    if idx < 0:
        return "unknown"
    try:
        data = json.loads(stdout[idx:])
    except json.JSONDecodeError:
        return "unknown"
    for entry in data:
        if entry.get("name") == session:
            return str(entry.get("activity", "unknown"))
    return "missing"


def _ao_wait_idle(
    session: str,
    workdir: pathlib.Path,
    timeout: int = 900,
    stable_reads: int = 3,
    poll_interval: int = 10,
    project: Optional[str] = None,
) -> str:
    """Poll `ao status --json` until the session is idle for `stable_reads`
    consecutive polls.

    During retry loops inside the agent (e.g. claude rate-limit backoff), a
    session can momentarily report "ready" between retry attempts before
    bouncing back to "active". Requiring N consecutive idle reads makes the
    wait robust against that.

    `project` filters the status query (`ao status -p <project> --json`),
    which is much faster than the unfiltered call when the fleet has many
    sessions.

    The inner ``ao status`` probe has its own 180 s subprocess timeout;
    a hung ``ao status`` must NOT crash the waiter — it is treated as a
    transient probe failure and the loop retries until the outer
    deadline elapses, then returns ``"timeout"``.

    Returns the last observed terminal activity ("exited", "ready",
    "missing"), or "timeout" if the deadline elapsed before idle stabilised.
    """
    deadline = time.monotonic() + timeout
    consecutive = 0
    status_cmd = ["ao", "status", "--json"]
    if project:
        status_cmd = ["ao", "status", "-p", project, "--json"]
    while time.monotonic() < deadline:
        try:
            proc = subprocess.run(
                status_cmd,
                cwd=workdir,
                capture_output=True,
                text=True,
                timeout=180,
                check=False,
                env=_handlers_shim._sanitized_env(),
            )
        except subprocess.TimeoutExpired:
            # The status probe itself hung — treat as transient and retry.
            time.sleep(poll_interval)
            continue
        if proc.returncode == 0:
            activity = _ao_parse_status(proc.stdout, session)
            if activity in ("exited", "missing"):
                return activity
            if activity == "ready":
                consecutive += 1
                last_terminal = "ready"
                if consecutive >= stable_reads:
                    return "ready"
            else:
                consecutive = 0
        time.sleep(poll_interval)
    return "timeout"
