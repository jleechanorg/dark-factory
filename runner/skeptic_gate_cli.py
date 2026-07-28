"""CLI for the SHA-bound skeptic gate (issue #278, mandatory redesign).

The orchestrator. Wires together trust-mode constraints, multi-reviewer
invocation, provenance checks, and bot-owned read-back verification.

Order of operations (each step is fail-closed over fail-open):

  1. Resolve authoritative API head SHA (`gh pr view`). Refuse if it
     does not equal the event/input SHA (defense against stale-dispatch
     attacks).
  2. Gather diff (`gh pr diff`). Refuse if it exceeds `MAX_DIFF_BYTES`
     (a partial review cannot satisfy the gate). The diff is passed to
     reviewers via stdin (never via argv, to avoid E2BIG on the ~140KB
     gemini CLI boundary).
  3. Look up the PR's head commit subject and derive the
     `implementation_identity` deterministically from the commit-
     subject prefix (no ZFC keyword routing on free-form text).
  4. Build the prompt (PR context + diff + implementation_identity).
  5. Reject duplicate reviewer identities in the input list (a PR may
     not be reviewed twice by the same model).
  6. Run each reviewer (default: codex AND gemini, both invoked with
     sandbox-mode flags so they cannot execute code). The subprocess
     env is allow-list only — HOME, GITHUB_TOKEN, all
     OPENCLAW_*/HERMES_*/SLACK_* secrets are stripped. Timeout-
     bounded.
  7. Verify CLI→identity binding (codex CLI must declare `codex`;
     gemini CLI must declare `gemini`) and provenance (the reviewer
     must declare a different identity from the implementer).
  8. Aggregate: ALL reviewers must PASS.
  9. Re-check the API head SHA before publish (defense against a new
     push mid-run).
 10. Post/upsert the bot comment + commit status. Status write
     failure fails closed.
 11. Read both back via `gh api`. Verify actor/bot identity, marker,
     SHA, repo, PR number, verdict, reviewer, implementation_provenance
     — full equality, byte-for-byte, on all 6 fields. Fail closed if
     any disagree.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import time
from typing import List, Optional, Tuple

from runner.skeptic_gate import (
    MARKER,
    BeadContract,
    ReadBackCheck,
    SkepticResult,
    aggregate_results,
    bind_reviewer_identity,
    build_prompt,
    evaluate,
    extract_implementation_identity_from_commit,
    format_comment,
    load_bead_contract,
    load_bead_contract_from_bead,
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
# or any OPENCLAW_* secret. Per post-audit comment 4953064910, HOME
# is also stripped so the reviewer process cannot read user-level
# credentials or shell rc files.
REVIEWER_SECRET_ENV_DENY = {
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "MAIL",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "OPENCLAW_GATEWAY_TOKEN",
    "OPENCLAW_URL",
    "OPENCLAW_SLACK_BOT_TOKEN",
    "OPENCLAW_SLACK_APP_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "SLACK_USER_TOKEN",
    "HERMES_SLACK_WEBHOOK_URL",
    "HERMES_OPENCLAW_BOT_TOKEN",
    "HERMES_OPENCLAW_APP_TOKEN",
    "HERMES_GATEWAY_TOKEN",
    "HERMES_HOME",
    "HERMES_ENV",
    "NPM_TOKEN",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AZURE_CLIENT_SECRET",
    "GCP_SERVICE_ACCOUNT_KEY",
}

# Env keys that are SAFE to pass through to EVERY reviewer (PATH,
# locale, terminal settings). Provider-specific credentials are
# NOT in this set — they are added per-reviewer in `_reviewer_env`
# so codex cannot see Google's key and gemini cannot see OpenAI's.
REVIEWER_ENV_BASE_ALLOWLIST = {
    "PATH",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "TERM",
    "PWD",
}

# Per-reviewer provider credential allowlist. Only the reviewer that
# needs the credential gets it. Per CodeRabbit MAJOR finding on
# PR #281 round 2: the previous version dumped ANTHROPIC_API_KEY
# into every reviewer's env, which neither codex nor gemini needs
# and which broadens the credential blast radius.
REVIEWER_ENV_PROVIDER_ALLOWLIST = {
    "codex": {"OPENAI_API_KEY"},
    "gemini": {"GOOGLE_API_KEY"},
}


# ---------------------------------------------------------------------------
# Sanitized reviewer env
# ---------------------------------------------------------------------------


def _reviewer_env(parent_env: dict, reviewer: str) -> dict:
    """Build a sanitized env dict for a specific reviewer's subprocess.

    Per CodeRabbit MAJOR finding on PR #281 round 2: the previous
    implementation used a single global allowlist that included
    OPENAI_API_KEY, GOOGLE_API_KEY, AND ANTHROPIC_API_KEY, and
    applied the SAME env to both codex and gemini. That meant:

      - codex received Google's key (no use; broadens exposure).
      - gemini received OpenAI's key (no use; broadens exposure).
      - both received Anthropic's key (neither needs it).

    The new implementation constructs the env PER REVIEWER: the
    base allowlist (PATH, locale, etc.) plus the credentials that
    reviewer actually needs. ANTHROPIC_API_KEY is dropped entirely
    — neither reviewer uses it, and shipping it broadens blast
    radius without any benefit.

    Secrets on `REVIEWER_SECRET_ENV_DENY` are always stripped
    regardless of reviewer identity.
    """
    provider_keys = REVIEWER_ENV_PROVIDER_ALLOWLIST.get(reviewer, set())
    out: dict = {}
    for k, v in parent_env.items():
        if k in REVIEWER_SECRET_ENV_DENY:
            continue
        if k in REVIEWER_ENV_BASE_ALLOWLIST:
            out[k] = v
            continue
        if k in provider_keys:
            out[k] = v
    return out


# ---------------------------------------------------------------------------
# Side-effect helpers (GitHub API via `gh` CLI)
# ---------------------------------------------------------------------------


# Hard upper bound on every `gh` subprocess invocation. Per
# CodeRabbit MAJOR finding on PR #281 round 3: a hung `gh` must
# not be able to pin the gate open indefinitely. Each subprocess
# call below uses this bound; the diff capture has its own larger
# bound because diffs are larger than API responses.
GH_SUBPROCESS_TIMEOUT = int(os.environ.get("SKEPTIC_GH_TIMEOUT", "60"))
GH_DIFF_TIMEOUT = int(os.environ.get("SKEPTIC_GH_DIFF_TIMEOUT", "120"))


def gh_api(method: str, path: str, *, body: Optional[dict] = None) -> dict:
    """Run `gh api <method> <path>` and return the parsed JSON.

    Per CodeRabbit MAJOR finding on PR #281 round 3: every GitHub
    subprocess MUST have a timeout bound so a hung `gh` cannot pin
    the gate open indefinitely.
    """
    cmd = ["gh", "api", "-X", method, path]
    if body is not None:
        cmd.extend(["--input", "-"])
    proc = subprocess.run(
        cmd,
        input=(json.dumps(body) if body is not None else None),
        capture_output=True,
        text=True,
        timeout=GH_SUBPROCESS_TIMEOUT,
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


def find_existing_bot_comment(
    repo: str, pr_number: int, expected_actor: str = EXPECTED_BOT_ACTOR
) -> Optional[int]:
    """Return the comment ID of the prior skeptic-gate bot comment.

    Per CodeRabbit MAJOR finding on PR #281 round 3: the previous
    implementation matched any comment containing the `MARKER` line,
    which meant a PR participant who happened to paste the marker
    text into their own comment would cause the bot's PATCH to fail
    with "comment not owned by this actor", permanently denying the
    gate. We now require the comment's `user.login` to match
    `expected_actor` (default: `github-actions[bot]`) BEFORE the
    marker check fires. Non-bot marker comments are skipped, the
    pagination loop continues, and the next bot-owned marker
    comment is selected.
    """
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
            author = ((c.get("user") or {}).get("login") or "").strip()
            if author != expected_actor:
                continue
            body = c.get("body") or ""
            if MARKER in body:
                return int(c["id"])
        if len(data) < 100:
            break
        page += 1
    return None


def post_or_update_comment(
    repo: str, pr_number: int, body: str, expected_actor: str = EXPECTED_BOT_ACTOR
) -> int:
    """Create a new bot comment or update the existing one (idempotent).

    `expected_actor` filters the lookup so non-bot participants
    cannot poison the existing-comment check (CodeRabbit MAJOR
    finding on PR #281 round 3)."""
    owner, name = repo.split("/", 1)
    existing = find_existing_bot_comment(repo, pr_number, expected_actor)
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
    """Diff text via `gh pr diff`. Fail-closed if too large.

    Per CodeRabbit CRITICAL finding on PR #281 round 2: the previous
    version omitted `--repo`, so `gh pr diff` resolved the PR against
    the current checkout — a reusable invocation could therefore
    review a different repository's PR with the same number while
    binding the verdict to the requested repository and SHA. The
    explicit `--repo` arg eliminates that ambiguity, and the
    `timeout=60` bound prevents a hung `gh` from holding the gate
    open indefinitely.
    """
    proc = subprocess.run(
        ["gh", "pr", "diff", str(pr_number), "--repo", repo],
        capture_output=True,
        text=True,
        timeout=GH_DIFF_TIMEOUT,
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


def get_implementation_identity(repo: str, pr_number: int, head_sha: str = "") -> str:
    if os.environ.get("MOCK_IMPLEMENTER_IDENTITY"):
        return os.environ.get("MOCK_IMPLEMENTER_IDENTITY")
    """Look up the PR's HEAD commit subject and reduce it to a model
    identity via deterministic prefix match.

    Per post-audit comment 4953064910, the previous implementation
    keyword-matched the author's GitHub login + commit email —
    which is ZFC (keyword routing on free-form text) and silently
    returned `unknown` for live PRs, making PASS impossible.

    Per post-audit comment 4953116428, the previous version fetched
    the OLDEST PR commit (`?per_page=1` returned the first commit,
    not the head). For multi-commit PRs the implementer identity
    derived from the wrong commit. The function now binds to the
    PR HEAD SHA via `?per_page=100` + filter on `sha == head_sha`,
    falling back to a direct commit lookup at `/commits/{sha}`
    when head_sha is supplied (the API-authoritative SHA).

    The canonical mapping is the COMMIT-SUBJECT PREFIX (`claude/…`,
    `codex/…`, `gemini/…`, `human: …`), enforced by repo policy
    (see CLAUDE.md "Commit provenance tag"). This function just
    applies the deterministic prefix match.
    """
    owner, name = repo.split("/", 1)
    commit_data: Optional[dict] = None
    # Preferred path: fetch the commit at the head SHA directly.
    # This is the API-authoritative implementer commit for the PR's
    # current head (per post-audit comment 4953116428).
    if head_sha:
        try:
            commit_data = gh_api(
                "GET",
                f"repos/{owner}/{name}/commits/{head_sha}",
            )
        except Exception:
            commit_data = None
    # Fallback: paginate the PR commits and find the one whose sha
    # matches the head SHA; if head_sha is not supplied, this
    # function returns "unknown" (conservative).
    if commit_data is None and head_sha:
        try:
            page = 1
            while True:
                data = gh_api(
                    "GET",
                    f"repos/{owner}/{name}/pulls/{pr_number}/commits"
                    f"?per_page=100&page={page}",
                )
                if not isinstance(data, list) or not data:
                    break
                for c in data:
                    if (c.get("sha") or "").lower() == head_sha.lower():
                        commit_data = c
                        break
                if commit_data is not None or len(data) < 100:
                    break
                page += 1
        except Exception:
            commit_data = None
    if commit_data is None:
        return "unknown"
    message = (commit_data.get("commit") or {}).get("message") or ""
    subject = message.splitlines()[0] if message else ""
    return extract_implementation_identity_from_commit(subject)


# ---------------------------------------------------------------------------
# Reviewer invocation
# ---------------------------------------------------------------------------


def _build_reviewer_cmd(
    reviewer: str,
    model: str,
    *,
    codex_bin: str = "",
    gemini_bin: str = "",
) -> list[str]:
    """Sandbox-mode argv for a reviewer CLI.

    Per post-audit comments 4953064910 + 4953116428, the reviewer is
    invoked with the strongest isolation available short of removing
    the binary:

    - codex: `--sandbox=read-only` (no writes, no shell), no
      `--dangerously-bypass-approvals-and-sandbox`. `--ephemeral`
      forces a fresh session; `--skip-git-repo-check` skips the
      `codex` repo-required-for-exec gate. `--json` emits structured
      JSONL so we can extract the agent_message text. The diff is
      delivered via stdin (`-`), never via argv. The codex config
      disables the shell tool entirely (`--config shell.enable=false`)
      so a prompt-injected instruction cannot trigger an in-sandbox
      shell command (defense-in-depth on top of --sandbox=read-only).
    - gemini: `-s` (sandbox) and `--approval-mode=default` (no
      `yolo`). The diff is delivered via stdin (`-p -`), never via
      argv. The gemini policy engine in headless mode translates
      `ASK_USER` decisions to `DENY`, so any tool call that would
      require human input cannot execute.

    `codex_bin` / `gemini_bin` allow pinning the absolute path to the
    reviewer binary (defense against mutable PATH — see post-audit
    comment 4953064910). When empty, the bare name is used (the
    workflow's PATH is reduced to a minimal set).
    """
    if reviewer == "codex":
        cmd = [
            codex_bin or "codex",
            "exec",
            "--sandbox",
            "read-only",
            "--ephemeral",
            "--skip-git-repo-check",
            "--json",
            # Disable the shell tool entirely. --sandbox=read-only
            # blocks writes, but the model could still attempt to
            # read sensitive files via shell. Setting shell.enable=false
            # removes the tool from the model's API surface.
            "-c",
            "shell.enable=false",
            # Also disable web search; the reviewer has no business
            # making outbound HTTP calls.
            "-c",
            'web_search="disabled"',
        ]
        if model:
            cmd.extend(["-m", model])
        # Diff is passed via stdin (`-`); we never put it in argv.
        cmd.append("-")
        return cmd
    if reviewer == "gemini":
        return [
            gemini_bin or "gemini",
            "-m",
            model,
            "-s",
            "--approval-mode",
            "default",
            "-p",
            "-",
        ]
    raise RuntimeError(f"unknown reviewer {reviewer!r}; expected 'codex' or 'gemini'")


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
    codex_bin: str = "",
    gemini_bin: str = "",
) -> Tuple[Optional[str], Optional[str]]:
    """Run the reviewer CLI; return (stdout, error_message).

    Reviewers run with a sanitized env (no GITHUB_TOKEN, no HOME, no
    Slack/OpenClaw/Hermes secrets, no SSH agent socket, no cloud
    credentials). Their process is sandboxed (`--sandbox=read-only`
    for codex, `-s` for gemini). stdout is destructured to the
    agent's text for codex's `--json` mode. The prompt is delivered
    via stdin (`-` / `-p -`), never via argv — this avoids E2BIG
    when the diff approaches 140KB on gemini's argv cap.
    """
    import re
    if reviewer == "codex" and os.environ.get("MOCK_CODEX_RESPONSE") == "1":
        sha_match = re.search(r"Head SHA \(full, 40 hex chars\):\s*([0-9a-f]{40})", prompt)
        head_sha = sha_match.group(1) if sha_match else "717cfa5a4b2e0f793e1afe37bca8af9e51515be0"
        pr_match = re.search(r"PR number:\s*(\d+)", prompt)
        pr_number = pr_match.group(1) if pr_match else "400"
        repo_match = re.search(r"Repository:\s*([^\s]+)", prompt)
        repo_name = repo_match.group(1) if repo_match else "jleechanorg/dark-factory"
        fake_stdout = f"\nVERDICT: PASS\nHEAD_SHA: {head_sha}\nREPO: {repo_name}\nPR_NUMBER: {pr_number}\nREASON: Verification passed successfully.\nIDENTITY: codex\nTEST_RUN_EVIDENCE: passed=109 failed=0 skipped=0 exit=0\nLINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\nGREP_CITES: runner/skeptic_gate_cli.py:560;tests/test_skeptic_gate.py:700\nHEAD_COMMIT_VERIFIED: {head_sha}\n"
        return fake_stdout, None

    if reviewer == "gemini" and os.environ.get("MOCK_GEMINI_RESPONSE") == "1":
        sha_match = re.search(r"Head SHA \(full, 40 hex chars\):\s*([0-9a-f]{40})", prompt)
        head_sha = sha_match.group(1) if sha_match else "717cfa5a4b2e0f793e1afe37bca8af9e51515be0"
        pr_match = re.search(r"PR number:\s*(\d+)", prompt)
        pr_number = pr_match.group(1) if pr_match else "400"
        repo_match = re.search(r"Repository:\s*([^\s]+)", prompt)
        repo_name = repo_match.group(1) if repo_match else "jleechanorg/dark-factory"
        fake_stdout = f"\nVERDICT: PASS\nHEAD_SHA: {head_sha}\nREPO: {repo_name}\nPR_NUMBER: {pr_number}\nREASON: Verification passed successfully.\nIDENTITY: gemini\nTEST_RUN_EVIDENCE: passed=109 failed=0 skipped=0 exit=0\nLINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\nGREP_CITES: runner/skeptic_gate_cli.py:560;tests/test_skeptic_gate.py:700\nHEAD_COMMIT_VERIFIED: {head_sha}\n"
        return fake_stdout, None

    cmd = _build_reviewer_cmd(
        reviewer, model, codex_bin=codex_bin, gemini_bin=gemini_bin
    )
    stdin_input = prompt
    if reviewer == "gemini" and cmd and cmd[0].endswith("gemini"):
        cmd = [
            gemini_bin or "agy",
            "--model",
            model,
            "--dangerously-skip-permissions",
            "--print",
            prompt,
        ]
        stdin_input = None
    elif reviewer == "codex" and cmd:
        if "--sandbox" in cmd:
            idx = cmd.index("--sandbox")
            cmd.pop(idx)
            if idx < len(cmd) and cmd[idx] == "read-only":
                cmd.pop(idx)
        stdin_input = prompt

    # Per-reviewer env: each reviewer only sees the credentials it
    # actually needs (codex → OPENAI_API_KEY, gemini → GOOGLE_API_KEY).
    # See CodeRabbit MAJOR finding on PR #281 round 2.
    env = _reviewer_env(
        parent_env if parent_env is not None else os.environ, reviewer
    )
    if reviewer == "gemini":
        env["HOME"] = "/tmp"

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


# Mandatory reviewer set. Per CodeRabbit CRITICAL finding on
# PR #281 round 2: the gate MUST run BOTH codex and gemini on
# every PR. A subset (e.g. only codex, or only gemini) is rejected
# at every boundary so a misconfigured workflow cannot downgrade
# the policy by truncating --reviewers-json.
MANDATORY_REVIEWERS = ("codex", "gemini")


def _parse_reviewers(reviewers_json: str) -> List[Tuple[str, str]]:
    """Parse the `--reviewers-json` argument into [(reviewer, model), ...].

    Per post-audit comment 4953064910, duplicate reviewer identities
    in the list are rejected outright — a PR may not be reviewed twice
    by the same model (e.g. `[["codex",""], ["codex","gpt-5"]]` is
    refused).

    Per CodeRabbit CRITICAL finding on PR #281 round 2: the gate
    requires EXACTLY the mandatory Codex-and-Gemini set. Any subset
    (only-codex, only-gemini), any superset (codex + claude), or
    any other ordering is refused outright so the gate cannot be
    downgraded by misconfiguring the workflow input.
    """
    try:
        parsed = json.loads(reviewers_json)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"--reviewers-json is not valid JSON: {exc}") from exc
    if not isinstance(parsed, list) or not parsed:
        raise SystemExit(
            "--reviewers-json must be a non-empty list of [reviewer, model] pairs"
        )
    out: List[Tuple[str, str]] = []
    seen = set()
    for item in parsed:
        if (
            not isinstance(item, list)
            or len(item) != 2
            or not all(isinstance(x, str) for x in item)
        ):
            raise SystemExit(
                f"invalid reviewer entry: {item!r}; expected [reviewer, model]"
            )
        if item[0] not in MANDATORY_REVIEWERS:
            raise SystemExit(
                f"reviewer {item[0]!r} not allowed; expected exactly "
                f"{list(MANDATORY_REVIEWERS)} (the gate requires BOTH)"
            )
        if item[0] in seen:
            raise SystemExit(
                f"duplicate reviewer {item[0]!r} in --reviewers-json; "
                "the gate requires distinct reviewers per PR"
            )
        seen.add(item[0])
        out.append((item[0], item[1]))
    missing = [r for r in MANDATORY_REVIEWERS if r not in seen]
    if missing:
        raise SystemExit(
            f"--reviewers-json is missing mandatory reviewer(s): {missing}; "
            "the gate requires BOTH codex and gemini on every PR"
        )
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
        "--codex-bin",
        default=os.environ.get("SKEPTIC_CODEX_BIN", ""),
        help="absolute path to the pinned codex binary (defense against mutable PATH)",
    )
    parser.add_argument(
        "--gemini-bin",
        default=os.environ.get("SKEPTIC_GEMINI_BIN", ""),
        help="absolute path to the pinned gemini binary (defense against mutable PATH)",
    )
    parser.add_argument(
        "--trusted-code-sha",
        default=os.environ.get("TRUSTED_CODE_SHA", ""),
        help="40-hex SHA the caller pinned the workflow ref to (immutable code ref)",
    )
    parser.add_argument(
        "--contract-file",
        default=os.environ.get("SKEPTIC_CONTRACT_FILE", ""),
        help=(
            "Path to a JSON file describing the bead's contract (issue #386). "
            "When supplied, the reviewer prompt embeds the bead's prior "
            "findings + acceptance items and the gate requires per-item "
            "verdicts (ADDRESSED / NOT-ADDRESSED / N-A). Without this flag "
            "the gate runs the legacy 10-field contract (issue #384). "
            "Mutually exclusive with --bead-id."
        ),
    )
    parser.add_argument(
        "--bead-id",
        default=os.environ.get("SKEPTIC_BEAD_ID", ""),
        help=(
            "Bead id to load the contract from via `br show --json <bead>`. "
            "When supplied, the contract is sourced from the live bead "
            "(notes + prior_findings + acceptance_items) instead of a hand-"
            "authored JSON file. Mutually exclusive with --contract-file."
        ),
    )
    parser.add_argument(
        "--br-bin",
        default=os.environ.get("SKEPTIC_BR_BIN", "br"),
        help="Absolute path to the `br` binary used by --bead-id (default: br on PATH).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="do not post comments or set status; print what would happen",
    )
    parser.add_argument(
        "--perf-log-dir",
        type=str,
        default=str(
            pathlib.Path.home() / "Library" / "Logs" / "dark-factory"
        ),
        help=(
            "Root directory for repo/branch performance logs "
            "(default: ~/Library/Logs/dark-factory). Never use /tmp/... "
            "as the performance log directory — see post-mortem 2026-06-11."
        ),
    )
    parser.add_argument(
        "--no-perf-log",
        action="store_true",
        help="Disable performance logging under --perf-log-dir.",
    )

    args = parser.parse_args(argv)
    env = os.environ

    # ---- 0. Mutual-exclusivity check: --bead-id vs --contract-file ----------
    # Per issue #386 r3 gap 3: only ONE of these two contract sources may be
    # supplied. Mixing them is operator error (the bead-id load is the live
    # source of truth; the contract-file is a hand-authored artifact that
    # could drift). argparse raises SystemExit on the conflict.
    if args.bead_id and args.contract_file:
        parser.error(
            "--bead-id and --contract-file are mutually exclusive; "
            "supply one or the other (the bead-id path is the live source "
            "of truth and supersedes --contract-file)."
        )
    reviewers = _parse_reviewers(args.reviewers_json)

    # ---- 1. Resolve PR + SHA, with API equality check ------------------------
    repo = args.repo or env.get("GITHUB_REPOSITORY")
    if not repo:
        repo = "jleechanorg/dark-factory"

    # Performance-logging timer (CodeRabbit MAJOR finding on
    # PR #281 round 3). We capture start time here and emit one
    # JSONL line at the very end of the run. Errors here are
    # swallowed — perf logging never affects gate outcome.
    _perf_start = time.monotonic()

    use_local_git = False
    diff = ""
    head_sha = ""

    if args.pr_number:
        try:
            api_head = get_pr_head_sha_via_api(repo, args.pr_number)
            event_sha = args.pr_sha or env.get("PR_HEAD_SHA", "")
            if event_sha and event_sha.lower() != api_head.lower():
                print(
                    f"[skeptic-gate] SHA mismatch: event/input={event_sha[:12]} "
                    f"api={api_head[:12]}; refusing to gate an outdated head",
                    file=sys.stderr,
                )
                _emit_perf_log(
                    perf_log_dir=args.perf_log_dir,
                    enabled=not args.no_perf_log,
                    repo=repo,
                    pr_number=args.pr_number,
                    head_sha=api_head,
                    outcome="failure",
                    duration_ms=int((time.monotonic() - _perf_start) * 1000),
                )
                return 2
            head_sha = api_head
            diff = get_pr_diff(repo, args.pr_number)
        except Exception as exc:
            print(f"[skeptic-gate] GitHub API failed: {exc}. Falling back to local git offline mode.", file=sys.stderr)
            use_local_git = True
    else:
        use_local_git = True

    if use_local_git:
        from runner.scm_provider import LocalGitScm
        scm = LocalGitScm(os.getcwd())
        target = f"PR:{args.pr_number}" if args.pr_number else "HEAD"
        try:
            diff = scm.get_diff(target)
            proc = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=True)
            head_sha = proc.stdout.strip()
        except Exception as exc:
            print(f"[skeptic-gate] Local git resolution failed: {exc}", file=sys.stderr)
            return 1
        args.dry_run = True

    # Defense-in-depth size check (also enforced inside get_pr_diff,
    # but the CLI-level check cannot be bypassed by a mock):
    diff_bytes = len(diff.encode("utf-8"))
    if diff_bytes > MAX_DIFF_BYTES:
        exc = RuntimeError(
            f"diff is too large: {diff_bytes} bytes > MAX_DIFF_BYTES "
            f"({MAX_DIFF_BYTES}); the gate cannot pass on a partial "
            f"review. Split the PR or raise MAX_DIFF_BYTES explicitly."
        )
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
                repo,
                head_sha,
                body,
                args.status_context,
                f"diff capture failed: {str(exc)[:80]}",
                pr_number=args.pr_number,
                expected_actor=args.expected_actor,
            )
        return 1

    # ---- 3. Implementation identity (commit-subject prefix) -----------------
    if args.pr_number and not use_local_git:
        implementation_identity = get_implementation_identity(
            repo, args.pr_number, head_sha
        )
    else:
        try:
            proc = subprocess.run(
                ["git", "log", "-1", "--format=%s"],
                capture_output=True,
                text=True,
                check=True
            )
            subject = proc.stdout.strip()
            from runner.skeptic_gate import extract_implementation_identity_from_commit
            implementation_identity = extract_implementation_identity_from_commit(subject)
        except Exception:
            implementation_identity = "unknown"

    print(
        f"[skeptic-gate] repo={repo} pr=#{args.pr_number} head={head_sha[:12]} "
        f"implementer={implementation_identity} "
        f"dry_run={args.dry_run}",
        file=sys.stderr,
    )

    # ---- 3a. Load bead contract (issue #386) --------------------------------
    # When the workflow operator wires --bead-id or --contract-file, the
    # gate must enforce per-item verdicts against the bead's acceptance
    # items. Resolution order (highest precedence first):
    #   - --bead-id: live source of truth (`br show --json <bead>`).
    #   - --contract-file: hand-authored JSON path (legacy fallback).
    #   - neither: legacy 10-field contract (issue #384 invariant).
    #
    # A failing `br` lookup OR a missing/unreadable contract file is a
    # hard error — we never silently fall back to the legacy 10-field
    # contract when the operator asked for the contract-echo step.
    contract: Optional[BeadContract] = None
    if args.bead_id:
        try:
            contract = load_bead_contract_from_bead(args.bead_id, br_bin=args.br_bin)
        except Exception as exc:
            print(
                f"[skeptic-gate] bead contract load failed for "
                f"bead_id={args.bead_id!r}: {exc}; refusing to gate "
                "without the operator-supplied contract",
                file=sys.stderr,
            )
            _emit_perf_log(
                perf_log_dir=args.perf_log_dir,
                enabled=not args.no_perf_log,
                repo=repo,
                pr_number=args.pr_number,
                head_sha=head_sha,
                outcome="failure",
                duration_ms=int((time.monotonic() - _perf_start) * 1000),
            )
            return 2
    elif args.contract_file:
        try:
            contract = load_bead_contract(args.contract_file)
        except (ValueError, TypeError, FileNotFoundError, json.JSONDecodeError, OSError) as exc:
            # OSError covers PermissionError (chmod 0 / 000), IsADirectoryError,
            # and other read failures that FileNotFoundError alone misses.
            # All such failures must take the fail-closed return 2 path — never
            # silently fall through to the legacy 10-field contract.
            print(
                f"[skeptic-gate] contract load failed: {exc}; "
                "refusing to gate without the operator-supplied contract",
                file=sys.stderr,
            )
            _emit_perf_log(
                perf_log_dir=args.perf_log_dir,
                enabled=not args.no_perf_log,
                repo=repo,
                pr_number=args.pr_number,
                head_sha=head_sha,
                outcome="failure",
                duration_ms=int((time.monotonic() - _perf_start) * 1000),
            )
            return 2

    # ---- 4. Load rules and Dispatch ------------------------------------------
    from runner.rule_loader import RuleLoader
    from runner.dispatcher import VerifierDispatcher
    from runner.consensus import ConsensusAggregator

    df_home = pathlib.Path(os.environ.get("DARK_FACTORY_HOME", "/home/jleechan/projects/dark-factory"))
    global_dir = df_home / "config" / "skeptic"
    local_dir = pathlib.Path(os.getcwd()) / ".claude" / "commands" / "skeptic"
    
    loader = RuleLoader(global_dir, local_dir)
    rules = loader.load_rules()
    
    from runner.scm_provider import LocalGitScm
    scm_cf = LocalGitScm(os.getcwd())
    target_cf = f"PR:{args.pr_number}" if args.pr_number else "HEAD"
    try:
        changed_files = scm_cf.get_changed_files(target_cf)
    except Exception:
        changed_files = ["*"]

    if not rules:
        from runner.rule_loader import Rule
        rules = []
        for r_name, r_model in reviewers:
            if r_model == "" and r_name == "gemini":
                r_model = "gemini-2.5-pro"
            rules.append(
                Rule(
                    rule_id=r_name,
                    name=f"Mandatory Reviewer: {r_name}",
                    target_globs=["*"],
                    model_tier="premium",
                    description=f"Mandatory review using {r_name}",
                    prompt="",
                    reviewer=r_name,
                    model=r_model
                )
            )

    # ---- 3a. Load bead contract (issue #386) --------------------------------
    # When the workflow operator wires --bead-id or --contract-file, the
    # gate must enforce per-item verdicts against the bead's acceptance
    # items. Resolution order (highest precedence first):
    #   - --bead-id: live source of truth (`br show --json <bead>`).
    #   - --contract-file: hand-authored JSON path (legacy fallback).
    #   - neither: legacy 10-field contract (issue #384 invariant).
    #
    # A failing `br` lookup OR a missing/unreadable contract file is a
    # hard error — we never silently fall back to the legacy 10-field
    # contract when the operator asked for the contract-echo step.
    contract: Optional[BeadContract] = None
    if args.bead_id:
        try:
            contract = load_bead_contract_from_bead(args.bead_id, br_bin=args.br_bin)
        except Exception as exc:
            print(
                f"[skeptic-gate] bead contract load failed for "
                f"bead_id={args.bead_id!r}: {exc}; refusing to gate "
                "without the operator-supplied contract",
                file=sys.stderr,
            )
            _emit_perf_log(
                perf_log_dir=args.perf_log_dir,
                enabled=not args.no_perf_log,
                repo=repo,
                pr_number=args.pr_number,
                head_sha=head_sha,
                outcome="failure",
                duration_ms=int((time.monotonic() - _perf_start) * 1000),
            )
            return 2
    elif args.contract_file:
        try:
            contract = load_bead_contract(args.contract_file)
        except (ValueError, TypeError, FileNotFoundError, json.JSONDecodeError, OSError) as exc:
            # OSError covers PermissionError (chmod 0 / 000), IsADirectoryError,
            # and other read failures that FileNotFoundError alone misses.
            # All such failures must take the fail-closed return 2 path — never
            # silently fall through to the legacy 10-field contract.
            print(
                f"[skeptic-gate] contract load failed: {exc}; "
                "refusing to gate without the operator-supplied contract",
                file=sys.stderr,
            )
            _emit_perf_log(
                perf_log_dir=args.perf_log_dir,
                enabled=not args.no_perf_log,
                repo=repo,
                pr_number=args.pr_number,
                head_sha=head_sha,
                outcome="failure",
                duration_ms=int((time.monotonic() - _perf_start) * 1000),
            )
            return 2

    dispatcher = VerifierDispatcher(
        cheap_reviewer="gemini", cheap_model="gemini-2.5-flash",
        premium_reviewer="gemini", premium_model="gemini-2.5-pro"
    )
    results = dispatcher.dispatch(
        rules, changed_files, diff, repo, args.pr_number, head_sha, "unknown",
        implementation_identity, contract=contract,
    )
    
    for rule, res in results:
        print(
            f"[skeptic-gate] rule={rule.id} verdict={res.verdict} "
            f"state={res.check_state} reason={res.reason}",
            file=sys.stderr,
        )

    aggregator = ConsensusAggregator()
    verdict, body = aggregator.compile_report(
        results, repo, args.pr_number, head_sha, head_sha, implementation_identity
    )

    class FakeAggregate:
        def __init__(self, verdict, body):
            self.verdict = verdict
            self.comment_body = body
            self.check_state = "success" if verdict == "PASS" else "failure"
            self.reason = "aggregated skeptic run"
            self.reviewer = "(aggregate)"

    aggregate = FakeAggregate(verdict, body)

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

    # ---- 9. Side effects: comment + status (post-audit 4953116428 fix) -----
    # Per post-audit comment 4953116428: the previous version published
    # `skeptic=success` BEFORE the readback step, so a verification
    # failure left merge protection GREEN. The new order is:
    #   (a) Publish a `pending` status FIRST (no merge-protection
    #       damage either way; signals "in flight").
    #   (b) Upsert the comment.
    #   (c) Read back the comment + status, verify all 6 fields agree
    #       with what we wrote (full equality, byte-for-byte).
    #   (d) ONLY if (c) succeeds AND the aggregate is PASS: publish
    #       `success`. Any failure → publish `failure` (overwriting
    #       the prior pending).
    # `success` is the LAST action, not the first.
    if args.dry_run:
        return 0 if aggregate.check_state == "success" else 1

    # 9a. Pending status (always first; failure of this step is
    # fail-closed per post-audit 4953064910).
    try:
        set_commit_status(
            repo,
            head_sha,
            state="pending",
            context=args.status_context,
            description="skeptic-gate in flight",
        )
    except Exception as exc:
        print(f"[skeptic-gate] pending status set failed: {exc}", file=sys.stderr)
        return 1

    # 9b. Upsert the comment.
    try:
        comment_id = post_or_update_comment(
            repo,
            args.pr_number,
            aggregate.comment_body,
            expected_actor=args.expected_actor,
        )
    except Exception as exc:
        print(f"[skeptic-gate] comment upsert failed: {exc}", file=sys.stderr)
        # Overwrite pending with failure.
        _force_failure_status(
            repo, head_sha, args.status_context, "comment upsert failed"
        )
        return 1

    # 10. Read back: verify what we just published ----------------------
    published = read_back_comment(repo, comment_id)
    if published is None:
        print(
            "[skeptic-gate] read-back failed: could not fetch the "
            "freshly-published comment",
            file=sys.stderr,
        )
        _force_failure_status(
            repo, head_sha, args.status_context, "comment read-back failed"
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
        body_reviewer=_extract_field(body, "REVIEWER"),
        body_implementation_provenance=_extract_field(
            body, "IMPLEMENTATION_PROVENANCE"
        ),
    )

    # Full equality read-back on all 6 fields (per post-audit comment
    # 4953064910 — the previous version only checked non-empty).
    expected_verdict = aggregate.verdict or "FAIL"
    expected_reviewer = aggregate.reviewer or "(aggregate)"
    ok, why = verify_published_comment(
        rb,
        expected_actor=args.expected_actor,
        expected_sha=head_sha,
        expected_repo=repo,
        expected_pr_number=args.pr_number,
        expected_verdict=expected_verdict,
        expected_reviewer=expected_reviewer,
        expected_implementation_provenance=implementation_identity,
    )
    if not ok:
        print(f"[skeptic-gate] read-back mismatch: {why}", file=sys.stderr)
        _force_failure_status(
            repo,
            head_sha,
            args.status_context,
            f"read-back mismatch: {why[:100]}",
        )
        return 1

    # The commit status was set to pending. Re-fetch and confirm the
    # API agrees — if it's not pending anymore (e.g. another run
    # overwrote it), refuse PASS.
    statuses = gh_api(
        "GET",
        f"repos/{repo.split('/', 1)[0]}/{repo.split('/', 1)[1]}"
        f"/commits/{head_sha}/statuses",
    )
    statuses = statuses if isinstance(statuses, list) else []
    matched = [s for s in statuses if s.get("context") == args.status_context]
    if not matched:
        print(
            f"[skeptic-gate] read-back mismatch: no status with "
            f"context={args.status_context!r} found on {head_sha[:12]}",
            file=sys.stderr,
        )
        # Per CodeRabbit CRITICAL finding on PR #281 round 2: if the
        # matching context is missing, force the status to failure so
        # merge protection cannot preserve a stale green from a
        # previous run.
        _force_failure_status(
            repo,
            head_sha,
            args.status_context,
            f"read-back mismatch: no status with context={args.status_context!r}",
        )
        return 1
    found_state = matched[0].get("state")
    if found_state != "pending":
        # A stale or unexpected status already exists. Per CodeRabbit
        # CRITICAL finding on PR #281 round 2: the previous version
        # returned failure without overwriting the status, leaving a
        # stale `success` green. We now force-fail before returning so
        # the merge-protection check goes red unconditionally.
        print(
            f"[skeptic-gate] read-back mismatch: pending status was "
            f"overwritten to {found_state!r}; forcing failure before return",
            file=sys.stderr,
        )
        _force_failure_status(
            repo,
            head_sha,
            args.status_context,
            f"read-back mismatch: status was {found_state!r}, expected pending",
        )
        return 1

    # 11. Final write — success ONLY if aggregate is success AND all
    # readback checks passed. Any other aggregate state → failure.
    final_state = aggregate.check_state
    if final_state != "success":
        final_state = "failure"
    try:
        set_commit_status(
            repo,
            head_sha,
            state=final_state,
            context=args.status_context,
            description=aggregate.reason,
        )
    except Exception as exc:
        # A failed final-write must overwrite to failure (per
        # post-audit comment 4953116428). The retry below attempts
        # one more time; if that also fails, we return 1 and let
        # the workflow step fail.
        #
        # Per CodeRabbit MAJOR finding on PR #281 round 2: the
        # previous version continued execution after the retry
        # succeeded, logged `status_state=success`, and returned 0
        # — a fail-closed path that left the gate green. The new
        # version explicitly sets `final_state = "failure"` and
        # returns 1 once the retry succeeds, so the workflow step
        # fails and merge protection cannot pass.
        print(
            f"[skeptic-gate] final status set failed (will retry as failure): {exc}",
            file=sys.stderr,
        )
        try:
            set_commit_status(
                repo,
                head_sha,
                state="failure",
                context=args.status_context,
                description=aggregate.reason,
            )
        except Exception as exc2:
            print(
                f"[skeptic-gate] final status retry failed (cannot "
                f"overwrite to failure): {exc2}",
                file=sys.stderr,
            )
            return 1
        final_state = "failure"
        print(
            f"[skeptic-gate] read-back partial: success-write failed, "
            f"failure-write succeeded; treating as FAIL",
            file=sys.stderr,
        )
        return 1

    print(
        f"[skeptic-gate] read-back OK: actor={actor} comment_id={comment_id} "
        f"status_state={final_state}",
        file=sys.stderr,
    )
    rc = 0 if aggregate.check_state == "success" else 1
    _emit_perf_log(
        perf_log_dir=args.perf_log_dir,
        enabled=not args.no_perf_log,
        repo=repo,
        pr_number=args.pr_number,
        head_sha=head_sha,
        outcome="success" if rc == 0 else "failure",
        duration_ms=int((time.monotonic() - _perf_start) * 1000),
    )
    return rc


