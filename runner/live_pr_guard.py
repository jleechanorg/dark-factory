"""dark-factory#828 item (d): refuse a live/open-PR target without an
explicit acknowledgement flag.

Real incident: a "review/verify only" run against a worktree whose branch
had a LIVE open PR (jleechanorg/worldarchitect.ai#9583) silently committed
and pushed to it, twice, from two different backends, in one session.
Nothing in the run's own preflight checked whether the target was a live,
externally-visible artifact before letting the pipeline touch it.

Detection is best-effort and FAILS OPEN on uncertainty (gh CLI missing,
not authenticated, network error, no PR exists for the current branch) —
the goal is to catch the confirmed case from the real incident, not to
require every throwaway run against a branch with no PR to have `gh`
configured. It only ever refuses on a POSITIVE, successfully-parsed
"this branch has an OPEN pull request" result.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
from typing import Optional


def detect_live_pr(workdir: pathlib.Path, timeout: int = 10) -> Optional[dict]:
    """Return {"number", "url", "state"} for workdir's current branch's PR
    iff it exists AND is OPEN. Returns None on any detection failure or
    ambiguity (fails open — see module docstring) or when the PR is not
    OPEN (closed/merged/draft-but-not-open branches are not "live").
    """
    if not workdir:
        return None
    try:
        proc = subprocess.run(
            ["gh", "pr", "view", "--json", "number,url,state"],
            cwd=str(workdir),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            stdin=subprocess.DEVNULL,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    try:
        data = json.loads(proc.stdout)
    except (ValueError, TypeError):
        return None
    if not isinstance(data, dict):
        return None
    if str(data.get("state", "")).strip().upper() != "OPEN":
        return None
    return data
