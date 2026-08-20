#!/usr/bin/env python3
"""
Contract test: verifies gate-count/naming consistency across the /af system.

This test ensures that:
1. daemon/src/verifier.rs::GateName enum defines exactly 8 gates
2. Documentation files don't claim a different gate count
3. factory-overlay.sh's required-key set matches the Rust verifier
4. No operator-facing instructions contain direct sqlite3 mutation commands

(Gate 8 = `vacuous_red_green`, added in PR #389 / r5 commit 175c6ad for
issue #387, bead jleechan-ijod — the runtime vacuous-test detector verdict
propagated from `PrEvidence.vacuous_red_green`.)

Run: .venv/bin/python -m pytest tests/test_af_gate_contract.py -v
"""
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).parent.parent


def extract_gate_names_from_rust() -> set[str]:
    """Extract GateName variants from daemon/src/verifier.rs."""
    verifier_path = ROOT / "daemon" / "src" / "verifier.rs"
    content = verifier_path.read_text()

    # Match: pub enum GateName { ... }
    enum_match = re.search(r'pub\s+enum\s+GateName\s*\{([^}]+)\}', content, re.DOTALL)
    if not enum_match:
        raise AssertionError(f"Could not find GateName enum in {verifier_path}")

    enum_body = enum_match.group(1)

    # Extract variant names (strip attributes and comments)
    variants = []
    for line in enum_body.split('\n'):
        line = line.strip()
        if not line or line.startswith('//'):
            continue
        # Match: VariantName, VariantName = ..., #[derive(...)] VariantName
        match = re.match(r'(?:\[#[^\]]+\]\s*)?(\w+)', line)
        if match:
            variants.append(match.group(1))

    # Map Rust variant names to JSON key names
    # Ci -> ci_green, NoConflicts -> no_conflicts, etc.
    gate_map = {
        'Ci': 'ci_green',
        'NoConflicts': 'no_conflicts',
        'CodeRabbitApproved': 'coderabbit',
        'BugbotClean': 'bugbot',
        'CommentsResolved': 'comments_resolved',
        'EvidenceFloor': 'evidence_review',
        'Skeptic': 'skeptic',
        'VacuousRedGreen': 'vacuous_red_green',
    }

    json_keys = set()
    for variant in variants:
        if variant in gate_map:
            json_keys.add(gate_map[variant])
        else:
            raise AssertionError(f"Unknown GateName variant: {variant}")

    return json_keys


def test_rust_verifier_has_8_gates():
    """Verify daemon/src/verifier.rs::GateName has exactly 8 variants.

    Issue #387 (bead jleechan-ijod) added gate 8 (`VacuousRedGreen`) in
    PR #389 / r5 commit 175c6ad. Keep this assertion in lock-step with the
    enum (verifier.rs:26-43) AND the `factory-overlay.sh` REQUIRED_KEYS
    (line 285) AND the canonical-vocabulary comment block at the top of
    `GateName::as_str` (verifier.rs:46-61).
    """
    gates = extract_gate_names_from_rust()
    assert len(gates) == 8, f"Expected 8 gates, got {len(gates)}: {gates}"
    print(f"✓ Rust verifier has {len(gates)} gates: {sorted(gates)}")


def test_docs_gate_count_matches_verifier():
    """Verify doc files don't claim a different gate count."""
    gates = extract_gate_names_from_rust()
    gate_count = len(gates)

    doc_files = [
        ROOT / ".claude" / "skills" / "auto-factory" / "SKILL.md",
        ROOT / ".claude" / "commands" / "af.md",
        ROOT / "GEMINI.md",
        ROOT / "docs" / "auto-factory-daemon-spec.md",
    ]

    # Pattern: digits followed by "gate" (case insensitive)
    gate_pattern = re.compile(r'(\d+)\s*gates?\b', re.IGNORECASE)

    errors = []
    for doc in doc_files:
        if not doc.exists():
            errors.append(f"{doc}: file not found")
            continue

        content = doc.read_text()
        matches = gate_pattern.findall(content)

        for match in matches:
            if int(match) != gate_count:
                errors.append(
                    f"{doc.name}: claims {match} gates, verifier has {gate_count}"
                )

    if errors:
        raise AssertionError("Gate count drift detected:\n" + "\n".join(errors))
    print(f"✓ All doc files match {gate_count}-gate contract")


