"""Regression guards for the binary-first slash-command contract."""
from __future__ import annotations

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
COMMANDS_DIR = ROOT / ".claude" / "commands"

PROOF_LABELS = (
    "# Literal command run:",
    "# Run ID:",
    "# CXDB SHA:",
    "# Final outcome:",
    "# Exit code:",
    "# Wall-clock:",
    "# Logs:",
    "# Evidence envelope:",
)


def _command(name: str) -> str:
    return (COMMANDS_DIR / name).read_text()


def _aliases(text: str) -> list[str]:
    match = re.search(r"^aliases:\s*\[(.*?)\]\s*$", text, re.MULTILINE)
    if not match:
        return []
    body = match.group(1).strip()
    if not body:
        return []
    return [part.strip().strip("\"'") for part in body.split(",")]


def test_f_and_fs_require_binary_proof_block() -> None:
    missing: list[str] = []
    for name in ("f.md", "fs.md"):
        text = _command(name)
        for label in PROOF_LABELS:
            if label not in text:
                missing.append(f"{name}: missing {label}")
        if "dark-factory \\" not in text:
            missing.append(f"{name}: missing literal dark-factory command shape")
    assert not missing, "Slash command binary proof block is incomplete:\n  " + "\n  ".join(missing)


def test_skill_call_is_not_valid_factory_run_proof() -> None:
    violations: list[str] = []
    forbidden_phrases = (
        "Quote the **actual** command or `Skill()`",
        "Quote the **actual** `Skill()`",
        "Skill() call you ran",
        "Skill(\"dark-factory\"",
    )
    for name in ("f.md", "fs.md", "factory.md"):
        text = _command(name)
        for phrase in forbidden_phrases:
            if phrase in text:
                violations.append(f"{name}: contains invalid proof phrase {phrase!r}")
    assert not violations, "Skill() must not be accepted as factory-run proof:\n  " + "\n  ".join(violations)


def test_workflow_or_skill_cannot_be_redeemed_by_pasted_proof_block() -> None:
    violations: list[str] = []
    invalid_patterns = (
        r"(?:in-Claude workflow|`Skill\(\)`(?: call| result)?).*unless .*proof block",
        r"unless (?:it includes |the )binary proof block",
    )
    for name in ("f.md", "fs.md"):
        compact = " ".join(_command(name).split())
        for pattern in invalid_patterns:
            if re.search(pattern, compact):
                violations.append(f"{name}: matches invalid permissive proof pattern {pattern!r}")
    assert not violations, "Workflow/Skill proof cannot be redeemed by pasted metadata:\n  " + "\n  ".join(violations)


def test_fs_has_no_read_only_in_session_modes() -> None:
    forbidden = ("/fs --show", "read-only, no pipeline invoked")
    invalid_patterns = (
        r"/fs --review[^\n]*(?:no pipeline|read-only|in-session)",
        r"/fs --review-attractor[^\n]*(?:no pipeline|read-only|in-session)",
    )
    violations: list[str] = []
    for path in sorted(COMMANDS_DIR.glob("*.md")):
        text = path.read_text()
        for phrase in forbidden:
            if phrase in text:
                violations.append(f"{path.name}: contains {phrase!r}")
        for pattern in invalid_patterns:
            if re.search(pattern, text):
                violations.append(f"{path.name}: matches {pattern!r}")
    assert not violations, "/fs must be binary-backed; read-only helpers belong in /factory-spec: " + ", ".join(violations)


def test_fs_alias_has_single_owner() -> None:
    owners = [
        path.name
        for path in sorted(COMMANDS_DIR.glob("*.md"))
        if "fs" in _aliases(path.read_text())
    ]
    assert owners == ["fs.md"]
