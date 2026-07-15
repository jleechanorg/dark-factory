"""Regression tests for reviewer outcome/verdict consistency (fix 3).

Tests that _enforce_outcome_verdict_consistency uses real normalization
via _normalize_outcome and only rewrites on a genuine outcome/verdict contradiction.
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
from runner.handler_verdict import _enforce_outcome_verdict_consistency  # noqa: E402


class TestEnforceOutcomeVerdictConsistency:
    """Tests for fix 3: only adjust verdict on true outcome/verdict contradiction."""

    def test_failure_outcome_with_pass_verdict_adjusted_to_fail(self):
        """outcome=failure with verdict=pass is a true contradiction → adjusted to fail."""
        result = Result(
            outcome="failure",
            output="some reviewer output",
            metadata={"verdict": "pass", "other_key": "value"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

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

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # Should be unchanged - consistent
        assert adjusted.metadata["verdict"] == "pass"
        # No adjustment key added
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "success"

    def test_error_outcome_with_unknown_verdict_unchanged(self):
        """outcome=error with verdict='unknown' (sentinel) → unchanged.

        'unknown' is not in the recognized verdict vocabulary, so the override
        cannot reason about it and leaves it untouched. This guards the infra
        path when the reviewer emitted no parseable verdict at all.
        """
        result = Result(
            outcome="error",
            output="some error occurred",
            metadata={"verdict": "unknown"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # Should be unchanged - 'unknown' is a sentinel, not a vocabulary token
        assert adjusted.metadata["verdict"] == "unknown"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "error"

    def test_no_verdict_in_metadata_does_not_raise(self):
        """Result with no 'verdict' in metadata should not raise."""
        result = Result(
            outcome="failure",
            output="some output",
            metadata={},  # No verdict key
        )

        # Should not raise
        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

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
        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.outcome == "failure"

    def test_case_insensitive_verdict_matching(self):
        """Verdict matching should be case-insensitive."""
        result = Result(
            outcome="failure",
            output="output",
            metadata={"verdict": "PASS"},  # uppercase
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["original_verdict"] == "PASS"  # preserved original case

    def test_failure_outcome_with_fail_verdict_unchanged(self):
        """outcome=failure with verdict=fail should remain unchanged."""
        result = Result(
            outcome="failure",
            output="failed review",
            metadata={"verdict": "fail"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

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

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

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

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.context_updates == {"key1": "value1"}

    def test_preserves_suggested_next_ids(self):
        """Adjustment should preserve suggested_next_ids."""
        result = Result(
            outcome="failure",
            output="output",
            metadata={"verdict": "pass"},
            suggested_next_ids=["next_node_1", "next_node_2"],
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.suggested_next_ids == ["next_node_1", "next_node_2"]

    # NEW cases for the corrected semantics

    def test_success_outcome_with_warn_verdict_unchanged_gate_strict_false(self):
        """outcome=success with verdict=warn (gate_strict=False) → consistent, unchanged."""
        result = Result(
            outcome="success",
            output="review with warnings",
            metadata={"verdict": "warn"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # warn→success (gate_strict=False), matches outcome=success → preserved
        assert adjusted.metadata["verdict"] == "warn"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "success"

    def test_success_outcome_with_approve_verdict_unchanged(self):
        """outcome=success with verdict=approve → consistent, unchanged."""
        result = Result(
            outcome="success",
            output="approved review",
            metadata={"verdict": "approve"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # approve→success, matches outcome=success → preserved
        assert adjusted.metadata["verdict"] == "approve"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "success"

    def test_success_outcome_with_approved_verdict_unchanged(self):
        """outcome=success with verdict=approved → consistent, unchanged."""
        result = Result(
            outcome="success",
            output="approved review",
            metadata={"verdict": "approved"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # approved→success, matches outcome=success → preserved
        assert adjusted.metadata["verdict"] == "approved"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "success"

    def test_failure_outcome_with_partial_verdict_unchanged(self):
        """outcome=failure with verdict=partial → consistent, unchanged."""
        result = Result(
            outcome="failure",
            output="partial review",
            metadata={"verdict": "partial"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # partial→failure, matches outcome=failure → preserved
        assert adjusted.metadata["verdict"] == "partial"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "failure"

    def test_failure_outcome_with_inconclusive_verdict_unchanged(self):
        """outcome=failure with verdict=inconclusive → consistent, unchanged."""
        result = Result(
            outcome="failure",
            output="inconclusive review",
            metadata={"verdict": "inconclusive"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # inconclusive→failure, matches outcome=failure → preserved
        assert adjusted.metadata["verdict"] == "inconclusive"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "failure"

    def test_failure_outcome_with_warn_verdict_adjusted_gate_strict_true(self):
        """outcome=failure with verdict=warn (gate_strict=True) → adjusted to fail."""
        result = Result(
            outcome="failure",
            output="review with warnings",
            metadata={"verdict": "warn"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=True)

        # warn→failure (gate_strict=True), matches outcome=failure → unchanged
        assert adjusted.metadata["verdict"] == "warn"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "failure"

    def test_success_outcome_with_warn_verdict_adjusted_gate_strict_true(self):
        """outcome=success with verdict=warn (gate_strict=True) → TRUE contradiction, adjusted."""
        result = Result(
            outcome="success",
            output="review with warnings",
            metadata={"verdict": "warn"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=True)

        # warn→failure (gate_strict=True), but outcome=success → contradiction
        assert adjusted.metadata["verdict"] == "pass"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "warn"
        assert adjusted.outcome == "success"

    def test_failure_outcome_with_approve_verdict_adjusted(self):
        """outcome=failure with verdict=approve → TRUE contradiction, adjusted to fail."""
        result = Result(
            outcome="failure",
            output="review output",
            metadata={"verdict": "approve"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # approve→success, but outcome=failure → contradiction
        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "approve"
        assert adjusted.outcome == "failure"

    def test_sentinel_unknown_verdict_unchanged(self):
        """verdict='unknown' is not in vocabulary → unchanged."""
        result = Result(
            outcome="failure",
            output="output",
            metadata={"verdict": "unknown"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "unknown"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata

    def test_sentinel_echo_verdict_unchanged(self):
        """verdict='echo:success' is not in vocabulary → unchanged."""
        result = Result(
            outcome="success",
            output="echo output",
            metadata={"verdict": "echo:success"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "echo:success"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata

    # B1 judo: extended truth table covers all 5 verdict values emitted by production.
    # Previously the rewrite only knew "pass" / "fail". Now: success→pass,
    # failure→fail, error→infra_failure, partial→fail, warn→pass.

    def test_error_outcome_with_fail_verdict_rewritten_to_infra_failure(self):
        """outcome=error with verdict=fail is contradictory → rewritten to infra_failure.

        'error' is an infra state (rc!=0 / unknown verdict). The canonical verdict
        token for an error outcome is 'infra_failure', not 'fail' (which means
        "the reviewer found real problems"). This distinction lets the Healer
        cluster infra crashes separately from real review failures.
        """
        result = Result(
            outcome="error",
            output="reviewer crashed",
            metadata={"verdict": "fail"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "infra_failure"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "fail"
        assert adjusted.outcome == "error"

    def test_partial_outcome_with_pass_verdict_rewritten_to_fail(self):
        """outcome=partial with verdict=pass is contradictory → rewritten to fail.

        A 'partial' outcome means the reviewer returned mixed results; the
        canonical verdict for partial is 'fail' (not 'pass'), so downstream
        routing treats it as a failed review.
        """
        result = Result(
            outcome="partial",
            output="mixed review",
            metadata={"verdict": "pass"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "pass"
        assert adjusted.outcome == "partial"

    def test_warn_outcome_with_fail_verdict_rewritten_to_pass(self):
        """outcome=warn with verdict=fail is contradictory → rewritten to pass.

        A 'warn' outcome means the reviewer raised non-blocking concerns;
        the canonical verdict for warn is 'pass'. Without this rewrite,
        warn outcomes would inherit a 'fail' verdict that misroutes the gate.
        """
        result = Result(
            outcome="warn",
            output="warned review",
            metadata={"verdict": "fail"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "pass"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "fail"
        assert adjusted.outcome == "warn"

    def test_error_outcome_with_pass_verdict_rewritten_to_infra_failure(self):
        """outcome=error with verdict=pass → rewritten to infra_failure (NOT pass).

        Previously outcome=error was passed through unchanged. Now error outcome
        with a recognized verdict token gets rewritten to its canonical token
        'infra_failure', preserving the original for audit.
        """
        result = Result(
            outcome="error",
            output="crashed",
            metadata={"verdict": "pass"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "infra_failure"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "pass"
        assert adjusted.outcome == "error"

    def test_partial_outcome_consistent_with_fail_verdict_unchanged(self):
        """outcome=partial with verdict=fail (both map to 'failure') → unchanged.

        partial→fail is already consistent with the partial→failure mapping,
        so the override leaves the verdict intact.
        """
        result = Result(
            outcome="partial",
            output="partial review",
            metadata={"verdict": "fail"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "fail"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "partial"

    def test_warn_outcome_consistent_with_warn_verdict_unchanged(self):
        """outcome=warn with verdict=warn → unchanged (self-consistent)."""
        result = Result(
            outcome="warn",
            output="warned review",
            metadata={"verdict": "warn"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        assert adjusted.metadata["verdict"] == "warn"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "warn"
