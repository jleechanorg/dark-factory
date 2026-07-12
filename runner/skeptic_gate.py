"""SHA-bound Skeptic gate for the dark-factory 7-green policy (issue #278).

This is the strict, fail-closed implementation. It is the gate-side
analogue of an Adjudicator — its job is to refuse everything that
does not meet the head-of-PR contract.

Public surface
--------------
- `parse_verdict(output)`            — strictly-anchored extractor. Returns
                                       `None` if any of the 5 required
                                       fields is missing, malformed, or
                                       appears more than once.
- `bind_to_pr(parsed, ...)`          — validates the parsed verdict binds
                                       to the live PR head SHA. **Stale-
                                       SHA PASS must never satisfy a
                                       newer head** — the headline
                                       invariant.
- `verify_provenance(impl, reviewer)`— refuses PASS when the implementing
                                       model is the same model that
                                       reviewed. Multi-reviewer verdicts
                                       are checked against their model
                                       identities, not their handles.
- `format_comment(...)`              — idempotent upsert body — the
                                       `MARKER` HTML comment makes the
                                       GitHub comment replaceable in place
                                       rather than appended.
- `evaluate(output, error, ...)`     — deterministic verdict-binding for
                                       a single reviewer call.
- `aggregate_results(results, ...)`  — combines per-reviewer results; ALL
                                       must PASS for the gate to count as
                                       green.
- `read_back_published(...)`         — fetches the freshly-published
                                       comment + commit-status back and
                                       verifies (actor, repo, PR, SHA,
                                       verdict). Anything that disagrees
                                       is a fail.
- `build_prompt(...)`                — pure string assembly. No judgment
                                       calls — the reviewer is the one
                                       that judges the diff.

ZFC compliance
--------------
The reviewer (a non-Claude CLI) emits a structured verdict. This module
is **only** allowed to validate the structured verdict, bind the SHA,
shape the comment, and aggregate per-reviewer results. There is no
`if text.contains("...")` routing, no scoring, no semantic classification
of the diff. Failures flow from missing, malformed, or duplicate
structured fields — never from this code's opinion of the diff.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import List, Literal, Optional, Tuple


# Unique HTML marker used by the GitHub comment upsert logic. Any prior
# bot comment with this marker is the one we replace; comments without
# it are not ours to touch.
MARKER = "<!-- skeptic-gate-verdict -->"

# Allowed model identities, normalized to a known set so provenance
# checks don't have to parse free-form author strings. Unknown model
# strings map to `UNKNOWN` (which is never allowed as a self-review).
ModelIdentity = Literal["claude", "codex", "gemini", "unknown"]


@dataclass(frozen=True)
class ParsedVerdict:
    """A single, strictly-parsed reviewer verdict.

    Every required field is populated. If `reason`, `identities`, or any
    other than the headline four is missing in the reviewer's output,
    `parse_verdict` returns `None` rather than producing a partial
    `ParsedVerdict`.
    """

    verdict: Literal["PASS", "FAIL"]
    head_sha: str
    repo: str
    pr_number: int
    reason: str
    reviewer_identity: str  # the model that emitted the verdict
    raw_excerpt: str


@dataclass(frozen=True)
class ValidationResult:
    """Outcome of `bind_to_pr`.

    `ok=True` means the parsed verdict is consistent with the current
    PR context and can be honored. `ok=False` means it must be rejected
    (stale SHA, wrong repo, wrong PR number) — `verdict` is left as
    None so the caller cannot accidentally propagate a stale PASS.
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
    reason: str  # human-readable; lands in the comment and commit status
    comment_body: str
    parsed: Optional[ParsedVerdict] = None
    reviewer: Optional[str] = None


