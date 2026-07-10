"""Regression tests for reviewer outcome/verdict consistency (fix 3).

Tests that _enforce_outcome_verdict_consistency normalizes the metadata
verdict to match the outcome, preventing contradictory reporting.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.handler_core import Result  # noqa: E402
from runner.handler_parallel_reviewer import _enforce_outcome_verdict_consistency  # noqa: E402


class TestEnforceOutcomeVerdictConsistency:
    """Tests for fix 3: verdict must match outcome."""

    def test_failure_outcome_with_pass_verdict_adjusted_to_fail(self):
        """outcome=failure with verdict=pass should be adjusted to fail."""
        result = Result(
            outcome="failure",
            output="some reviewer output",
            metadata={"verdict": "pass", "other_key": "value"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        # Verdict should be changed to match outcome
        assert adjusted.metadata["verdict"] == "fail"
        # Adjustment tracking should be added
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "pass"
        # Outcome should remain unchanged
        assert adjusted.outcome == "failure"

    def test_success_outcome_with_pass_verdict_unchanged(self):
        """outcome=success with verdict=pass should remain unchanged."""
        result = Result(
            outcome="success",
            output="passed review",
            metadata={"verdict": "pass"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        # Should be unchanged
        assert adjusted.metadata["verdict"] == "pass"
        # No adjustment key added
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "success"

    def test_error_outcome_with_pass_verdict_adjusted_to_fail(self):
        """outcome=error with verdict=pass should be adjusted to fail."""
        result = Result(
            outcome="error",
            output="some error occurred",
            metadata={"verdict": "pass"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "pass"
        assert adjusted.outcome == "error"

    def test_partial_outcome_with_pass_verdict_adjusted_to_fail(self):
        """outcome=partial with verdict=pass should be adjusted to fail."""
        result = Result(
            outcome="partial",
            output="partial review",
            metadata={"verdict": "pass"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "pass"
        assert adjusted.outcome == "partial"

    def test_no_verdict_in_metadata_does_not_raise(self):
        """Result with no 'verdict' in metadata should not raise."""
        result = Result(
            outcome="failure",
            output="some output",
            metadata={},  # No verdict key
        )

        # Should not raise
        adjusted = _enforce_outcome_verdict_consistency(result)

        # Outcome unchanged, no adjustment keys added
        assert adjusted.outcome == "failure"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata

    def test_none_metadata_does_not_raise(self):
        """Result with None metadata should not raise."""
        result = Result(
            outcome="failure",
            output="some output",
            metadata=None,
        )

        # Should not raise
        adjusted = _enforce_outcome_verdict_consistency(result)

        assert adjusted.outcome == "failure"

    def test_case_insensitive_verdict_matching(self):
        """Verdict matching should be case-insensitive."""
        result = Result(
            outcome="failure",
            output="output",
            metadata={"verdict": "PASS"},  # uppercase
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["original_verdict"] == "pass"  # normalized to lowercase

    def test_failure_outcome_with_fail_verdict_unchanged(self):
        """outcome=failure with verdict=fail should remain unchanged."""
        result = Result(
            outcome="failure",
            output="failed review",
            metadata={"verdict": "fail"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        assert adjusted.metadata["verdict"] == "fail"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "failure"

    def test_preserves_other_metadata_fields(self):
        """Adjustment should preserve other metadata fields."""
        result = Result(
            outcome="failure",
            output="output",
            metadata={
                "verdict": "pass",
                "reviewer_backend": "codex",
                "custom_field": "custom_value",
            },
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        # Other fields preserved
        assert adjusted.metadata["reviewer_backend"] == "codex"
        assert adjusted.metadata["custom_field"] == "custom_value"
        # Adjustment fields added
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "pass"

    def test_preserves_context_updates(self):
        """Adjustment should preserve context_updates."""
        result = Result(
            outcome="failure",
            output="output",
            metadata={"verdict": "pass"},
            context_updates={"key1": "value1"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        assert adjusted.context_updates == {"key1": "value1"}

    def test_preserves_suggested_next_ids(self):
        """Adjustment should preserve suggested_next_ids."""
        result = Result(
            outcome="failure",
            output="output",
            metadata={"verdict": "pass"},
            suggested_next_ids=["next_node_1", "next_node_2"],
        )

        adjusted = _enforce_outcome_verdict_consistency(result)

        assert adjusted.suggested_next_ids == ["next_node_1", "next_node_2"]
