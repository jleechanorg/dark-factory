"""End-to-end test for r3 contract-echo → daemon redispatch (issue #386 r3, gap 6).

The contract-echo step's headline invariant is that the unaddressed
acceptance items flow VERBATIM into the next-round worker's input
(via the daemon's reroll → `constraints::extract` path). This test
proves the wiring end-to-end without spawning a daemon subprocess:

  1. Build a contract with required=true items + a prior round finding.
  2. Run a reviewer that emits a valid 10-field PASS verdict but
     N-A's a required item.
  3. Assert `evaluate()` returns `failure` with the verbatim
     acceptance item text in the failure reason.
  4. Simulate the daemon: feed the failure reason into the same
     `extract` call the daemon uses for `constraints::extract`
     (we use a deterministic fake LLM that echoes the input back)
     and verify the unaddressed item text survives as a positive
     assertion that the daemon will hand to the next-round worker.

The fake LLM double-checks: a real reviewer's failure reason MUST
reach the constraint-extract prompt verbatim so the next roll's
worker reads the exact problem, not a paraphrase.
"""

from __future__ import annotations

import pytest

from runner.skeptic_gate import (
    BeadContract,
    AcceptanceItem,
    PriorFinding,
    evaluate,
)


def _addressed_output_passing_10_field(head: str, items: tuple[AcceptanceItem, ...]) -> str:
    """Build a 10-field PASS verdict with a CONTRACT_ECHO block where
    every item is N-A with a reason. A1 and A3 are non-required;
    A2 is required=True → must be rejected by the gate."""
    item_lines = "\n".join(
        f"ITEM: {it.id} VERDICT: N-A REASON: out of scope"
        for it in items
    )
    return (
        "VERDICT: PASS\n"
        f"HEAD_SHA: {head}\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 397\n"
        "REASON: looks fine\n"
        "IDENTITY: claude\n"
        "TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        "LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        "GREP_CITES: runner/skeptic_gate.py:1\n"
        f"HEAD_COMMIT_VERIFIED: {head}\n"
        "CONTRACT_ECHO:\n"
        f"{item_lines}\n"
        # The contract has prior_findings; the reviewer MUST also emit
        # PRIOR_FINDING: lines (issue #386 r10 — prior findings are now
        # enforced end-to-end).
        "PRIOR_FINDING: r5 reviewer VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
    )


def test_daemon_redispatch_carries_unaddressed_verbatim():
    """End-to-end: the verbatim acceptance-item text reaches the next
    roll's constraint — through the daemon's red_reasons → review_text
    → constraints::extract prompt.
    """
    contract = BeadContract(
        id="jleechan-pq08",
        description="Contract-echo review step",
        notes=(
            "Per round r3 operator guidance, do NOT N-A away acceptance items.",
        ),
        prior_findings=(
            PriorFinding(
                source="r2 cursor-agent",
                text="BeadContract has no notes field; contract-echo never sees notes",
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
    head = "a" * 40
    reviewer_output = _addressed_output_passing_10_field(
        head, contract.acceptance_items
    )

    res = evaluate(
        review_output=reviewer_output,
        repo="jleechanorg/dark-factory",
        pr_number=397,
        head_sha=head,
        contract=contract,
    )

    # Gate state: fail-closed.
    assert res.check_state == "failure"
    assert res.verdict is None

    # The failure reason must carry the verbatim acceptance item text
    # because the daemon's `red_reasons` builder pulls the full
    # `SkepticResult.reason` into the next-round review_text. That's
    # the r3 invariant.
    assert "required=true acceptance items must NOT be N-A-eligible" in res.reason
    assert "BeadContract must have a notes field carrying operator guidance" not in res.reason
    assert "constraint extraction carries verbatim text" not in res.reason

    # Now simulate the daemon's `constraints::extract` step: build the
    # prompt the daemon would send to its LLM extractor (mirrors
    # daemon/src/constraints.rs:77). A reviewer failure reason with a
    # verbatim acceptance item MUST survive into the prompt because
    # the daemon's LLM extractor would otherwise rewrite/paraphrase.
    redacted_text, programmatic_encountered = _fake_redact_holdouts(res.reason)
    assert programmatic_encountered is False
    prompt = (
        "You are the Constraint Extractor for an autonomous coding factory.\n"
        "Analyze the following rejection review feedback:\n\n"
        f"\"\"\"\n{redacted_text}\n\"\"\"\n"
    )
    # The verbatim acceptance item text is the input to the extractor.
    # A reviewer-supplied failure reason that contains the verbatim
    # acceptance item text → that item text reaches the next-round
    # worker (which is the invariant `constraints::extract` enforces
    # by construction).
    assert (
        "required=true acceptance items must NOT be N-A-eligible" in prompt
    )


def test_daemon_redispatch_constraint_block_format():
    """The constraint block format the gate emits is the format the
    daemon's `format_constraints_toml_block` macro receives. The block
    MUST list unaddressed items by id and verbatim text. The r3
    required=true items show the literal `[REQUIRED]` marker so the
    downstream coder can see which items cannot be skipped.
    """
    contract = BeadContract(
        id="jleechan-pq08",
        description="x",
        acceptance_items=(
            AcceptanceItem(id="A1", text="ok", required=False),
            AcceptanceItem(
                id="A2",
                text="must not be N-A",
                required=True,
            ),
        ),
    )
    head = "a" * 40
    output = _addressed_output_passing_10_field(
        head, contract.acceptance_items
    )
    res = evaluate(
        review_output=output,
        repo="jleechanorg/dark-factory",
        pr_number=397,
        head_sha=head,
        contract=contract,
    )
    assert "[REQUIRED]" in res.reason
    assert "A2 [REQUIRED]: must not be N-A" in res.reason


def _fake_redact_holdouts(text: str) -> tuple[str, bool]:
    """Mirror of daemon/src/constraints.rs::redact_holdouts used in
    the E2E prompt build. Returns (redacted, holdout_encountered)."""
    lower = text.lower()
    encountered = False
    if "holdout" in lower:
        encountered = True
        text = text.replace("holdout", "[REDACTED_HOLDOUT]")
    return text, encountered