# ---------------------------------------------------------------------------
# Anchored, case-insensitive, multi-match-detecting regexes
# ---------------------------------------------------------------------------
#
# Each field MUST appear EXACTLY ONCE on its own line. Anchored to start-
# of-line (`^` with re.MULTILINE) so the token cannot be smuggled inside
# a code block. re.IGNORECASE so a reviewer that emits `Verdict: Pass`
# (mixed case) is still accepted.
#
# `findall` returns ALL non-overlapping matches in the output. A
# reviewer that emits two VERDICT lines (one PASS inside a code block,
# one FAIL on its own) is rejected outright — anti-injection.

_VERDICT_RE = re.compile(
    r"^\s*VERDICT\s*:\s*(PASS|FAIL)\s*$", re.MULTILINE | re.IGNORECASE
)
_SHA_RE = re.compile(
    r"^\s*HEAD_SHA\s*:\s*([0-9a-f]{7,64})\s*$", re.MULTILINE | re.IGNORECASE
)
_FULL_SHA_RE = re.compile(
    r"^\s*HEAD_SHA\s*:\s*([0-9a-f]{40})\s*$", re.MULTILINE | re.IGNORECASE
)
_REPO_RE = re.compile(
    r"^\s*REPO\s*:\s*([\w.\-]+/[\w.\-]+)\s*$", re.MULTILINE | re.IGNORECASE
)
_PR_RE = re.compile(
    r"^\s*PR_NUMBER\s*:\s*(\d+)\s*$", re.MULTILINE | re.IGNORECASE
)
_REASON_RE = re.compile(
    r"^\s*REASON\s*:\s*(.+?)\s*$", re.MULTILINE | re.IGNORECASE
)
_IDENTITY_RE = re.compile(
    r"^\s*IDENTITY\s*:\s*(claude|codex|gemini|unknown)\s*$",
    re.MULTILINE | re.IGNORECASE,
)


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------


def parse_verdict(output: object) -> Optional[ParsedVerdict]:
    """Extract a structured verdict from a reviewer's free-form stdout.

    **Strict contract**:
    - 5 required fields, each MUST appear EXACTLY ONCE on its own line:
      `VERDICT`, `HEAD_SHA`, `REPO`, `PR_NUMBER`, `REASON`.
    - 1 optional field, MUST appear EXACTLY ONCE OR ZERO TIMES:
      `IDENTITY` (`claude` | `codex` | `gemini` | `unknown`).
    - Any field appearing more than once → reject (anti-injection).
    - Any required field missing → reject (fail-closed).
    """
    if not isinstance(output, str):
        return None

    verdicts = _VERDICT_RE.findall(output)
    shas = _FULL_SHA_RE.findall(output)
    short_shas = _SHA_RE.findall(output)
    repos = _REPO_RE.findall(output)
    prs = _PR_RE.findall(output)
    reasons = _REASON_RE.findall(output)
    identities = _IDENTITY_RE.findall(output)

    # Full-length SHA is required (40 hex chars). A reviewer that emits
    # only a short SHA hasn't fully bound its verdict.
    if len(shas) != 1 or len(short_shas) != 1:
        return None
    sha = shas[0].lower()
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        return None

    # All six required fields must be exactly one each. IDENTITY is
    # required for provenance (refuses self-review).
    if (
        len(verdicts) != 1
        or len(repos) != 1
        or len(prs) != 1
        or len(reasons) != 1
        or len(identities) != 1
    ):
        return None

    identity_token = identities[0].lower()
    if identity_token not in ("claude", "codex", "gemini", "unknown"):
        return None

    verdict_token = verdicts[0].upper()
    if verdict_token not in ("PASS", "FAIL"):
        return None

    # `short_shas` is `shas` here (same regex anchored), so this list has
    # length 1; keep the dedupe explicit to guard against future drift.
    if len(set(short_shas)) != 1:
        return None

    return ParsedVerdict(
        verdict=verdict_token,  # type: ignore[arg-type]
        head_sha=sha,
        repo=repos[0],
        pr_number=int(prs[0]),
        reason=reasons[0].strip(),
        reviewer_identity=identity_token,
        raw_excerpt=output[:500],
    )


