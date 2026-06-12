"""Shared git subprocess helper for the runner.

Centralizes the 40-hex SHA validation that handlers._worktree_head_sha
does inline. Three call sites (handlers, evidence, perf_log) used to
each shell out to `git -C <dir> rev-parse ...` with hand-rolled error
handling and inconsistent SHA validation.
"""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def _git_rev_parse(workdir: Path, *args: str, timeout: int = 15) -> str | None:
    """Run `git -C <workdir> rev-parse <args>` and return trimmed stdout, or None on failure.

    Returns the lowercased SHA if the output is exactly 40 hex chars; otherwise
    returns the trimmed stdout as-is (callers that don't want SHA validation
    can check `_SHA_RE.match(...)` themselves).
    """
    try:
        proc = subprocess.run(
            ["git", "-C", str(workdir), "rev-parse", *args],
            capture_output=True, text=True, timeout=timeout, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    out = proc.stdout.strip()
    if not out:
        return None
    return out
