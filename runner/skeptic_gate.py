"""SHA-bound Skeptic gate for the dark-factory 7-green policy (issue #278).

Public surface
--------------
- `parse_verdict(output)`     — pull `VERDICT:` and `HEAD_SHA:` lines out of the
                                reviewer's stdout. Returns `None` on missing /
                                malformed input (fail-closed contract).
- `bind_to_pr(parsed, ...)`   — verify the parsed verdict binds to the
                                current PR (SHA / repo / number all match).
                                The headline invariant: **stale-SHA PASS must
                                never satisfy a newer head**.
- `format_comment(...)`       — render the idempotent bot comment body. Always
                                contains the upsert marker so the workflow's
                                `gh api` upsert can find and replace the prior
                                comment instead of creating a new one.
- `evaluate(review_output, review_error, ...)` — deterministic verdict-binding
                                step used by the workflow's `skeptic_gate.py`
                                CLI. Returns a `SkepticResult` with
                                `check_state` ∈ {`success`, `failure`},
                                `verdict`, and a `comment_body` ready to post.
- `build_prompt(...)`         — assemble the reviewer prompt (PR context +
                                diff + required output contract). Pure
                                string assembly; no judgment calls.

ZFC compliance
--------------
The reviewer (a non-Claude CLI) is the one that judges the diff. This module
is **only** allowed to validate the structured verdict, bind the SHA, and
shape the comment. There is no `if text.contains("...")` routing, no scoring,
no semantic classification of the diff. Failures flow from missing or
malformed structured fields, never from the deterministic code's opinion
of the diff.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Literal, Optional, TypedDict


# Unique HTML marker used by the GitHub comment upsert logic. Any prior bot
# comment with this marker is the one we replace; comments without it are
# not ours to touch.
MARKER = "<!-- skeptic-gate-verdict -->"

# Anchored regexes. We deliberately use `^...$` with re.MULTILINE so the
# verdict line must be on its own line — this prevents an attacker (or an
# over-eager reviewer) from sneaking a "VERDICT: PASS" inside a code block
# or comment. Case-insensitive verdict token; SHA is normalized to lowercase.
_VERDICT_RE = re.compile(r"^\s*VERDICT\s*:\s*(PASS|FAIL)\s*$", re.MULTILINE | re.IGNORECASE)
_SHA_RE = re.compile(r"^\s*HEAD_SHA\s*:\s*([0-9a-f]{7,64})\s*$", re.MULTILINE | re.IGNORECASE)
_REPO_RE = re.compile(r"^\s*REPO\s*:\s*([\w.\-]+/[\w.\-]+)\s*$", re.MULTILINE)
_PR_RE = re.compile(r"^\s*PR_NUMBER\s*:\s*(\d+)\s*$", re.MULTILINE)
_REASON_RE = re.compile(r"^\s*REASON\s*:\s*(.+?)\s*$", re.MULTILINE)


@dataclass(frozen=True)
class ParsedVerdict:
    verdict: Literal["PASS", "FAIL"]
    head_sha: str
    repo: Optional[str]
    pr_number: Optional[int]
    raw_excerpt: str
    reason: Optional[str] = None


@dataclass(frozen=True)
class ValidationResult:
    """Outcome of `bind_to_pr`.

    `ok=True` means the parsed verdict is consistent with the current PR
    context and the verdict can be honored. `ok=False` means it must be
    rejected (stale SHA, wrong repo, wrong PR number) — `verdict` is left
    as None so the caller cannot accidentally propagate a stale PASS.
    """

    ok: bool
    reason: str
    verdict: Optional[Literal["PASS", "FAIL"]] = None
    parsed: Optional[ParsedVerdict] = None


@dataclass(frozen=True)
class SkepticResult:
    """What the deterministic evaluator hands back to the workflow shell."""

    check_state: Literal["success", "failure"]
    verdict: Optional[Literal["PASS", "FAIL"]]
    reason: str  # human-readable; lands in the comment and the commit status
    comment_body: str
    parsed: Optional[ParsedVerdict] = None


# ----------------------------------------------------------------------------
# Parsing
# ----------------------------------------------------------------------------


def parse_verdict(output: object) -> Optional[ParsedVerdict]:
    """Extract a structured verdict from a reviewer's free-form stdout.

    Returns `None` if the output is not a string, or if it is missing either
    of the two required fields (`VERDICT` or `HEAD_SHA`), or if either field
    is malformed. Returning `None` triggers fail-closed behavior downstream.
    """
    if not isinstance(output, str):
        return None
    verdict_match = _VERDICT_RE.search(output)
    sha_match = _SHA_RE.search(output)
    if verdict_match is None or sha_match is None:
        return None
    verdict = verdict_match.group(1).upper()
    if verdict not in ("PASS", "FAIL"):
        # Defensive — the regex already constrains this, but be explicit.
        return None
    head_sha = sha_match.group(1).lower()
    repo_match = _REPO_RE.search(output)
    pr_match = _PR_RE.search(output)
    reason_match = _REASON_RE.search(output)
    return ParsedVerdict(
        verdict=verdict,  # type: ignore[arg-type]
        head_sha=head_sha,
        repo=repo_match.group(1) if repo_match else None,
        pr_number=int(pr_match.group(1)) if pr_match else None,
        reason=reason_match.group(1).strip() if reason_match else None,
        raw_excerpt=output[:500],
    )


# ----------------------------------------------------------------------------
# Binding
# ----------------------------------------------------------------------------


def bind_to_pr(
    parsed: ParsedVerdict,
    *,
    expected_repo: str,
    expected_pr_number: int,
    expected_head_sha: str,
) -> ValidationResult:
    """Validate that the parsed verdict matches the current PR head.

    Stale-SHA detection is the headline invariant. We compare the SHA the
    reviewer bound to against `expected_head_sha` (the PR's current head).
    A mismatch means the reviewer reviewed a different (older) commit; the
    verdict cannot be honored, even if it said PASS.
    """
    expected_head_sha_norm = expected_head_sha.lower()
    if parsed.head_sha != expected_head_sha_norm:
        return ValidationResult(
            ok=False,
            reason=(
                f"stale SHA: verdict binds to {parsed.head_sha[:12]}, "
                f"current head is {expected_head_sha_norm[:12]}"
            ),
        )
    if parsed.repo is not None and parsed.repo != expected_repo:
        return ValidationResult(
            ok=False,
            reason=f"repo mismatch: verdict says {parsed.repo}, expected {expected_repo}",
        )
    if parsed.pr_number is not None and parsed.pr_number != expected_pr_number:
        return ValidationResult(
            ok=False,
            reason=(
                f"PR number mismatch: verdict says {parsed.pr_number}, "
                f"expected {expected_pr_number}"
            ),
        )
    return ValidationResult(
        ok=True,
        reason="verdict binds to expected PR head",
        verdict=parsed.verdict,
        parsed=parsed,
    )


# ----------------------------------------------------------------------------
# Comment formatting (idempotent upsert via MARKER)
# ----------------------------------------------------------------------------


def comment_marker() -> str:
    return MARKER


def format_comment(
    *,
    verdict: Literal["PASS", "FAIL"],
    head_sha: str,
    expected_head_sha: str,
    repo: str,
    pr_number: int,
    reviewer: str,
    reason: str = "",
) -> str:
    """Render the bot comment body.

    The `MARKER` HTML comment is always present so the GitHub upsert logic
    can find the prior comment and replace it. A stale-SHA PASS is preserved
    verbatim in the body (so the audit trail shows what the reviewer said)
    but is visually marked STALE so the gate consumer knows not to honor it.
    """
    head_sha_norm = head_sha.lower()
    expected_norm = expected_head_sha.lower()
    stale = head_sha_norm != expected_norm

    # When stale, the actual gate state is FAIL even if the reviewer said PASS.
    display_state = verdict
    if stale:
        display_state = "FAIL"  # type: ignore[assignment]

    stale_block = ""
    if stale:
        stale_block = (
            "\n> ⚠️ **STALE VERDICT** — this comment binds to "
            f"`{head_sha_norm[:12]}` but the current PR head is "
            f"`{expected_norm[:12]}`. The gate treats this as **FAIL**.\n"
        )

    reason_block = f"\n**Reason:** {reason}\n" if reason else ""

    return (
        f"{MARKER}\n"
        f"## Skeptic Gate — `{display_state}`\n\n"
        f"**VERDICT: {verdict}**\n"
        f"**HEAD_SHA: {head_sha_norm}**\n"
        f"**REPO: {repo}**\n"
        f"**PR_NUMBER: {pr_number}**\n"
        f"**REVIEWER: {reviewer}**\n"
        f"{reason_block}"
        f"{stale_block}\n"
        f"---\n"
        f"<sub>Skeptic gate verdict for PR #{pr_number} at head "
        f"`{head_sha_norm[:12]}`. Stale-SHA verdicts never satisfy a newer "
        f"head; the most recent run on the current head is the only one that "
        f"counts.</sub>\n"
    )


# ----------------------------------------------------------------------------
# evaluate — the deterministic verdict-binding step the workflow calls
# ----------------------------------------------------------------------------


def evaluate(
    *,
    review_output: Optional[str] = None,
    review_error: Optional[str] = None,
    repo: str,
    pr_number: int,
    head_sha: str,
    base_sha: str,  # noqa: ARG001 — kept in the signature for future use
    diff: str,  # noqa: ARG001 — kept in the signature for future use
    reviewer: str,
) -> SkepticResult:
    """Decide the gate outcome from the reviewer's stdout (or its absence).

    - `review_output` is the reviewer's stdout if it succeeded.
    - `review_error` is the captured error/timeout/missing-binary message.
      At least one is meaningful; if both are None/empty, the reviewer did
      not run at all and the gate fails closed.

    Returns a `SkepticResult` whose `check_state` is either `success` (only
    when a parsed verdict binds to the current head) or `failure` for
    every other case: missing reviewer, malformed output, stale SHA, or
    explicit FAIL.
    """
    # Case 1: reviewer did not produce any output OR errored out.
    if review_error or not (review_output and review_output.strip()):
        reason = "reviewer unavailable"
        if review_error:
            reason = f"reviewer unavailable: {review_error.strip()[:200]}"
        elif not review_output:
            reason = "reviewer produced no output"
        else:
            reason = "reviewer produced empty output"
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=reviewer,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
        )

    # Case 2: reviewer output is unparseable.
    parsed = parse_verdict(review_output)
    if parsed is None:
        reason = (
            "reviewer output was unparseable "
            "(missing VERDICT: or HEAD_SHA: line, or malformed SHA)"
        )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=reviewer,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
        )

    # Case 3: parsed, now bind to the current PR head. A stale SHA here is
    # the headline invariant — we MUST fail closed even if the reviewer
    # said PASS.
    binding = bind_to_pr(
        parsed,
        expected_repo=repo,
        expected_pr_number=pr_number,
        expected_head_sha=head_sha,
    )
    if not binding.ok:
        # The body is rendered with the reviewer's stated verdict so the
        # audit trail is honest, but `check_state` is FAIL because the
        # binding failed.
        body = format_comment(
            verdict=parsed.verdict,
            head_sha=parsed.head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=reviewer,
            reason=binding.reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=binding.reason,
            comment_body=body,
            parsed=parsed,
        )

    # Case 4: bound successfully. The verdict (PASS or FAIL) is propagated.
    body = format_comment(
        verdict=parsed.verdict,
        head_sha=parsed.head_sha,
        expected_head_sha=head_sha,
        repo=repo,
        pr_number=pr_number,
        reviewer=reviewer,
        reason=parsed.reason or "",
    )
    return SkepticResult(
        check_state="success" if parsed.verdict == "PASS" else "failure",
        verdict=parsed.verdict,
        reason=parsed.reason or "verdict bound to current head",
        comment_body=body,
        parsed=parsed,
    )


# ----------------------------------------------------------------------------
# Prompt building — pure string assembly, no judgment
# ----------------------------------------------------------------------------


_PROMPT_TEMPLATE = """You are an INDEPENDENT SKEPTIC reviewer for the dark-factory
PR-gate. You are NOT the implementing model. You did not write this diff.
Your only job is to emit a single structured verdict bound to the exact
commit SHA you were shown.

# PR context

- Repository: {repo}
- PR number: {pr_number}
- Head SHA: {head_sha}
- Base SHA: {base_sha}

# Diff under review

```
{diff}
```

# Output contract — REQUIRED, no extra prose

Emit EXACTLY this format, on its own lines, with the values substituted:

    VERDICT: <PASS|FAIL>
    HEAD_SHA: {head_sha}
    REPO: {repo}
    PR_NUMBER: {pr_number}
    REASON: <one-sentence justification>

Rules:
- Use `VERDICT: PASS` only if the diff is small, well-scoped, and free of
  destructive operations, secret leakage, or out-of-scope changes. Otherwise
  use `VERDICT: FAIL`.
- `HEAD_SHA` MUST equal the head SHA above. Do not invent a different SHA.
- Do not include any other text. The deterministic gate will reject anything
  that does not match this contract exactly.
"""


def build_prompt(
    *,
    repo: str,
    pr_number: int,
    head_sha: str,
    base_sha: str,
    diff: str,
) -> str:
    """Assemble the prompt sent to the independent reviewer CLI.

    Pure string assembly — no model call, no judgment. The reviewer model
    is the one that emits the structured verdict; this function only frames
    the request so the contract is unambiguous.
    """
    return _PROMPT_TEMPLATE.format(
        repo=repo,
        pr_number=pr_number,
        head_sha=head_sha,
        base_sha=base_sha,
        diff=diff,
    )


__all__ = [
    "MARKER",
    "ParsedVerdict",
    "SkepticResult",
    "ValidationResult",
    "bind_to_pr",
    "build_prompt",
    "comment_marker",
    "evaluate",
    "format_comment",
    "parse_verdict",
]
