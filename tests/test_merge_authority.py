"""Fail-closed exact-head 7-green merge authority (jleechan-goal-unattended-e2e-2026-07-17-bze8.1).

The auto-merge authority must verify all seven gates at the EXACT PR head SHA
immediately before merge. It fails closed on:

  - missing / unknown / stale / rate-limited / unparseable evidence
  - CodeRabbit not having a formal APPROVED review at the exact head SHA
  - Bugbot not having zero error-severity findings
  - any unresolved review thread
  - `/er` not returning PASS
  - the github-actions Skeptic workflow not returning PASS at the exact head

A "success status context" alone (without a formal review) is NOT
approval. A disposition note or operator assertion cannot substitute for
any gate; missing evidence blocks merge.

Per-gate telemetry includes source actor, source URL/check/review ID,
observed SHA, and timestamp — emitted on every assessment so the audit
trail is reconstructable.

Run: python3 -m pytest tests/test_merge_authority.py -v
"""

from __future__ import annotations

import json
import os
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional

ROOT = Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.merge_authority import (  # noqa: E402  (path injection above)
    GateEvidence,
    GateName,
    GateStatus,
    MergeDecision,
    MergeVerdict,
    assess_merge_authority,
)


# ---------------------------------------------------------------------------
# Test helpers
# ---------------------------------------------------------------------------


def _iso_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _all_seven_green(head_sha: str = "a" * 40) -> Dict[GateName, GateEvidence]:
    """Build the seven-gate evidence set with every gate Green at `head_sha`."""
    return {
        GateName.CI: GateEvidence(
            gate=GateName.CI,
            status=GateStatus.GREEN,
            head_sha=head_sha,
            source_actor="github-actions[bot]",
            source_url="https://github.com/jleechanorg/dark-factory/runs/1",
            source_id="check_run_1",
            observed_at=_iso_now(),
        ),
        GateName.NO_CONFLICTS: GateEvidence(
            gate=GateName.NO_CONFLICTS,
            status=GateStatus.GREEN,
            head_sha=head_sha,
            source_actor="github-api",
            source_url="https://github.com/jleechanorg/dark-factory/pulls/1",
            source_id="mergeable:MERGEABLE",
            observed_at=_iso_now(),
        ),
        GateName.CODERABBIT_APPROVED: GateEvidence(
            gate=GateName.CODERABBIT_APPROVED,
            status=GateStatus.GREEN,
            head_sha=head_sha,
            source_actor="coderabbitai",
            source_url="https://github.com/jleechanorg/dark-factory/pulls/1#pullrequestreview-1",
            source_id="review:APPROVED:1",
            observed_at=_iso_now(),
        ),
        GateName.BUGBOT_CLEAN: GateEvidence(
            gate=GateName.BUGBOT_CLEAN,
            status=GateStatus.GREEN,
            head_sha=head_sha,
            source_actor="cursor[bot]",
            source_url="https://github.com/jleechanorg/dark-factory/pulls/1",
            source_id="bugbot_error_count:0",
            observed_at=_iso_now(),
        ),
        GateName.COMMENTS_RESOLVED: GateEvidence(
            gate=GateName.COMMENTS_RESOLVED,
            status=GateStatus.GREEN,
            head_sha=head_sha,
            source_actor="github-graphql",
            source_url="https://api.github.com/graphql",
            source_id="unresolved_thread_count:0",
            observed_at=_iso_now(),
        ),
        GateName.EVIDENCE_REVIEW: GateEvidence(
            gate=GateName.EVIDENCE_REVIEW,
            status=GateStatus.GREEN,
            head_sha=head_sha,
            source_actor="claude-sonnet-4-6",
            source_url="https://github.com/jleechanorg/dark-factory/pulls/1",
            source_id="er:PASS",
            observed_at=_iso_now(),
        ),
        GateName.SKEPTIC: GateEvidence(
            gate=GateName.SKEPTIC,
            status=GateStatus.GREEN,
            head_sha=head_sha,
            source_actor="github-actions[bot]",
            source_url="https://github.com/jleechanorg/dark-factory/actions/runs/2",
            source_id="skeptic:PASS",
            observed_at=_iso_now(),
        ),
    }


# ---------------------------------------------------------------------------
# Happy path: all 7 gates green at exact head → MERGE
# ---------------------------------------------------------------------------


def test_all_seven_green_at_exact_head_merges():
    """All 7 gates Green at the exact head SHA → MERGE."""
    evidence = _all_seven_green()
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.MERGE, decision
    assert decision.failing_gate is None


# ---------------------------------------------------------------------------
# Exact-head SHA binding — every gate must bind to current head
# ---------------------------------------------------------------------------


