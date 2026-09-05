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

Two detection paths, tried in order:
1. `gh pr view` — cheap, works when the local branch name is checked out
   normally and matches the PR's head branch (the common case).
2. A HEAD-SHA fallback via `gh pr list` — `gh pr view`'s branch-name
   lookup silently fails (git error, gh treats it as "no PR") on a
   detached-HEAD checkout or a locally-renamed branch, both routine in
   this repo's own `/tmp/df-*`-style disposable-worktree conventions.
   Found by an independent adversarial review of PR #828's fix (this
   guard) before merge: those setups made the guard fail open in cases
   that are NOT rare/adversarial, just this repo's normal review workflow.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
from typing import Optional


def _run_gh(args: list[str], workdir: pathlib.Path, timeout: int) -> Optional[str]:
    try:
        proc = subprocess.run(
            args,
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
    return proc.stdout


def _open_pr_from_json(stdout: Optional[str]) -> Optional[dict]:
    if stdout is None:
        return None
    try:
        data = json.loads(stdout)
    except (ValueError, TypeError):
        return None
    if not isinstance(data, dict):
        return None
    if str(data.get("state", "")).strip().upper() != "OPEN":
        return None
    return data


def _head_sha(workdir: pathlib.Path, timeout: int) -> Optional[str]:
    try:
        proc = subprocess.run(
            ["git", "rev-parse", "HEAD"],
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
    return proc.stdout.strip() or None


def _detect_live_pr_by_head_sha(workdir: pathlib.Path, timeout: int) -> Optional[dict]:
    """Fallback for detached HEAD / locally-renamed branches: `gh pr
    view`'s branch-name lookup has nothing to match there, but the PR's
    head commit SHA is independent of any local branch name."""
    sha = _head_sha(workdir, timeout)
    if not sha:
        return None
    stdout = _run_gh(
        ["gh", "pr", "list", "--state", "open", "--json", "number,url,state,headRefOid"],
        workdir,
        timeout,
    )
    if stdout is None:
        return None
    try:
        candidates = json.loads(stdout)
    except (ValueError, TypeError):
        return None
    if not isinstance(candidates, list):
        return None
    for pr in candidates:
        if not isinstance(pr, dict):
            continue
        if str(pr.get("headRefOid", "")).strip().lower() == sha.lower():
            if str(pr.get("state", "")).strip().upper() == "OPEN":
                return {k: v for k, v in pr.items() if k in ("number", "url", "state")}
    return None


def detect_live_pr(workdir: pathlib.Path, timeout: int = 10) -> Optional[dict]:
    """Return {"number", "url", "state"} for workdir's current branch's PR
    iff it exists AND is OPEN. Returns None on any detection failure or
    ambiguity (fails open — see module docstring) or when the PR is not
    OPEN (closed/merged/draft-but-not-open branches are not "live").
    """
    if not workdir:
        return None
    stdout = _run_gh(
        ["gh", "pr", "view", "--json", "number,url,state"], workdir, timeout
    )
    result = _open_pr_from_json(stdout)
    if result is not None:
        return result
    return _detect_live_pr_by_head_sha(workdir, timeout)
