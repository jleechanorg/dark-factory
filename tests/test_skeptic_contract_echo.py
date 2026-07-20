"""Tests for the contract-echo review step (issue #386).

The skeptic gate currently evaluates the diff in isolation. Per
issue #386, the gate must also receive the bead's contract — the
bead description, prior-round findings, and acceptance items — and
require the reviewer to emit per-item verdicts
(`ADDRESSED file:line` / `NOT-ADDRESSED` / `N-A` with reason). Any
`NOT-ADDRESSED` for an acceptance item = gate red with that exact
item as the constraint for the next roll. The constraint extraction
MUST carry the unaddressed item verbatim so the worker reads the
exact problem, not a paraphrase.

The contract is the durable input to the gate: the bead author
writes it once, the gate enforces it on every round. A fixture
where the diff omits one noted acceptance item MUST fail closed
citing exactly that item.

Public surface under test (added to `runner.skeptic_gate`):

  - `BeadContract`          — the contract: id, description,
                              prior_findings, acceptance_items.
  - `load_bead_contract(p)` — load from a JSON file path or dict.
  - `parse_contract_echo`  — extract per-item verdicts from a
                              reviewer's `CONTRACT_ECHO:` block.
  - `evaluate_contract_echo` — fail-closed check: every
                              acceptance item must be
                              ADDRESSED or N-A; any NOT-ADDRESSED
                              makes the gate red.
  - `build_prompt(contract=...)` — embeds the contract in the
                              reviewer's prompt.

These tests are the ironclad exit criteria: passing all of them is
a hard precondition for claiming the contract-echo step is wired.
"""

from __future__ import annotations

import json

import pytest

