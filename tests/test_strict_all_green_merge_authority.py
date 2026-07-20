#!/usr/bin/env python3
"""
Strict-all-green merge authority contract (bze8.1 / issue #328).

Verifies that the autonomous merge path requires STRICT all-green (every
gate verdict green, no unknowns) OR an explicit operator disposition
record. The old "no-red" predicate permitted `unknown` verdicts and
merged #365/#375/#382 with structural-pending unknowns — the regression
class this contract exists to prevent.

This test exercises:
  1. auto-merge-guard.sh gate-vocabulary predicate (extracted from the
     production script via awk) — strict-all-green + operator-disposition
     overrides unknowns; otherwise exit 1.
  2. factory-overlay.sh bead-closed-check — same fail-closed behavior
     on the bead lifecycle path (DISPATCHED/ATTESTED → READY).
  3. daemon/src/verifier.rs — `strict_all_green`, `nongreen_gates`,
     and `merge_authority` are exposed and behave as documented.

Run: .venv/bin/python -m pytest tests/test_strict_all_green_merge_authority.py -v
"""
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).parent.parent


def _predicate_block_from_guard() -> str:
    """Extract the python heredoc predicate block from auto-merge-guard.sh.

    The production script embeds a python heredoc between
    `python3 -c '` and the trailing `'` line; this helper slices the
    block out so the test runs the EXACT same code production does.
    """
    guard = (ROOT / "daemon" / "scripts" / "auto-merge-guard.sh").read_text()
    lines = guard.splitlines()
    capture = False
    block = []
    for line in lines:
        if re.match(r"^import json, sys$", line):
            capture = True
            block.append(line)
            continue
        if capture and re.match(r"^sys\.exit\(0\)'$", line):
            break
        if capture:
            block.append(line)
    return "\n".join(block)


def _run_predicate(input_json: str) -> tuple[int, str]:
    """Run the extracted python predicate and return (rc, stdout)."""
    block = _predicate_block_from_guard()
    proc = subprocess.run(
        ["python3", "-c", block],
        input=input_json,
        capture_output=True,
        text=True,
    )
    return proc.returncode, proc.stdout


def _gate_assessment(gates: dict, *, operator_disposition: str = "") -> str:
    """Build a synthetic GATE_ASSESSMENT JSONL line for the predicate."""
    ctx = {"pr_number": 99999, "gates": gates, "all_green": True}
    if operator_disposition:
        ctx["operator_disposition"] = operator_disposition
    return json.dumps(
        {
            "timestamp": "2026-07-19T00:00:00Z",
            "eventType": "GATE_ASSESSMENT",
            "bead_id": "test-bead",
            "attempt": 1,
            "state": "ATTESTED",
            "context": ctx,
        }
    )


def test_strict_all_green_succeeds_on_all_pass():
    """Every gate `pass` → exit 0, message contains `strict-all-green`."""
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    rc, out = _run_predicate(_gate_assessment(gates))
    assert rc == 0, f"expected exit 0 on strict-all-green, got {rc} ({out!r})"
    assert "strict-all-green" in out, f"unexpected message: {out!r}"


def test_warn_treated_as_pass_in_strict_all_green():
    """`warn` is treated as `pass` under strict-all-green (non-blocking)."""
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    gates["coderabbit"] = "warn"
    rc, out = _run_predicate(_gate_assessment(gates))
    assert rc == 0, f"warn must be non-blocking under strict-all-green, got {rc}"


def test_unknown_without_disposition_blocks_merge():
    """Single `unknown` without disposition → exit 1 (ESCALATION_REQUIRED).

    This is the regression-class test for #365/#375/#382: pre-fix, the
    auto-merge-guard permitted unknowns and merged anyway.
    """
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    gates["coderabbit"] = "unknown"
    gates["bugbot"] = "unknown"
    rc, out = _run_predicate(_gate_assessment(gates))
    assert rc == 1, (
        f"unknown WITHOUT operator disposition must block (exit 1); got rc={rc} ({out!r}). "
        f"This is the regression-class fix for #328 / bze8.1."
    )
    assert "ESCALATION_REQUIRED" in out, f"missing escalation message: {out!r}"


def test_unknown_with_operator_disposition_proceeds():
    """Operator disposition override → exit 0 (the only authorized bypass)."""
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    gates["coderabbit"] = "unknown"
    rc, out = _run_predicate(
        _gate_assessment(
            gates,
            operator_disposition="OPERATOR_DISPOSITION: CodeRabbit quota wall",
        )
    )
    assert rc == 0, (
        f"operator disposition must authorize the merge (exit 0); got rc={rc} ({out!r})"
    )
    assert "unknowns-overridden" in out, f"unexpected message: {out!r}"


def test_unknown_with_disposition_missing_token_blocks_merge():
    """Non-empty `operator_disposition` but missing the literal token → exit 1.

    Prevents a permissive bypass where any non-empty string would qualify.
    The token `OPERATOR_DISPOSITION:` is the canonical signal.
    """
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    gates["coderabbit"] = "unknown"
    rc, _ = _run_predicate(
        _gate_assessment(gates, operator_disposition="manual override jleechan 2026-07-19")
    )
    assert rc == 1, (
        f"missing OPERATOR_DISPOSITION: token must NOT authorize the merge; got rc={rc}"
    )


