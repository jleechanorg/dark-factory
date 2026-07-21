"""Tests for the skeptic-gate CLI contract-echo wiring (issue #386).

CodeRabbit finding on PR #397: the r1/r2 contract-echo step added the
public surface to `runner.skeptic_gate` (load_bead_contract,
parse_contract_echo, evaluate_contract_echo, build_prompt(contract=...),
evaluate(contract=...)) but the CLI (`runner/skeptic_gate_cli.py`)
never plumbed a `--contract-file` flag through to either `build_prompt`
or `evaluate`. The reviewer in production still saw the legacy 10-field
prompt and the gate never enforced per-item verdicts — the contract-echo
step was effectively dead code at the CLI layer.

These tests prove:
  - `--contract-file` parses and loads the contract via load_bead_contract.
  - The loaded contract is passed to `build_prompt(contract=...)`.
  - The loaded contract is passed to `evaluate(contract=...)`.
  - When `--contract-file` is omitted, the CLI runs the legacy no-contract
    path (backwards-compatible, issue #384 invariant).
  - A missing/unreadable contract file fails closed (exit 2) — the gate
    never silently falls back to no-contract when the operator asked
    for the contract-echo step.
"""

from __future__ import annotations

import json
import os
import sys
from typing import List, Optional, Tuple

import pytest

import runner.skeptic_gate_cli as cli_mod
from runner.skeptic_gate import (
    BeadContract,
    SkepticResult,
    build_prompt,
    evaluate,
)


HEAD_SHA = "abcdef1234567890abcdef1234567890abcdef12"
REPO = "jleechanorg/dark-factory"
PR_NUMBER = 397
DIFF = "+x\n-y\n"


def _sample_contract_dict() -> dict:
    return {
        "id": "jleechan-pq08",
        "description": "Contract-echo review step (issue #386, r2 CLI wiring)",
        "prior_findings": [
            {"source": "r5 reviewer", "text": "missing acceptance closure"},
        ],
        "acceptance_items": [
            {"id": "A1", "text": "fixture with omitted item -> red"},
            {"id": "A2", "text": "verbatim constraint extraction"},
        ],
    }


@pytest.fixture
def contract_json(tmp_path):
    p = tmp_path / "contract.json"
    p.write_text(json.dumps(_sample_contract_dict()))
    return p


def _patch_cli_dependencies(
    monkeypatch,
    *,
    contract_path: Optional[str],
    review_output_for: Optional[object] = None,
    review_output: Optional[str] = None,
):
    """Replace the CLI's external calls so we can drive `main` end-to-end.

    Captures the args passed to `build_prompt` and `evaluate` so the
    assertions can verify the contract was threaded through both
    call sites (the headline CodeRabbit finding).

    `review_output_for` is a callable taking the reviewer name and
    returning the output for that reviewer (used when different
    reviewers need different IDENTITY declarations, e.g. codex vs
    gemini). For backwards compat, `review_output` is a single static
    string used for every reviewer.
    """
    captured: dict = {
        "build_prompt_calls": [],
        "evaluate_calls": [],
    }

    def fake_get_pr_head_sha_via_api(repo, pr_number):
        return HEAD_SHA

    def fake_get_pr_diff(repo, pr_number):
        return DIFF

    def fake_get_implementation_identity(repo, pr_number, head_sha=""):
        return "claude"

    real_build_prompt = cli_mod.build_prompt

    def capture_build_prompt(*args, **kwargs):
        captured["build_prompt_calls"].append(kwargs)
        return real_build_prompt(*args, **kwargs)

    real_evaluate = cli_mod.evaluate

    def capture_evaluate(*args, **kwargs):
        captured["evaluate_calls"].append(kwargs)
        return real_evaluate(*args, **kwargs)

    if review_output_for is None:
        if review_output is None:
            raise ValueError("must supply review_output or review_output_for")
        def _static_for(reviewer_name):
            return review_output
        review_output_for = _static_for

    def fake_invoke_reviewer(
        reviewer, model, prompt, *, parent_env=None, timeout=900,
        codex_bin="", gemini_bin="",
    ):
        return review_output_for(reviewer), None

    def fake_publish_failure(*args, **kwargs):
        return None

    def fake_emit_perf_log(*args, **kwargs):
        return None

    monkeypatch.setattr(cli_mod, "get_pr_head_sha_via_api", fake_get_pr_head_sha_via_api)
    monkeypatch.setattr(cli_mod, "get_pr_diff", fake_get_pr_diff)
    monkeypatch.setattr(cli_mod, "get_implementation_identity", fake_get_implementation_identity)
    monkeypatch.setattr(cli_mod, "build_prompt", capture_build_prompt)
    monkeypatch.setattr(cli_mod, "evaluate", capture_evaluate)
    monkeypatch.setattr(cli_mod, "invoke_reviewer", fake_invoke_reviewer)
    monkeypatch.setattr(cli_mod, "_publish_failure", fake_publish_failure)
    monkeypatch.setattr(cli_mod, "_emit_perf_log", fake_emit_perf_log)
    return captured


