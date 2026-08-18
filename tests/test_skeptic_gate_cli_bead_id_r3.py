"""Tests for the r3 CLI bead-loading wiring (issue #386 r3, gap 2 + gap 3).

PR #397 r2 wired `--contract-file` (a hand-authored JSON path) but
not the live `br show --json <bead>` source. r3 adds `--bead-id`
and `--br-bin`, so production can set `SKEPTIC_BEAD_ID` and the
gate loads the bead's actual notes + prior findings + acceptance
items instead of relying on a hand-authored file that may drift
from the bead source.

These tests cover:
  - `--bead-id` flows through to `build_prompt(contract=...)` and
    `evaluate(contract=...)` (the r3 invariant).
  - A `br` failure (subprocess error) closes the gate (exit 2),
    never silently fallback to the legacy 10-field contract.
  - `--bead-id` is mutually exclusive with `--contract-file`.
"""

from __future__ import annotations

import json
import sys

import pytest

import runner.skeptic_gate_cli as cli_mod
from runner.skeptic_gate import (
    AcceptanceItem,
    BeadContract,
    PriorFinding,
)


REPO = "jleechanorg/dark-factory"
PR_NUMBER = 397
HEAD_SHA = "a" * 40


BEAD_JSON = {
    "id": "jleechan-pq08",
    "description": "contract-echo review step",
    "notes": ["r3 operator guidance: do not N-A away items"],
    "prior_findings": [
        {"source": "r2 cursor-agent", "text": "no notes field"},
    ],
    "acceptance_items": [
        {"id": "A1", "text": "must add notes field"},
        {"id": "A2", "text": "required must not N-A", "required": True},
    ],
}


def _beaded_contract() -> BeadContract:
    """Hand-mirror what the live `br show --json` payload produces.
    Used as the expected contract after the loader has fired."""
    return BeadContract(
        id=BEAD_JSON["id"],
        description=BEAD_JSON["description"],
        notes=tuple(BEAD_JSON["notes"]),
        prior_findings=tuple(
            PriorFinding(source=p["source"], text=p["text"])
            for p in BEAD_JSON["prior_findings"]
        ),
        acceptance_items=tuple(
            AcceptanceItem(
                id=a["id"],
                text=a["text"],
                required=bool(a.get("required") or False),
            )
            for a in BEAD_JSON["acceptance_items"]
        ),
    )


def _valid_pass_output(identity: str = "codex", *, with_contract_echo: bool = True) -> str:
    head = HEAD_SHA
    base = (
        "VERDICT: PASS\n"
        f"HEAD_SHA: {head}\n"
        f"REPO: {REPO}\n"
        f"PR_NUMBER: {PR_NUMBER}\n"
        "REASON: ok\n"
        f"IDENTITY: {identity}\n"
        "TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        "LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        "GREP_CITES: runner/skeptic_gate.py:1\n"
        f"HEAD_COMMIT_VERIFIED: {head}\n"
    )
    if with_contract_echo:
        # r10 (issue #386): prior_findings are now enforced, so the
        # fixture output must also emit PRIOR_FINDING: lines for each
        # prior finding on the contract.
        base += (
            "CONTRACT_ECHO:\n"
            "ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
            "ITEM: A2 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:2\n"
            "PRIOR_FINDING: r2 cursor-agent VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:1\n"
        )
    return base


def _patch_cli_for_dry_run(monkeypatch, *, fake_contract: BeadContract):
    """Stub the CLI's external surface so `cli_mod.main([...])` runs
    end-to-end in dry-run mode. Returns a dict capturing build_prompt
    + evaluate kwargs so tests can assert what reached them.
    """
    captured = {"build_prompt_calls": [], "evaluate_calls": []}

    real_build_prompt = cli_mod.build_prompt
    real_evaluate = cli_mod.evaluate

    def fake_get_pr_head_sha_via_api(*args, **kwargs):
        return HEAD_SHA

    def fake_get_pr_diff(*args, **kwargs):
        return "diff --git a/x b/x"

    def fake_get_implementation_identity(*args, **kwargs):
        return "claude"

    def fake_load_from_bead(bead_id, br_bin="br"):
        return fake_contract

    def capture_build_prompt(*args, **kwargs):
        captured["build_prompt_calls"].append(kwargs)
        return real_build_prompt(*args, **kwargs)

    def capture_evaluate(*args, **kwargs):
        captured["evaluate_calls"].append(kwargs)
        return real_evaluate(*args, **kwargs)

    def fake_invoke_reviewer(reviewer, model, prompt, **kwargs):
        # The CLI binding enforces codex CLI → IDENTITY=codex and
        # gemini CLI → IDENTITY=gemini; mirror that here.
        identity = "gemini" if reviewer == "gemini" else "codex"
        return _valid_pass_output(identity=identity, with_contract_echo=True), None

    def fake_publish_failure(*args, **kwargs):
        return None

    def fake_emit_perf_log(*args, **kwargs):
        return None

    monkeypatch.setattr(cli_mod, "get_pr_head_sha_via_api", fake_get_pr_head_sha_via_api)
    monkeypatch.setattr(cli_mod, "get_pr_diff", fake_get_pr_diff)
    monkeypatch.setattr(cli_mod, "get_implementation_identity", fake_get_implementation_identity)
    monkeypatch.setattr(cli_mod, "load_bead_contract_from_bead", fake_load_from_bead)
    monkeypatch.setattr(cli_mod, "build_prompt", capture_build_prompt)
    monkeypatch.setattr(cli_mod, "evaluate", capture_evaluate)
    monkeypatch.setattr(cli_mod, "invoke_reviewer", fake_invoke_reviewer)
    monkeypatch.setattr(cli_mod, "_publish_failure", fake_publish_failure)
    monkeypatch.setattr(cli_mod, "_emit_perf_log", fake_emit_perf_log)
    return captured


