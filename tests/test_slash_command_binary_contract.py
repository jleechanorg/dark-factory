"""Regression guards for the binary-first slash-command contract."""
from __future__ import annotations

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
COMMANDS_DIR = ROOT / ".claude" / "commands"
SKILLS_DIR = ROOT / ".claude" / "skills"

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


def _skill(name: str) -> str:
    return (SKILLS_DIR / name / "SKILL.md").read_text()


def _aliases(text: str) -> list[str]:
    match = re.search(r"^aliases:\s*\[(.*?)\]\s*$", text, re.MULTILINE)
    if not match:
        return []
    body = match.group(1).strip()
    if not body:
        return []
    return [part.strip().strip("\"'") for part in body.split(",")]


def test_f_and_fs_require_binary_proof_block() -> None:
    """f.md/fs.md are thin stubs pointing at a canonical SKILL.md; the
    binary proof-block contract lives there (single source of truth), not
    duplicated in the command file. Verify the skill each command points
    at actually carries the full contract, and that the stub itself still
    points at that skill (so the pointer can't silently rot)."""
    missing: list[str] = []
    command_to_skill = {"f.md": "dark-factory", "fs.md": "factory-spec"}
    for command_name, skill_name in command_to_skill.items():
        command_text = _command(command_name)
        if f"skills/{skill_name}/SKILL.md" not in command_text:
            missing.append(f"{command_name}: no longer points at skills/{skill_name}/SKILL.md")

        skill_text = _skill(skill_name)
        for label in PROOF_LABELS:
            if label not in skill_text:
                missing.append(f"{skill_name}/SKILL.md: missing {label}")
        if "dark-factory \\" not in skill_text:
            missing.append(f"{skill_name}/SKILL.md: missing literal dark-factory command shape")
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


def test_f_defaults_reviewer_calibration_on() -> None:
    """/f and /factory are thin stubs; the reviewer-calibration contract
    lives in dark-factory/SKILL.md (single source of truth) rather than
    being duplicated in each command file. Verify both stubs still point
    at that skill, and that the skill carries the full contract."""
    f_text = _command("f.md")
    factory_text = _command("factory.md")
    assert "skills/dark-factory/SKILL.md" in f_text, "f.md no longer points at dark-factory/SKILL.md"
    assert "skills/dark-factory/SKILL.md" in factory_text, "factory.md no longer points at dark-factory/SKILL.md"

    skill_text = _skill("dark-factory")
    required_phrases = (
        "--reviewer-calibration=true` is the default",
        "--reviewer-calibration=false",
        "## Reviewer calibration",
        "evidence/<run-id>/reviewer-calibration/",
        "codex exec --yolo -m gpt-5.3-codex-spark",
        "Do not claim delegated subagents underperformed raw Codex unless",
    )
    missing = [phrase for phrase in required_phrases if phrase not in skill_text]
    assert not missing, "dark-factory/SKILL.md missing reviewer calibration contract: " + ", ".join(missing)
