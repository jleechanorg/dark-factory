"""install.sh must mirror every user-facing repo-local command/skill to
~/.claude/ so it resolves from any cwd, not just from inside this repo.

Regression: PR #829 added .claude/commands/fr.md, .claude/commands/factory-review.md,
and .claude/skills/factory-review/ (a review-only entry point meant to be invoked
from an UNRELATED calling repo), but install.sh's hardcoded mirror lists were never
updated -- `/fr` silently only worked when the caller's cwd was this repo itself,
defeating its own purpose. Found live: the user reported "/fr" not resolving.
"""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INSTALL_SH = (ROOT / "install.sh").read_text(encoding="utf-8")


def _mirror_list(marker: str) -> list[str]:
    for line in INSTALL_SH.splitlines():
        line = line.strip()
        if line.startswith(marker):
            inside = line.split("in", 1)[1].split(";", 1)[0].strip()
            return inside.split()
    raise AssertionError(f"no line starting with {marker!r} found in install.sh")


def test_fr_and_factory_review_commands_are_mirrored():
    cmds = _mirror_list("for cmd in")
    assert "fr" in cmds, "install.sh must mirror /fr to ~/.claude/commands/"
    assert "factory-review" in cmds, "install.sh must mirror /factory-review to ~/.claude/commands/"


def test_factory_review_skill_is_mirrored():
    skills = _mirror_list("for skill in")
    assert "factory-review" in skills, "install.sh must mirror the factory-review skill to ~/.claude/skills/"


def test_every_repo_local_command_referenced_in_dark_factory_skill_is_mirrored():
    """The `dark-factory` skill's own SKILL.md documents /f, /fs, /factory,
    /factory-spec, /fr, /factory-review as the user-facing entry points this
    repo ships -- every one of them must be in install.sh's mirror list."""
    documented = {"f", "fs", "factory", "factory-spec", "fr", "factory-review"}
    cmds = set(_mirror_list("for cmd in"))
    missing = documented - cmds
    assert not missing, f"commands documented but not mirrored by install.sh: {missing}"