# ---------------------------------------------------------------------------
# --contract-file flows through to build_prompt AND evaluate
# ---------------------------------------------------------------------------


def test_cli_passes_contract_to_build_prompt_when_contract_file_supplied(
    monkeypatch, contract_json
):
    """Headline CodeRabbit finding: when --contract-file is supplied,
    the contract MUST be passed to build_prompt so the reviewer sees the
    bead's prior findings + acceptance items."""
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=str(contract_json),
        review_output_for=_output_for_reviewer,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--contract-file", str(contract_json),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    assert rc == 0
    assert len(captured["build_prompt_calls"]) >= 1
    kwargs = captured["build_prompt_calls"][0]
    assert "contract" in kwargs, "build_prompt must accept a contract kwarg"
    assert isinstance(kwargs["contract"], BeadContract)
    assert kwargs["contract"].id == "jleechan-pq08"
    assert len(kwargs["contract"].acceptance_items) == 2


def test_cli_passes_contract_to_evaluate_when_contract_file_supplied(
    monkeypatch, contract_json
):
    """Headline CodeRabbit finding: when --contract-file is supplied,
    the contract MUST also be passed to evaluate so the post-parse
    contract-echo enforcement runs and per-item verdicts are checked."""
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=str(contract_json),
        review_output_for=_output_for_reviewer,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--contract-file", str(contract_json),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    assert rc == 0
    # Two reviewers (codex AND gemini are mandatory); one evaluate call each.
    assert len(captured["evaluate_calls"]) >= 2
    for kwargs in captured["evaluate_calls"]:
        assert "contract" in kwargs, "evaluate must accept a contract kwarg"
        assert isinstance(kwargs["contract"], BeadContract)
        assert kwargs["contract"].id == "jleechan-pq08"


# ---------------------------------------------------------------------------
# Backwards-compat: no --contract-file = legacy 10-field contract path
# ---------------------------------------------------------------------------


def test_cli_runs_legacy_path_when_no_contract_file(monkeypatch):
    """Backwards-compatibility: when --contract-file is NOT supplied,
    the CLI must still run with the legacy 10-field contract (issue #384).
    Both `build_prompt` and `evaluate` receive `contract=None`."""
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=None,
        review_output_for=_output_for_reviewer_no_contract,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    assert rc == 0
    for kwargs in captured["build_prompt_calls"]:
        assert kwargs.get("contract") is None
    for kwargs in captured["evaluate_calls"]:
        assert kwargs.get("contract") is None


# ---------------------------------------------------------------------------
# Fail-closed: missing contract file is a hard error, not a silent fallback
# ---------------------------------------------------------------------------


def test_cli_fails_closed_on_missing_contract_file(monkeypatch, tmp_path):
    """A contract file that doesn't exist must fail closed. The gate
    never silently falls back to the no-contract path when the operator
    asked for the contract-echo step."""
    missing_path = tmp_path / "no-such-contract.json"
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=str(missing_path),
        review_output_for=_output_for_reviewer,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--contract-file", str(missing_path),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    # 2 = "refusing to gate without the operator-supplied contract".
    assert rc == 2
    # No reviewer invocation must have happened.
    assert captured["build_prompt_calls"] == []
    assert captured["evaluate_calls"] == []


def test_cli_fails_closed_on_malformed_contract_file(monkeypatch, tmp_path):
    """A contract file with no acceptance_items is invalid input —
    load_bead_contract raises ValueError; the CLI must fail closed."""
    bad = tmp_path / "bad-contract.json"
    bad.write_text(json.dumps({
        "id": "x",
        "description": "x",
        "prior_findings": [],
        "acceptance_items": [],
    }))
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=str(bad),
        review_output_for=_output_for_reviewer,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--contract-file", str(bad),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    assert rc == 2
    assert captured["build_prompt_calls"] == []
    assert captured["evaluate_calls"] == []


# ---------------------------------------------------------------------------
# --contract-file env-var fallback (so workflows can set it via env)
# ---------------------------------------------------------------------------


