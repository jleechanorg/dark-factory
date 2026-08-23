#!/usr/bin/env python3
"""Runner outage check & pre-merge smoke step (Candidate A).

Surfaces GitHub Actions runner outages early before jobs queue indefinitely
in UNSTABLE state, alerting operators to wait or consider --admin merges.

Usage:
  scripts/check_runner_outage.py --repo jleechanorg/dark-factory
  scripts/check_runner_outage.py --repo jleechanorg/dark-factory --pr 123 --post-comment
  scripts/check_runner_outage.py --json
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys


EXIT_OK = 0
EXIT_INVOCATION = 2
EXIT_RUNNER_OUTAGE = 3

OUTAGE_MESSAGE = "RUNNER OUTAGE — consider --admin or wait"


def count_online_runners(payload: dict) -> int:
    """Return the number of online runners from a GitHub API runners response."""
    runners = payload.get("runners", [])
    return sum(1 for r in runners if r.get("status") == "online")


def fetch_runners(repo: str, *, gh_binary: str = "gh") -> dict:
    """Query GitHub API for runner inventory for the given repo or its org."""
    owner, _, _ = repo.partition("/")
    
    # Try repo-level runners first
    proc = subprocess.run(
        [gh_binary, "api", f"/repos/{repo}/actions/runners"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode == 0:
        try:
            payload = json.loads(proc.stdout)
            if payload.get("runners") or payload.get("total_count", 0) > 0:
                return payload
        except json.JSONDecodeError:
            pass

    # Fallback to org-level runners if repo-level has no runners or endpoint
    if owner:
        proc_org = subprocess.run(
            [gh_binary, "api", f"/orgs/{owner}/actions/runners?per_page=100"],
            capture_output=True,
            text=True,
            check=False,
        )
        if proc_org.returncode == 0:
            try:
                return json.loads(proc_org.stdout)
            except json.JSONDecodeError:
                pass

    if proc.returncode != 0:
        raise RuntimeError(f"gh api failed: {proc.stderr.strip()}")
    
    return json.loads(proc.stdout) if proc.stdout.strip() else {"runners": [], "total_count": 0}


def post_outage_comment(repo: str, pr_number: int, *, gh_binary: str = "gh") -> bool:
    """Post runner outage notice to the given PR."""
    body = f"⚠️ **[dark-factory /pre-merge]** {OUTAGE_MESSAGE}"
    proc = subprocess.run(
        [gh_binary, "pr", "comment", str(pr_number), "--repo", repo, "--body", body],
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.returncode == 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        default=os.environ.get("GITHUB_REPOSITORY", "jleechanorg/dark-factory"),
        help="GitHub repo (owner/repo) to check runners for (default: jleechanorg/dark-factory)",
    )
    parser.add_argument("--pr", type=int, help="PR number to annotate if outage detected")
    parser.add_argument(
        "--post-comment",
        action="store_true",
        help="Post a comment to the PR if runner outage is detected",
    )
    parser.add_argument("--json", action="store_true", help="Emit JSON output")
    args = parser.parse_args(argv)

    try:
        payload = fetch_runners(args.repo)
    except Exception as exc:
        msg = f"ERROR: Failed to fetch runner status: {exc}"
        if args.json:
            print(json.dumps({"status": "ERROR", "message": str(exc), "online_runners": 0}))
        else:
            print(msg, file=sys.stderr)
        return EXIT_INVOCATION

    online_count = count_online_runners(payload)

    if online_count == 0:
        if args.post_comment and args.pr:
            post_outage_comment(args.repo, args.pr)

        if args.json:
            print(json.dumps({
                "status": "RUNNER_OUTAGE",
                "message": OUTAGE_MESSAGE,
                "online_runners": 0,
                "repo": args.repo,
            }, indent=2))
        else:
            print(f"WARNING: {OUTAGE_MESSAGE}", file=sys.stderr)
            print(f"0 online runners found for {args.repo}.", file=sys.stderr)

        return EXIT_RUNNER_OUTAGE

    if args.json:
        print(json.dumps({
            "status": "ONLINE",
            "message": f"{online_count} online runners available",
            "online_runners": online_count,
            "repo": args.repo,
        }, indent=2))
    else:
        print(f"OK: {online_count} online runners available for {args.repo}.")

    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
