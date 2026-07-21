"""Tests for the reviewer reproduction-receipt gate.

A reviewer PASS is only trustworthy if the review transcript shows the
reviewer re-ran a real build/test runner AND captured exit code 0. A
read-only PASS (no transcript / no runner) and a pass-despite-red-suite
(nonzero-only exits) are both downgraded to failure. Opt-in per node via
the ``receipt_required="true"`` DOT attribute (same no-regression pattern
as ``gate_strict``).
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.handler_core import Result  # noqa: E402
# Import via handlers first to avoid circular import at test collection time
import runner.handlers  # noqa: F401 - forces full module init before handler_parallel_reviewer
from runner.handler_parallel_reviewer import (  # noqa: E402
    _enforce_reproduction_receipt,
    _receipt_required_flag,
)
from runner.handler_verdict import _reproduction_receipt_gap  # noqa: E402


class TestReproductionReceiptGap:
    """Pure-text receipt analysis in handler_verdict."""

    @pytest.mark.parametrize(
        "transcript",
        [
            "$ uv run pytest\n3 passed\nexit: 0\n",
            "$ npm test\n...\nexit code: 0\n",
            "$ go test ./...\nok\nexited with 0\n",
            "$ cargo test\n...\nreturned 0\n",
            "$ ./gradlew test\n...\nexit: 0\n",           # leading ./
            "$ bash run_tests.sh\n...\nexited with 0\n",  # trailing chars after 'test'
            "$ python -m unittest discover\nOK\nexit code: 0\n",
            "$ python3.11 -m unittest\nOK\nexit code: 0\n",  # versioned interpreter
            "$ py -m pytest -q\n5 passed\nexit: 0\n",        # Windows launcher
        ],
    )
    def test_successful_reproduction_holds(self, transcript):
        assert _reproduction_receipt_gap(transcript) == ""

    def test_empty_transcript_is_a_gap(self):
        gap = _reproduction_receipt_gap("")
        assert "no transcript" in gap

    def test_narrative_without_runner_is_a_gap(self):
        gap = _reproduction_receipt_gap("I read the diff carefully. Looks correct.\nexit: 0")
        assert "runner_found=False" in gap

    def test_runner_without_exit_code_is_a_gap(self):
        gap = _reproduction_receipt_gap("I would run pytest but did not.")
        assert "exit_code_found=False" in gap

    @pytest.mark.parametrize(
        "transcript",
        [
            "$ uv run pytest\n2 failed, 10 passed\nexit code: 1\n",
            "$ npm test\nFAIL src/app.test.ts\nexited with 1\n",
            "$ go test ./...\nFAIL\texample.com/pkg\nexit: 2\n",
        ],
    )
    def test_failed_reproduction_is_a_gap(self, transcript):
        gap = _reproduction_receipt_gap(transcript)
        assert "FAILED" in gap

    def test_mixed_exits_with_zero_hold(self):
        # Nonzero exits from setup steps are fine when a successful run exists.
        transcript = "$ bash setup.sh\nexit: 1\n$ uv run pytest\n12 passed\nexit code: 0\n"
        assert _reproduction_receipt_gap(transcript) == ""

    def test_fabricated_prose_is_a_documented_limit(self):
        # KNOWN CEILING, pinned deliberately: prose that name-drops a runner
        # and "exit code: 0" passes even if nothing was executed. Regex text
        # analysis ends here; the fix is engine-captured execution. If this
        # starts failing, the gate got stronger — update docs and delete pin.
        assert _reproduction_receipt_gap(
            "We are confident uv run pytest would succeed. exit code: 0"
        ) == ""

    def test_zero_after_failed_run_is_a_documented_limit(self):
        # KNOWN CEILING, pinned deliberately: the regex cannot associate an
        # exit code with the command that produced it, so ANY captured zero
        # satisfies the gate. Same ceiling as fabrication; same fix.
        assert _reproduction_receipt_gap(
            "$ uv run pytest\n2 failed\nexit code: 1\n$ echo cleanup\nexit: 0\n"
        ) == ""


class TestEnforceReproductionReceipt:
    """Result-level enforcement in handler_parallel_reviewer."""

    def test_success_without_receipt_downgraded(self):
        result = Result(
            outcome="success",
            output="Reviewed the diff. All good.\nVerdict: PASS",
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt(result)
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        assert adjusted.metadata["original_verdict"] == "pass"
        assert "reproduction receipt" in adjusted.metadata["receipt_gap"]
        # The gap is appended to output so route-back feedback is actionable.
        assert "reproduction receipt" in adjusted.output

    def test_success_with_receipt_unchanged(self):
        result = Result(
            outcome="success",
            output="$ uv run pytest\n12 passed\nexit code: 0\nVerdict: PASS",
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt(result)
        assert adjusted is result

    def test_failure_never_touched(self):
        # A failure needs no reproduction; downgrading logic must not mask
        # the original route-back reason either.
        result = Result(outcome="failure", output="Verdict: FAIL", metadata={"verdict": "fail"})
        assert _enforce_reproduction_receipt(result) is result

    def test_error_never_touched(self):
        result = Result(outcome="error", output="", metadata={})
        assert _enforce_reproduction_receipt(result) is result

    def test_success_with_failed_reproduction_downgraded(self):
        result = Result(
            outcome="success",
            output="$ uv run pytest\n2 failed\nexit code: 1\nVerdict: PASS",
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt(result)
        assert adjusted.outcome == "failure"
        assert "FAILED" in adjusted.metadata["receipt_gap"]

    def test_preserves_other_metadata_and_context(self):
        result = Result(
            outcome="success",
            output="narrative only",
            metadata={"verdict": "pass", "reviewer_backend": "claude"},
            context_updates={"k": "v"},
        )
        adjusted = _enforce_reproduction_receipt(result)
        assert adjusted.metadata["reviewer_backend"] == "claude"
        assert adjusted.context_updates == {"k": "v"}

    def test_does_not_clobber_pre_existing_original_verdict(self):
        """When ``_enforce_outcome_verdict_consistency`` already wrote the
        raw reviewer verdict into ``original_verdict``, the receipt gate
        must honor that pre-existing value — overwriting it with the
        post-consistency canonical token would mask the actual reviewer
        output from audit readers.
        """
        result = Result(
            outcome="success",
            output="narrative only",
            metadata={
                # Consistency ran first, recorded the raw reviewer output
                "verdict": "pass",
                "verdict_adjusted_for_consistency": "true",
                "original_verdict": "approve",  # raw reviewer token
            },
        )
        adjusted = _enforce_reproduction_receipt(result)
        # The pre-existing raw verdict survives the receipt downgrade.
        assert adjusted.metadata["original_verdict"] == "approve"
        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        assert "receipt_gap" in adjusted.metadata


class TestReceiptRequiredFlag:
    """Opt-in flag parsing — same no-regression rules as gate_strict."""

    class _Node:
        def __init__(self, attrs):
            self.attrs = attrs

    @pytest.mark.parametrize("raw", [True, "true", "TRUE", "1", "yes", 1])
    def test_enabled_values(self, raw):
        assert _receipt_required_flag(self._Node({"receipt_required": raw})) is True

    @pytest.mark.parametrize("raw", [None, False, "", "false", "0", "no", "typo"])
    def test_disabled_values(self, raw):
        attrs = {} if raw is None else {"receipt_required": raw}
        assert _receipt_required_flag(self._Node(attrs)) is False