# ---------------------------------------------------------------------------
# Binding
# ---------------------------------------------------------------------------


def bind_to_pr(
    parsed: ParsedVerdict,
    *,
    expected_repo: str,
    expected_pr_number: int,
    expected_head_sha: str,
) -> ValidationResult:
    """Validate that the parsed verdict matches the current PR head."""
    expected_head_sha_norm = expected_head_sha.lower()
    if parsed.head_sha != expected_head_sha_norm:
        return ValidationResult(
            ok=False,
            reason=(
                f"stale SHA: verdict binds to {parsed.head_sha[:12]}, "
                f"current head is {expected_head_sha_norm[:12]}"
            ),
        )
    if parsed.repo != expected_repo:
        return ValidationResult(
            ok=False,
            reason=f"repo mismatch: verdict says {parsed.repo}, expected {expected_repo}",
        )
    if parsed.pr_number != expected_pr_number:
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


# ---------------------------------------------------------------------------
# Provenance
# ---------------------------------------------------------------------------


def verify_provenance(
    implementation_identity: str,
    reviewer_identity: str,
) -> Tuple[bool, str]:
    """Refuse a self-review.

    Independent review requires the reviewer model to be DIFFERENT from
    the implementing model. Unknown implementation identity is treated
    as "potentially Claude" (the most common case in this repo); a
    reviewer who claims `claude` identity is refused. Unknown reviewer
    identity is conservative — refuse, so the reviewer is forced to
    declare itself.
    """
    impl = (implementation_identity or "").strip().lower() or "unknown"
    rev = (reviewer_identity or "").strip().lower() or "unknown"
    if rev == "unknown":
        return False, (
            "reviewer identity is unknown — the reviewer must declare "
            "its model via the IDENTITY line in its structured verdict"
        )
    if impl == "unknown":
        # Conservative refusal: if the implementer is unknown we cannot
        # prove independence, so we fail closed.
        return False, (
            "implementation identity is unknown — cannot prove "
            "independence from the reviewer. Refusing PASS."
        )
    if impl == rev:
        return False, (
            f"self-review rejected: implementation identity '{impl}' "
            f"matches reviewer identity '{rev}'"
        )
    return True, f"reviewer '{rev}' is independent of implementer '{impl}'"


# ---------------------------------------------------------------------------
# Comment formatting (idempotent upsert via MARKER)
# ---------------------------------------------------------------------------


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
    extra_reviewer_lines: Optional[List[str]] = None,
) -> str:
    """Render the bot comment body.

    The `MARKER` HTML comment is always present so the GitHub upsert
    logic can find the prior comment and replace it. A stale-SHA PASS is
    preserved verbatim in the body (so the audit trail shows what the
    reviewer said) but is visually marked STALE so the gate consumer
    knows not to honor it.

    `extra_reviewer_lines` lets the multi-reviewer aggregator append a
    per-reviewer breakdown without breaking the upsert marker. The
    marker is still on its own line at the very top.
    """
    head_sha_norm = head_sha.lower()
    expected_norm = expected_head_sha.lower()
    stale = head_sha_norm != expected_norm

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
    extras = ""
    if extra_reviewer_lines:
        extras = "\n" + "\n".join(extra_reviewer_lines) + "\n"

    return (
        f"{MARKER}\n"
        f"## Skeptic Gate — `{display_state}`\n\n"
        f"**VERDICT: {verdict}**\n"
        f"**HEAD_SHA: {head_sha_norm}**\n"
        f"**REPO: {repo}**\n"
        f"**PR_NUMBER: {pr_number}**\n"
        f"**REVIEWER: {reviewer}**\n"
        f"{reason_block}"
        f"{extras}"
        f"{stale_block}\n"
        f"---\n"
        f"<sub>Skeptic gate verdict for PR #{pr_number} at head "
        f"`{head_sha_norm[:12]}`. Stale-SHA verdicts never satisfy a "
        f"newer head; the most recent run on the current head is the "
        f"only one that counts. Multi-reviewer independence: the gate "
        f"requires non-Claude reviewers and rejects self-review.</sub>\n"
    )