def test_cli_loads_contract_from_env_var_when_flag_omitted(
    monkeypatch, contract_json
):
    """The SKEPTIC_CONTRACT_FILE env var is the documented fallback when
    the operator wants the contract-echo step without changing argv."""
    monkeypatch.setenv("SKEPTIC_CONTRACT_FILE", str(contract_json))
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=str(contract_json),
        review_output_for=_output_for_reviewer,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    assert rc == 0
    assert captured["build_prompt_calls"][0]["contract"].id == "jleechan-pq08"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _valid_pass_output(identity: str = "codex", *, with_contract_echo: bool = True) -> str:
    """A 10-field PASS verdict. Optionally append a `CONTRACT_ECHO:` block
    addressing every acceptance item. `identity` must match the reviewer's
    CLI binding (codex CLI must declare `codex`; gemini CLI must declare
    `gemini`).
    """
    head = HEAD_SHA
    base = (
        f"VERDICT: PASS\n"
        f"HEAD_SHA: {head}\n"
        f"REPO: {REPO}\n"
        f"PR_NUMBER: {PR_NUMBER}\n"
        f"REASON: ok\n"
        f"IDENTITY: {identity}\n"
        f"TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        f"LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        f"GREP_CITES: runner/skeptic_gate.py:1\n"
        f"HEAD_COMMIT_VERIFIED: {head}\n"
    )
    if with_contract_echo:
        # r10 (issue #386): prior_findings are now enforced end-to-end,
        # so the fixture output must also emit PRIOR_FINDING: lines for
        # the contract's prior finding (r5 reviewer in the CLI
        # fixture).
        base += (
            "CONTRACT_ECHO:\n"
            "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
            "ITEM: A2 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:2\n"
            "PRIOR_FINDING: r5 reviewer VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        )
    return base


def _output_for_reviewer(reviewer_name: str, *, with_contract_echo: bool = True) -> str:
    """Map reviewer name → matching IDENTITY in the verdict. The CLI
    refuses a verdict whose IDENTITY doesn't match the CLI binding."""
    identity = "gemini" if reviewer_name == "gemini" else "codex"
    return _valid_pass_output(identity=identity, with_contract_echo=with_contract_echo)


def _output_for_reviewer_no_contract(reviewer_name: str) -> str:
    """For the legacy no-contract path: the reviewer MUST NOT emit a
    CONTRACT_ECHO block (the strict 10-field parser rejects it as
    an 11th field)."""
    return _output_for_reviewer(reviewer_name, with_contract_echo=False)


# ---------------------------------------------------------------------------
# r10 regression tests (CodeRabbit feedback on PR #418)
# ---------------------------------------------------------------------------


def test_cli_fails_closed_on_unreadable_contract_file(monkeypatch, tmp_path):
    """An existing but unreadable contract file (chmod 000) must take
    the fail-closed exit-2 path. Previously the `except` only caught
    FileNotFoundError + JSON/validation errors, so a PermissionError
    leaked through and the gate silently fell back to the legacy
    10-field contract (issue #386 r10 CodeRabbit gap 1)."""
    unreadable = tmp_path / "unreadable-contract.json"
    unreadable.write_text(json.dumps(_sample_contract_dict()))
    unreadable.chmod(0o000)
    # Root always has read access on POSIX (CAP_DAC_OVERRIDE), so
    # skip this assertion when running as root.
    if os.access(str(unreadable), os.R_OK):
        unreadable.chmod(0o000)
        # Some root environments ignore 0o000 on tmpfs; fall back to
        # chmod 0o000 on a directory path so the test still documents
        # the contract — but skip the assertion rather than fabricate
        # an OK signal.
        if os.access(str(unreadable), os.R_OK):
            pytest.skip("root can read 0o000 files; cannot simulate PermissionError")
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=str(unreadable),
        review_output_for=_output_for_reviewer,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--contract-file", str(unreadable),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    assert rc == 2
    # No reviewer invocation must have happened.
    assert captured["build_prompt_calls"] == []
    assert captured["evaluate_calls"] == []


def test_cli_fails_closed_when_reviewer_omits_prior_finding(monkeypatch, tmp_path):
    """When the contract has prior_findings but the reviewer's
    CONTRACT_ECHO block omits the PRIOR_FINDING: lines, the gate must
    fail closed (issue #386 r10 CodeRabbit gap 2). The reviewer cannot
    return PASS while silently skipping the bead author's prior findings
    — they MUST address every one (or N-A with a reason)."""
    contract_json = tmp_path / "contract.json"
    contract_json.write_text(json.dumps(_sample_contract_dict()))
    captured = _patch_cli_dependencies(
        monkeypatch,
        contract_path=str(contract_json),
        # Reviewer output: addresses acceptance items but OMITS the
        # PRIOR_FINDING: line for the contract's prior finding
        # ("r5 reviewer"). The gate must fail closed on this.
        review_output=_output_for_reviewer_omitting_prior_findings,
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--contract-file", str(contract_json),
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-2.5-pro"]]',
    ])
    # rc != 0 (gate red) — the prior finding was omitted and the
    # evaluator marked it unaddressed.
    assert rc != 0
    # The evaluator was invoked at least once (so the contract-echo
    # path ran).
    assert captured["evaluate_calls"], "evaluate() was never called"


def _output_for_reviewer_omitting_prior_findings(reviewer_name: str) -> str:
    """A reviewer output that addresses acceptance items but OMITS the
    PRIOR_FINDING: lines. The contract-echo gate must fail closed on
    this (issue #386 r10 CodeRabbit gap 2)."""
    identity = "gemini" if reviewer_name == "gemini" else "codex"
    head = HEAD_SHA
    return (
        f"VERDICT: PASS\n"
        f"HEAD_SHA: {head}\n"
        f"REPO: {REPO}\n"
        f"PR_NUMBER: {PR_NUMBER}\n"
        f"REASON: ok\n"
        f"IDENTITY: {identity}\n"
        f"TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        f"LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        f"GREP_CITES: runner/skeptic_gate.py:1\n"
        f"HEAD_COMMIT_VERIFIED: {head}\n"
        "CONTRACT_ECHO:\n"
        "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        "ITEM: A2 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:2\n"
        # NOTE: no PRIOR_FINDING: line — must fail closed.
    )