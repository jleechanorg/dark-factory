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
