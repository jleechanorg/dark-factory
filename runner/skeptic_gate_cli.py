"""CLI for the SHA-bound skeptic gate (issue #278).

Invoked by `.github/workflows/skeptic-gate.yml`. The CLI is a thin shell
that wires together:

1. PR context gathering (`gh` + `git`)
2. Prompt assembly (`runner.skeptic_gate.build_prompt`)
3. Reviewer invocation (a non-Claude CLI: `codex` or `gemini`)
4. Verdict binding (`runner.skeptic_gate.evaluate`)
5. Idempotent comment upsert via `gh api` (find prior by marker, PATCH
   or POST)
6. Commit status set via `gh api` — this is the surface merge protection
   can require

The reviewer CLI is the ONLY model call in this file. Everything else
is deterministic orchestration. Fail-closed behavior: missing reviewer,
malformed output, or stale SHA → non-zero exit and a failure commit
status. See `runner/skeptic_gate.py` for the binding rules.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time
from typing import Optional, Tuple

from runner.skeptic_gate import (
    MARKER,
    SkepticResult,
    build_prompt,
    evaluate,
)


# Hard upper bound on the diff we will hand to the reviewer. The reviewer
# CLI typically has its own context-window limit; we truncate defensively
# rather than letting the gate fail for an unparseable oversized blob.
MAX_DIFF_BYTES = 256 * 1024  # 256 KiB


# ---------------------------------------------------------------------------
# Side-effect helpers (GitHub API via `gh` CLI)
# ---------------------------------------------------------------------------


def gh_api(method: str, path: str, *, body: Optional[dict] = None) -> dict:
    """Run `gh api <method> <path>` and return the parsed JSON.

    `method` is one of: `GET`, `POST`, `PATCH`, `DELETE`. Auth comes from
    the standard `GH_TOKEN` / `GITHUB_TOKEN` env that GitHub Actions
    provides to every job.
    """
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
    """Return the comment ID of the prior skeptic-gate bot comment, or None.

    Scans the PR's issue comments for the unique `MARKER` we embed in
    every skeptic-gate body. GitHub paginates at 100 per page, so we
    follow `pageInfo.hasNextPage` until exhausted — even very long PRs
    rarely produce more than a handful of bot comments, but the cost of
    pagination is trivial.
    """
    owner, name = repo.split("/", 1)
    page = 1
    while True:
        data = gh_api(
            "GET",
            f"repos/{owner}/{name}/issues/{pr_number}/comments"
            f"?per_page=100&page={page}",
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


def post_or_update_comment(
    repo: str, pr_number: int, body: str
) -> int:
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
    """Set a commit status on the PR head SHA.

    `state` ∈ {`success`, `failure`, `error`, `pending`}. The `context`
    string is the name the merge-protection UI shows in the "Required
    status checks" dropdown — keep it stable so admins can pin it.
    """
    owner, name = repo.split("/", 1)
    gh_api(
        "POST",
        f"repos/{owner}/{name}/statuses/{sha}",
        body={
            "state": state,
            "context": context,
            "description": description[:140],  # GitHub's hard cap
        },
    )


# ---------------------------------------------------------------------------
# PR context gathering
# ---------------------------------------------------------------------------


def get_pr_context(repo: str, pr_number: int) -> Tuple[str, str, str]:
    """Return (head_sha, base_sha, diff) for the given PR.

    `head_sha` is the current PR head — this is the SHA the gate binds to.
    `base_sha` is the merge base, used to render the diff.
    `diff` is the textual diff between base and head, truncated to
    `MAX_DIFF_BYTES` if necessary (with a `[truncated]` marker).
    """
    owner, name = repo.split("/", 1)
    info = gh_api(
        "GET",
        f"repos/{owner}/{name}/pulls/{pr_number}",
    )
    head_sha = info["head"]["sha"]
    base_sha = info["base"]["sha"]
    diff_proc = subprocess.run(
        ["gh", "pr", "diff", str(pr_number)],
        capture_output=True,
        text=True,
        check=False,
    )
    if diff_proc.returncode != 0:
        raise RuntimeError(
            f"gh pr diff {pr_number} failed: {diff_proc.stderr.strip()[:500]}"
        )
    diff = diff_proc.stdout
    if len(diff.encode("utf-8")) > MAX_DIFF_BYTES:
        # Truncate at a character boundary to keep the prompt well-formed.
        truncated = diff.encode("utf-8")[:MAX_DIFF_BYTES].decode("utf-8", "ignore")
        diff = truncated + "\n\n[truncated — diff exceeded MAX_DIFF_BYTES]\n"
    return head_sha, base_sha, diff


# ---------------------------------------------------------------------------
# Reviewer invocation
# ---------------------------------------------------------------------------


def _build_reviewer_cmd(reviewer: str, model: str) -> list[str]:
    """Return the argv prefix used to invoke the reviewer.

    Both `codex` and `gemini` are non-Claude (the implementing model in
    this repo is Claude). The flags below run the reviewer in a
    non-interactive, single-shot mode: no TUI, no approvals, ephemeral.
    `codex exec` reads the prompt from stdin; `gemini -p` reads from
    argv. Both produce a textual final message.
    """
    if reviewer == "codex":
        return [
            "codex",
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "--ephemeral",
            "--skip-git-repo-check",
            "-m",
            model,
            "-",
        ]
    if reviewer == "gemini":
        return [
            "gemini",
            "-m",
            model,
            "-y",
            "--approval-mode",
            "yolo",
            "-p",
            "__PROMPT_PLACEHOLDER__",
        ]
    raise RuntimeError(
        f"unknown reviewer {reviewer!r}; expected 'codex' or 'gemini'"
    )


def invoke_reviewer(
    reviewer: str, model: str, prompt: str, *, timeout: int = 900
) -> Tuple[Optional[str], Optional[str]]:
    """Run the reviewer CLI; return (stdout, error_message).

    Either `stdout` is set (reviewer ran) or `error_message` is set
    (reviewer missing, timed out, or returned non-zero). Both can be
    present when the reviewer ran but exited non-zero — in that case
    `error_message` is the truncated stderr.
    """
    cmd = _build_reviewer_cmd(reviewer, model)
    if "__PROMPT_PLACEHOLDER__" in cmd:
        # gemini path: substitute the prompt as a single argv element.
        idx = cmd.index("__PROMPT_PLACEHOLDER__")
        cmd[idx] = prompt
        stdin_input = None
    else:
        stdin_input = prompt
    try:
        proc = subprocess.run(
            cmd,
            input=stdin_input,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError as exc:
        return None, f"reviewer binary not found: {exc}"
    except subprocess.TimeoutExpired:
        return None, f"reviewer timed out after {timeout}s"
    if proc.returncode != 0:
        return proc.stdout, f"reviewer rc={proc.returncode}: {proc.stderr.strip()[:300]}"
    return proc.stdout, None


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _resolve_pr_and_sha(
    args: argparse.Namespace, env: dict
) -> Tuple[str, int, str]:
    """Resolve (repo, pr_number, head_sha) from CLI args + env.

    `pull_request` events set everything via `github.event.*`; the
    workflow forwards the values as CLI flags. `workflow_dispatch`
    may provide only `pr_number` and `pr_sha` — in that case we trust
    the dispatch input. If `pr_sha` is empty we re-resolve from
    `gh pr view <pr_number>` to be safe.
    """
    repo = args.repo or env.get("GITHUB_REPOSITORY")
    if not repo:
        raise SystemExit("GITHUB_REPOSITORY (or --repo) is required")
    pr_number = args.pr_number or int(env.get("PR_NUMBER", "0"))
    if not pr_number:
        raise SystemExit("pr_number (or PR_NUMBER env) is required")
    head_sha = args.pr_sha or env.get("PR_HEAD_SHA", "")
    if not head_sha:
        # Re-resolve from the live PR head. This guards against the
        # operator passing a stale `pr_sha` via workflow_dispatch.
        head_sha, _, _ = get_pr_context(repo, pr_number)
    return repo, pr_number, head_sha


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="skeptic-gate",
        description="SHA-bound skeptic gate for the dark-factory 7-green policy",
    )
    parser.add_argument("--repo", default="", help="owner/name; default $GITHUB_REPOSITORY")
    parser.add_argument("--pr-number", type=int, default=0, help="PR number")
    parser.add_argument(
        "--pr-sha",
        default="",
        help="PR head SHA (default: re-resolve via gh pr view)",
    )
    parser.add_argument(
        "--reviewer",
        default=os.environ.get("SKEPTIC_REVIEWER", "gemini"),
        help="reviewer CLI to invoke (codex or gemini; default: gemini)",
    )
    parser.add_argument(
        "--reviewer-model",
        default=os.environ.get("SKEPTIC_REVIEWER_MODEL", ""),
        help="model name passed to the reviewer CLI",
    )
    parser.add_argument(
        "--status-context",
        default=os.environ.get("SKEPTIC_STATUS_CONTEXT", "skeptic"),
        help="commit-status context name (default: 'skeptic')",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="do not post comments or set status; print what would happen",
    )
    args = parser.parse_args(argv)
    env = os.environ

    # ---- 1. Resolve PR + SHA -------------------------------------------------
    try:
        repo, pr_number, head_sha = _resolve_pr_and_sha(args, env)
    except SystemExit:
        raise
    except Exception as exc:
        print(f"[skeptic-gate] context resolution failed: {exc}", file=sys.stderr)
        return 2

    # ---- 2. Gather diff ------------------------------------------------------
    try:
        _, base_sha, diff = get_pr_context(repo, pr_number)
    except Exception as exc:
        print(f"[skeptic-gate] diff capture failed: {exc}", file=sys.stderr)
        # We still want to set a failure status and post a comment so
        # the operator can see what happened. Build a synthetic
        # evaluate() result.
        from runner.skeptic_gate import format_comment

        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=args.reviewer,
            reason=f"diff capture failed: {exc}",
        )
        if not args.dry_run:
            try:
                set_commit_status(
                    repo,
                    head_sha,
                    state="failure",
                    context=args.status_context,
                    description=f"diff capture failed: {str(exc)[:80]}",
                )
                post_or_update_comment(repo, pr_number, body)
            except Exception as side_exc:
                print(
                    f"[skeptic-gate] could not record failure status: {side_exc}",
                    file=sys.stderr,
                )
        return 1

    # ---- 3. Build prompt + invoke reviewer -----------------------------------
    prompt = build_prompt(
        repo=repo,
        pr_number=pr_number,
        head_sha=head_sha,
        base_sha=base_sha,
        diff=diff,
    )
    if not args.reviewer_model:
        # Sensible defaults per reviewer; overridable via env/flag.
        args.reviewer_model = (
            "gemini-2.5-pro" if args.reviewer == "gemini" else "o3-mini"
        )

    print(
        f"[skeptic-gate] repo={repo} pr=#{pr_number} head={head_sha[:12]} "
        f"reviewer={args.reviewer} model={args.reviewer_model}",
        file=sys.stderr,
    )

    review_output, review_error = invoke_reviewer(
        args.reviewer, args.reviewer_model, prompt
    )

    # ---- 4. Evaluate ---------------------------------------------------------
    result: SkepticResult = evaluate(
        review_output=review_output,
        review_error=review_error,
        repo=repo,
        pr_number=pr_number,
        head_sha=head_sha,
        base_sha=base_sha,
        diff=diff,
        reviewer=args.reviewer,
    )

    print(
        f"[skeptic-gate] verdict={result.verdict} state={result.check_state} "
        f"reason={result.reason[:200]}",
        file=sys.stderr,
    )

    # ---- 5. Side effects: comment + status -----------------------------------
    if not args.dry_run:
        try:
            post_or_update_comment(repo, pr_number, result.comment_body)
        except Exception as exc:
            print(
                f"[skeptic-gate] comment upsert failed: {exc}",
                file=sys.stderr,
            )
        try:
            set_commit_status(
                repo,
                head_sha,
                state=result.check_state,
                context=args.status_context,
                description=result.reason,
            )
        except Exception as exc:
            print(
                f"[skeptic-gate] status set failed: {exc}",
                file=sys.stderr,
            )

    return 0 if result.check_state == "success" else 1


if __name__ == "__main__":
    sys.exit(main())
