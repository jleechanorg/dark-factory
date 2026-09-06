"""Tests for _parse_verdict regression (incl. PR #39 RED proof).

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.handlers import _parse_verdict  # noqa: E402


def test_parse_verdict_pass_warn_fail():
    assert _parse_verdict("blah\nVERDICT: PASS\n")[1] == "success"
    assert _parse_verdict("Overall: WARN — minor")[1] == "success"
    assert _parse_verdict("verdict: FAIL")[1] == "failure"
    assert _parse_verdict("**Verdict: APPROVE.** Clean deletion commit.")[1] == "success"
    assert _parse_verdict("Verdict: REQUEST CHANGES — presumptive blocker.")[1] == "failure"
    assert _parse_verdict("Verdict: PARTIAL")[1] == "failure"
    # "inconclusive" is NOT a rejection — the reviewer found nothing to
    # act on, distinct from a real failure finding (dark-factory#827/#828).
    # Maps to "error" (an EXISTING classify-safe bucket), not a fabricated
    # outcome value — see _VERDICT_NORMALIZE's comment in handler_verdict.py.
    assert _parse_verdict("verdict: INCONCLUSIVE")[1] == "error"
    # Standalone-line fallback fires when no marker is present.
    assert _parse_verdict("everything is fine\nPASS\n")[1] == "success"
    # Prose that contains the word "pass" inside another phrase is NOT a
    # verdict — conservative fail-safe default (no marker at all, no
    # standalone match) stays "failure", unchanged.
    assert _parse_verdict("everything is fine\nresult: pass needed")[1] == "failure"


def test_old_spec_review_verdict_tokens_are_unparseable():
    """RED proof for the PR #39 finding: the old spec_review.md contract
    instructed `VERDICT: success` / `VERDICT: failure` — neither token is in
    _VERDICT_TOKEN, so both grade as ("unknown", "failure"). Unrecognized
    marker content that ISN'T a null sentinel stays the conservative
    fail-safe "failure" — only a literal null/none/n-a remainder (the exact
    dark-factory#828 incident shape) grades as "inconclusive" (see
    test_null_verdict_marker_is_inconclusive_not_failure below)."""
    assert _parse_verdict("VERDICT: success") == ("unknown", "failure")
    assert _parse_verdict("VERDICT: failure") == ("unknown", "failure")
    # The replacement contract is parseable.
    assert _parse_verdict("verdict: pass")[1] == "success"
    assert _parse_verdict("verdict: fail")[1] == "failure"


def test_null_verdict_marker_is_error_not_failure():
    """dark-factory#827/#828 exact repro shape: a gate renders its verdict
    marker template but the substitution produced a null sentinel
    (`VERDICT: None`) instead of a real token — e.g. a Python `None` object
    stringified into the template. This is "the reviewer produced no
    verdict", NOT "the reviewer rejected the diff", and must grade as
    ("null", "error") so callers do not route it to a fix/coder node with
    nothing actionable to act on. "error" (not a fabricated third outcome
    bucket) because `_normalized_result` re-buckets every outcome through
    `_classify_outcome` before DOT-edge routing sees it, and "error" is the
    one existing bucket that both (a) survives that re-bucketing unchanged
    and (b) already means "infra state, not a verdict disagreement"."""
    assert _parse_verdict("**VERDICT: None**") == ("null", "error")
    assert _parse_verdict("Verdict: None") == ("null", "error")
    assert _parse_verdict("verdict: null") == ("null", "error")
    assert _parse_verdict("Overall: N/A") == ("null", "error")
    assert _parse_verdict("Verdict:") == ("null", "error")
    assert _parse_verdict("Verdict:   ") == ("null", "error")


def test_gate_strict_overrides_warn_to_failure():
    """F6 (jleechan-9ia): when gate_strict=True, a `warn` verdict must
    normalize to `failure` instead of the legacy warn→success mapping.

    Opt-in per gate node via the `gate_strict="true"` DOT attribute
    (parsed as bool via _NODE_BOOL_ATTRS in runner/parser.py). Existing
    graphs without the attribute keep the legacy mapping.
    """
    # Default: warn→success (legacy)
    assert _parse_verdict("VERDICT: WARN — minor")[1] == "success"
    # Strict: warn→failure
    assert _parse_verdict("VERDICT: WARN — minor", gate_strict=True)[1] == "failure"
    # Standalone fallback path is also strict-aware
    assert _parse_verdict("all good\nwarn\n", gate_strict=True)[1] == "failure"
    # pass/fail unaffected by strict
    assert _parse_verdict("VERDICT: PASS", gate_strict=True)[1] == "success"
    assert _parse_verdict("VERDICT: FAIL", gate_strict=True)[1] == "failure"
    # Gate_strict=False (explicit) is same as default
    assert _parse_verdict("VERDICT: WARN", gate_strict=False)[1] == "success"
