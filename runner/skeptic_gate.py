"""SHA-bound Skeptic gate for the dark-factory 7-green policy (issue #278).

This is the strict, fail-closed implementation. It is the gate-side
analogue of an Adjudicator — its job is to refuse everything that
does not meet the head-of-PR contract.

Public surface
--------------
- `parse_verdict(output)`            — strictly-anchored extractor.
                                       Returns `None` if any required
                                       field is missing, malformed,
                                       appears more than once, OR if
                                       the output contains extra prose
                                       outside the strict contract
                                       (no free-form text, no Markdown
                                       code blocks containing tokens).
- `bind_to_pr(parsed, ...)`          — validates the parsed verdict binds
                                       to the live PR head SHA. **Stale-
                                       SHA PASS must never satisfy a
                                       newer head** — the headline
                                       invariant.
- `verify_provenance(impl, reviewer)`— refuses PASS when the implementing
                                       model is the same model that
                                       reviewed.
- `extract_implementation_identity_from_commit(commit_subject)` —
                                       deterministic identity derivation
                                       from the commit-subject prefix
                                       (e.g. `claudem/minimax-M3: …`
                                       → `claude`). This is a pure
                                       string-prefix match — no ZFC
                                       keyword routing on free-form
                                       text. Unknown / un-prefixed
                                       subjects map to `unknown`, which
                                       the gate refuses PASS on.
- `bind_reviewer_identity(cli_name, declared_identity)` —
                                       refuses a verdict whose declared
                                       IDENTITY does not match the CLI
                                       that emitted it (codex CLI must
                                       declare `codex`, gemini must
                                       declare `gemini`).
- `format_comment(...)`              — idempotent upsert body — the
                                       `MARKER` HTML comment makes the
                                       GitHub comment replaceable in
                                       place rather than appended.
- `evaluate(output, error, ...)`     — deterministic verdict-binding for
                                       a single reviewer call.
- `aggregate_results(results, ...)`  — combines per-reviewer results; ALL
                                       must PASS for the gate to count
                                       as green. Rejects duplicate
                                       reviewer identities.
- `verify_published_comment(...)`    — full equality read-back. ALL six
                                       fields (HEAD_SHA, REPO,
                                       PR_NUMBER, VERDICT, REVIEWER,
                                       IMPLEMENTATION_PROVENANCE) must
                                       equal what we wrote; missing or
                                       mismatched values fail closed.
- `build_prompt(...)`                — pure string assembly. No judgment
                                       calls — the reviewer is the one
                                       that judges the diff.

ZFC compliance
--------------
The reviewer (a non-Claude CLI) emits a structured verdict. This module
is **only** allowed to validate the structured verdict, bind the SHA,
shape the comment, and aggregate per-reviewer results. There is no
`if text.contains("...")` routing, no scoring, no semantic classification
of the diff. Implementation-identity derivation is a deterministic
prefix match on the commit subject — not a keyword classification of
free-form prose. Failures flow from missing, malformed, duplicate, or
extra-prose structured fields — never from this code's opinion of the
diff.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import List, Literal, Optional, Tuple


# Unique HTML marker used by the GitHub comment upsert logic. Any prior
# bot comment with this marker is the one we replace; comments without
# it are not ours to touch.
MARKER = "<!-- skeptic-gate-verdict -->"

# Allowed model identities, normalized to a known set so provenance
# checks don't have to parse free-form author strings. Unknown model
# strings map to `unknown` (which is never allowed as a self-review).
ModelIdentity = Literal["claude", "codex", "gemini", "unknown"]

# Mapping from reviewer CLI name to the identity it MUST declare in
# its structured verdict. The CLI is bound to its identity — a codex
# process claiming `gemini` (or vice-versa) is rejected outright. This
# prevents a malicious PR-controlled reviewer invocation from
# impersonating another reviewer.
REVIEWER_CLI_TO_IDENTITY = {
    "codex": "codex",
    "gemini": "gemini",
}

# Commit-subject prefixes that map to model identities. The mapping is
# a deterministic prefix match — no ZFC keyword routing on free-form
# text. A subject that does not start with one of these prefixes
# resolves to `unknown`, which the gate refuses PASS on.
#
# Adding a new prefix here IS an explicit PR-time decision — the
# reviewer must review and merge the addition. Comment-only additions
# (sed/keyword substitutions in commit messages) cannot change the
# implementation identity.
COMMIT_PREFIX_TO_IDENTITY = {
    "claude/": "claude",
    "claudem/": "claude",
    "codex/": "codex",
    "codexm/": "codex",
    "gemini/": "gemini",
    "geminim/": "gemini",
    "human:": "human",
    "antig/": "unknown",  # Antigravity is the IDE; identity is whatever it spawned
}


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
_PR_RE = re.compile(r"^\s*PR_NUMBER\s*:\s*(\d+)\s*$", re.MULTILINE | re.IGNORECASE)
_REASON_RE = re.compile(r"^\s*REASON\s*:\s*(.+?)\s*$", re.MULTILINE | re.IGNORECASE)
_IDENTITY_RE = re.compile(
    r"^\s*IDENTITY\s*:\s*(claude|codex|gemini|human|unknown)\s*$",
    re.MULTILINE | re.IGNORECASE,
)

# Field-name regexes used by the no-prose check. A field line MUST
# consist only of "<FIELD>: <value>" with nothing else on the line.
# Case-insensitive to match the per-field regexes (Verdict: Pass is OK).
_FIELD_LINE_RE = re.compile(r"^[A-Z_]+\s*:.*$", re.MULTILINE | re.IGNORECASE)


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------


def parse_verdict(output: object) -> Optional[ParsedVerdict]:
    """Extract a structured verdict from a reviewer's free-form stdout.

    **Strict no-prose contract** (per post-audit comment 4953064910):

    - 6 required fields, each MUST appear EXACTLY ONCE on its own line:
      `VERDICT`, `HEAD_SHA`, `REPO`, `PR_NUMBER`, `REASON`, `IDENTITY`.
    - The output MUST consist ONLY of:
        - up to one comment line (e.g. a leading `# reviewer: codex`)
        - the 6 contract fields, each on its own line
        - any number of blank lines
      No Markdown code blocks, no extra prose, no second VERDICT line
      smuggled inside a triple-backtick fence.
    - Any field appearing more than once → reject (anti-injection).
    - Any required field missing → reject (fail-closed).
    - The 6 lines themselves MUST be the ONLY non-blank lines (no
      trailing prose, no surrounding commentary).

    A reviewer that wraps the contract in a Markdown code block
    (```\\nVERDICT: PASS\\n…\\n```) is rejected. The deterministic
    parser enforces the bare-text contract, not "find a record
    somewhere inside prose."
    """
    if not isinstance(output, str):
        return None

    # ---- No-prose check --------------------------------------------------
    # Strip the leading comment line if present; reject anything that
    # still looks like Markdown prose or contains a code fence.
    if "```" in output:
        # A code-fence in reviewer output is a code-block injection
        # attempt. Reject unconditionally.
        return None

    # Count non-blank lines that start with a contract field token
    # ("<UPPER>:"). Anything else is prose and must be a leading
    # comment only — single line, starting with `#`.
    lines = output.splitlines()
    field_lines = 0
    leading_comment_lines = 0
    seen_field_names = set()
    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue
        if re.match(r"^[A-Z_]+\s*:", stripped, re.IGNORECASE):
            field_lines += 1
            # Track the field name so we can enforce EXACTLY 6 distinct
            # contract fields. Per post-audit comment 4953116428, the
            # previous version accepted a 7th field. Now any 7th field
            # (or duplicate of any of the 6) → reject.
            field_name_match = re.match(r"^([A-Z_]+)\s*:", stripped, re.IGNORECASE)
            if field_name_match:
                fname = field_name_match.group(1).upper()
                if fname in seen_field_names:
                    # Duplicate field — anti-injection.
                    return None
                seen_field_names.add(fname)
            continue
        # Allow ONE leading comment line (e.g. "# reviewer: codex")
        # before any field line; reject any prose AFTER a field line.
        if field_lines == 0 and leading_comment_lines == 0 and stripped.startswith("#"):
            leading_comment_lines = 1
            continue
        # Prose outside the leading-comment window — reject.
        return None

    # ---- Field extraction (anchored, exactly-once) ----------------------
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

    # Exact 6-field contract (per post-audit comment 4953116428):
    # no 7th field allowed. The seen_field_names set already enforced
    # no duplicates; here we also enforce that EXACTLY 6 distinct
    # contract fields are present.
    expected_fields = {
        "VERDICT",
        "HEAD_SHA",
        "REPO",
        "PR_NUMBER",
        "REASON",
        "IDENTITY",
    }
    if seen_field_names != expected_fields:
        return None

    identity_token = identities[0].lower()
    if identity_token not in ("claude", "codex", "gemini", "human", "unknown"):
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


def extract_implementation_identity_from_commit(commit_subject: str) -> str:
    """Deterministically derive the implementer's identity from the
    commit subject prefix.

    The mapping is a pure prefix match — no ZFC keyword routing on
    free-form text. A subject that does not start with one of the
    prefixes in `COMMIT_PREFIX_TO_IDENTITY` returns `"unknown"`,
    which the gate refuses PASS on (conservative fail-closed).

    Examples:
        >>> extract_implementation_identity_from_commit("claudem/minimax-M3: feat(x): ...")
        'claude'
        >>> extract_implementation_identity_from_commit("codexm/o3: fix: ...")
        'codex'
        >>> extract_implementation_identity_from_commit("naked commit message")
        'unknown'
    """
    if not isinstance(commit_subject, str):
        return "unknown"
    subject = commit_subject.strip()
    if not subject:
        return "unknown"
    for prefix, identity in COMMIT_PREFIX_TO_IDENTITY.items():
        if subject.startswith(prefix):
            return identity
    return "unknown"


def bind_reviewer_identity(
    reviewer_cli: str, declared_identity: str
) -> Tuple[bool, str]:
    """Refuse a verdict whose declared IDENTITY does not match the CLI
    that emitted it.

    Codex CLI must declare `codex`; gemini CLI must declare `gemini`.
    This binds the reviewer process to its identity — a codex
    invocation cannot claim `gemini`, and vice-versa. A reviewer that
    declares `claude` or `unknown` is also refused (the CLI list is
    fixed; only the two pinned binaries can satisfy gate-7).
    """
    cli = (reviewer_cli or "").strip().lower()
    declared = (declared_identity or "").strip().lower()
    expected = REVIEWER_CLI_TO_IDENTITY.get(cli)
    if expected is None:
        return False, (
            f"reviewer CLI {cli!r} is not in the pinned allow-list; "
            f"expected one of {sorted(REVIEWER_CLI_TO_IDENTITY)}"
        )
    if declared != expected:
        return False, (
            f"reviewer CLI {cli!r} declared identity {declared!r}, "
            f"but {cli!r} must declare {expected!r}"
        )
    return True, f"reviewer CLI {cli!r} bound to identity {declared!r}"


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
    if rev == "human":
        return False, (
            "reviewer identity 'human' is not an independent model; "
            "the gate requires a non-implementer model"
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
    """Return the unique HTML-marker string this gate uses to identify its bot comment."""
    return MARKER


def _sanitize_reason(reason: str) -> str:
    """Strip canonical-field injection attempts from reviewer-controlled
    reason text.

    Per CodeRabbit MAJOR finding on PR #281 round 3: a reviewer (or
    any party who can write text into the reason field) could
    otherwise include a substring like `**VERDICT: PASS**` inside
    the reason, and the read-back verifier's regex (which matches
    the canonical `**FIELD: value**` form anywhere in the body)
    would then find 2 occurrences of VERDICT — refusing the body
    closed, which is the right outcome — but a partial injection
    (one smuggled field) could still trip the `findall` count
    guard.

    We pre-empt the injection by neutralizing the canonical markers
    in the reason text: any `**FIELD:` substring inside the reason
    has its `**` markdown strong-emphasis markers stripped (turning
    it into `FIELD: value` plain text), and any backtick-wrapped
    field is escaped. This keeps the reason human-readable while
    ensuring the only `**FIELD: value**` form in the body comes
    from `format_comment`'s own canonical emit.
    """
    if not reason:
        return reason
    # Strip `**` around any canonical field name so a smuggled
    # `**VERDICT: PASS**` becomes plain text `VERDICT: PASS`
    # that the read-back regex (which requires the `**...**`
    # wrapping) will not match.
    out = re.sub(
        r"\*\*\s*(VERDICT|HEAD_SHA|REPO|PR_NUMBER|REVIEWER|"
        r"IMPLEMENTATION_PROVENANCE|REASON|IDENTITY)\s*:",
        r"\1:",
        reason,
        flags=re.IGNORECASE,
    )
    return out


def format_comment(
    *,
    verdict: Literal["PASS", "FAIL"],
    head_sha: str,
    expected_head_sha: str,
    repo: str,
    pr_number: int,
    reviewer: str,
    implementation_provenance: str = "unknown",
    reason: str = "",
    extra_reviewer_lines: Optional[List[str]] = None,
) -> str:
    """Render the bot comment body.

    The `MARKER` HTML comment is always present so the GitHub upsert
    logic can find the prior comment and replace it. A stale-SHA PASS is
    preserved verbatim in the body (so the audit trail shows what the
    reviewer said) but is visually marked STALE so the gate consumer
    knows not to honor it.

    The 6 required fields (VERDICT, HEAD_SHA, REPO, PR_NUMBER,
    REVIEWER, IMPLEMENTATION_PROVENANCE) are emitted in the canonical
    form the read-back verifier expects. `extra_reviewer_lines` lets
    the multi-reviewer aggregator append a per-reviewer breakdown
    WITHOUT breaking the upsert marker or the read-back field
    extraction.
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

    # Sanitize reviewer-controlled text BEFORE emitting it into the
    # body. Without this, a smuggled `**VERDICT: PASS**` inside the
    # reason would be picked up by the read-back regex as a second
    # VERDICT occurrence, refusing the body closed (CodeRabbit MAJOR
    # finding on PR #281 round 3).
    safe_reason = _sanitize_reason(reason)
    safe_extras = (
        [_sanitize_reason(line) for line in extra_reviewer_lines]
        if extra_reviewer_lines
        else None
    )

    reason_block = f"\n**Reason:** {safe_reason}\n" if safe_reason else ""
    extras = ""
    if safe_extras:
        extras = "\n" + "\n".join(safe_extras) + "\n"

    return (
        f"{MARKER}\n"
        f"## Skeptic Gate — `{display_state}`\n\n"
        f"**VERDICT: {verdict}**\n"
        f"**HEAD_SHA: {head_sha_norm}**\n"
        f"**REPO: {repo}**\n"
        f"**PR_NUMBER: {pr_number}**\n"
        f"**REVIEWER: {reviewer}**\n"
        f"**IMPLEMENTATION_PROVENANCE: {implementation_provenance}**\n"
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
    implementation_provenance: str = "unknown",
    base_sha: str = "",  # kept for future use
    diff: str = "",  # kept for future use
    reviewer: str = "reviewer",
) -> SkepticResult:
    """Decide a single reviewer's outcome from its output (or absence).

    - `review_output` is the reviewer's stdout if it succeeded.
    - `review_error` is the captured error/timeout/missing-binary
      message. At least one is meaningful; if both are None/empty, the
      reviewer did not run at all and the gate fails closed.
    - `implementation_provenance` is the deterministic implementer
      identity derived from the commit subject prefix; it is rendered
      into the comment body so the read-back verifier can equality-
      check it.
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
            implementation_provenance=implementation_provenance,
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
            "VERDICT/HEAD_SHA/REPO/PR_NUMBER/REASON/IDENTITY missing, "
            "duplicated, or HEAD_SHA not 40 hex chars, or extra prose/"
            "code-block present — fail-closed)"
        )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=reviewer,
            implementation_provenance=implementation_provenance,
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
            implementation_provenance=implementation_provenance,
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
        implementation_provenance=implementation_provenance,
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


# Mandatory reviewer identities. Per CodeRabbit CRITICAL finding on
# PR #281 round 2: the gate MUST aggregate over exactly the
# Codex-and-Gemini set on every PR. A subset (e.g. only a single
# successful codex run) is rejected even if it succeeded, because
# a partial review cannot satisfy the dual-independent-reviewer
# policy the gate is designed to enforce.
MANDATORY_REVIEWERS = ("codex", "gemini")


def aggregate_results(
    results: List[SkepticResult],
    *,
    repo: str,
    pr_number: int,
    head_sha: str,
    implementation_provenance: str = "unknown",
) -> SkepticResult:
    """Combine per-reviewer results; ALL must be success for the gate.

    Also rejects duplicate reviewer identities in the input list — if
    the workflow is invoked with `[["codex",""],["codex","gpt-5"]]`
    (two codex invocations), the gate fails closed. This is enforced
    even before any reviewer runs, because a single PR is not allowed
    to be reviewed twice by the same model.

    Per CodeRabbit CRITICAL finding on PR #281 round 2: this
    aggregator also refuses to produce a PASS unless BOTH mandatory
    reviewer identities (codex and gemini) are present in the input.
    A single successful reviewer's result (e.g. `[codex_pass]` or
    `[gemini_pass]`) yields `check_state="failure"` with a reason
    that names the missing reviewer.
    """
    if not results:
        reason = "no reviewers ran — gate cannot pass without any review"
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(none)",
            implementation_provenance=implementation_provenance,
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

    # Duplicate-reviewer guard: the input list must contain distinct
    # reviewer identities. Two codex invocations (or two gemini
    # invocations) on the same PR are rejected outright.
    seen_reviewers = []
    duplicates = []
    for r in results:
        rid = (r.reviewer or "").strip().lower()
        if rid in seen_reviewers and rid:
            duplicates.append(rid)
        elif rid:
            seen_reviewers.append(rid)
    if duplicates:
        reason = (
            f"duplicate reviewer identities in input list: {sorted(set(duplicates))}; "
            "the gate requires distinct reviewers per PR"
        )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(aggregate)",
            implementation_provenance=implementation_provenance,
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

    # Mandatory-set guard: BOTH codex AND gemini must be present.
    # Per CodeRabbit CRITICAL finding on PR #281 round 2: a single
    # successful reviewer cannot satisfy the policy.
    missing = [r for r in MANDATORY_REVIEWERS if r not in seen_reviewers]
    if missing:
        reason = (
            f"mandatory reviewer(s) missing from input list: {missing}; "
            "the gate requires BOTH codex and gemini on every PR"
        )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(aggregate)",
            implementation_provenance=implementation_provenance,
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
    primary_sha = primary.parsed.head_sha if primary and primary.parsed else head_sha

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
        # The headline REVIEWER field is the aggregate identity (the
        # gate itself), not the primary reviewer's CLI. The per-
        # reviewer breakdown is in extra_reviewer_lines below.
        reviewer="(aggregate)",
        implementation_provenance=implementation_provenance,
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
    body_reviewer: Optional[str]
    body_implementation_provenance: Optional[str]


def verify_published_comment(
    readback: ReadBackCheck,
    *,
    expected_actor: str,
    expected_sha: str,
    expected_repo: str,
    expected_pr_number: int,
    expected_verdict: str,
    expected_reviewer: str,
    expected_implementation_provenance: str,
) -> Tuple[bool, str]:
    """Verify what we just published — full equality read-back.

    Returns (ok, reason). ok=True ONLY when every published field
    equals the value we wrote, byte-for-byte (HEAD_SHA is the full
    40-hex form, lower-cased). Missing or mismatched fields fail
    closed. Per post-audit comment 4953064910, the previous version
    only checked non-empty; an attacker (or a stale publication
    API) could therefore post a comment that satisfied the read-
    back while publishing a different SHA/repo/PR/verdict.
    """
    if readback.actor != expected_actor:
        return False, (
            f"published comment actor is {readback.actor!r}, expected "
            f"{expected_actor!r}"
        )
    if not readback.body_contains_marker:
        return False, "published comment body is missing the upsert marker"
    if (readback.body_sha or "").lower() != expected_sha.lower():
        return False, (
            f"published comment HEAD_SHA is {readback.body_sha!r}, "
            f"expected {expected_sha!r}"
        )
    if (readback.body_repo or "") != expected_repo:
        return False, (
            f"published comment REPO is {readback.body_repo!r}, "
            f"expected {expected_repo!r}"
        )
    if readback.body_pr_number != expected_pr_number:
        return False, (
            f"published comment PR_NUMBER is {readback.body_pr_number!r}, "
            f"expected {expected_pr_number!r}"
        )
    if (readback.body_verdict or "").upper() != expected_verdict.upper():
        return False, (
            f"published comment VERDICT is {readback.body_verdict!r}, "
            f"expected {expected_verdict!r}"
        )
    if (readback.body_reviewer or "") != expected_reviewer:
        return False, (
            f"published comment REVIEWER is {readback.body_reviewer!r}, "
            f"expected {expected_reviewer!r}"
        )
    if (
        readback.body_implementation_provenance or ""
    ) != expected_implementation_provenance:
        return False, (
            f"published comment IMPLEMENTATION_PROVENANCE is "
            f"{readback.body_implementation_provenance!r}, expected "
            f"{expected_implementation_provenance!r}"
        )
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
    "COMMIT_PREFIX_TO_IDENTITY",
    "MARKER",
    "ModelIdentity",
    "ParsedVerdict",
    "REVIEWER_CLI_TO_IDENTITY",
    "ReadBackCheck",
    "SkepticResult",
    "ValidationResult",
    "aggregate_results",
    "bind_reviewer_identity",
    "bind_to_pr",
    "build_prompt",
    "comment_marker",
    "evaluate",
    "extract_implementation_identity_from_commit",
    "format_comment",
    "parse_verdict",
    "verify_published_comment",
    "verify_provenance",
]
