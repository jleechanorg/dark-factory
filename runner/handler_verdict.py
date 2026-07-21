"""Verdict parsing + worktree SHA binding + outcome/verdict consistency.

Canonical owner of reviewer verdict semantics. Runtime callers and reviewer
handlers must go through this module — never inline-copy a verdict token map
or a verdict/consistency helper next to a handler.

Owns:
  * `_VERDICT_NORMALIZE` — map raw verdict token → success/failure.
  * `_HEAD_SHA_ECHO_RE` — ``head_sha: <40-hex>`` extractor.
  * `_worktree_head_sha` — full 40-char HEAD SHA via ``_git_rev_parse``.
    Heavily monkeypatched by tests via
    ``monkeypatch.setattr("runner.handlers._worktree_head_sha", ...)``, so
    the ``runner/handlers.py`` shim re-exports this name and the production
    callers in this codebase look it up via ``runner.handlers._worktree_head_sha``
    (late binding) so the monkeypatches stay in effect.
  * `_verify_head_sha_echo` — check observed SHA matches expected.
  * `_VERDICT_TOKEN`, `_MARKER_RE`, `_MARKER_PRESENT_RE`, `_STANDALONE_RE` —
    anchored marker regex + bare-marker detector + standalone-line fallback.
  * `_parse_verdict` — ``(raw_verdict, normalized_outcome)``; rejects
    misclassification attempts.
  * `_enforce_outcome_verdict_consistency` — canonical ownership of the
    "verdict must match outcome" rule. Moved here from
    ``runner/handler_parallel_reviewer`` so the verdict module owns verdict
    semantics end-to-end (parse → normalize → consistency enforce).
"""

from __future__ import annotations

import pathlib
import re
from typing import TYPE_CHECKING, Optional

from ._git import _SHA_RE, _git_rev_parse

if TYPE_CHECKING:  # pragma: no cover — import only used for type checkers
    from .handler_core import Result


_VERDICT_NORMALIZE = {
    "pass": "success",
    "warn": "success",
    "approve": "success",   # review-style vocabulary ("Verdict: APPROVE")
    "approved": "success",
    "fail": "failure",
    "partial": "failure",
    "inconclusive": "failure",
    "insufficient": "failure",
    "invalid": "failure",
    "incomplete": "failure",
    "conditional": "failure",  # non-standard verdict (architectural concern) → failure
    "reject": "failure",
    "rejected": "failure",
    "blocker": "failure",
}

# A gate response must echo back `head_sha: <40-hex>` so we can bind the
# verdict to the exact worktree SHA the gate was meant to review. Without
# this binding a late-arriving verdict could be applied to a different
# commit. Missing/mismatched echo → outcome=error (NOT failure — distinct
# so the Healer clusters it as an infra issue, like rc!=0 + unknown verdict).
_HEAD_SHA_ECHO_RE = re.compile(
    r"head_sha\s*:\s*([0-9a-fA-F]{40})\b",
    re.IGNORECASE,
)


def _worktree_head_sha(workdir: pathlib.Path) -> Optional[str]:
    """Return the full 40-char HEAD SHA for `workdir`, or None on failure."""
    sha = _git_rev_parse(workdir, "HEAD")
    if sha is None:
        return None
    if _SHA_RE.match(sha.lower()):
        return sha.lower()
    return None


def _verify_head_sha_echo(text: str, expected_sha: str) -> tuple[bool, str]:
    """Check that `text` contains a `head_sha: <expected>` line.

    Returns (ok, observed_sha). ok=True iff the echoed SHA matches expected.
    observed_sha is "" if no head_sha line is present.
    """
    matches = list(_HEAD_SHA_ECHO_RE.finditer(text or ""))
    if not matches:
        return False, ""
    # If multiple appear, take the LAST one — same convention as verdict parsing.
    observed = matches[-1].group(1).lower()
    return observed == expected_sha.lower(), observed

# The set of recognized verdict tokens, shared by the marker + standalone regexes.
_VERDICT_TOKEN = (
    r"(?:pass|warn|approved?|fail|partial|inconclusive|insufficient|invalid"
    r"|incomplete|conditional|rejected?|blocker)"
)

# Anchored regex: a verdict token must follow a marker ("verdict:", "overall:",
# "normalized:") on the same line. The gap between the marker and the captured
# token may contain ONLY decoration (whitespace, markdown like ``**``, emoji —
# any non-word char) and *qualifier verdict-tokens* (e.g. "CONDITIONAL PASS",
# "PARTIAL PASS"); backtracking captures the LAST token. It must NOT contain
# arbitrary alphabetic prose — a bare ``[^\n]*`` wildcard would lift "fail" out
# of "verdict: not a fail", which is precisely the misclassification the
# hardening tests forbid. Non-token word runs ("not", "a") break the match, so
# the caller falls through to the "marker present but invalid" → unknown path.
_MARKER_RE = re.compile(
    r"(?:verdict|overall|normalized)\s*:\s*"
    r"(?:" + _VERDICT_TOKEN + r"\b|[^\w\n])*"
    r"(" + _VERDICT_TOKEN + r")\b",
    re.IGNORECASE,
)

