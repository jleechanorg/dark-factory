"""Tests for the structured reviewer reproduction receipt gate (issue #426).

Issue #426 (ZFC): the regex-based receipt gate in ``runner/handler_verdict.py``
classifies free-text prose with ``_RECEIPT_RUNNER_RE`` / ``_RECEIPT_EXIT_RE`` —
a sloppy or adversarial reviewer can fabricate "ran: pytest / exit code: 0"
and pass the gate without ever running anything. The fix is to capture the
*execution* itself as a structured event the gate verifies, not the prose.

This module tests the structured path:

  1. ``_record_reviewer_receipt`` writes a typed record into ctx.state.
  2. ``_check_structured_receipt`` validates the captured records against the
     outcome: at least one record with ``exit_code == 0`` and a matching
     ``head_sha``.
  3. ``_enforce_reproduction_receipt`` consults the structured path FIRST.
  4. The regex path is retained ONLY as a low-trust fallback when no
     structured receipts exist (back-compat for handlers that haven't been
     instrumented yet).

Coverage matrix:
  - Fabricated prose with no real execution → must FAIL (regex ceiling closes).
  - Real exit=0 + matching SHA → must PASS.
  - Real nonzero exit → must FAIL.
  - SHA mismatch (record claims a different SHA than the worktree) → must FAIL.
  - No structured receipts → fall back to regex path (low-trust, marked).
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.handler_core import Result  # noqa: E402
import runner.handlers  # noqa: F401 - forces full module init before handler_parallel_reviewer
from runner.handler_verdict import (  # noqa: E402
    _check_structured_receipt,
    _record_reviewer_receipt,
    _reproduction_receipt_gap,
    _reset_reviewer_receipts_for_test,
)
from runner.handler_parallel_reviewer import _enforce_reproduction_receipt  # noqa: E402


# -- minimal ctx stub for state-only testing -------------------------------


class _Ctx:
    def __init__(self, workdir: pathlib.Path | None = None):
        self.state: dict = {}
        self.workdir = workdir


# ---------------------------------------------------------------------------
# Structured-receipt primitives
# ---------------------------------------------------------------------------


class TestRecordReviewerReceipt:
    def test_records_command_cwd_exit_sha_lane(self, tmp_path):
        ctx = _Ctx(workdir=tmp_path)
        _record_reviewer_receipt(
            ctx,
            command=["uv", "run", "pytest", "-q"],
            cwd=str(tmp_path),
            exit_code=0,
            head_sha="abcdef0123456789abcdef0123456789abcdef01",
            lane_id="primary",
        )
        receipts = ctx.state["_reviewer_receipts"]
        assert len(receipts) == 1
        rec = receipts[0]
        assert rec["command"] == ["uv", "run", "pytest", "-q"]
        assert rec["cwd"] == str(tmp_path)
        assert rec["exit_code"] == 0
        assert rec["head_sha"] == "abcdef0123456789abcdef0123456789abcdef01"
        assert rec["lane_id"] == "primary"

    def test_appends_multiple_receipts(self, tmp_path):
        ctx = _Ctx(workdir=tmp_path)
        sha = "abcdef0123456789abcdef0123456789abcdef01"
        for lane in ("primary", "shadow_codex"):
            _record_reviewer_receipt(
                ctx, command=["echo", "x"], cwd=str(tmp_path),
                exit_code=0, head_sha=sha, lane_id=lane,
            )
        assert len(ctx.state["_reviewer_receipts"]) == 2


class TestCheckStructuredReceipt:
    HEAD = "abcdef0123456789abcdef0123456789abcdef01"

    def test_no_receipts_returns_structured_gap(self, tmp_path):
        ctx = _Ctx(workdir=tmp_path)
        gap = _check_structured_receipt(ctx, expected_sha=self.HEAD)
        assert "structured receipt" in gap
        assert "no execution recorded" in gap

    def test_exit_zero_with_matching_sha_holds(self, tmp_path):
        ctx = _Ctx(workdir=tmp_path)
        _record_reviewer_receipt(
            ctx, command=["pytest"], cwd=str(tmp_path),
            exit_code=0, head_sha=self.HEAD, lane_id="primary",
        )
        assert _check_structured_receipt(ctx, expected_sha=self.HEAD) == ""

    def test_only_nonzero_exits_fails(self, tmp_path):
        ctx = _Ctx(workdir=tmp_path)
        _record_reviewer_receipt(
            ctx, command=["pytest"], cwd=str(tmp_path),
            exit_code=1, head_sha=self.HEAD, lane_id="primary",
        )
        gap = _check_structured_receipt(ctx, expected_sha=self.HEAD)
        assert "FAILED" in gap
        assert "1" in gap

    def test_sha_mismatch_fails_even_with_zero_exit(self, tmp_path):
        """A reviewer can claim an exit-zero record under a different SHA —
        this is the exact spoof that closes when the structured path binds
        the receipt to the worktree head the gate is grading.
        """
        ctx = _Ctx(workdir=tmp_path)
        _record_reviewer_receipt(
            ctx, command=["pytest"], cwd=str(tmp_path),
            exit_code=0, head_sha="0000000000000000000000000000000000000000",
            lane_id="primary",
        )
        gap = _check_structured_receipt(ctx, expected_sha=self.HEAD)
        assert "head_sha mismatch" in gap

    def test_mixed_lanes_with_one_good_receipt_holds(self, tmp_path):
        ctx = _Ctx(workdir=tmp_path)
        _record_reviewer_receipt(
            ctx, command=["setup"], cwd=str(tmp_path),
            exit_code=1, head_sha=self.HEAD, lane_id="primary",
        )
        _record_reviewer_receipt(
            ctx, command=["pytest"], cwd=str(tmp_path),
            exit_code=0, head_sha=self.HEAD, lane_id="primary",
        )
        assert _check_structured_receipt(ctx, expected_sha=self.HEAD) == ""


# ---------------------------------------------------------------------------
# _enforce_reproduction_receipt: structured-first behavior
# ---------------------------------------------------------------------------


class TestEnforceReceiptStructuredFirst:
    HEAD = "abcdef0123456789abcdef0123456789abcdef01"

    def setup_method(self):
        # Always reset cross-test state before each case so leakage from
        # earlier receipts (state is module-level) does not bleed.
        _reset_reviewer_receipts_for_test()

    def test_no_structured_receipts_falls_back_to_regex_low_trust(self):
        """Back-compat path: handlers that have not been instrumented yet
        still get the regex gate (with low-trust semantics). The receipt
        gate marks the downgrade as ``receipt_low_trust="regex"`` so audit
        readers see the path was used.
        """
        # Fabricated prose that the regex gate would have caught only as a
        # documented limit (see test_reviewer_reproduction_receipt.py).
        result = Result(
            outcome="success",
            output="All good. Verdict: PASS",
            metadata={"verdict": "pass", "_reviewer_receipts": []},
        )
        adjusted = _enforce_reproduction_receipt(result, expected_sha=self.HEAD)
        # The regex path STILL catches "no transcript" → receipt missing.
        assert adjusted.outcome == "failure"
        assert adjusted.metadata.get("receipt_downgraded") == "true"
        # Path label identifies low-trust regex fallback.
        assert adjusted.metadata.get("receipt_path") == "regex_low_trust"

    def test_structured_exit_zero_with_matching_sha_passes(self):
        receipts = [
            {
                "command": ["pytest", "-q"],
                "cwd": "/tmp",
                "exit_code": 0,
                "head_sha": self.HEAD,
                "lane_id": "primary",
            }
        ]
        result = Result(
            outcome="success",
            output="All good. Verdict: PASS",
            metadata={"verdict": "pass", "_reviewer_receipts": receipts},
        )
        adjusted = _enforce_reproduction_receipt(result, expected_sha=self.HEAD)
        assert adjusted.outcome == "success"
        assert adjusted.metadata.get("receipt_path") == "structured"
        assert "receipt_downgraded" not in adjusted.metadata

    def test_structured_exit_nonzero_downgrades_even_with_spoofed_prose(self):
        """The structured-path ground truth: a real nonzero exit CANNOT be
        hidden by review-output prose that claims success.
        """
        receipts = [
            {
                "command": ["pytest", "-q"],
                "cwd": "/tmp",
                "exit_code": 2,
                "head_sha": self.HEAD,
                "lane_id": "primary",
            }
        ]
        result = Result(
            outcome="success",
            # Reviewer prose claims PASS despite the real failure —
            # the structured receipt wins.
            output="Tests pass! exit code: 0. Verdict: PASS",
            metadata={"verdict": "pass", "_reviewer_receipts": receipts},
        )
        adjusted = _enforce_reproduction_receipt(result, expected_sha=self.HEAD)
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        assert adjusted.metadata["receipt_path"] == "structured"
        assert "FAILED" in adjusted.metadata["receipt_gap"]

    def test_structured_sha_mismatch_downgrades_even_with_zero_exit(self):
        """A reviewer claiming an exit-zero record under a different SHA
        cannot pass — the receipt binds to the worktree head the gate is
        grading.
        """
        receipts = [
            {
                "command": ["pytest", "-q"],
                "cwd": "/tmp",
                "exit_code": 0,
                # Different from the head the gate is reviewing.
                "head_sha": "0000000000000000000000000000000000000000",
                "lane_id": "primary",
            }
        ]
        result = Result(
            outcome="success",
            output="Verdict: PASS",
            metadata={"verdict": "pass", "_reviewer_receipts": receipts},
        )
        adjusted = _enforce_reproduction_receipt(result, expected_sha=self.HEAD)
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_path"] == "structured"
        assert "head_sha mismatch" in adjusted.metadata["receipt_gap"]

    def test_failure_outcome_never_touched_by_structured_path(self):
        """Failure and error outcomes are pass-through; the receipt gate
        only protects success.
        """
        receipts = [
            {
                "command": ["pytest"], "cwd": "/tmp", "exit_code": 0,
                "head_sha": self.HEAD, "lane_id": "primary",
            }
        ]
        result = Result(
            outcome="failure",
            output="Verdict: FAIL",
            metadata={"verdict": "fail", "_reviewer_receipts": receipts},
        )
        assert _enforce_reproduction_receipt(result, expected_sha=self.HEAD) is result

    def test_preserves_pre_existing_original_verdict(self):
        """The structured path must honor ``original_verdict`` left by
        consistency normalization, exactly like the regex path.
        """
        receipts: list = []
        result = Result(
            outcome="success",
            output="narrative only",
            metadata={
                "verdict": "pass",
                "verdict_adjusted_for_consistency": "true",
                "original_verdict": "approve",
                "_reviewer_receipts": receipts,
            },
        )
        adjusted = _enforce_reproduction_receipt(result, expected_sha=self.HEAD)
        assert adjusted.metadata["original_verdict"] == "approve"
        assert adjusted.metadata["verdict"] == "fail"


# ---------------------------------------------------------------------------
# Existing regex path still works (no regression).
# ---------------------------------------------------------------------------


class TestRegexPathUnchanged:
    """The regex gap detector remains untouched for back-compat."""

    def test_successful_reproduction_holds(self):
        transcript = "$ uv run pytest\n3 passed\nexit code: 0\n"
        assert _reproduction_receipt_gap(transcript) == ""

    def test_failed_reproduction_is_a_gap(self):
        transcript = "$ uv run pytest\n2 failed\nexit code: 1\n"
        gap = _reproduction_receipt_gap(transcript)
        assert "FAILED" in gap