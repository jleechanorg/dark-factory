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
    assert _parse_verdict("verdict: INCONCLUSIVE")[1] == "failure"
    # Standalone-line fallback fires when no marker is present.
    assert _parse_verdict("everything is fine\nPASS\n")[1] == "success"
    # Prose that contains the word "pass" inside another phrase is NOT a verdict.
    assert _parse_verdict("everything is fine\nresult: pass needed")[1] == "failure"


def test_old_spec_review_verdict_tokens_are_unparseable():
    """RED proof for the PR #39 finding: the old spec_review.md contract
    instructed `VERDICT: success` / `VERDICT: failure` — neither token is in
    _VERDICT_TOKEN, so both grade as ("unknown", "failure")."""
    assert _parse_verdict("VERDICT: success") == ("unknown", "failure")
    assert _parse_verdict("VERDICT: failure") == ("unknown", "failure")
    # The replacement contract is parseable.
    assert _parse_verdict("verdict: pass")[1] == "success"
    assert _parse_verdict("verdict: fail")[1] == "failure"


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