# ---------------------------------------------------------------------------
# --bead-id flows through to build_prompt and evaluate
# ---------------------------------------------------------------------------


def test_bead_id_flows_to_build_prompt(monkeypatch):
    captured = _patch_cli_for_dry_run(
        monkeypatch, fake_contract=_beaded_contract()
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--bead-id", "jleechan-pq08",
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-3.7-pro"]]',
    ])
    assert rc == 0
    assert len(captured["build_prompt_calls"]) >= 1
    kwargs = captured["build_prompt_calls"][0]
    assert "contract" in kwargs
    assert isinstance(kwargs["contract"], BeadContract)
    assert kwargs["contract"].id == "jleechan-pq08"
    assert kwargs["contract"].notes[0].startswith("r3 operator guidance")


def test_bead_id_flows_to_evaluate(monkeypatch):
    captured = _patch_cli_for_dry_run(
        monkeypatch, fake_contract=_beaded_contract()
    )
    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--bead-id", "jleechan-pq08",
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-3.7-pro"]]',
    ])
    assert rc == 0
    assert len(captured["evaluate_calls"]) >= 1
    for kwargs in captured["evaluate_calls"]:
        assert "contract" in kwargs
        assert isinstance(kwargs["contract"], BeadContract)
        assert kwargs["contract"].id == "jleechan-pq08"
        # The required=true flag must survive.
        a2 = next(
            it for it in kwargs["contract"].acceptance_items if it.id == "A2"
        )
        assert a2.required is True


# ---------------------------------------------------------------------------
# --bead-id + --contract-file is mutually exclusive
# ---------------------------------------------------------------------------


def test_bead_id_and_contract_file_mutually_exclusive(monkeypatch):
    """Both flags set → SystemExit (caller catches and exits 2)."""
    # Patch everything so even the SystemExit branch is contained.
    _patch_cli_for_dry_run(monkeypatch, fake_contract=_beaded_contract())
    # main() raises SystemExit directly when both flags are set
    # before any I/O happens. argparse triggers this — we just need
    # the SystemExit to surface.
    with pytest.raises(SystemExit):
        cli_mod.main([
            "--repo", REPO,
            "--pr-number", str(PR_NUMBER),
            "--bead-id", "jleechan-pq08",
            "--contract-file", "/tmp/contract.json",
            "--dry-run",
            "--reviewers-json", '[["codex", ""], ["gemini", "gemini-3.7-pro"]]',
        ])


# ---------------------------------------------------------------------------
# br failure closes the gate (exit 2)
# ---------------------------------------------------------------------------


def test_br_failure_closes_gate(monkeypatch):
    """A failing `br show --json` (e.g. bead not found) closes the
    gate — exit 2, never a silent fallback to the legacy 10-field
    contract."""

    def fake_load_failing(bead_id, br_bin="br"):
        raise RuntimeError(
            f"br show --json {bead_id!r} failed (rc=2): bead not found"
        )

    monkeypatch.setattr(cli_mod, "load_bead_contract_from_bead", fake_load_failing)
    # Stub the gh API surface too — CI runners don't have GH_TOKEN,
    # so the real `gh api` call would 4 before the br failure ever
    # raises. Mirrors the dry-run stubs used by the other tests in
    # this file so the test exercises the br-failure branch in
    # isolation.
    monkeypatch.setattr(cli_mod, "get_pr_head_sha_via_api", lambda *a, **k: HEAD_SHA)
    monkeypatch.setattr(cli_mod, "get_pr_diff", lambda *a, **k: "diff --git a/x b/x")
    monkeypatch.setattr(cli_mod, "get_implementation_identity", lambda *a, **k: "claude")

    rc = cli_mod.main([
        "--repo", REPO,
        "--pr-number", str(PR_NUMBER),
        "--bead-id", "jleechan-pq08",
        "--dry-run",
        "--reviewers-json", '[["codex", ""], ["gemini", "gemini-3.7-pro"]]',
    ])
    assert rc == 2
