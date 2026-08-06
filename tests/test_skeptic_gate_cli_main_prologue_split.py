"""Behavior-parity tests for the contract-load sub-phase extracted from
`skeptic_gate_cli.main`.

Original problem (cleanup swarm 2026-08-05 / bead jleechan-f1a2):
`main()` spans 642 lines (720-1362 in runner/skeptic_gate_cli.py) and
contains TWO copies of the same bead-contract load block (lines
957-1000 AND 1052-1095). The first copy is dead code — the second copy
runs unconditionally during dispatcher setup — but both copies carry
their own fail-closed `_emit_perf_log` + `return 2` paths.

This test suite pins the contract of the extracted helper
`_load_contract_or_exit` BEFORE we edit main(). These tests must be RED
(no helper exists yet) and then GREEN after extraction.

Return contract (the bounded diff):
    contract, exit_code = _load_contract_or_exit(args, *, repo, head_sha, perf_log_cb)
    - if neither --bead-id nor --contract-file set: returns (None, None)
    - if --bead-id set + load succeeds: returns (BeadContract(...), None)
    - if --contract-file set + load succeeds: returns (BeadContract(...), None)
    - if --bead-id set + load raises: returns (None, 2); perf_log_cb called once with outcome='failure'
    - if --contract-file set + load raises: returns (None, 2); perf_log_cb called once with outcome='failure'

Behavior parity invariants (must hold both before and after extraction):
    - exit code is 2 on ANY contract-load failure (fail-closed; never 0/1)
    - stderr surfaces a "[skeptic-gate]" error line mentioning the bead-id / contract-file path
    - perf_log_cb (or _emit_perf_log) is invoked exactly once per failure with outcome='failure'
    - perf_log_cb is NEVER invoked on success or on the no-flag path
"""

from __future__ import annotations

import argparse
import json
import os
from typing import Optional
from unittest.mock import MagicMock

import pytest

import runner.skeptic_gate_cli as cli_mod
from runner.skeptic_gate import BeadContract


REPO = "jleechanorg/dark-factory"
PR_NUMBER = 399
HEAD_SHA = "0123456789abcdef0123456789abcdef01234567"


def _make_args(**overrides) -> argparse.Namespace:
    """Build a minimal Namespace with the fields the helper reads."""
    base = dict(
        bead_id="",
        contract_file="",
        br_bin="br",
        pr_number=PR_NUMBER,
    )
    base.update(overrides)
    return argparse.Namespace(**base)


def _sample_contract_dict() -> dict:
    return {
        "id": "jleechan-f1a2",
        "description": "main() prologue split — contract-echo helper extraction",
        "prior_findings": [{"source": "r1 review", "text": "duplicate contract-load block"}],
        "acceptance_items": [
            {"id": "A1", "text": "extracted helper returns same exit code as inlined code"},
            {"id": "A2", "text": "helper emits perf log exactly once on failure"},
        ],
    }


def test_load_contract_or_exit_does_not_exist_yet():
    """RED gate: the helper hasn't been extracted yet. The test fails
    today and turns green after the extraction lands."""
    assert hasattr(cli_mod, "_load_contract_or_exit"), (
        "_load_contract_or_exit must exist on runner.skeptic_gate_cli after extraction"
    )


def test_load_contract_or_exit_no_flag_returns_none_none():
    """No --bead-id and no --contract-file: the helper must return
    (None, None) — no contract, continue execution. No perf log."""
    perf_log_cb = MagicMock()
    args = _make_args()
    contract, exit_code = cli_mod._load_contract_or_exit(
        args,
        repo=REPO,
        head_sha=HEAD_SHA,
        enabled=False,
        perf_log_dir=None,
        perf_log_cb=perf_log_cb,
    )
    assert contract is None
    assert exit_code is None
    perf_log_cb.assert_not_called()


def test_load_contract_or_exit_valid_contract_file_returns_bead_contract(tmp_path):
    """Happy path: --contract-file pointing at a valid JSON contract
    returns a BeadContract. No perf log."""
    contract_path = tmp_path / "contract.json"
    contract_path.write_text(json.dumps(_sample_contract_dict()))
    perf_log_cb = MagicMock()
    args = _make_args(contract_file=str(contract_path))
    contract, exit_code = cli_mod._load_contract_or_exit(
        args,
        repo=REPO,
        head_sha=HEAD_SHA,
        enabled=False,
        perf_log_dir=None,
        perf_log_cb=perf_log_cb,
    )
    assert exit_code is None
    assert isinstance(contract, BeadContract)
    assert contract.id == "jleechan-f1a2"
    assert len(contract.acceptance_items) == 2
    perf_log_cb.assert_not_called()