from runner.skeptic_gate import (
    BeadContract,
    AcceptanceItem,
    PriorFinding,
    ContractEchoItem,
    ContractEchoReport,
    CONTRACT_ECHO_LINE_RE,
    load_bead_contract,
    parse_contract_echo,
    evaluate_contract_echo,
    build_prompt,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


HEAD_SHA = "abcdef1234567890abcdef1234567890abcdef12"


def _sample_contract() -> BeadContract:
    """A canonical contract with two acceptance items + one prior finding."""
    return BeadContract(
        id="jleechan-pq08",
        description=(
            "Contract-echo review step: per-item verdicts vs bead acceptance "
            "criteria + prior findings (issue #386)"
        ),
        prior_findings=(
            PriorFinding(
                source="r5 reviewer",
                text=(
                    "rN attempts ship without closing their own bead's "
                    "acceptance criteria or the prior round's findings"
                ),
            ),
        ),
        acceptance_items=(
            AcceptanceItem(
                id="A1",
                text=(
                    "fixture where the diff omits one noted acceptance item -> "
                    "gate red citing exactly that item"
                ),
            ),
            AcceptanceItem(
                id="A2",
                text=(
                    "constraint extraction carries the unaddressed items "
                    "verbatim so the worker reads the exact problem"
                ),
            ),
        ),
    )


def _addressed_output(items: list[ContractEchoItem]) -> str:
    """A reviewer output where every item is ADDRESSED with a file:line."""
    lines = [f"ITEM: {it.id} VERDICT: ADDRESSED CITE: {it.cite}" for it in items]
    return "CONTRACT_ECHO:\n" + "\n".join(lines)


def _mixed_output() -> str:
    """A reviewer output where one item is NOT-ADDRESSED (the fixture case).

    This represents the exact failure mode the issue calls out: the
    diff is small, well-scoped, and the reviewer's other gates all
    pass — but the bead's acceptance items are NOT all addressed.
    The contract-echo step MUST fail closed.
    """
    return (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        "ITEM: A2 VERDICT: NOT-ADDRESSED REASON: omitted from diff\n"
    )


# ===========================================================================
# BeadContract — loading + structural shape
# ===========================================================================


def test_load_bead_contract_from_dict():
    """The contract can be loaded from a plain dict (the typical in-memory
    shape used by callers wiring the gate into a pipeline)."""
    contract = load_bead_contract(
        {
            "id": "jleechan-pq08",
            "description": "Contract-echo review step",
            "prior_findings": [
                {"source": "r5", "text": "missing acceptance closure"},
            ],
            "acceptance_items": [
                {"id": "A1", "text": "fixture with omitted item -> red"},
                {"id": "A2", "text": "verbatim constraint extraction"},
            ],
        }
    )
    assert isinstance(contract, BeadContract)
    assert contract.id == "jleechan-pq08"
    assert len(contract.acceptance_items) == 2
    assert contract.acceptance_items[0].id == "A1"
    assert contract.acceptance_items[1].id == "A2"
    assert len(contract.prior_findings) == 1
    assert contract.prior_findings[0].source == "r5"


def test_load_bead_contract_from_json_file(tmp_path):
    """The contract can be loaded from a JSON file path (the typical
    durable shape — the bead author writes the file once, the worker
    and gate both read it)."""
    p = tmp_path / "contract.json"
    p.write_text(
        json.dumps(
            {
                "id": "jleechan-pq08",
                "description": "x",
                "prior_findings": [],
                "acceptance_items": [{"id": "A1", "text": "y"}],
            }
        )
    )
    contract = load_bead_contract(p)
    assert contract.id == "jleechan-pq08"
    assert contract.acceptance_items[0].text == "y"


def test_load_bead_contract_rejects_missing_acceptance_items():
    """A contract with no acceptance items is malformed — the gate has
    nothing to verify per-item against. Reject loudly."""
    with pytest.raises(ValueError):
        load_bead_contract(
            {
                "id": "x",
                "description": "x",
                "prior_findings": [],
                "acceptance_items": [],
            }
        )


def test_load_bead_contract_rejects_duplicate_item_ids():
    """Duplicate acceptance item IDs are ambiguous (per-item verdicts
    would not be uniquely addressable). Reject."""
    with pytest.raises(ValueError):
        load_bead_contract(
            {
                "id": "x",
                "description": "x",
                "prior_findings": [],
                "acceptance_items": [
                    {"id": "A1", "text": "first"},
                    {"id": "A1", "text": "duplicate"},
                ],
            }
        )


def test_load_bead_contract_rejects_invalid_path_type():
    """load_bead_contract must reject anything that isn't a dict or a
    path-like object — a stringified JSON literal in argv is a known
    injection surface."""
    with pytest.raises(TypeError):
        load_bead_contract(42)
    with pytest.raises(TypeError):
        load_bead_contract(["not", "a", "contract"])


# ===========================================================================
# parse_contract_echo — extract per-item verdicts from reviewer output
# ===========================================================================


def test_parse_contract_echo_extracts_all_addressed():
    contract = _sample_contract()
    output = _addressed_output(
        [
            ContractEchoItem(
                id="A1",
                verdict="ADDRESSED",
                cite="runner/skeptic_gate.py:42",
                reason="",
            ),
            ContractEchoItem(
                id="A2",
                verdict="ADDRESSED",
                cite="runner/skeptic_gate.py:43",
                reason="",
            ),
        ]
    )
    report = parse_contract_echo(output, contract)
    assert report is not None
    assert len(report.items) == 2
    assert all(it.verdict == "ADDRESSED" for it in report.items)
    assert report.items[0].cite == "runner/skeptic_gate.py:42"


def test_parse_contract_echo_rejects_non_string_input():
    contract = _sample_contract()
    assert parse_contract_echo(None, contract) is None
    assert parse_contract_echo(42, contract) is None


def test_parse_contract_echo_requires_block():
    """A reviewer output without a `CONTRACT_ECHO:` block has not
    addressed the bead's contract. The deterministic side rejects."""
    contract = _sample_contract()
    out = (
        "VERDICT: PASS\n"
        f"HEAD_SHA: {HEAD_SHA}\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 386\n"
        "REASON: ok\n"
        "IDENTITY: codex\n"
    )
    assert parse_contract_echo(out, contract) is None


def test_parse_contract_echo_handles_na_with_reason():
    contract = _sample_contract()
    out = (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        "ITEM: A2 VERDICT: N-A REASON: not applicable to this round\n"
    )
    report = parse_contract_echo(out, contract)
    assert report is not None
    na = [it for it in report.items if it.verdict == "N-A"]
    assert len(na) == 1
    assert na[0].id == "A2"
    assert "not applicable" in na[0].reason.lower()


def test_parse_contract_echo_rejects_na_without_reason():
    """`N-A` without a reason is an unsupported classification — the
    gate cannot route a 'why' to the worker. Reject."""
    contract = _sample_contract()
    out = (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        "ITEM: A2 VERDICT: N-A\n"
    )
    assert parse_contract_echo(out, contract) is None


def test_parse_contract_echo_flags_unknown_item_id():
    """If the reviewer cites an item that isn't on the contract, the
    per-item verdicts don't address the contract items. The
    deterministic side flags the gap."""
    contract = _sample_contract()
    out = (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        "ITEM: A99 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
    )
    report = parse_contract_echo(out, contract)
    assert report is not None
    ids = {it.id for it in report.items}
    assert "A99" in ids  # the unknown id is recorded
    # `evaluate_contract_echo` will catch the missing A2 below.
    covered = {it.id for it in report.items}
    assert "A2" not in covered


# ===========================================================================
# evaluate_contract_echo — the headline invariant
# ===========================================================================


def test_evaluate_contract_echo_passes_when_all_addressed():
    """Happy path: every acceptance item is ADDRESSED with a real
    file:line cite → gate green for the contract-echo step."""
    contract = _sample_contract()
    output = _addressed_output(
        [
            ContractEchoItem(
                id="A1",
                verdict="ADDRESSED",
                cite="runner/skeptic_gate.py:1",
                reason="",
            ),
            ContractEchoItem(
                id="A2",
                verdict="ADDRESSED",
                cite="runner/skeptic_gate.py:2",
                reason="",
            ),
        ]
    )
    report = parse_contract_echo(output, contract)
    assert report is not None
    verdict = evaluate_contract_echo(report, contract)
    assert verdict.ok is True
    assert verdict.unaddressed_items == ()


def test_evaluate_contract_echo_fails_closed_on_omitted_item():
    """The fixture case from issue #386: the diff omits one noted
    acceptance item. The reviewer says NOT-ADDRESSED. The gate MUST
    fail closed AND the constraint extraction MUST carry the
    unaddressed item verbatim — the worker reads the exact problem,
    not a paraphrase.
    """
    contract = _sample_contract()
    output = _mixed_output()
    report = parse_contract_echo(output, contract)
    assert report is not None
    verdict = evaluate_contract_echo(report, contract)
    assert verdict.ok is False
    assert len(verdict.unaddressed_items) == 1
    unaddressed = verdict.unaddressed_items[0]
    assert unaddressed.id == "A2"
    # Verbatim: the gate must surface the bead's acceptance text
    # EXACTLY as the author wrote it, not a paraphrase.
    assert unaddressed.text == contract.acceptance_items[1].text


def test_evaluate_contract_echo_constraint_carries_verbatim_text():
    """Headline invariant from issue #386: the constraint extraction
    carries the unaddressed items verbatim. A worker reading the
    constraint MUST see the original text, character for character."""
    contract = _sample_contract()
    output = _mixed_output()
    report = parse_contract_echo(output, contract)
    assert report is not None
    verdict = evaluate_contract_echo(report, contract)
    # The constraint string is the canonical input to the next roll —
    # the worker will read this and act on it. Paraphrasing here
    # would silently change the contract, which the contract-echo
    # step is explicitly designed to prevent.
    assert "A2" in verdict.constraint
    assert (
        "constraint extraction carries the unaddressed items verbatim"
        in verdict.constraint
    )


def test_evaluate_contract_echo_treats_missing_report_as_not_addressed():
    """A reviewer output without a `CONTRACT_ECHO:` block is
    equivalent to NOT-ADDRESSED for every item — the gate refuses
    PASS."""
    contract = _sample_contract()
    verdict = evaluate_contract_echo(None, contract)
    assert verdict.ok is False
    assert len(verdict.unaddressed_items) == 2
    assert {it.id for it in verdict.unaddressed_items} == {"A1", "A2"}


def test_evaluate_contract_echo_all_na_is_pass():
    """All `N-A` with reasons is acceptable (the items genuinely don't
    apply to this round, and the reviewer justified each one)."""
    contract = _sample_contract()
    output = (
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: N-A REASON: superseded by issue #400\n"
        "ITEM: A2 VERDICT: N-A REASON: handled in a different PR\n"
    )
    report = parse_contract_echo(output, contract)
    assert report is not None
    verdict = evaluate_contract_echo(report, contract)
    assert verdict.ok is True
    assert verdict.unaddressed_items == ()


# ===========================================================================
# build_prompt — the contract is in the prompt
# ===========================================================================


def test_build_prompt_embeds_contract_when_supplied():
    """When the caller passes a contract, the prompt MUST include the
    contract's id, every acceptance item, and the prior findings so
    the reviewer can emit per-item verdicts."""
    contract = _sample_contract()
    prompt = build_prompt(
        repo="jleechanorg/dark-factory",
        pr_number=386,
        head_sha=HEAD_SHA,
        base_sha="0000000000000000000000000000000000000000",
        diff="+x",
        implementation_identity="claude",
        contract=contract,
    )
    assert "jleechan-pq08" in prompt
    assert "A1" in prompt
    assert "A2" in prompt
    # The verbatim text of every item is in the prompt so the
    # reviewer can echo the EXACT item IDs and text the gate expects.
    assert "fixture where the diff omits" in prompt
    assert "constraint extraction carries" in prompt
    # Prior findings are surfaced so the reviewer addresses them too.
    assert "r5 reviewer" in prompt
    assert "rN attempts ship" in prompt
    # The contract-echo block format is documented for the reviewer.
    assert "CONTRACT_ECHO" in prompt
    assert "ITEM: <id> VERDICT: <ADDRESSED|NOT-ADDRESSED|N-A>" in prompt


def test_build_prompt_without_contract_omits_contract_block():
    """For backwards compatibility, build_prompt without a contract
    must NOT add the actual `# Bead contract` block (the section
    that surfaces bead_id, prior findings, and acceptance items).
    The legacy diff-only mode is still valid for repos that don't
    have a bead."""
    prompt = build_prompt(
        repo="jleechanorg/dark-factory",
        pr_number=386,
        head_sha=HEAD_SHA,
        base_sha="0000000000000000000000000000000000000000",
        diff="+x",
        implementation_identity="claude",
    )
    # These specific lines only appear in the actual block (the
    # `Bead id:` line is unique to the contract section).
    assert "## Acceptance items" not in prompt
    assert "## Prior findings" not in prompt


# ===========================================================================
# Regex sanity — the per-item line format
# ===========================================================================


def test_contract_echo_line_regex_matches_canonical_form():
    line = "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:42"
    m = CONTRACT_ECHO_LINE_RE.match(line)
    assert m is not None
    assert m.group("id") == "A1"
    assert m.group("verdict") == "ADDRESSED"
    assert m.group("cite") == "runner/skeptic_gate.py:42"


def test_contract_echo_line_regex_matches_na_with_reason():
    line = "ITEM: A2 VERDICT: N-A REASON: not applicable this round"
    m = CONTRACT_ECHO_LINE_RE.match(line)
    assert m is not None
    assert m.group("id") == "A2"
    assert m.group("verdict") == "N-A"
    assert "not applicable" in m.group("reason").lower()


# ===========================================================================
# Integration with evaluate / aggregate_results — the gate's headline check
# ===========================================================================
#
# When a contract is supplied to the gate, a reviewer output that
# passes the 10-field contract (issue #384) but does NOT include a
# valid `CONTRACT_ECHO:` block MUST fail closed. The bead's contract
# is the durable input; ignoring it is a regression.


from runner.skeptic_gate import evaluate, aggregate_results, ParsedVerdict, ParsedTestRun, ParsedLintRun  # noqa: E402


def _verdict_with_contract_echo(contract: BeadContract) -> str:
    """Build a 10-field PASS verdict + a valid `CONTRACT_ECHO:` block
    addressing every acceptance item."""
    head = HEAD_SHA
    item_lines = "\n".join(
        f"ITEM: {item.id} VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1"
        for item in contract.acceptance_items
    )
    return (
        f"VERDICT: PASS\n"
        f"HEAD_SHA: {head}\n"
        f"REPO: jleechanorg/dark-factory\n"
        f"PR_NUMBER: 386\n"
        f"REASON: ok\n"
        f"IDENTITY: codex\n"
        f"TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        f"LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        f"GREP_CITES: runner/skeptic_gate.py:1\n"
        f"HEAD_COMMIT_VERIFIED: {head}\n"
        f"CONTRACT_ECHO:\n"
        f"{item_lines}\n"
    )


def test_evaluate_passes_when_contract_echo_addresses_all_items():
    """End-to-end: a reviewer output that includes a valid
    `CONTRACT_ECHO:` block addressing every acceptance item yields a
    success state. Without the contract, the same output would also
    pass (the legacy 10-field contract is intact). With the contract,
    the block is required and verified."""
    contract = _sample_contract()
    out = _verdict_with_contract_echo(contract)
    result = evaluate(
        review_output=out,
        repo="jleechanorg/dark-factory",
        pr_number=386,
        head_sha=HEAD_SHA,
        implementation_provenance="claude",
        contract=contract,
    )
    assert result.check_state == "success"
    assert result.verdict == "PASS"


def test_evaluate_fails_closed_when_contract_echo_missing():
    """End-to-end: a reviewer output that passes the 10-field contract
    but does NOT include a `CONTRACT_ECHO:` block fails closed when a
    contract is supplied. The failure reason cites the contract-echo
    requirement, not a generic unparseable verdict."""
    contract = _sample_contract()
    out = (
        f"VERDICT: PASS\n"
        f"HEAD_SHA: {HEAD_SHA}\n"
        f"REPO: jleechanorg/dark-factory\n"
        f"PR_NUMBER: 386\n"
        f"REASON: ok\n"
        f"IDENTITY: codex\n"
        f"TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        f"LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        f"GREP_CITES: runner/skeptic_gate.py:1\n"
        f"HEAD_COMMIT_VERIFIED: {HEAD_SHA}\n"
    )
    result = evaluate(
        review_output=out,
        repo="jleechanorg/dark-factory",
        pr_number=386,
        head_sha=HEAD_SHA,
        implementation_provenance="claude",
        contract=contract,
    )
    assert result.check_state == "failure"
    assert result.verdict is None
    assert "contract" in result.reason.lower() or "acceptance" in result.reason.lower()


def test_evaluate_fails_closed_when_one_item_not_addressed():
    """End-to-end: a reviewer output with a `CONTRACT_ECHO:` block
    where one item is `NOT-ADDRESSED` fails closed and surfaces the
    verbatim constraint (issue #386 acceptance criterion)."""
    contract = _sample_contract()
    head = HEAD_SHA
    out = (
        f"VERDICT: PASS\n"
        f"HEAD_SHA: {head}\n"
        f"REPO: jleechanorg/dark-factory\n"
        f"PR_NUMBER: 386\n"
        f"REASON: ok\n"
        f"IDENTITY: codex\n"
        f"TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        f"LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        f"GREP_CITES: runner/skeptic_gate.py:1\n"
        f"HEAD_COMMIT_VERIFIED: {head}\n"
        f"CONTRACT_ECHO:\n"
        f"ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        f"ITEM: A2 VERDICT: NOT-ADDRESSED REASON: omitted from diff\n"
    )
    result = evaluate(
        review_output=out,
        repo="jleechanorg/dark-factory",
        pr_number=386,
        head_sha=HEAD_SHA,
        implementation_provenance="claude",
        contract=contract,
    )
    assert result.check_state == "failure"
    assert result.verdict is None
    # The constraint carries the verbatim text from the bead author's
    # contract — the worker reading this sees the exact problem.
    assert "A2" in result.reason
    assert (
        "constraint extraction carries the unaddressed items verbatim"
        in result.reason
    )
