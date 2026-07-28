"""Tests for the r3 contract-echo fixes (issue #386, bead jleechan-pq08).

The r2 PR #397 shipped contract-echo as a library but missed the r3 gaps
called out by the cursor-agent review of head 23bc056:

  (1) BeadContract has no `notes` field — operator guidance from
      bead.notes never reaches the reviewer prompt (only description
      is embedded).
  (2) No bead-loading path — nothing converts `br show <id>`
      (description + notes + prior findings) → contract JSON.
  (5) N-A always passes when reason is non-empty — no `required`
      item semantics, so reviewer can N-A away mandatory acceptance
      criteria.
  (7 P2) prior_findings are prompt-only — not per-item enforced.

These tests are the red baseline: each one fails on r2's head
(23bc056) and passes once the r3 implementation is in.

Following the TDD mandate: write failing test first, watch it fail,
then implement until it passes.
"""

from __future__ import annotations

import io
import json
import sys
from pathlib import Path

import pytest

from runner.skeptic_gate import (
    BeadContract,
    AcceptanceItem,
    PriorFinding,
    ContractEchoItem,
    ContractEchoReport,
    parse_contract_echo,
    evaluate_contract_echo,
    build_prompt,
    evaluate,
    load_bead_contract,
)


# ---------------------------------------------------------------------------
# Shared fixtures
# ---------------------------------------------------------------------------


def _two_item_contract() -> BeadContract:
    return BeadContract(
        id="jleechan-pq08",
        description="Contract-echo review step",
        acceptance_items=(
            AcceptanceItem(id="A1", text="must add required field"),
            AcceptanceItem(id="A2", text="must reject N-A when required"),
        ),
    )


def _three_item_contract_with_required() -> BeadContract:
    """The exact contract the round-trip test should be using: three items,
    one marked required=True. Replaces the r2 fixture which only had 2 items
    (gap 4) and which had no concept of required (gap 5).
    """
    return BeadContract(
        id="jleechan-pq08",
        description="Contract-echo review step r3",
        prior_findings=(
            PriorFinding(
                source="r2 cursor-agent",
                text="BeadContract missing notes field",
            ),
        ),
        acceptance_items=(
            AcceptanceItem(
                id="A1",
                text="BeadContract must have a notes field carrying operator guidance",
            ),
            AcceptanceItem(
                id="A2",
                text="required=true acceptance items must NOT be N-A-eligible",
                required=True,
            ),
            AcceptanceItem(
                id="A3",
                text="constraint extraction carries verbatim text",
            ),
        ),
    )


def _all_na_output(contract: BeadContract) -> str:
    head = "a" * 40
    lines = [
        "VERDICT: PASS",
        f"HEAD_SHA: {head}",
        "REPO: jleechanorg/dark-factory",
        "PR_NUMBER: 397",
        "REASON: looks fine",
        "IDENTITY: claude",
        "TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0",
        "LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0",
        "GREP_CITES: runner/skeptic_gate.py:243",
        f"HEAD_COMMIT_VERIFIED: {head}",
        "CONTRACT_ECHO:",
    ]
    for it in contract.acceptance_items:
        lines.append(f"ITEM: {it.id} VERDICT: N-A REASON: out of scope")
    for pf in contract.prior_findings:
        lines.append(f"PRIOR_FINDING: {pf.source} VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:243")
    return "\n".join(lines) + "\n"


def _mixed_required_na_output() -> str:
    """All three items reported as N-A. A1 and A3 are eligible; A2 is
    `required=True` and must be rejected as unaddressed (gap 5)."""
    return (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: N-A REASON: covered by general pattern\n"
        "ITEM: A2 VERDICT: N-A REASON: not needed\n"
        "ITEM: A3 VERDICT: N-A REASON: skip\n"
        "PRIOR_FINDING: r2 cursor-agent VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:243\n"
    )


# ===========================================================================
# (1) BeadContract has a `notes` field; build_prompt embeds it
# ===========================================================================


