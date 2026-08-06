"""TDD red→green evidence for the contract-echo extraction (bead jleechan-t5ld).

These tests prove the extraction itself — the contract-echo subsystem
now lives in `runner.skeptic_contract_echo` and `runner.skeptic_gate`
re-exports the symbols for back-compat.

Pre-extraction (red) baseline: at PR #397 head 23bc056 these tests
would fail because the symbols are not yet in `skeptic_contract_echo`.
Post-extraction (green) baseline: at this PR's head, all four pass —
the new module owns the subsystem and the re-exports wire through.
"""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from runner import skeptic_contract_echo
from runner import skeptic_gate


def test_contract_echo_subsystem_lives_in_new_module():
    """The dataclasses and helpers now live in `skeptic_contract_echo`,
    not in `skeptic_gate`. The re-exports are wired so `skeptic_gate`
    still resolves them, but the canonical `__module__` of every
    contract-echo class is the new module.
    """
    assert skeptic_contract_echo.AcceptanceItem.__module__ == "runner.skeptic_contract_echo"
    assert skeptic_contract_echo.BeadContract.__module__ == "runner.skeptic_contract_echo"
    assert skeptic_contract_echo.PriorFinding.__module__ == "runner.skeptic_contract_echo"
    assert skeptic_contract_echo.ContractEchoItem.__module__ == "runner.skeptic_contract_echo"
    assert skeptic_contract_echo.ContractEchoReport.__module__ == "runner.skeptic_contract_echo"
    assert skeptic_contract_echo.ContractEchoVerdictResult.__module__ == "runner.skeptic_contract_echo"
    assert skeptic_contract_echo.PriorFindingEcho.__module__ == "runner.skeptic_contract_echo"


def test_skeptic_gate_reexports_contract_echo_symbols():
    """`skeptic_gate` re-exports the contract-echo symbols so legacy
    callers (skeptic_gate_cli, tests that pre-date the extraction)
    keep working without modification.
    """
    # Same identity — re-exports share the underlying class object.
    assert skeptic_gate.AcceptanceItem is skeptic_contract_echo.AcceptanceItem
    assert skeptic_gate.BeadContract is skeptic_contract_echo.BeadContract
    assert skeptic_gate.PriorFinding is skeptic_contract_echo.PriorFinding
    assert skeptic_gate.ContractEchoItem is skeptic_contract_echo.ContractEchoItem
    assert skeptic_gate.ContractEchoReport is skeptic_contract_echo.ContractEchoReport
    assert skeptic_gate.ContractEchoVerdictResult is skeptic_contract_echo.ContractEchoVerdictResult
    assert skeptic_gate.PriorFindingEcho is skeptic_contract_echo.PriorFindingEcho
    assert skeptic_gate.load_bead_contract is skeptic_contract_echo.load_bead_contract
    assert skeptic_gate.load_bead_contract_from_bead is skeptic_contract_echo.load_bead_contract_from_bead
    assert skeptic_gate.parse_contract_echo is skeptic_contract_echo.parse_contract_echo
    assert skeptic_gate.evaluate_contract_echo is skeptic_contract_echo.evaluate_contract_echo
    assert skeptic_gate._strip_contract_echo_block is skeptic_contract_echo._strip_contract_echo_block
    assert skeptic_gate.CONTRACT_ECHO_LINE_RE is skeptic_contract_echo.CONTRACT_ECHO_LINE_RE


def test_br_show_json_subprocess_wrapper_lives_in_new_module():
    """`_br_show_json` moved to the new module — tests that monkeypatch
    it MUST target `skeptic_contract_echo._br_show_json`, not
    `skeptic_gate._br_show_json`.
    """
    # The helper is bound to the new module's globals.
    func_globals_name = skeptic_contract_echo._br_show_json.__globals__["__name__"]
    assert func_globals_name == "runner.skeptic_contract_echo"

    # `skeptic_gate` no longer carries its own copy of `_br_show_json`.
    assert not hasattr(skeptic_gate, "_br_show_json")


def test_load_bead_contract_from_bead_uses_new_module_subprocess(monkeypatch):
    """End-to-end: stubbing `_br_show_json` on the NEW module makes
    `load_bead_contract_from_bead` (whether imported via skeptic_gate
    or skeptic_contract_echo) use the stub. Same dict shape that
    `br show --json <id>` would return, fed through the same loader.
    """
    payload = {
        "id": "jleechan-t5ld",
        "description": "extract contract-echo",
        "acceptance_items": [{"id": "A1", "text": "module moved"}],
    }

    def fake_show(_bead_id: str, br_bin: str = "br") -> str:
        import json as _json
        return _json.dumps(payload)

    monkeypatch.setattr(skeptic_contract_echo, "_br_show_json", fake_show)

    # Through the new module directly.
    contract = skeptic_contract_echo.load_bead_contract_from_bead("jleechan-t5ld")
    assert contract.id == "jleechan-t5ld"
    assert len(contract.acceptance_items) == 1
    assert contract.acceptance_items[0].id == "A1"
    assert contract.acceptance_items[0].text == "module moved"

    # Through the re-export on skeptic_gate (legacy callers).
    contract_legacy = skeptic_gate.load_bead_contract_from_bead("jleechan-t5ld")
    assert contract_legacy == contract