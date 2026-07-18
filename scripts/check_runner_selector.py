#!/usr/bin/env python3
"""Drift check for self-hosted runner labels (bead jleechan-z284, issue #286).

Compares the configured SELF_HOSTED_RUNNER_LABELS conjunction (a JSON array
of label names that ALL must be present on a runner) against the live
GitHub Actions runner inventory for the target organization.

Exit codes:
  0 — at least ``--min-matches`` runners satisfy the selector
  1 — selector matches fewer runners than ``--min-matches`` (drift detected)
  2 — invalid invocation (bad args, malformed selector, gh/auth failure)
  3 — no runners online at all (fleet-wide outage, distinct from drift)

Usage:
  check_runner_selector.py --org jleechanorg
  check_runner_selector.py --selector '["self-hosted","ezgha"]' --min-matches 1
  check_runner_selector.py --json   # machine-readable output

The check is intentionally separate from the GitHub-hosted vs self-hosted
debate: it only asks "does the selector match anything real?" — failing loud
when zero runners qualify so a future label mistake queues jobs forever.

The drift check is read-only: it never mutates repo state, only observes.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from typing import Iterable


# Exit codes — keep stable; CI workflows depend on them.
EXIT_OK = 0
EXIT_DRIFT = 1          # selector matches fewer than --min-matches runners
EXIT_INVOCATION = 2     # bad args / malformed selector / gh failed
EXIT_FLEET_DOWN = 3     # no online runners at all (distinct from drift)


def _normalize_labels(raw: object) -> list[str]:
    """Coerce a parsed-JSON selector value into a stable list[str].

    Accepts the exact shapes we expect from the GitHub repo-variable API:
      - JSON array of strings:        ["self-hosted","ezgha"]
      - JSON string of an array:      '["self-hosted","ezgha"]'
      - JSON string of bare labels:   "self-hosted,ezgha" (defensive)
    """
    if isinstance(raw, list):
        labels = [str(x).strip() for x in raw]
    elif isinstance(raw, str):
        text = raw.strip()
        # JSON array string?
        if text.startswith("["):
            parsed = json.loads(text)
            if not isinstance(parsed, list):
                raise ValueError("selector JSON must be an array of strings")
            labels = [str(x).strip() for x in parsed]
        else:
            labels = [tok.strip() for tok in text.split(",") if tok.strip()]
    else:
        raise ValueError(f"selector must be a JSON array of strings, got {type(raw).__name__}")
    return [label for label in labels if label]


def load_selector(arg_value: str | None) -> list[str]:
    """Resolve the selector from CLI > env > repo variable fallback."""
    if arg_value is not None:
        return _normalize_labels(json.loads(arg_value))
    env_value = os.environ.get("SELF_HOSTED_RUNNER_LABELS")
    if env_value:
        return _normalize_labels(env_value)
    # Fall back to the GitHub repo variable so the script works in CI without
    # explicit args. We deliberately don't fail loudly if gh is unavailable
    # here — callers can pass --selector explicitly.
    repo = os.environ.get("GITHUB_REPOSITORY") or ""
    if not repo:
        raise ValueError(
            "no --selector, no SELF_HOSTED_RUNNER_LABELS env var, "
            "and no GITHUB_REPOSITORY to read the repo variable from"
        )
    owner, _, name = repo.partition("/")
    if not owner or not name:
        raise ValueError(f"GITHUB_REPOSITORY {repo!r} is not owner/name")
    proc = subprocess.run(
        ["gh", "api", f"repos/{owner}/{name}/actions/variables/SELF_HOSTED_RUNNER_LABELS"],
        check=False, capture_output=True, text=True,
    )
    if proc.returncode != 0:
        raise ValueError(
            f"gh api failed (rc={proc.returncode}): {proc.stderr.strip()}"
        )
    payload = json.loads(proc.stdout)
    return _normalize_labels(payload.get("value", ""))


def fetch_org_runners(org: str, *, gh_binary: str = "gh") -> list[dict]:
    """Return the live runner inventory for ``org`` as a list of dicts.

    Each dict carries at minimum ``name``, ``status`` (``online``/``offline``),
    ``busy`` (bool), and ``labels`` (list[str]). Raises on gh failure so the
    caller can map it to a distinct exit code.
    """
    proc = subprocess.run(
        [gh_binary, "api", f"orgs/{org}/actions/runners?per_page=100"],
        check=False, capture_output=True, text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"gh api failed (rc={proc.returncode}): {proc.stderr.strip()}"
        )
    payload = json.loads(proc.stdout)
    runners = payload.get("runners", [])
    for runner in runners:
        runner["labels"] = [label["name"] for label in runner.get("labels", [])]
    return runners


def select_matching(
    runners: Iterable[dict], selector: Iterable[str],
) -> list[dict]:
    """Return runners that carry every label in ``selector`` (conjunction)."""
    required = set(selector)
    return [
        runner for runner in runners
        if required.issubset(set(runner.get("labels", [])))
    ]


def render_human(
    *,
    org: str, selector: list[str], online: list[dict],
    matches: list[dict], min_matches: int,
) -> str:
    online_names = [r["name"] for r in online]
    match_names = [r["name"] for r in matches]
    lines = [
        f"Org:                 {org}",
        f"Selector (AND):      {selector}",
        f"Online runners:      {len(online)} ({', '.join(online_names) or 'none'})",
        f"Matching runners:    {len(matches)} ({', '.join(match_names) or 'none'})",
        f"Required minimum:    {min_matches}",
    ]
    verdict = "PASS" if len(matches) >= min_matches else "DRIFT"
    lines.append(f"Verdict:             {verdict}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--org", default=os.environ.get("DRIFT_CHECK_ORG", "jleechanorg"),
        help="GitHub org whose runners to inspect (default: jleechanorg)",
    )
    parser.add_argument(
        "--selector",
        help=(
            "JSON-encoded label conjunction (overrides env/repo variable). "
            "Example: '[\"self-hosted\",\"ezgha\"]'"
        ),
    )
    parser.add_argument(
        "--min-matches", type=int, default=1,
        help="Minimum runners that must satisfy the selector (default: 1)",
    )
    parser.add_argument(
        "--json", action="store_true", help="Emit machine-readable JSON instead of text",
    )
    parser.add_argument(
        "--include-offline", action="store_true",
        help="Count offline runners too (default: only online runners count)",
    )
    args = parser.parse_args(argv)

    try:
        selector = load_selector(args.selector)
    except (ValueError, json.JSONDecodeError) as exc:
        msg = f"ERROR: invalid selector: {exc}"
        if args.json:
            print(json.dumps({"verdict": "ERROR", "reason": str(exc)}))
        else:
            print(msg, file=sys.stderr)
        return EXIT_INVOCATION

    if not selector:
        msg = "ERROR: selector resolved to an empty list"
        if args.json:
            print(json.dumps({"verdict": "ERROR", "reason": msg}))
        else:
            print(msg, file=sys.stderr)
        return EXIT_INVOCATION

    try:
        runners = fetch_org_runners(args.org)
    except (RuntimeError, json.JSONDecodeError) as exc:
        msg = f"ERROR: failed to fetch runners: {exc}"
        if args.json:
            print(json.dumps({"verdict": "ERROR", "reason": msg}))
        else:
            print(msg, file=sys.stderr)
        return EXIT_INVOCATION

    online = [r for r in runners if r.get("status") == "online"]
    pool = runners if args.include_offline else online

    # FLEET_DOWN only applies when the operator asked about online runners.
    # --include-offline is an explicit "look past liveness" signal — the
    # operator wants to know whether the selector would match *if* a runner
    # were up, so FLEET_DOWN would be a misleading false alarm there.
    if not online and not args.include_offline:
        msg = "FLEET_DOWN: no runners are online"
        if args.json:
            print(json.dumps({
                "verdict": "FLEET_DOWN",
                "org": args.org,
                "selector": selector,
                "online_count": 0,
                "matches": [],
            }))
        else:
            print(msg, file=sys.stderr)
        return EXIT_FLEET_DOWN

    matches = select_matching(pool, selector)

    if args.json:
        print(json.dumps({
            "verdict": "PASS" if len(matches) >= args.min_matches else "DRIFT",
            "org": args.org,
            "selector": selector,
            "online_count": len(online),
            "match_count": len(matches),
            "min_matches": args.min_matches,
            "matches": [
                {"name": r["name"], "labels": r.get("labels", [])} for r in matches
            ],
        }, indent=2))
    else:
        print(render_human(
            org=args.org, selector=selector, online=online,
            matches=matches, min_matches=args.min_matches,
        ))

    return EXIT_OK if len(matches) >= args.min_matches else EXIT_DRIFT


if __name__ == "__main__":
    sys.exit(main())