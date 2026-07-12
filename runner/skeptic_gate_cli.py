"""CLI for the SHA-bound skeptic gate (issue #278, mandatory redesign).

The orchestrator. Wires together trust-mode constraints, multi-reviewer
invocation, provenance checks, and bot-owned read-back verification.

Order of operations (each step is fail-closed over fail-open):

  1. Resolve authoritative API head SHA (`gh pr view`). Refuse if it
     does not equal the event/input SHA (defense against stale-dispatch
     attacks).
  2. Gather diff (`gh pr diff`). Refuse if it exceeds `MAX_DIFF_BYTES` —
     a partial review cannot satisfy the gate.
  3. Look up the commit author. Derive the `implementation_identity`.
  4. Build the prompt (PR context + diff + implementation_identity).
  5. Run each reviewer (default: codex AND gemini, both with sandbox-
     mode flags so they cannot execute code). Strip secrets from the
     env passed to the reviewer. Timeout-bounded.
  6. Verify provenance for each reviewer (the reviewer must declare a
     different identity from the implementer).
  7. Aggregate: ALL reviewers must PASS.
  8. Re-check the API head SHA before publish (defense against a new
     push mid-run).
  9. Post/upsert the bot comment + commit status.
 10. Read both back via `gh api`. Verify actor/bot identity, marker,
     SHA, repo, PR number, verdict. Fail closed if any disagree.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import asdict
from typing import List, Optional, Tuple

from runner.skeptic_gate import (
    MARKER,
    ReadBackCheck,
    SkepticResult,
    aggregate_results,
    build_prompt,
    evaluate,
    format_comment,
    parse_verdict,
    verify_published_comment,
    verify_provenance,
)


# ---------------------------------------------------------------------------
# Limits and defaults
# ---------------------------------------------------------------------------

# Hard upper bound on the diff we will hand to the reviewer. We do NOT
# silently truncate — a partial review cannot satisfy the gate.
MAX_DIFF_BYTES = 1024 * 1024  # 1 MiB

# Default reviewer list. Both must PASS.
DEFAULT_REVIEWERS_JSON = '[["codex", ""], ["gemini", "gemini-2.5-pro"]]'

# Expected actor on the freshly-published comment. The read-back step
# refuses anything else (defense against a reviewer-bound identity
# slipping a comment in via the bot).
EXPECTED_BOT_ACTOR = "github-actions[bot]"

# Env keys that MUST NOT leak into the reviewer subprocess. Even
# read-only model invocations shouldn't see GITHUB_TOKEN, GH_TOKEN,
# or any OPENCLAW_* secret.
REVIEWER_SECRET_ENV_DENY = {
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "OPENCLAW_GATEWAY_TOKEN",
    "OPENCLAW_URL",
    "OPENCLAW_SLACK_BOT_TOKEN",
    "OPENCLAW_SLACK_APP_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "HERMES_SLACK_WEBHOOK_URL",
    "HERMES_OPENCLAW_BOT_TOKEN",
    "HERMES_OPENCLAW_APP_TOKEN",
}

# Env keys that we DO pass through (read-only signal): reviewer CLIs
# need their own credentials + reasonable APIs to read files.
REVIEWER_ENV_ALLOWLIST = {
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "TERM",
    "PWD",
    "SHELL",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "NPM_CONFIG_PREFIX",
    "NPM_TOKEN",  # needed for codex; treat as reviewer-owned
    "OPENAI_API_KEY",  # codex uses OpenAI
    "GOOGLE_API_KEY",  # gemini uses Google
}


# ---------------------------------------------------------------------------
# Sanitized reviewer env
# ---------------------------------------------------------------------------


def _reviewer_env(parent_env: dict) -> dict:
    """Build a sanitized env dict for the reviewer subprocess.

    Pass-through is allowlist-based. Secrets (`REVIEWER_SECRET_ENV_DENY`)
    are dropped. Everything else from the parent is *forbidden* unless
    it's on the allowlist.
    """
    out: dict = {}
    for k, v in parent_env.items():
        if k in REVIEWER_SECRET_ENV_DENY:
            continue
        if k in REVIEWER_ENV_ALLOWLIST:
            out[k] = v
    return out


# ---------------------------------------------------------------------------
# Side-effect helpers (GitHub API via `gh` CLI)
# ---------------------------------------------------------------------------


def gh_api(method: str, path: str, *, body: Optional[dict] = None) -> dict:
    """Run `gh api <method> <path>` and return the parsed JSON."""
    cmd = ["gh", "api", "-X", method, path]
    if body is not None:
        cmd.extend(["--input", "-"])
    proc = subprocess.run(
        cmd,
        input=(json.dumps(body) if body is not None else None),
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"gh api {method} {path} failed (rc={proc.returncode}): "
            f"{proc.stderr.strip()[:500]}"
        )
    if not proc.stdout.strip():
        return {}
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"gh api {method} {path} returned non-JSON: {proc.stdout[:200]!r}"
        ) from exc


def find_existing_bot_comment(repo: str, pr_number: int) -> Optional[int]:
    """Return the comment ID of the prior skeptic-gate bot comment."""
    owner, name = repo.split("/", 1)
    page = 1
    while True:
        data = gh_api(
            "GET",
            f"repos/{owner}/{name}/issues/{pr_number}/comments?per_page=100&page={page}",
        )
        if not isinstance(data, list):
            break
        for c in data:
            body = c.get("body") or ""
            if MARKER in body:
                return int(c["id"])
        if len(data) < 100:
            break
        page += 1
    return None


def post_or_update_comment(repo: str, pr_number: int, body: str) -> int:
    """Create a new bot comment or update the existing one (idempotent)."""
    owner, name = repo.split("/", 1)
    existing = find_existing_bot_comment(repo, pr_number)
    if existing is not None:
        result = gh_api(
            "PATCH",
            f"repos/{owner}/{name}/issues/comments/{existing}",
            body={"body": body},
        )
        return int(result.get("id", existing))
    result = gh_api(
        "POST",
        f"repos/{owner}/{name}/issues/{pr_number}/comments",
        body={"body": body},
    )
    return int(result.get("id", 0))


def set_commit_status(
    repo: str, sha: str, *, state: str, context: str, description: str
) -> None:
    """Set a commit status on the PR head SHA."""
    owner, name = repo.split("/", 1)
    gh_api(
        "POST",
        f"repos/{owner}/{name}/statuses/{sha}",
        body={
            "state": state,
            "context": context,
            "description": description[:140],
        },
    )


def read_back_comment(repo: str, comment_id: int) -> Optional[dict]:
    """Read back a freshly-published comment. Returns the full comment
    object, or None if it cannot be fetched (which itself is a fail-
    closed signal — the deterministic side treats None as a guard rail
    breach)."""
    owner, name = repo.split("/", 1)
    try:
        return gh_api("GET", f"repos/{owner}/{name}/issues/comments/{comment_id}")
    except Exception:
        return None


# ---------------------------------------------------------------------------
# PR context gathering (with API head SHA equality check)
# ---------------------------------------------------------------------------


def get_pr_head_sha_via_api(repo: str, pr_number: int) -> str:
    """Authoritative head SHA via the GitHub API.

    This is the source of truth we compare the event/input SHA against.
    If the two disagree, the dispatch is stale — fail closed.
    """
    owner, name = repo.split("/", 1)
    info = gh_api("GET", f"repos/{owner}/{name}/pulls/{pr_number}")
    return str(info["head"]["sha"])


def get_pr_diff(repo: str, pr_number: int) -> str:
    """Diff text via `gh pr diff`. Fail-closed if too large."""
    proc = subprocess.run(
        ["gh", "pr", "diff", str(pr_number)],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"gh pr diff {pr_number} failed: {proc.stderr.strip()[:500]}"
        )
    diff = proc.stdout
    diff_bytes = len(diff.encode("utf-8"))
    if diff_bytes > MAX_DIFF_BYTES:
        raise RuntimeError(
            f"diff is too large: {diff_bytes} bytes > MAX_DIFF_BYTES "
            f"({MAX_DIFF_BYTES}); the gate cannot pass on a partial "
            f"review. Split the PR or raise MAX_DIFF_BYTES explicitly."
        )
    return diff


def get_commit_author_identity(repo: str, pr_number: int) -> str:
    """Look up the PR's commit author and reduce it to a model identity.

    The mapping is intentionally generous: anything that looks like a
    bot identity returns `unknown` (because we cannot prove the
    implementer was Claude and therefore cannot prove a reviewer is
    independent of it).
    """
    owner, name = repo.split("/", 1)
    try:
        data = gh_api(
            "GET",
            f"repos/{owner}/{name}/pulls/{pr_number}/commits?per_page=1",
        )
    except Exception:
        return "unknown"
    if not isinstance(data, list) or not data:
        return "unknown"
    author = (data[0].get("author") or {}).get("login") or ""
    email = (
        (data[0].get("commit", {}).get("author", {}) or {}).get("email", "") or ""
    )
    blob = f"{author.lower()} {email.lower()}"
    if "claude" in blob:
        return "claude"
    if "codex" in blob or "openai" in blob:
        return "codex"
    if "gemini" in blob or "google" in blob:
        return "gemini"
    return "unknown"


# ---------------------------------------------------------------------------
# Reviewer invocation
# ---------------------------------------------------------------------------


def _build_reviewer_cmd(reviewer: str, model: str) -> list[str]:
    """Sandbox-mode argv for a reviewer CLI.

    The reviewer reads diff text and emits a verdict. It must NOT be
    allowed to execute code, write files, or escalate privileges:
    - codex: `--sandbox=read-only` (no tool execution), no
      `--dangerously-bypass-approvals-and-sandbox`.
    - gemini: `-s` (sandbox) and `--approval-mode=default` (no `yolo`).
    """
    if reviewer == "codex":
        cmd = [
            "codex",
            "exec",
            "--sandbox",
            "read-only",
            "--ephemeral",
            "--skip-git-repo-check",
            "--json",
        ]
        if model:
            cmd.extend(["-m", model])
        cmd.append("-")
        return cmd
    if reviewer == "gemini":
        return [
            "gemini",
            "-m",
            model,
            "-s",
            "--approval-mode",
            "default",
            "-p",
            "__PROMPT_PLACEHOLDER__",
        ]
    raise RuntimeError(
        f"unknown reviewer {reviewer!r}; expected 'codex' or 'gemini'"
    )


def _extract_codex_message(stdout: str) -> str:
    """Pull the structured agent_message text out of codex `--json` JSONL.

    codex emits one JSON object per line. We take the LAST
    `agent_message` event. If none exists, return empty string so
    parse_verdict returns None and the gate fails closed.
    """
    last_text = ""
    for line in stdout.splitlines():
        line = line.strip()
        if not line or not line.startswith("{"):
            continue
        try:
            event = json.loads(line)
        except Exception:
            continue
        item = event.get("item") or {}
        if (
            event.get("type") == "item.completed"
            and item.get("type") == "agent_message"
            and isinstance(item.get("text"), str)
        ):
            last_text = item["text"]
    return last_text


def invoke_reviewer(
    reviewer: str,
    model: str,
    prompt: str,
    *,
    parent_env: Optional[dict] = None,
    timeout: int = 900,
) -> Tuple[Optional[str], Optional[str]]:
    """Run the reviewer CLI; return (stdout, error_message).

    Reviewers run with a sanitized env (no GITHUB_TOKEN, no Slack/OpenClaw
    secrets). Their process is sandboxed (`--sandbox=read-only` for
    codex, `-s` for gemini). stdout is destructured to the agent's text
    for codex's `--json` mode.
    """
    cmd = _build_reviewer_cmd(reviewer, model)
    if "__PROMPT_PLACEHOLDER__" in cmd:
        idx = cmd.index("__PROMPT_PLACEHOLDER__")
        cmd[idx] = prompt
        stdin_input = None
    else:
        stdin_input = prompt

    env = _reviewer_env(parent_env if parent_env is not None else os.environ)

    try:
        proc = subprocess.run(
            cmd,
            input=stdin_input,
            capture_output=True,
            text=True,
            env=env,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError as exc:
        return None, f"reviewer binary not found: {exc}"
    except subprocess.TimeoutExpired:
        return None, f"reviewer timed out after {timeout}s"
    if proc.returncode != 0:
        return proc.stdout, (
            f"reviewer rc={proc.returncode}: {proc.stderr.strip()[:300]}"
        )
    if reviewer == "codex":
        return _extract_codex_message(proc.stdout), None
    return proc.stdout, None


# ---------------------------------------------------------------------------
# CLI argument parsing
# ---------------------------------------------------------------------------


def _parse_reviewers(reviewers_json: str) -> List[Tuple[str, str]]:
    """Parse the `--reviewers-json` argument into [(reviewer, model), ...]."""
    try:
        parsed = json.loads(reviewers_json)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"--reviewers-json is not valid JSON: {exc}") from exc
    if not isinstance(parsed, list) or not parsed:
        raise SystemExit(
            "--reviewers-json must be a non-empty list of "
            "[reviewer, model] pairs"
        )
    out: List[Tuple[str, str]] = []
    for item in parsed:
        if (
            not isinstance(item, list)
            or len(item) != 2
            or not all(isinstance(x, str) for x in item)
        ):
            raise SystemExit(
                f"invalid reviewer entry: {item!r}; expected [reviewer, model]"
            )
        if item[0] not in ("codex", "gemini"):
            raise SystemExit(
                f"reviewer {item[0]!r} not allowed; expected 'codex' or 'gemini'"
            )
        out.append((item[0], item[1]))
    return out


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="skeptic-gate",
        description="SHA-bound skeptic gate for the dark-factory 7-green policy",
    )
    parser.add_argument(
        "--repo", default="", help="owner/name; default $GITHUB_REPOSITORY"
    )
    parser.add_argument("--pr-number", type=int, default=0, help="PR number")
    parser.add_argument(
        "--pr-sha",
        default="",
        help="PR head SHA override (must match API; default: re-resolve)",
    )
    parser.add_argument(
        "--reviewers-json",
        default=os.environ.get("SKEPTIC_REVIEWERS_JSON", DEFAULT_REVIEWERS_JSON),
        help=(
            "JSON list of [reviewer, model] pairs. ALL must PASS. "
            "Default: " + DEFAULT_REVIEWERS_JSON
        ),
    )
    parser.add_argument(
        "--status-context",
        default=os.environ.get("SKEPTIC_STATUS_CONTEXT", "skeptic"),
        help="commit-status context name (default: 'skeptic')",
    )
    parser.add_argument(
        "--expected-actor",
        default=os.environ.get("SKEPTIC_EXPECTED_ACTOR", EXPECTED_BOT_ACTOR),
        help="actor the read-back step expects on the published comment",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="do not post comments or set status; print what would happen",
    )
    args = parser.parse_args(argv)
    env = os.environ

    reviewers = _parse_reviewers(args.reviewers_json)

    # ---- 1. Resolve PR + SHA, with API equality check ------------------------
    repo = args.repo or env.get("GITHUB_REPOSITORY")
    if not repo:
        raise SystemExit("GITHUB_REPOSITORY (or --repo) is required")
    if not args.pr_number:
        raise SystemExit("pr_number is required")

    api_head = get_pr_head_sha_via_api(repo, args.pr_number)
    event_sha = args.pr_sha or env.get("PR_HEAD_SHA", "")
    if event_sha and event_sha.lower() != api_head.lower():
        print(
            f"[skeptic-gate] SHA mismatch: event/input={event_sha[:12]} "
            f"api={api_head[:12]}; refusing to gate an outdated head",
            file=sys.stderr,
        )
        return 2
    head_sha = api_head  # authoritative

    # ---- 2. Gather diff (fail-closed on oversize) -----------------------------
    try:
        diff = get_pr_diff(repo, args.pr_number)
    except Exception as exc:
        print(f"[skeptic-gate] diff capture failed: {exc}", file=sys.stderr)
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=args.pr_number,
            reviewer="(none)",
            reason=f"diff capture failed: {exc}",
        )
        if not args.dry_run:
            _publish_failure(
                repo, head_sha, body, args.status_context,
                f"diff capture failed: {str(exc)[:80]}",
            )
        return 1

    # ---- 3. Implementation identity (commit author) ---------------------------
    implementation_identity = get_commit_author_identity(repo, args.pr_number)

    # ---- 4. Build prompt ----------------------------------------------------
    prompt = build_prompt(
        repo=repo,
        pr_number=args.pr_number,
        head_sha=head_sha,
        base_sha="unknown",
        diff=diff,
        implementation_identity=implementation_identity,
    )

    print(
        f"[skeptic-gate] repo={repo} pr=#{args.pr_number} head={head_sha[:12]} "
        f"implementer={implementation_identity} "
        f"reviewers={[r for r, _ in reviewers]}",
        file=sys.stderr,
    )

    # ---- 5. Run each reviewer (multi-reviewer independence) -----------------
    per_reviewer: List[SkepticResult] = []
    for reviewer_name, model in reviewers:
        if model == "" and reviewer_name == "gemini":
            model = "gemini-2.5-pro"
        print(
            f"[skeptic-gate] invoking reviewer={reviewer_name} "
            f"model={model or '<account-default>'}",
            file=sys.stderr,
        )
        review_output, review_error = invoke_reviewer(
            reviewer_name, model, prompt, parent_env=dict(env)
        )
        result = evaluate(
            review_output=review_output,
            review_error=review_error,
            repo=repo,
            pr_number=args.pr_number,
            head_sha=head_sha,
            base_sha="unknown",
            diff=diff,
            reviewer=reviewer_name,
        )
        per_reviewer.append(result)
        print(
            f"[skeptic-gate] reviewer={reviewer_name} "
            f"verdict={result.verdict} state={result.check_state} "
            f"reason={result.reason[:200]}",
            file=sys.stderr,
        )

    # ---- 6. Provenance check (per reviewer) --------------------------------
    proven = []
    for r in per_reviewer:
        if r.parsed is None:
            proven.append((False, f"{r.reviewer} did not produce a parseable verdict", r))
            continue
        ok, why = verify_provenance(
            implementation_identity, r.parsed.reviewer_identity
        )
        proven.append((ok, why, r))

    # Force any non-independent reviewer into FAIL even if its verdict
    # was PASS — the audit trail preserves the verdict text but the gate
    # state is FAIL.
    for (ok, why, r) in proven:
        if not ok:
            r2 = SkepticResult(
                check_state="failure",
                verdict=None,
                reason=why,
                comment_body=format_comment(
                    verdict="FAIL",
                    head_sha=head_sha,
                    expected_head_sha=head_sha,
                    repo=repo,
                    pr_number=args.pr_number,
                    reviewer=r.reviewer,
                    reason=why,
                ),
                parsed=r.parsed,
                reviewer=r.reviewer,
            )
            # replace in per_reviewer
            idx = per_reviewer.index(r)
            per_reviewer[idx] = r2

    # ---- 7. Aggregate: ALL reviewers must PASS -----------------------------
    aggregate = aggregate_results(
        per_reviewer,
        repo=repo,
        pr_number=args.pr_number,
        head_sha=head_sha,
    )

    # ---- 8. Pre-publish API head re-check ---------------------------------
    if not args.dry_run:
        api_head_2 = get_pr_head_sha_via_api(repo, args.pr_number)
        if api_head_2.lower() != head_sha.lower():
            print(
                f"[skeptic-gate] HEAD SHA changed mid-run: "
                f"{head_sha[:12]} -> {api_head_2[:12]}; abandoning publish",
                file=sys.stderr,
            )
            return 2

    print(
        f"[skeptic-gate] AGGREGATE verdict={aggregate.verdict} "
        f"state={aggregate.check_state} "
        f"reason={aggregate.reason[:200]}",
        file=sys.stderr,
    )

    # ---- 9. Side effects: comment + status ---------------------------------
    if args.dry_run:
        return 0 if aggregate.check_state == "success" else 1

    try:
        comment_id = post_or_update_comment(
            repo, args.pr_number, aggregate.comment_body
        )
    except Exception as exc:
        print(f"[skeptic-gate] comment upsert failed: {exc}", file=sys.stderr)
        return 1
    try:
        set_commit_status(
            repo,
            head_sha,
            state=aggregate.check_state,
            context=args.status_context,
            description=aggregate.reason,
        )
    except Exception as exc:
        print(f"[skeptic-gate] status set failed: {exc}", file=sys.stderr)

    # ---- 10. Read back: verify what we just published ----------------------
    published = read_back_comment(repo, comment_id)
    if published is None:
        print(
            "[skeptic-gate] read-back failed: could not fetch the "
            "freshly-published comment",
            file=sys.stderr,
        )
        return 1

    actor = (published.get("user") or {}).get("login") or ""
    body = published.get("body") or ""

    rb = ReadBackCheck(
        actor=actor,
        body_contains_marker=MARKER in body,
        body_sha=_extract_field(body, "HEAD_SHA"),
        body_repo=_extract_field(body, "REPO"),
        body_pr_number=_extract_int(body, "PR_NUMBER"),
        body_verdict=_extract_field(body, "VERDICT"),
    )

    ok, why = verify_published_comment(rb, expected_actor=args.expected_actor)
    if not ok:
        print(f"[skeptic-gate] read-back mismatch: {why}", file=sys.stderr)
        return 1

    # The commit status was set to (state, description). Re-fetch and
    # confirm the API agrees.
    statuses = gh_api(
        "GET",
        f"repos/{repo.split('/', 1)[0]}/{repo.split('/', 1)[1]}"
        f"/commits/{head_sha}/statuses",
    )
    statuses = statuses if isinstance(statuses, list) else []
    matched = [
        s for s in statuses
        if s.get("context") == args.status_context
    ]
    if not matched:
        print(
            f"[skeptic-gate] read-back mismatch: no status with "
            f"context={args.status_context!r} found on {head_sha[:12]}",
            file=sys.stderr,
        )
        return 1
    found_state = matched[0].get("state")
    expected_state = aggregate.check_state
    if found_state != expected_state:
        print(
            f"[skeptic-gate] read-back mismatch: status state is "
            f"{found_state!r}, expected {expected_state!r}",
            file=sys.stderr,
        )
        return 1

    print(
        f"[skeptic-gate] read-back OK: actor={actor} comment_id={comment_id} "
        f"status_state={found_state}",
        file=sys.stderr,
    )
    return 0 if aggregate.check_state == "success" else 1


def _publish_failure(
    repo: str, head_sha: str, body: str, context: str, description: str
) -> None:
    """Best-effort publish of a failure (used for diff-capture / pre-flight
    failures). Always swallows secondary errors so the failure path itself
    is observable."""
    try:
        set_commit_status(repo, head_sha, state="failure", context=context,
                           description=description)
    except Exception as exc:
        print(f"[skeptic-gate] set_commit_status failed: {exc}", file=sys.stderr)
    try:
        post_or_update_comment(repo, _pr_number_for_desc(description), body)
    except Exception:
        pass


def _pr_number_for_desc(description: str) -> int:
    """Best-effort parse of PR number from a description string.

    Used only to publish failure observations — never the source of
    truth (the real PR number comes from the API call)."""
    m = re.search(r"PR #(\d+)", description)
    if m:
        return int(m.group(1))
    return 0


# ---------------------------------------------------------------------------
# Body field extractors for the read-back verifier
# ---------------------------------------------------------------------------


_RE_SHA_LINE = re.compile(r"HEAD_SHA:\s*([0-9a-f]+)", re.IGNORECASE)
_RE_REPO_LINE = re.compile(r"REPO:\s*([\w.\-]+/[\w.\-]+)", re.IGNORECASE)
_RE_PR_LINE = re.compile(r"PR_NUMBER:\s*(\d+)", re.IGNORECASE)
_RE_VERDICT_LINE = re.compile(r"VERDICT:\s*(PASS|FAIL)", re.IGNORECASE)


def _extract_field(body: str, name: str) -> Optional[str]:
    pat = {
        "HEAD_SHA": _RE_SHA_LINE,
        "REPO": _RE_REPO_LINE,
        "VERDICT": _RE_VERDICT_LINE,
    }.get(name)
    if pat is None:
        return None
    m = pat.search(body)
    return m.group(1) if m else None


def _extract_int(body: str, name: str) -> Optional[int]:
    s = _extract_field(body, name)
    return int(s) if s and s.isdigit() else None


if __name__ == "__main__":
    sys.exit(main())