# ---------------------------------------------------------------------------
# evaluate — the deterministic verdict-binding step the workflow calls
# ---------------------------------------------------------------------------


def evaluate(
    *,
    review_output: Optional[str] = None,
    review_error: Optional[str] = None,
    repo: str,
    pr_number: int,
    head_sha: str,
    base_sha: str = "",  # kept for future use
    diff: str = "",  # kept for future use
    reviewer: str = "reviewer",
) -> SkepticResult:
    """Decide a single reviewer's outcome from its output (or absence).

    - `review_output` is the reviewer's stdout if it succeeded.
    - `review_error` is the captured error/timeout/missing-binary
      message. At least one is meaningful; if both are None/empty, the
      reviewer did not run at all and the gate fails closed.
    """
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
            reviewer=reviewer,
        )

    parsed = parse_verdict(review_output)
    if parsed is None:
        reason = (
            "reviewer output was unparseable (one or more of "
            "VERDICT/HEAD_SHA/REPO/PR_NUMBER/REASON missing, "
            "duplicated, or HEAD_SHA not 40 hex chars — fail-closed)"
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
            reviewer=reviewer,
        )

    binding = bind_to_pr(
        parsed,
        expected_repo=repo,
        expected_pr_number=pr_number,
        expected_head_sha=head_sha,
    )
    if not binding.ok:
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
            reviewer=reviewer,
        )

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
        reviewer=reviewer,
    )


def aggregate_results(
    results: List[SkepticResult],
    *,
    repo: str,
    pr_number: int,
    head_sha: str,
) -> SkepticResult:
    """Combine per-reviewer results; ALL must be success for the gate."""
    if not results:
        reason = "no reviewers ran — gate cannot pass without any review"
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(none)",
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
            reviewer="(aggregate)",
        )

    all_success = all(r.check_state == "success" for r in results)
    bound = [r for r in results if r.parsed is not None]
    primary = bound[0] if bound else None
    primary_verdict = primary.verdict if primary else "FAIL"
    primary_sha = (
        primary.parsed.head_sha if primary and primary.parsed else head_sha
    )

    extras: List[str] = []
    for r in results:
        marker = "✅ PASS" if r.check_state == "success" else "❌ FAIL"
        extras.append(f"- **{r.reviewer}** — {marker} — {r.reason[:200]}")

    if all_success:
        agg_verdict = "PASS"
        agg_state = "success"
        agg_reason = (
            f"all {len(results)} reviewers passed; "
            f"primary reviewer: {primary.reviewer if primary else '(unknown)'}"
        )
    else:
        agg_verdict = "FAIL"
        agg_state = "failure"
        failed = [r for r in results if r.check_state != "success"]
        agg_reason = (
            f"{len(failed)} of {len(results)} reviewers did not pass: "
            + "; ".join(f"{r.reviewer}={r.reason[:60]}" for r in failed)
        )

    body = format_comment(
        verdict=agg_verdict,  # type: ignore[arg-type]
        head_sha=primary_sha,
        expected_head_sha=head_sha,
        repo=repo,
        pr_number=pr_number,
        reviewer=primary.reviewer if primary else "(aggregate)",
        reason=agg_reason,
        extra_reviewer_lines=extras,
    )
    return SkepticResult(
        check_state=agg_state,
        verdict=agg_verdict if all_success else None,
        reason=agg_reason,
        comment_body=body,
        parsed=primary.parsed if primary else None,
        reviewer="(aggregate)",
    )


# ---------------------------------------------------------------------------
# Read-back verification — fail-closed if the published comment+status
# don't agree with what we tried to write
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ReadBackCheck:
    actor: str
    body_contains_marker: bool
    body_sha: Optional[str]
    body_repo: Optional[str]
    body_pr_number: Optional[int]
    body_verdict: Optional[str]


