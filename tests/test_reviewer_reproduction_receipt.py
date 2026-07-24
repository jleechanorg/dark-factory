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


class TestPerLaneReceiptEnforcement:
    """Each reviewer lane (primary + shadows) must carry its own reproduction
    receipt BEFORE transcripts are concatenated for downstream handoff.

    Codex post-merge review of #407 finding 4: ``handler_dispatch.py``
    concatenates primary+shadow transcripts, then ``handler_parallel_reviewer``
    checks the COMBINED text once — one lane's genuine test-run receipt
    blesses every lane (read-only primary PASS + one shadow's exit-0 still
    passes). Regression: primary-no-receipt + shadow-with-receipt → primary
    lane must fail even though combined text has a receipt.
    """

    def test_primary_without_receipt_fails_alone(self):
        """A primary lane with no reproduction receipt must be downgraded
        on its own; the receipt check is lane-scoped, not transcript-scoped.
        """
        result = Result(
            outcome="success",
            output="Reviewed the diff. Looks correct.\nVerdict: PASS",
            metadata={"verdict": "pass"},
        )
        # Lane-scoped: only the primary transcript is checked, not any
        # concatenated downstream text.
        adjusted = _enforce_reproduction_receipt(result)
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["receipt_downgraded"] == "true"

    def test_primary_with_receipt_passes_alone(self):
        """A primary lane with its own receipt holds even if downstream
        shadows later concatenate 'no receipt' prose.
        """
        result = Result(
            outcome="success",
            output="$ uv run pytest\n12 passed\nexit code: 0\nVerdict: PASS",
            metadata={"verdict": "pass"},
        )
        # Lane-scoped: shadow text does not contaminate primary receipt check.
        adjusted = _enforce_reproduction_receipt(result)
        assert adjusted is result

    def test_combined_receipt_does_not_save_primary(self):
        """Regression for finding 4: the combined transcript contains a
        receipt (from the shadow lane), but the primary lane alone has none.
        Per-lane enforcement must downgrade the primary lane."""
        primary_output = "Reviewed the diff. Looks correct.\nVerdict: PASS"
        # The "combined" downstream text adds a shadow with a receipt.
        combined_text = primary_output + (
            "\n\n---\n\n"
            "## Parallel Codex Gate Review\n"
            "$ uv run pytest\n12 passed\nexit code: 0\nVerdict: PASS\n"
        )
        # The lane-scoped check must look at the primary text only.
        gap_primary_only = _reproduction_receipt_gap(primary_output)
        gap_combined = _reproduction_receipt_gap(combined_text)
        assert gap_primary_only, "primary-only text must trigger a gap"
        assert not gap_combined, "combined text does contain a receipt (this is the bug)"
        # The lane enforcement returns a downgraded result for primary.
        primary_result = Result(
            outcome="success",
            output=primary_output,
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt(primary_result)
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        # The combined-text check (legacy, transcript-scoped) WOULD have
        # passed; this regression asserts lane-scoped check fails.

    def test_shadow_without_receipt_fails_shadow_lane(self):
        """A shadow lane that fails to reproduce must be flagged per-lane.
        ``_finish_shadow_gate_review`` records the shadow output in
        ``ctx.state[<node>.shadow_<backend>_gate_output]``; the per-lane
        receipt check is run against that single-lane text.
        """
        # The shadow's own transcript has no receipt.
        shadow_output = (
            "Reviewed the diff. Looks correct.\nVerdict: PASS"
        )
        gap = _reproduction_receipt_gap(shadow_output)
        assert gap, "shadow lane without receipt must trigger a gap"
        # Downgrade the shadow lane alone (this is what the new per-lane
        # hook in ``_finish_shadow_gate_review`` will perform).
        shadow_result = Result(
            outcome="success",
            output=shadow_output,
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt(shadow_result)
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        assert "shadow" in adjusted.metadata.get("receipt_gap_lane", "") or (
            "reproduction receipt" in adjusted.metadata["receipt_gap"]
        )


class TestPerLaneHookContract:
    """The per-lane enforcement hook on each lane (primary and each shadow)
    must be wired so callers can identify which lane failed. The contract:
    lane metadata gets a ``receipt_downgraded`` flag and the per-lane gap is
    recorded under ``receipt_gap`` so audit readers can pinpoint the lane.
    """

    def test_lane_hook_records_lane_name(self):
        """The per-lane hook must record which lane failed so downstream
        aggregation can isolate the responsible lane."""
        from runner.handler_parallel_reviewer import (
            _enforce_reproduction_receipt_for_lane,
        )
        result = Result(
            outcome="success",
            output="narrative only",
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt_for_lane(result, lane_name="shadow_codex")
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        assert adjusted.metadata.get("receipt_gap_lane") == "shadow_codex"

    def test_lane_hook_passthrough_on_receipt_holds(self):
        from runner.handler_parallel_reviewer import (
            _enforce_reproduction_receipt_for_lane,
        )
        result = Result(
            outcome="success",
            output="$ uv run pytest\n12 passed\nexit code: 0\nVerdict: PASS",
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt_for_lane(result, lane_name="primary")
        assert adjusted is result