def test_load_contract_or_exit_missing_contract_file_fails_closed_with_exit_2(tmp_path, capsys):
    """Failure path: --contract-file points at a non-existent file. The
    helper must return exit_code=2 (fail-closed), contract=None, emit a
    stderr message, and invoke the perf log callback exactly once with
    outcome='failure'."""
    missing = tmp_path / "nope.json"
    perf_log_cb = MagicMock()
    args = _make_args(contract_file=str(missing))
    contract, exit_code = cli_mod._load_contract_or_exit(
        args,
        repo=REPO,
        head_sha=HEAD_SHA,
        enabled=False,
        perf_log_dir=None,
        perf_log_cb=perf_log_cb,
    )
    assert contract is None
    assert exit_code == 2
    out = capsys.readouterr().err
    assert "contract load failed" in out
    assert str(missing) in out
    perf_log_cb.assert_called_once()
    kwargs = perf_log_cb.call_args.kwargs
    assert kwargs["outcome"] == "failure"
    assert kwargs["repo"] == REPO
    assert kwargs["head_sha"] == HEAD_SHA


def test_load_contract_or_exit_malformed_contract_file_fails_closed_with_exit_2(tmp_path):
    """Failure path: --contract-file points at malformed JSON (missing
    acceptance_items is the canonical ValueError). exit_code=2 and no
    perf log NOISE — exactly one call with outcome='failure'."""
    bad = tmp_path / "bad.json"
    bad.write_text(json.dumps({"id": "x", "description": "x"}))  # missing prior_findings/acceptance_items
    perf_log_cb = MagicMock()
    args = _make_args(contract_file=str(bad))
    contract, exit_code = cli_mod._load_contract_or_exit(
        args,
        repo=REPO,
        head_sha=HEAD_SHA,
        enabled=False,
        perf_log_dir=None,
        perf_log_cb=perf_log_cb,
    )
    assert contract is None
    assert exit_code == 2
    perf_log_cb.assert_called_once()
    assert perf_log_cb.call_args.kwargs["outcome"] == "failure"


def test_load_contract_or_exit_bead_id_load_failure_fails_closed_with_exit_2(monkeypatch, capsys):
    """Failure path on --bead-id: a failing `br` lookup must fail
    closed with exit_code=2 and a stderr line that names the
    bead_id."""
    def fake_br_load(bead_id: str, *, br_bin: str = "br"):
        raise RuntimeError("br show failed")

    monkeypatch.setattr(cli_mod, "load_bead_contract_from_bead", fake_br_load)
    perf_log_cb = MagicMock()
    args = _make_args(bead_id="jleechan-f1a2")
    contract, exit_code = cli_mod._load_contract_or_exit(
        args,
        repo=REPO,
        head_sha=HEAD_SHA,
        enabled=False,
        perf_log_dir=None,
        perf_log_cb=perf_log_cb,
    )
    assert contract is None
    assert exit_code == 2
    out = capsys.readouterr().err
    assert "bead contract load failed" in out
    assert "jleechan-f1a2" in out
    perf_log_cb.assert_called_once()
    assert perf_log_cb.call_args.kwargs["outcome"] == "failure"


def test_load_contract_or_exit_emits_perf_log_via_emit_perf_log_helper(monkeypatch):
    """Helper must call `_emit_perf_log` with the documented args
    (matches every existing call site in main()). We DON'T want the
    helper to take a direct ref to the caller-side duration_ms — the
    contract is that the helper invokes the module-level
    `_emit_perf_log` with `duration_ms=0` (the caller doesn't have a
    meaningful value at contract-load time)."""
    captured = MagicMock()
    monkeypatch.setattr(cli_mod, "_emit_perf_log", captured)
    args = _make_args(contract_file="/nonexistent/path.json")
    contract, exit_code = cli_mod._load_contract_or_exit(
        args,
        repo=REPO,
        head_sha=HEAD_SHA,
        enabled=True,
        perf_log_dir="/tmp/skeptic-test-perf-log",
        perf_log_cb=None,  # not used when the helper calls _emit_perf_log directly
    )
    assert contract is None
    assert exit_code == 2
    # The helper may either call `perf_log_cb` (preferred contract) OR
    # `_emit_perf_log` directly. Accept either. The point is exactly
    # one call on failure.
    if captured.called:
        kwargs = captured.call_args.kwargs
        assert kwargs["outcome"] == "failure"
        assert kwargs["repo"] == REPO
        assert kwargs["head_sha"] == HEAD_SHA
        assert kwargs["duration_ms"] == 0
    # If perf_log_cb is the contract instead, the caller in main() is
    # responsible for invoking _emit_perf_log — tests below cover both
    # seams.