def test_stale_coderabbit_approval_blocks_merge():
    """CodeRabbit APPROVED at an older head SHA → BLOCK as fail-closed.

    Replicates the PR293/PR300 class: a stale PASS must never satisfy a
    newer head. The auto-merge authority must SHA-bind CodeRabbit's
    APPROVED review to the current PR head.
    """
    evidence = _all_seven_green()
    evidence[GateName.CODERABBIT_APPROVED] = GateEvidence(
        gate=GateName.CODERABBIT_APPROVED,
        status=GateStatus.GREEN,
        head_sha="b" * 40,  # different SHA than the current head
        source_actor="coderabbitai",
        source_url="https://github.com/jleechanorg/dark-factory/pulls/1#pullrequestreview-1",
        source_id="review:APPROVED:1",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK, decision
    assert decision.failing_gate == GateName.CODERABBIT_APPROVED
    assert "stale" in decision.reason.lower() or "head" in decision.reason.lower()


def test_stale_skeptic_pass_blocks_merge():
    """A Skeptic PASS at an older SHA must NOT satisfy the current head.

    Same SHA-binding invariant the skeptic gate enforces: stale-SHA
    PASS is unconditionally rejected.
    """
    evidence = _all_seven_green()
    evidence[GateName.SKEPTIC] = GateEvidence(
        gate=GateName.SKEPTIC,
        status=GateStatus.GREEN,
        head_sha="c" * 40,
        source_actor="github-actions[bot]",
        source_url="https://github.com/jleechanorg/dark-factory/actions/runs/2",
        source_id="skeptic:PASS",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.SKEPTIC


def test_stale_er_pass_blocks_merge():
    """An `/er PASS` at an older head SHA must NOT satisfy the current head.

    Mirrors the verifier.rs `parse_er_verdict_since` discipline:
    comments older than the head commit are stale evidence.
    """
    evidence = _all_seven_green()
    evidence[GateName.EVIDENCE_REVIEW] = GateEvidence(
        gate=GateName.EVIDENCE_REVIEW,
        status=GateStatus.GREEN,
        head_sha="d" * 40,
        source_actor="claude-sonnet-4-6",
        source_url="https://github.com/jleechanorg/dark-factory/pulls/1",
        source_id="er:PASS",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.EVIDENCE_REVIEW


# ---------------------------------------------------------------------------
# Missing / unknown / unparseable / rate-limited evidence
# ---------------------------------------------------------------------------


def test_unknown_gate_status_blocks_merge():
    """A single Unknown gate (e.g. CodeRabbit quota wall) blocks merge.

    Fail-closed: cannot-verify is not "pass". `Unknown` differs from `Red`
    only by reason text — both block.
    """
    evidence = _all_seven_green()
    evidence[GateName.CODERABBIT_APPROVED] = GateEvidence(
        gate=GateName.CODERABBIT_APPROVED,
        status=GateStatus.UNKNOWN,
        head_sha="a" * 40,
        source_actor="coderabbitai",
        source_url="https://github.com/jleechanorg/dark-factory/pulls/1",
        source_id="rate-limited",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.CODERABBIT_APPROVED


def test_missing_gate_blocks_merge():
    """A gate without any evidence blocks merge — fail-closed on absent input."""
    evidence = _all_seven_green()
    del evidence[GateName.BUGBOT_CLEAN]
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.BUGBOT_CLEAN


def test_unparseable_evidence_blocks_merge():
    """An unparseable gate payload (no source_id, no observed_at) blocks merge.

    Garbage in / garbage out — the merge authority cannot honor evidence
    that does not satisfy the per-gate telemetry contract.
    """
    evidence = _all_seven_green()
    evidence[GateName.CI] = GateEvidence(
        gate=GateName.CI,
        status=GateStatus.GREEN,
        head_sha="a" * 40,
        source_actor="",
        source_url="",
        source_id="",
        observed_at="",
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.CI


# ---------------------------------------------------------------------------
# CodeRabbit — formal APPROVED review required (not status context)
# ---------------------------------------------------------------------------


def test_coderabbit_success_status_context_alone_is_not_approval():
    """A green CI / status context from coderabbit[bot] is NOT approval.

    The merge authority must require a formal `review:APPROVED` review
    record, not a CI status check. This is the "status-context-without-
    review" regression class.
    """
    evidence = _all_seven_green()
    # Replace the formal APPROVED review with only a CI status check that
    # happens to come from coderabbit's bot account — no review record.
    evidence[GateName.CODERABBIT_APPROVED] = GateEvidence(
        gate=GateName.CODERABBIT_APPROVED,
        status=GateStatus.GREEN,
        head_sha="a" * 40,
        source_actor="coderabbitai",
        source_url="https://github.com/jleechanorg/dark-factory/runs/99",
        source_id="check_run:coderabbit_ci",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.CODERABBIT_APPROVED
    assert "approval" in decision.reason.lower() or "review" in decision.reason.lower()


def test_coderabbit_changes_requested_blocks_merge():
    """CodeRabbit CHANGES_REQUESTED at the exact head blocks merge.

    This is the merged-head CHANGES_REQUESTED regression class — even if
    the PR has been merged elsewhere, the merge authority refuses
    without an APPROVED review at the current head.
    """
    evidence = _all_seven_green()
    evidence[GateName.CODERABBIT_APPROVED] = GateEvidence(
        gate=GateName.CODERABBIT_APPROVED,
        status=GateStatus.RED,
        head_sha="a" * 40,
        source_actor="coderabbitai",
        source_url="https://github.com/jleechanorg/dark-factory/pulls/1#pullrequestreview-2",
        source_id="review:CHANGES_REQUESTED:2",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.CODERABBIT_APPROVED


# ---------------------------------------------------------------------------
# Bugbot — zero error-severity findings required
# ---------------------------------------------------------------------------


def test_bugbot_with_error_findings_blocks_merge():
    """A Bugbot error-severity finding blocks merge.

    Bugbot must have zero error-severity findings; warn/info findings
    alone are non-blocking.
    """
    evidence = _all_seven_green()
    evidence[GateName.BUGBOT_CLEAN] = GateEvidence(
        gate=GateName.BUGBOT_CLEAN,
        status=GateStatus.RED,
        head_sha="a" * 40,
        source_actor="cursor[bot]",
        source_url="https://github.com/jleechanorg/dark-factory/pulls/1",
        source_id="bugbot_error_count:3",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.BUGBOT_CLEAN


# ---------------------------------------------------------------------------
# Disposition / operator-assertion bypass — must NOT satisfy a missing gate
# ---------------------------------------------------------------------------


def test_disposition_note_does_not_bypass_missing_gate():
    """A disposition note or operator assertion cannot bypass a missing gate.

    Even if `disposition_note` is provided and explicitly asserts "this
    PR is green", the merge authority still requires per-gate evidence
    at the exact head. Disposition is metadata, not a gate substitute.
    """
    evidence = _all_seven_green()
    del evidence[GateName.SKEPTIC]  # Skeptic gate absent
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
        disposition_note="OPERATOR_OVERRIDE: PR is green, merge it",
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.SKEPTIC
    # The disposition note MUST appear in the audit telemetry, but it MUST
    # NOT have changed the verdict.
    assert decision.disposition_note == "OPERATOR_OVERRIDE: PR is green, merge it"


def test_disposition_note_does_not_bypass_red_gate():
    """A disposition note cannot downgrade a Red gate to Green."""
    evidence = _all_seven_green()
    evidence[GateName.EVIDENCE_REVIEW] = GateEvidence(
        gate=GateName.EVIDENCE_REVIEW,
        status=GateStatus.RED,
        head_sha="a" * 40,
        source_actor="claude-sonnet-4-6",
        source_url="https://github.com/jleechanorg/dark-factory/pulls/1",
        source_id="er:FAIL",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
        disposition_note="OPERATOR_OVERRIDE: /er FAIL is a non-blocker",
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.EVIDENCE_REVIEW


# ---------------------------------------------------------------------------
# Per-gate telemetry — source actor / url / id / SHA / timestamp
# ---------------------------------------------------------------------------


def test_per_gate_telemetry_contains_required_provenance_fields():
    """Every gate's evidence must carry source actor, url/id, head SHA, timestamp."""
    evidence = _all_seven_green()
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    for gate, ev in decision.gate_telemetry.items():
        assert ev.source_actor, f"{gate}: missing source_actor"
        assert ev.source_url, f"{gate}: missing source_url"
        assert ev.source_id, f"{gate}: missing source_id"
        assert ev.head_sha, f"{gate}: missing head_sha"
        assert ev.observed_at, f"{gate}: missing observed_at"


def test_decision_serializes_for_telemetry_emission():
    """The decision shape is JSON-serializable for telemetry emission.

    `auto-merge-guard.sh` consumes the assessment line and emits it
    into the daemon log; the shape must round-trip cleanly.
    """
    evidence = _all_seven_green()
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,
        disposition_note="audit-trail test",
    )
    payload = decision.to_dict()
    encoded = json.dumps(payload)
    decoded = json.loads(encoded)
    assert decoded["verdict"] == "MERGE"
    assert decoded["pr_number"] == 1
    assert decoded["expected_head_sha"] == "a" * 40
    assert decoded["disposition_note"] == "audit-trail test"
    assert len(decoded["gate_telemetry"]) == 7
    for gate_name, ev in decoded["gate_telemetry"].items():
        assert "source_actor" in ev
        assert "source_url" in ev
        assert "source_id" in ev
        assert "head_sha" in ev
        assert "observed_at" in ev
        assert "status" in ev


# ---------------------------------------------------------------------------
# Gate set completeness — exactly seven gates required
# ---------------------------------------------------------------------------


def test_only_seven_named_gates_accepted():
    """The merge authority knows exactly seven gates and rejects unknown names."""
    from runner.merge_authority import ALL_GATE_NAMES

    assert len(ALL_GATE_NAMES) == 7


def test_extra_unknown_gate_is_ignored_but_known_gates_still_assessed():
    """Unknown keys in the gates dict are ignored; missing required gates still block.

    This protects the authority from silently swallowing a missing gate
    by means of a typo'd gate name.
    """
    evidence = _all_seven_green()
    del evidence[GateName.EVIDENCE_REVIEW]
    evidence["bogus_gate_name"] = GateEvidence(  # type: ignore[index]
        gate=GateName.CI,  # placeholder; field value doesn't matter here
        status=GateStatus.GREEN,
        head_sha="a" * 40,
        source_actor="x",
        source_url="x",
        source_id="x",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=1,
        expected_head_sha="a" * 40,
        gates=evidence,  # type: ignore[arg-type]
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.EVIDENCE_REVIEW


# ---------------------------------------------------------------------------
# Regression fixtures — PR293/PR300 classes
# ---------------------------------------------------------------------------


def test_pr293_merged_head_with_changes_requested_blocks_merge():
    """PR293 regression: PR was merged in the wild despite a CodeRabbit
    CHANGES_REQUESTED review at the current head. The merge authority
    must refuse the next merge attempt on the same head.
    """
    evidence = _all_seven_green()
    # CodeRabbit left a CHANGES_REQUESTED review at the current head.
    evidence[GateName.CODERABBIT_APPROVED] = GateEvidence(
        gate=GateName.CODERABBIT_APPROVED,
        status=GateStatus.RED,
        head_sha="a" * 40,
        source_actor="coderabbitai",
        source_url="https://github.com/jleechanorg/dark-factory/pulls/293#pullrequestreview-99",
        source_id="review:CHANGES_REQUESTED:99",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=293,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.CODERABBIT_APPROVED


def test_pr300_rate_limited_reviewer_blocks_merge():
    """PR300 regression: CodeRabbit was rate-limited and emitted no
    review at the current head. The merge authority must refuse a
    merge based on absence — `Unknown` is not approval.
    """
    evidence = _all_seven_green()
    evidence[GateName.CODERABBIT_APPROVED] = GateEvidence(
        gate=GateName.CODERABBIT_APPROVED,
        status=GateStatus.UNKNOWN,
        head_sha="a" * 40,
        source_actor="coderabbitai",
        source_url="",
        source_id="rate-limited:HTTP 429",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=300,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.CODERABBIT_APPROVED


def test_stale_sha_pass_blocks_merge_pr293_class():
    """PR293-class regression: a Skeptic PASS at a stale SHA must not
    satisfy a newer head. The gate must SHA-bind the verdict to the
    current head.
    """
    evidence = _all_seven_green()
    evidence[GateName.SKEPTIC] = GateEvidence(
        gate=GateName.SKEPTIC,
        status=GateStatus.GREEN,
        head_sha="e" * 40,  # stale
        source_actor="github-actions[bot]",
        source_url="https://github.com/jleechanorg/dark-factory/actions/runs/999",
        source_id="skeptic:PASS",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=293,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.SKEPTIC


def test_status_context_without_review_blocks_merge_pr300_class():
    """PR300-class regression: a CI status check from coderabbit[bot]
    without a formal review record is not approval. The merge
    authority must require a review:APPROVED source_id, not a
    check_run:coderabbit_ci status.
    """
    evidence = _all_seven_green()
    evidence[GateName.CODERABBIT_APPROVED] = GateEvidence(
        gate=GateName.CODERABBIT_APPROVED,
        status=GateStatus.GREEN,
        head_sha="a" * 40,
        source_actor="coderabbitai",
        source_url="https://github.com/jleechanorg/dark-factory/runs/300",
        source_id="check_run:coderabbit_ci",
        observed_at=_iso_now(),
    )
    decision = assess_merge_authority(
        pr_number=300,
        expected_head_sha="a" * 40,
        gates=evidence,
    )
    assert decision.verdict == MergeVerdict.BLOCK
    assert decision.failing_gate == GateName.CODERABBIT_APPROVED