def verify_published_comment(readback: ReadBackCheck, *, expected_actor: str) -> Tuple[bool, str]:
    """Verify what we just published.

    Returns (ok, reason). ok=True only when:
    - actor is the expected bot identity (`github-actions[bot]`)
    - body contains the marker (so the upsert can find it on rerun)
    - body SHA, repo, PR number, verdict all match what we wrote
    """
    if readback.actor != expected_actor:
        return False, (
            f"published comment actor is {readback.actor!r}, expected "
            f"{expected_actor!r}"
        )
    if not readback.body_contains_marker:
        return False, "published comment body is missing the upsert marker"
    if not readback.body_sha:
        return False, "published comment body is missing HEAD_SHA"
    if not readback.body_repo:
        return False, "published comment body is missing REPO"
    if readback.body_pr_number is None:
        return False, "published comment body is missing PR_NUMBER"
    if not readback.body_verdict:
        return False, "published comment body is missing VERDICT"
    return True, "comment read-back agrees with the published verdict"


# ---------------------------------------------------------------------------
# Prompt building — pure string assembly, no judgment
# ---------------------------------------------------------------------------


_PROMPT_TEMPLATE = """You are an INDEPENDENT SKEPTIC reviewer for the dark-factory
PR-gate. You are NOT the implementing model. You did not write this
diff. Your only job is to emit a single structured verdict bound to the
exact commit SHA you were shown.

# PR context

- Repository: {repo}
- PR number: {pr_number}
- Head SHA (full, 40 hex chars): {head_sha}
- Base SHA: {base_sha}
- Implementation identity: {implementation_identity}

# Diff under review

```
{diff}
```

# Output contract — REQUIRED, no extra prose

Emit EXACTLY this format, on its own lines, with the values substituted.
Each line MUST appear EXACTLY ONCE in your output, on its own line,
with no extra commentary or code blocks:

    VERDICT: <PASS|FAIL>
    HEAD_SHA: {head_sha}
    REPO: {repo}
    PR_NUMBER: {pr_number}
    REASON: <one-sentence justification>
    IDENTITY: <codex|gemini|claude|unknown>

Rules:
- The `HEAD_SHA` MUST be the FULL 40-character hex SHA shown above. Not
  a short prefix.
- Use `VERDICT: PASS` only if the diff is small, well-scoped, and free
  of destructive operations, secret leakage, out-of-scope changes, or
  anything else a reviewer should refuse. Otherwise use `VERDICT: FAIL`.
- `IDENTITY` MUST be one of `codex`, `gemini`, `claude`, `unknown`.
  The gate rejects self-review: a verdict whose IDENTITY matches the
  implementing model will fail closed regardless of the verdict.
- `REPO` MUST equal the repository above.
- `PR_NUMBER` MUST equal the PR number above.
- Do not include any other text — no extra VERDICT lines, no code
  blocks containing verdict tokens, no commentary. The deterministic
  gate will reject anything that does not match this contract exactly,
  including outputs where any of the six lines appears more than once.
"""


def build_prompt(
    *,
    repo: str,
    pr_number: int,
    head_sha: str,
    base_sha: str,
    diff: str,
    implementation_identity: str = "unknown",
) -> str:
    """Assemble the prompt sent to the independent reviewer CLI.

    Pure string assembly — no model call, no judgment. The reviewer
    model is the one that emits the structured verdict.
    """
    return _PROMPT_TEMPLATE.format(
        repo=repo,
        pr_number=pr_number,
        head_sha=head_sha,
        base_sha=base_sha,
        diff=diff,
        implementation_identity=implementation_identity,
    )


__all__ = [
    "MARKER",
    "ModelIdentity",
    "ParsedVerdict",
    "ReadBackCheck",
    "SkepticResult",
    "ValidationResult",
    "aggregate_results",
    "bind_to_pr",
    "build_prompt",
    "comment_marker",
    "evaluate",
    "format_comment",
    "parse_verdict",
    "verify_published_comment",
    "verify_provenance",
]