def test_BeadContract_has_notes_field():
    """Gap 1: BeadContract carries operator notes from bead.notes.

    The bead author types operator guidance into `br bead <id>`
    notes. The reviewer prompt must receive that guidance. r2's
    dataclass has no notes field at all — a fresh BeadContract(...)
    with `notes=...` raises TypeError.
    """
    contract = BeadContract(
        id="jleechan-pq08",
        description="",
        notes=(
            "Round r3 guidance: do not N-A away acceptance items.",
            "Wires into daemon reroll as next-round constraints.",
        ),
    )
    assert contract.notes == (
        "Round r3 guidance: do not N-A away acceptance items.",
        "Wires into daemon reroll as next-round constraints.",
    )
    # Default value: omitted notes -> empty tuple (back-compat).
    minimal = BeadContract(id="x", description="y")
    assert minimal.notes == ()


def test_build_prompt_embeds_notes_when_contract_has_notes():
    """build_prompt(contract=...) must surface bead.notes as a
    distinct "OPERATOR_GUIDANCE" or "NOTES" block — visible to the
    reviewer as a separate section so they know it's authoritative
    guidance distinct from description.
    """
    contract = BeadContract(
        id="jleechan-pq08",
        description="<desc>",
        notes=("PROBE_OPERATOR_NOTE_9876",),
    )
    prompt = build_prompt(
        diff="x",
        repo="jleechanorg/dark-factory",
        pr_number=1,
        head_sha="0" * 40,
        base_sha="0" * 40,
        contract=contract,
    )
    assert "PROBE_OPERATOR_NOTE_9876" in prompt


# ===========================================================================
# (2) bead-loading path: br show -> contract
# ===========================================================================


def test_load_bead_contract_from_br_show_output(monkeypatch):
    """Gap 2: a path from `br show <id>` (the actual bead source) to
    BeadContract. Production currently hand-authors the JSON —
    nothing reads the bead. This test fails until we add a converter
    that pulls description+notes from `br`'s structured output.
    """
    from runner import skeptic_gate

    fake_br_output = {
        "id": "jleechan-pq08",
        "description": (
            "Contract-echo review step: per-item verdicts vs bead acceptance "
            "criteria + prior findings (jleechanorg/dark-factory#386)"
        ),
        "notes": (
            "ATTEMPT r3 GUIDANCE (cursor-agent review of PR #397 head 23bc056 "
            "— 6 P1s + 1 P2): (1) BeadContract has no notes field; ...",
            "EXTERNAL REF: jleechanorg/dark-factory#386",
        ),
        "prior_findings": [
            {"source": "r2 cursor-agent", "text": "no notes field"},
        ],
        "acceptance_items": [
            {"id": "A1", "text": "must add notes field"},
            {"id": "A2", "text": "must reject N-A on required=true", "required": True},
        ],
    }

    def fake_show(_bead_id: str, br_bin: str = "br") -> str:
        return json.dumps(fake_br_output)

    monkeypatch.setattr(skeptic_gate, "_br_show_json", fake_show)

    contract = skeptic_gate.load_bead_contract_from_bead("jleechan-pq08", br_bin="br")
    assert isinstance(contract, BeadContract)
    assert contract.id == "jleechan-pq08"
    assert "ATTEMPT r3 GUIDANCE" in contract.description or "ATTEMPT r3 GUIDANCE" in "\n".join(contract.notes)
    assert any("PROBE_NOTES_BR_LOAD" in n for n in contract.notes) is False  # sanity: not the probe
    assert len(contract.acceptance_items) == 2
    assert contract.acceptance_items[1].required is True


# ===========================================================================
# (5) required=true acceptance items are not N-A-eligible
# ===========================================================================