def test_factory_overlay_required_keys_match_verifier():
    """Verify factory-overlay.sh's required-key set matches verifier."""
    gates = extract_gate_names_from_rust()
    expected_count = len(gates)

    overlay_path = ROOT / "daemon" / "factory-overlay.sh"
    content = overlay_path.read_text()

    # Extract REQUIRED_KEYS set from the gate-assessment Python block
    # Match: REQUIRED_KEYS = {"ci_green", ...}
    required_match = re.search(
        r'REQUIRED_KEYS\s*=\s*\{([^}]+)\}',
        content
    )

    if not required_match:
        raise AssertionError("Could not find REQUIRED_KEYS in factory-overlay.sh")

    keys_str = required_match.group(1)
    # Extract quoted strings
    required_keys = set(re.findall(r'"(\w+)"', keys_str))

    missing = gates - required_keys
    extra = required_keys - gates

    errors = []
    if missing:
        errors.append(f"Required keys missing in overlay: {sorted(missing)}")
    if extra:
        errors.append(f"Extra keys in overlay not in verifier: {sorted(extra)}")
    if len(required_keys) != expected_count:
        errors.append(
            f"Required key count mismatch: overlay has {len(required_keys)}, verifier has {expected_count}"
        )

    if errors:
        raise AssertionError("Factory-overlay.sh drift:\n" + "\n".join(errors))
    print(f"✓ factory-overlay.sh required keys match verifier: {sorted(required_keys)}")


def test_no_direct_sqlite3_mutation_in_docs():
    """Verify no operator-facing docs contain direct sqlite3 mutation instructions."""
    doc_dirs = [
        ROOT / ".claude" / "skills",
        ROOT / "docs",
    ]

    # Patterns that indicate direct sqlite3 mutation instructions to operators
    bad_patterns = [
        (re.compile(r'sqlite3\s+\$.*\s+(UPDATE|INSERT|DELETE)', re.IGNORECASE),
         "direct sqlite3 mutation"),
        (re.compile(r'run\s+sqlite3.*UPDATE', re.IGNORECASE),
         "sqlite3 UPDATE instruction"),
    ]

    errors = []
    for doc_dir in doc_dirs:
        if not doc_dir.exists():
            continue

        for md_file in doc_dir.rglob("*.md"):
            if "factory-overlay.sh" in str(md_file):
                continue  # Implementation file, not operator-facing

            content = md_file.read_text(errors="ignore")

            for pattern, desc in bad_patterns:
                if pattern.search(content):
                    rel_path = md_file.relative_to(ROOT)
                    errors.append(f"{rel_path}: contains {desc}")

    if errors:
        raise AssertionError(
            "Direct sqlite3 mutation instructions found in docs:\n"
            + "\n".join(errors)
        )
    print("✓ No direct sqlite3 mutation instructions in operator docs")


def test_auto_factory_requires_generic_host_and_two_phase_intake():
    """Pin factory intake to a capable current host and label only after proof."""
    skill_path = ROOT / ".claude" / "skills" / "auto-factory" / "SKILL.md"
    content = skill_path.read_text()
    command_path = ROOT / ".claude" / "commands" / "auto-factory.md"
    command = command_path.read_text()

    forbidden_personal_routing = [
        "jeff-ubuntu",
        "Every `/af` run operates through SSH",
        "Production `/af` execution is Linux-only",
    ]
    assert not [
        marker
        for marker in forbidden_personal_routing
        if marker in content or marker in command
    ]

    required_contract = [
        "invocation host is the candidate factory host",
        "supports `target_repo`",
        "DARK_FACTORY_ROOT",
        "ai.dark-factory.af-tick",
        "ai.dark-factory.daemon.service",
        'systemctl --user show "$unit" --property=WorkingDirectory --value',
        'br --db "$BR_DB" where',
        'br --db "$BR_DB" sync --status --json',
        'br --db "$BR_DB" doctor --quick',
        "create without the `factory` label",
        'br --db "$BR_DB" show "$bead_id" --json',
        'br --db "$BR_DB" update "$bead_id" --add-label factory --json',
        "intake verified; adoption pending",
        "jsonl_newer",
        "db_newer",
    ]

    missing = [entry for entry in required_contract if entry not in content]
    assert not missing, (
        "auto-factory skill is missing generic intake safeguards: "
        f"{missing}"
    )
    assert content.index("Execution host + Bead authority preflight") < content.index(
        "$H init"
    )
    assert content.index("create without the `factory` label") < content.index(
        'br --db "$BR_DB" show "$bead_id" --json'
    )
    assert content.index('br --db "$BR_DB" show "$bead_id" --json') < content.index(
        'br --db "$BR_DB" update "$bead_id" --add-label factory --json'
    )
    assert content.index(
        'br --db "$BR_DB" update "$bead_id" --add-label factory --json'
    ) < content.index("intake verified; adoption pending")
    assert "Wait for any spawned coder subagents" not in command
    assert not re.search(r"(?m)^br (?!--db )", content)