# Bare marker (presence of "verdict:"/"overall:"/"normalized:" anywhere) — used to
# detect that the gate *attempted* to emit a verdict line. If that's present we
# trust only the regex above; we don't fall back to scanning the whole tail,
# because the fallback can lift "fail" out of compound phrases like
# "verdict: not a fail".
_MARKER_PRESENT_RE = re.compile(
    r"(?:verdict|overall|normalized)\s*:",
    re.IGNORECASE,
)

# Fallback: a verdict line standing alone (whitespace + token + optional
# trailing punctuation). Stricter than a free `\b` scan so prose like
# "not a fail" doesn't slip through.
_STANDALONE_RE = re.compile(
    r"^\s*(pass|warn|fail|partial|inconclusive|conditional)\b[\s.!:]*$",
    re.IGNORECASE | re.MULTILINE,
)


def _parse_verdict(text: str, *, gate_strict: bool = False) -> tuple[str, str]:
    """Extract a normalized verdict from gate output.

    Strategy:
      1. Look for explicit marker lines (`Verdict: PASS`). The LAST valid marker
         wins — gates may emit progress lines before the authoritative one.
      2. If a marker word was present but no matching token followed it,
         return ("unknown", "failure") — do NOT fall back; the gate's own
         marker line is the contract.
      3. With no marker at all, scan the last 40 lines for a *standalone*
         verdict token (not embedded in prose).

    Args:
      text: Gate output text to parse.
      gate_strict: When True, a `warn` verdict is normalized to `failure`
        instead of `success`. Opt-in per gate node via the `gate_strict="true"`
        DOT attribute (see jleechan-9ia / F6). Default False preserves the
        legacy warn→success mapping so existing graphs do not regress.

    Returns (raw_verdict, normalized_outcome). Unknown returns ("unknown", "failure").
    """
    body = text or ""
    matches = list(_MARKER_RE.finditer(body))
    if matches:
        raw = matches[-1].group(1).lower()
        return raw, _normalize_outcome(raw, gate_strict=gate_strict)

    if _MARKER_PRESENT_RE.search(body):
        # A verdict marker existed but with an invalid token — refuse to guess.
        return "unknown", "failure"

    tail = "\n".join(body.splitlines()[-40:])
    fallback = list(_STANDALONE_RE.finditer(tail))
    if fallback:
        raw = fallback[-1].group(1).lower()
        return raw, _normalize_outcome(raw, gate_strict=gate_strict)
    return "unknown", "failure"


def _normalize_outcome(raw_verdict: str, *, gate_strict: bool) -> str:
    """Map a raw verdict token to a success/failure outcome.

    When ``gate_strict`` is True, a `warn` verdict is treated as `failure`
    (the gate flagged a real concern the operator must address). Default
    preserves the legacy warn→success mapping.
    """
    if gate_strict and raw_verdict == "warn":
        return "failure"
    return _VERDICT_NORMALIZE.get(raw_verdict, "failure")