def test_required_item_with_na_verdict_is_unaddressed():
    """Gap 5: an acceptance item marked `required=True` cannot be
    N-A'd away. r2's evaluate_contract_echo treats any N-A with a
    non-empty reason as a pass — required items get bypassed.

    With r3: N-A on required=True item -> unaddressed.
    """
    contract = _three_item_contract_with_required()
    output = _mixed_required_na_output()
    report = parse_contract_echo(output, contract)
    assert report is not None
    result = evaluate_contract_echo(report, contract)
    assert not result.ok
    # A2 was N-A but required=True -> must be unaddressed.
    unaddressed_ids = {it.id for it in result.unaddressed_items}
    assert "A2" in unaddressed_ids
    assert "A1" not in unaddressed_ids
    assert "A3" not in unaddressed_ids
    # Constraint text must carry A2's verbatim wording (the required one).
    assert "required=true acceptance items must NOT be N-A-eligible" in result.constraint


def test_required_false_na_is_still_pass():
    """Backwards-compat: N-A on a non-required item still passes
    when reason is non-empty."""
    contract = _two_item_contract()  # both default required=False
    output = (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: N-A REASON: out of scope\n"
        "ITEM: A2 VERDICT: N-A REASON: irrelevant\n"
    )
    report = parse_contract_echo(output, contract)
    result = evaluate_contract_echo(report, contract)
    assert result.ok


# ===========================================================================
# (7 P2) prior_findings are per-item enforced, not just prompt-only
# ===========================================================================


def test_evaluate_contract_echo_returns_prior_findings_unaddressed(monkeypatch):
    """Gap 7 P2: prior_findings currently flow into the prompt but
    are not per-item enforced. r2's evaluate_contract_echo doesn't
    check them at all. r3 adds Optional(report_prior_findings=...)
    so the gate fails closed on unaddressed prior findings too.
    """
    contract = _three_item_contract_with_required()
    # Add a second prior finding that the reviewer must address.
    pf = PriorFinding(
        source="r2 CodeRabbit",
        text="PR #397 needs test_cli_wiring for contract-file",
    )
    contract2 = BeadContract(
        id=contract.id,
        description=contract.description,
        prior_findings=contract.prior_findings + (pf,),
        acceptance_items=contract.acceptance_items,
    )

    # A reviewer who only emits acceptance-item verdicts but omits
    # prior-findings coverage = fail.
    output = (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        "ITEM: A2 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:2\n"
        "ITEM: A3 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:3\n"
    )
    report = parse_contract_echo(output, contract2)

    from runner import skeptic_gate as sg
    result = sg.evaluate_contract_echo(
        report,
        contract2,
        report_prior_findings=(),
    )
    assert not result.ok
    # The unaddressed prior finding must surface verbatim in the
    # constraint string so the next roll's worker sees it.
    assert "PR #397 needs test_cli_wiring for contract-file" in result.constraint


# ===========================================================================
# End-to-end: evaluate() contract-echo path with required=true
# ===========================================================================


def test_evaluate_fails_when_required_item_na_with_reason():
    """End-to-end gate: a reviewer that emits ALL required fields,
    a clean 10-field contract, and N-As a required=true item must
    fail the gate closed. The failure reason must cite the required
    item verbatim.
    """
    contract = _three_item_contract_with_required()
    output = _all_na_output(contract)
    head = "a" * 40
    res = evaluate(
        review_output=output,
        repo="jleechanorg/dark-factory",
        pr_number=397,
        head_sha=head,
        contract=contract,
    )
    assert res.check_state == "failure"
    assert res.verdict is None
    # Required item A2 must be in the failure reason.
    assert "required=true acceptance items must NOT be N-A-eligible" in res.reason


# ===========================================================================
# AcceptanceItem accepts required kwarg
# ===========================================================================


def test_AcceptanceItem_required_kwarg():
    """AcceptanceItem now accepts `required: bool` (default False)."""
    required_item = AcceptanceItem(id="X1", text="x", required=True)
    assert required_item.required is True
    opt_item = AcceptanceItem(id="X2", text="x")
    assert opt_item.required is False