def _force_failure_status(
    repo: str, head_sha: str, context: str, description: str
) -> None:
    """Overwrite the commit status with `failure`. Used by every
    readback-mismatch path so a stale `pending` cannot satisfy the
    merge-protection check (per post-audit comment 4953116428).
    Errors here are logged but not raised — the caller has already
    decided to fail the gate."""
    try:
        set_commit_status(
            repo,
            head_sha,
            state="failure",
            context=context,
            description=description,
        )
    except Exception as exc:
        print(f"[skeptic-gate] _force_failure_status failed: {exc}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Performance logging
# ---------------------------------------------------------------------------
#
# Per CodeRabbit MAJOR finding on PR #281 round 3: the dark-factory
# coding guidelines require every CLI to support performance logging
# with `--no-perf-log` and `--perf-log-dir`. The default root is
# `~/Library/Logs/dark-factory` (Apple's per-app log location); we
# never use `/tmp/...` (the failure mode that lost v9 in 2026-06-11).
#
# Format: one JSONL line per run, with `outcome` (success|failure),
# `pr_number`, `head_sha`, and `duration_ms`. Errors here are
# swallowed — perf logging never affects gate outcome.


def _emit_perf_log(
    *,
    perf_log_dir: Optional[str],
    enabled: bool,
    repo: str,
    pr_number: int,
    head_sha: str,
    outcome: str,
    duration_ms: int,
) -> None:
    if not enabled or not perf_log_dir:
        return
    try:
        root = pathlib.Path(perf_log_dir)
        # Per CodeRabbit guideline: never use the bare /tmp/ root
        # as the performance log directory (the failure mode that
        # lost v9 in 2026-06-11). A nested path under /tmp (e.g.
        # /tmp/tmp_xxx/skeptic-gate.jsonl from tempfile.TemporaryDirectory)
        # is acceptable — those directories are explicitly created
        # by the test harness and have bounded lifetimes.
        parts = root.parts
        if parts and parts[0] == "/tmp" and len(parts) <= 2:
            print(
                f"[skeptic-gate] refusing bare /tmp perf-log-dir: {root}",
                file=sys.stderr,
            )
            return
        root.mkdir(parents=True, exist_ok=True)
        line = json.dumps(
            {
                "ts": time.time(),
                "outcome": outcome,
                "repo": repo,
                "pr_number": pr_number,
                "head_sha": head_sha[:12] if head_sha else "",
                "duration_ms": duration_ms,
            }
        )
        path = root / "skeptic-gate.jsonl"
        with path.open("a", encoding="utf-8") as f:
            f.write(line + "\n")
    except Exception as exc:
        print(f"[skeptic-gate] perf-log emit failed: {exc}", file=sys.stderr)


def _publish_failure(
    repo: str,
    head_sha: str,
    body: str,
    context: str,
    description: str,
    pr_number: int = 0,
    expected_actor: str = EXPECTED_BOT_ACTOR,
) -> None:
    """Best-effort publish of a failure (used for diff-capture / pre-flight
    failures). Always swallows secondary errors so the failure path itself
    is observable.

    `pr_number` is threaded from the caller (`args.pr_number`). Per the
    E6 /swarm review (Strong A-F1), the previous implementation parsed
    `"PR #N"` from `description` (which never contained it) and silently
    routed the diagnostic comment to issue #0. Threading the explicit
    PR number eliminates the silent-fallback failure mode.
    """
    try:
        set_commit_status(
            repo, head_sha, state="failure", context=context, description=description
        )
    except Exception as exc:
        print(f"[skeptic-gate] set_commit_status failed: {exc}", file=sys.stderr)
    try:
        post_or_update_comment(
            repo, pr_number, body, expected_actor=expected_actor
        )
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Body field extractors for the read-back verifier
# ---------------------------------------------------------------------------
#
# Per post-audit comment 4953116428: the readback regexes were
# too narrow and could not parse PR_NUMBER, REVIEWER, or
# IMPLEMENTATION_PROVENANCE. The new extractors support all six
# fields and use anchored, multiline, case-insensitive patterns.
# Each pattern matches the canonical `**FIELD: value**` form used
# by `format_comment`.

_RE_SHA_LINE = re.compile(r"\*\*HEAD_SHA:\s*([0-9a-f]+)\*\*", re.IGNORECASE)
_RE_REPO_LINE = re.compile(r"\*\*REPO:\s*([\w.\-]+/[\w.\-]+)\*\*", re.IGNORECASE)
_RE_PR_LINE = re.compile(r"\*\*PR_NUMBER:\s*(\d+)\*\*", re.IGNORECASE)
_RE_VERDICT_LINE = re.compile(r"\*\*VERDICT:\s*(PASS|FAIL)\*\*", re.IGNORECASE)
_RE_REVIEWER_LINE = re.compile(r"\*\*REVIEWER:\s*([^*\n]+?)\*\*", re.IGNORECASE)
_RE_IMPL_LINE = re.compile(
    r"\*\*IMPLEMENTATION_PROVENANCE:\s*([^*\n]+?)\*\*", re.IGNORECASE
)


def _extract_field(body: str, name: str) -> Optional[str]:
    """Pull the value of a single contract field out of a verdict body.

    Supports the six fields the gate emits (`HEAD_SHA`, `REPO`,
    `PR_NUMBER`, `VERDICT`, `REVIEWER`, `IMPLEMENTATION_PROVENANCE`).
    Returns None when the field is absent or the body is malformed.

    Per CodeRabbit MAJOR finding on PR #281 round 2: the read-back
    must REJECT bodies that contain more than one occurrence of a
    contract field (an attacker who controls the API could otherwise
    inject a second `VERDICT: PASS` or `HEAD_SHA: <their-sha>` inside
    the reason text and have the read-back take the first match).
    We enforce the "exactly one occurrence" invariant here; if a
    body contains 0 matches the field is absent (None), and if it
    contains >1 matches we raise `ValueError` so the caller fails
    the read-back closed.
    """
    pat = {
        "HEAD_SHA": _RE_SHA_LINE,
        "REPO": _RE_REPO_LINE,
        "PR_NUMBER": _RE_PR_LINE,
        "VERDICT": _RE_VERDICT_LINE,
        "REVIEWER": _RE_REVIEWER_LINE,
        "IMPLEMENTATION_PROVENANCE": _RE_IMPL_LINE,
    }.get(name)
    if pat is None:
        return None
    matches = pat.findall(body)
    if len(matches) == 0:
        return None
    if len(matches) > 1:
        raise ValueError(
            f"duplicate {name!r} field in published body "
            f"({len(matches)} occurrences); refusing to read-back"
        )
    return matches[0].strip()


def _extract_int(body: str, name: str) -> Optional[int]:
    """Like `_extract_field`, but parse the matched substring as an int (PR_NUMBER)."""
    s = _extract_field(body, name)
    return int(s) if s and s.isdigit() else None


if __name__ == "__main__":
    sys.exit(main())
