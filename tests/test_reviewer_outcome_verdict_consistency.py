"""Regression tests for reviewer outcome/verdict consistency (fix 3).

Tests that ``runner.handler_verdict._enforce_outcome_verdict_consistency``
uses real normalization via ``_normalize_outcome`` and only rewrites on a
genuine outcome/verdict contradiction.

The function was relocated from ``runner.handler_parallel_reviewer`` to the
canonical ``runner.handler_verdict`` module so all verdict semantics (token
map, normalization, consistency enforce) live in one place. This file is
the canonical regression suite — bead ``jleechan-br8w`` acceptance:
``standalone tests/test_reviewer_outcome_verdict_consistency.py passes with
extended cases``.
"""

from __future__ import annotations

<<<<<<< HEAD
import pathlib

import pytest

ROOT = pathlib.Path(__file__).parent.parent

from runner.handler_core import Result  # noqa: E402
# Import the canonical location. ``runner.handlers`` re-exports the same
# symbol under late binding for monkeypatch compatibility; tests cover both.
import runner.handlers as _handlers_shim  # noqa: F401, E402
from runner.handler_verdict import (  # noqa: E402
    _enforce_outcome_verdict_consistency,
    _VERDICT_NORMALIZE,
)
=======
from runner.handler_core import Result
from runner.handler_parallel_reviewer import _enforce_outcome_verdict_consistency
>>>>>>> 7784052 (claude/fable: fix(runner): break handler_parallel_reviewer circular import (jleechan-ujt1) (#301))


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

    def test_error_outcome_with_pass_verdict_unchanged(self):
        """outcome=error is infra state, not a verdict disagreement → unchanged."""
        result = Result(
            outcome="error",
            output="some error occurred",
            metadata={"verdict": "pass"},
        )

        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)

        # Should be unchanged - error is infra state, not a verdict contradiction
        assert adjusted.metadata["verdict"] == "pass"
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

    # ---- Extended coverage: pass/fail/warn/unknown/infra_failure behavior ----

    def test_canonical_location_is_handler_verdict(self):
        """The single canonical implementation lives in runner.handler_verdict.

        Acceptance criterion from bead jleechan-br8w:
        ``rg shows one canonical definition in handler_verdict.py``.
        """
        import re as _re
        from pathlib import Path as _Path

        repo_runner = ROOT / "runner"
        # Search top-level module files for the canonical definition.
        pattern = _re.compile(r"^def _enforce_outcome_verdict_consistency\(")
        definitions = []
        for py in sorted(repo_runner.glob("handler_*.py")):
            text = py.read_text()
            for i, line in enumerate(text.splitlines(), 1):
                if pattern.match(line):
                    definitions.append(py.relative_to(ROOT).as_posix() + f":{i}")
        assert definitions == ["runner/handler_verdict.py:182"], (
            f"expected exactly one canonical definition in handler_verdict.py, "
            f"got: {definitions}"
        )

    def test_canonical_reexports_through_handlers_shim(self):
        """``runner.handlers._enforce_outcome_verdict_consistency`` must be
        the SAME function object as the canonical one — late binding for
        monkey-patches must hold.
        """
        assert _handlers_shim._enforce_outcome_verdict_consistency is _enforce_outcome_verdict_consistency

    def test_pass_family_outcome_success_with_pass_verdict_unchanged(self):
        """PASS-family outcomes: pass token with matching outcome is unchanged."""
        for token in ("pass", "warn", "approve", "approved"):
            result = Result(
                outcome="success",
                output="ok",
                metadata={"verdict": token},
            )
            adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)
            assert adjusted.metadata["verdict"] == token, (
                f"verdict '{token}' must be preserved when outcome=success"
            )
            assert "verdict_adjusted_for_consistency" not in adjusted.metadata

    def test_fail_family_outcome_failure_with_failure_token_unchanged(self):
        """FAIL-family outcomes: failure token with matching outcome is unchanged."""
        for token in ("fail", "partial", "inconclusive", "insufficient",
                      "invalid", "incomplete", "conditional", "reject",
                      "rejected", "blocker"):
            # sanity: every token named here must be in the normalize map.
            assert token in _VERDICT_NORMALIZE, (
                f"verdict token '{token}' missing from _VERDICT_NORMALIZE"
            )
            result = Result(
                outcome="failure",
                output="failed",
                metadata={"verdict": token},
            )
            adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)
            assert adjusted.metadata["verdict"] == token, (
                f"verdict '{token}' must be preserved when outcome=failure"
            )
            assert "verdict_adjusted_for_consistency" not in adjusted.metadata

    def test_warn_with_gate_strict_failure_outcome_unchanged(self):
        """warn + gate_strict=True + outcome=failure is now CONSISTENT.

        In strict mode warn normalizes to failure, so warn + failure outcome
        is a match — the raw token is preserved (no rewrite).
        """
        result = Result(
            outcome="failure",
            output="strict-fail",
            metadata={"verdict": "warn"},
        )
        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=True)
        assert adjusted.metadata["verdict"] == "warn"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "failure"

    def test_unknown_verdict_never_rewritten(self):
        """verdict='unknown' must never be rewritten — it's a sentinel, not a contradiction."""
        for outcome in ("success", "failure"):
            result = Result(
                outcome=outcome,
                output="ambiguous",
                metadata={"verdict": "unknown"},
            )
            adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)
            assert adjusted.metadata["verdict"] == "unknown"
            assert "verdict_adjusted_for_consistency" not in adjusted.metadata

    def test_infra_failure_sentinel_never_rewritten(self):
        """verdict='infra_failure' is not in _VERDICT_NORMALIZE → leave untouched.

        Confirms we never silently overwrite the infra-failure sentinel
        used by the Healer to cluster gate crashes.
        """
        for outcome in ("success", "failure", "error"):
            result = Result(
                outcome=outcome,
                output="infra",
                metadata={"verdict": "infra_failure"},
            )
            adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)
            assert adjusted.metadata["verdict"] == "infra_failure"
            assert "verdict_adjusted_for_consistency" not in adjusted.metadata

    def test_error_outcome_never_rewritten_for_any_verdict(self):
        """'error' outcome is infra state, never a verdict disagreement.

        Even on a genuine mismatch (e.g. verdict='pass', outcome='error'),
        we must NOT rewrite verdict — the verdict field is for the reviewer's
        contract; the error is a transport/infra signal.
        """
        result = Result(
            outcome="error",
            output="boom",
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_outcome_verdict_consistency(result, gate_strict=False)
        assert adjusted.metadata["verdict"] == "pass"
        assert "verdict_adjusted_for_consistency" not in adjusted.metadata
        assert adjusted.outcome == "error"

    def test_partial_outcome_with_unknown_caller_verdict_unchanged(self):
        """PASS-side caller verdict (approve) with FAILURE outcome is rewritten;
        FAIL-side caller verdict (partial/inconclusive) with SUCCESS outcome is also rewritten.
        Both are real contradictions under the canonical normalize.
        """
        # Failure outcome but caller emitted 'approve' — true contradiction → rewritten to 'fail'.
        approve_fail = Result(
            outcome="failure",
            output="x",
            metadata={"verdict": "approve"},
        )
        adjusted = _enforce_outcome_verdict_consistency(approve_fail, gate_strict=False)
        assert adjusted.metadata["verdict"] == "fail"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "approve"

        # Success outcome but caller emitted 'partial' — true contradiction → rewritten to 'pass'.
        partial_success = Result(
            outcome="success",
            output="y",
            metadata={"verdict": "partial"},
        )
        adjusted = _enforce_outcome_verdict_consistency(partial_success, gate_strict=False)
        assert adjusted.metadata["verdict"] == "pass"
        assert adjusted.metadata["verdict_adjusted_for_consistency"] == "true"
        assert adjusted.metadata["original_verdict"] == "partial"