def test_fail_always_blocks():
    """Any `fail` verdict → exit 1, even with operator disposition."""
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    gates["ci_green"] = "fail"
    rc, out = _run_predicate(
        _gate_assessment(
            gates,
            operator_disposition="OPERATOR_DISPOSITION: CI flake",
        )
    )
    assert rc == 1, f"`fail` must always block; got rc={rc} ({out!r})"


def test_structured_object_verdict_pass_is_treated_as_pass():
    """Structured `{verdict: pass}` must be normalized to `pass`."""
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    gates["coderabbit"] = {"verdict": "pass", "evidence": []}
    rc, _ = _run_predicate(_gate_assessment(gates))
    assert rc == 0, f"structured pass verdict must authorize the merge; got rc={rc}"


def test_structured_object_verdict_unknown_blocks_without_disposition():
    """Structured `{verdict: unknown}` blocks the same as the string form."""
    gates = {g: "pass" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    gates["coderabbit"] = {"verdict": "unknown", "evidence": ["no review yet"]}
    rc, _ = _run_predicate(_gate_assessment(gates))
    assert rc == 1, f"structured unknown verdict must block without disposition; got rc={rc}"


def test_legacy_aliases_back_compat():
    """Legacy `green`/`red` aliases still resolve correctly."""
    gates = {g: "green" for g in [
        "ci_green", "no_conflicts", "coderabbit", "bugbot",
        "comments_resolved", "evidence_review", "skeptic",
    ]}
    rc, _ = _run_predicate(_gate_assessment(gates))
    assert rc == 0, "legacy `green` alias must authorize"
    gates["bugbot"] = "red"
    rc, _ = _run_predicate(_gate_assessment(gates))
    assert rc == 1, "legacy `red` alias must block"


def test_unparseable_input_blocks_merge():
    """Empty / unparseable GATE_ASSESSMENT → exit 1 (block on missing)."""
    rc, _ = _run_predicate("not-json")
    assert rc == 1, f"unparseable input must block; got rc={rc}"


def test_verifier_rs_exposes_strict_all_green_helper():
    """`verifier::strict_all_green` is the single source of truth — pin its API.

    The shell predicate and the verifier function MUST agree on the rule;
    a future refactor that decouples them is the regression vector. This
    test pins the Rust-side surface so any change is intentional.
    """
    verifier = (ROOT / "daemon" / "src" / "verifier.rs").read_text()
    assert "pub fn strict_all_green(" in verifier, (
        "verifier.rs must expose strict_all_green — this is the rule the "
        "auto-merge-guard reads (single source of truth)"
    )
    assert "pub fn merge_authority(" in verifier, (
        "verifier.rs must expose merge_authority — autonomous merge path's "
        "decision is StrictAllGreen | OperatorDisposition | EscalationRequired"
    )
    assert "pub enum MergeAuthority" in verifier, (
        "MergeAuthority enum missing — required by tick.rs to gate READY_FOR_MERGE"
    )
    assert "pub fn nongreen_gates(" in verifier, (
        "nongreen_gates missing — used for ESCALATION_REQUIRED telemetry"
    )


def test_auto_merge_guard_uses_strict_all_green_predicate():
    """The guard script must call the strict-all-green predicate (not the legacy no-red)."""
    guard = (ROOT / "daemon" / "scripts" / "auto-merge-guard.sh").read_text()
    assert "latest_assessment_strict_all_green" in guard, (
        "auto-merge-guard.sh must use the strict-all-green predicate; "
        "the legacy `latest_assessment_no_red` allowed unknowns to merge"
    )
    assert "ESCALATION_REQUIRED" in guard, (
        "auto-merge-guard.sh must emit an ESCALATION_REQUIRED-style refusal "
        "when unknowns are present without an operator disposition"
    )
    assert "OPERATOR_DISPOSITION:" in guard, (
        "auto-merge-guard.sh must recognize the canonical operator disposition token"
    )


def test_factory_overlay_bead_closed_check_uses_strict_all_green():
    """`factory-overlay.sh bead-closed-check` must mirror the strict-all-green rule."""
    overlay = (ROOT / "daemon" / "factory-overlay.sh").read_text()
    # Locate the bead-closed-check section
    idx = overlay.find("bead-closed-check)")
    assert idx > 0, "factory-overlay.sh missing bead-closed-check section"
    section_end = overlay.find("\n\n", idx)
    section = overlay[idx:section_end if section_end > 0 else None]
    assert "OPERATOR_DISPOSITION:" in section, (
        "bead-closed-check must recognize the operator disposition override"
    )
    assert "ESCALATION_REQUIRED" not in section, (
        "bead-closed-check should silently park (no escalation telemetry at the "
        "shell layer) — escalation telemetry belongs to the tick path"
    )