def _enforce_outcome_verdict_consistency(result: "Result", *, gate_strict: bool = False) -> "Result":
    """Enforce that verdict matches outcome to prevent contradictory reporting.

    Canonical owner of the outcome/verdict consistency rule. Previously
    lived in ``runner/handler_parallel_reviewer``; relocated here so all
    verdict semantics (token map → normalization → consistency enforcement)
    live in one module. Direct imports from this module are cycle-free.

    Bug 2: When stale spec artifacts from a prior errored run cause the
    reviewer to read outdated content, it can emit ``outcome=failure`` with
    ``verdict=pass`` (or vice versa). This function uses real normalization
    via ``_normalize_outcome`` and only rewrites on a GENUINE disagreement
    between outcome and normalized verdict.

    Contract (preserved exactly from the prior location):
      * Sentinel / unparseable / echo verdicts
        (``""``, ``"unknown"``, ``"echo:success"``, ``"infra_failure"``, …)
        are NOT in the verdict vocabulary — leave them untouched; we cannot
        judge a contradiction we cannot normalize.
      * ``"error"`` outcome is an infra state, not a verdict disagreement —
        never rewrite on an error outcome.
      * On a genuine contradiction, rewrite verdict to the canonical token
        matching the outcome (``pass`` / ``fail``) and record audit fields:
        ``verdict_adjusted_for_consistency`` → ``"true"``,
        ``original_verdict`` → unchanged raw value (preserved for audit).
      * On consistency (e.g. warn→success with ``gate_strict=False``, or
        approve→success, partial→failure), preserve the raw token EXACTLY —
        no adjustment fields are written.
    """
    # Lazy import to keep this module's load graph independent of handler_core;
    # callers always go through the canonical name exported here.
    from .handler_core import Result as _Result

    md = result.metadata or {}
    raw_original = str(md.get("verdict", ""))
    raw = raw_original.strip().lower()
    # Only reason about RECOGNIZED verdict tokens.
    if raw not in _VERDICT_NORMALIZE:
        return result
    outcome = result.outcome
    # "error" is an infra state, not a verdict disagreement.
    if outcome not in ("success", "failure"):
        return result
    normalized = _normalize_outcome(raw, gate_strict=gate_strict)
    if normalized == outcome:
        # verdict is CONSISTENT with outcome — preserve the raw token EXACTLY.
        return result
    # Genuine contradiction: rewrite to a canonical token matching the outcome,
    # preserving the original for audit.
    new_verdict = "pass" if outcome == "success" else "fail"
    new_md = dict(md)
    new_md["verdict"] = new_verdict
    new_md["verdict_adjusted_for_consistency"] = "true"
    new_md["original_verdict"] = raw_original
    return _Result(
        outcome=result.outcome,
        output=result.output,
        metadata=new_md,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=result.context_updates,
    )


# Reproduction receipt: a reviewer PASS is only trustworthy when the review
# transcript shows the reviewer actually RE-RAN a build/test runner and that
# run SUCCEEDED (exit 0). Without that, a PASS is read-only theater; with a
# nonzero-only exit trail, the reviewer reproduced a FAILURE and passed it
# anyway. No outer \b anchors on runners: leading "./" and trailing chars
# (e.g. "run_tests.sh") make \b-wrapping reject legitimate reproductions.
_RECEIPT_RUNNER_RE = re.compile(
    r"(uv run pytest|pytest|python[0-9.]* -m (?:pytest|unittest)|py -m (?:pytest|unittest)|"
    r"npm (?:test|run|ci)|yarn (?:test|run)|pnpm (?:test|run)|vitest|jest|npx playwright|"
    r"playwright test|go (?:test|build)|cargo (?:test|build)|\bmake |\bmvn |gradle|gradlew|"
    r"bazel (?:test|build)|ctest|rspec|mix test|\btox\b|cmake --build|run_tests|"
    r"bash \S*test|bash \S+\.sh|\./run)",
    re.IGNORECASE,
)

# One capture group over the exit-code digits so the gate can require a
# SUCCESSFUL reproduction, not merely a captured exit code. KNOWN CEILING:
# a regex cannot bind an exit code to the command that produced it, so any
# captured zero satisfies the gate; likewise fabricated prose naming a runner
# plus "exit code: 0" passes. This stops read-only PASSes and honest-but-
# failed reproductions, not a lying reviewer — the full fix is engine-captured
# execution. Both ceilings are pinned in tests/test_reviewer_reproduction_receipt.py.
_RECEIPT_EXIT_RE = re.compile(
    r"(?:exit[_ ]?code\s*[:=]?\s*|exit\s*[:=]\s*|exited with\s+|returned\s+|"
    r"\$\?\s*[:=]?\s*|\bexit\s+)(\d+)\b",
    re.IGNORECASE,
)


def _reproduction_receipt_gap(text: str) -> str:
    """Return why `text` fails as a reproduction receipt, or "" if it holds.

    Callers apply this ONLY to a success outcome — a failure verdict needs no
    reproduction. Pure text analysis; network-free.
    """
    body = text or ""
    if not body.strip():
        return (
            "reproduction receipt: review passed but produced no transcript — "
            "a PASS must re-run the build/test and capture its exit code, not "
            "review from narrative alone"
        )
    has_runner = bool(_RECEIPT_RUNNER_RE.search(body))
    exit_codes = [int(m.group(1)) for m in _RECEIPT_EXIT_RE.finditer(body)]
    if not (has_runner and exit_codes):
        return (
            "reproduction receipt: review passed without a reproduced build/test "
            f"and captured exit code (runner_found={has_runner}, "
            f"exit_code_found={bool(exit_codes)}) — re-run the suite/build and "
            "record its exit code in the review output, or the PASS is read-only"
        )
    if 0 not in exit_codes:
        return (
            "reproduction receipt: review passed but its reproduced build/test "
            f"FAILED (captured exit codes: {sorted(set(exit_codes))}) — a PASS "
            "requires a successful reproduction (exit code 0)"
        )
    return ""