def test_auto_merge_guard_required_keys_match_verifier():
    """Pin daemon/scripts/auto-merge-guard.sh's predicate REQUIRED set to verifier.rs.

    Background (jleechan-ni1k / issue #437 P2): PR #431 added gate 8
    (`vacuous_red_green`) to `daemon/src/verifier.rs` and
    `daemon/factory-overlay.sh`'s REQUIRED_KEYS, but
    `daemon/scripts/auto-merge-guard.sh`'s predicate `REQUIRED` set was
    left at 7 keys. A 7-key report that passed every other gate could
    still strict-green because the predicate never required the runtime
    vacuous-test detector's verdict. This contract test pins the guard's
    REQUIRED set to the verifier's GateName enum (canonical source:
    `daemon/src/verifier.rs::GateName::as_str`) so future gate additions
    cannot drift one consumer from the others.
    """
    gates = extract_gate_names_from_rust()
    guard_path = ROOT / "daemon" / "scripts" / "auto-merge-guard.sh"
    content = guard_path.read_text()

    # Extract the predicate's REQUIRED set. The guard has TWO sets in
    # scope: REQUIRED (inside the python heredoc, used by the gate
    # predicate) and the outer bash variable names. Match the python
    # set literal (single-line `REQUIRED = {...}`).
    required_match = re.search(
        r'REQUIRED\s*=\s*\{([^}]+)\}',
        content,
    )
    if not required_match:
        raise AssertionError(
            "Could not find REQUIRED set in auto-merge-guard.sh — has the "
            "predicate block been rewritten? Path: "
            f"{guard_path}"
        )

    keys_str = required_match.group(1)
    required_keys = set(re.findall(r'"(\w+)"', keys_str))

    missing = gates - required_keys
    extra = required_keys - gates

    errors = []
    if missing:
        errors.append(
            f"REQUIRED set missing canonical gates: {sorted(missing)}"
        )
    if extra:
        errors.append(
            f"REQUIRED set has extra keys not in verifier: {sorted(extra)}"
        )

    if errors:
        raise AssertionError(
            "auto-merge-guard.sh predicate REQUIRED drift:\n"
            + "\n".join(errors)
        )
    print(
        f"✓ auto-merge-guard.sh predicate REQUIRED set matches verifier: "
        f"{sorted(required_keys)}"
    )


def test_auto_merge_guard_has_no_failopen_on_empty_live_head():
    """Pin the P1 fail-closed contract on empty live_head_sha.

    Background (jleechan-ni1k / issue #437 P1): the outer guard ran
    `gh pr view ... --jq .headRefOid 2>/dev/null || true` which silently
    swallowed an empty headRefOid response. The fix is to detect the
    empty string, emit a distinct `STALE:LIVE_HEAD_MISSING` reason, and
    refuse to merge. This contract test pins that:
      1. The script contains the `STALE:LIVE_HEAD_MISSING` reason token.
      2. The script does NOT have `|| true` swallowing the headRefOid
         lookup (the exact fail-open construct the fix targets).
    """
    guard_path = ROOT / "daemon" / "scripts" / "auto-merge-guard.sh"
    content = guard_path.read_text()

    errors = []
    if "STALE:LIVE_HEAD_MISSING" not in content:
        errors.append(
            "auto-merge-guard.sh missing STALE:LIVE_HEAD_MISSING reason "
            "(empty live_head_sha must fail-closed with a distinct token)"
        )
    if re.search(r"headRefOid[^\n]*\|\| *true", content):
        errors.append(
            "auto-merge-guard.sh still uses `|| true` to swallow empty "
            "headRefOid — fail-open contract violated"
        )

    if errors:
        raise AssertionError(
            "auto-merge-guard.sh fail-open contract violation:\n"
            + "\n".join(errors)
        )
    print("✓ auto-merge-guard.sh fail-closed on empty live_head_sha")


if __name__ == "__main__":
    import pytest
    pytest.main([__file__, "-v"])
