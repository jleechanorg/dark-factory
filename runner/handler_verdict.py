"""Verdict parsing + worktree SHA binding for reviewer gates.

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
"""

from __future__ import annotations

import pathlib
import re
from typing import Optional

from ._git import _SHA_RE, _git_rev_parse


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
