"""Regression guard: every ``Skill("X", args="...")`` dispatch in a slash
command must reference only flags documented in the target skill's SKILL.md.

Closes the gap exposed on 2026-06-22 (Codex P1 during PR #93 closeout): the
slash commands ``.claude/commands/f.md`` and ``.claude/commands/fs.md``
dispatched ``Skill("dark-factory", args="--mode <pr|feature> --phase-until
spec_validation ...")`` even though the ``dark-factory`` skill only
documents ``--pipeline``/``--feature``/``--backend``/``--state``. The flags
were decorative — the runner CLI does not parse them. PR #93 fix #2 and
fix #3 rewrote both slash commands to dispatch via the workflow file
directly. This test prevents re-introduction.

What it asserts
---------------
For every ``Skill("X", args="...")`` call inside any file in
``.claude/commands/``:

1. The skill ``X`` must be resolvable (either in-repo or in ``~/.claude/skills/``).
2. Every ``--flag`` referenced in ``args`` must appear in the target
   skill's documented arg list. The arg list is the set of ``--flag``
   tokens found in bullet-list items (lines matching ``^\\s*[-*]\\s+--flag``)
   within the skill body.

What it does NOT assert
-----------------------
- Flags mentioned only in prose are NOT considered documented (they must
  appear in a structured flag-list to be safely referenced by callers).
- The contents of the skill body outside the arg list are not checked.
- ``Skill("X", ...)`` calls without an ``args=...`` kwarg are skipped
  (covered by separate dispatch tests).
"""
from __future__ import annotations

import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
COMMANDS_DIR = ROOT / ".claude" / "commands"
SKILLS_DIRS = (ROOT / ".claude" / "skills", pathlib.Path.home() / ".claude" / "skills")

# Match `Skill("X", ...args="..."...)` with named-or-positional args string.
# Captures (1) skill name, (2) args string.
SKILL_CALL_RE = re.compile(
    r'Skill\(\s*["\']([a-z][a-z0-9_-]*)["\']\s*,\s*[^)]*?args\s*=\s*["\']([^"\']*)["\']',
    re.DOTALL,
)

# Match a `--flag` token (not embedded in a longer word). Captures (1) flag name.
FLAG_RE = re.compile(r'(?<![\w-])--([a-z][a-z0-9-]+)')

# Match a documented flag in a skill body (bullet-list item).
# Captures (1) flag name. Conservative: only matches `--flag` at the start
# of a bullet item, after `- ` or `* `.
DOCUMENTED_FLAG_RE = re.compile(r'^\s*[-*]\s+`?--([a-z][a-z0-9-]+)')


def _resolve_skill(name: str) -> pathlib.Path:
    """Find the SKILL.md for skill `name`. Search in-repo first, then ~/.claude."""
    for root in SKILLS_DIRS:
        candidate = root / name / "SKILL.md"
        if candidate.exists():
            return candidate
    raise FileNotFoundError(
        f"Skill {name!r} not found. Searched: "
        + ", ".join(str(d / name / "SKILL.md") for d in SKILLS_DIRS)
    )


def _documented_flags(skill_md: pathlib.Path) -> set[str]:
    """Return the set of --flags documented in the skill body (bullet lists only)."""
    flags: set[str] = set()
    for line in skill_md.read_text().splitlines():
        m = DOCUMENTED_FLAG_RE.match(line)
        if m:
            flags.add(m.group(1))
    return flags


def _referenced_flags(args_str: str) -> set[str]:
    """Return the set of --flag names referenced in a Skill(...) args string.

    Strips sentinel placeholder tokens (anything containing `<...>` or `|`)
    so the test does not flag e.g. `<pr|feature>` as a missing flag.
    """
    raw = set(FLAG_RE.findall(args_str))
    return {f for f in raw if "<" not in f and "|" not in f and "…" not in f}


def _slash_command_files() -> list[pathlib.Path]:
    """All .md files in .claude/commands/, sorted."""
    return sorted(COMMANDS_DIR.glob("*.md"))


def test_every_skill_dispatch_resolves_a_documented_skill():
    """Every Skill("X", args="...") target must have a discoverable SKILL.md.

    This is a cheap pre-check: if a skill cannot be resolved, the per-file
    parametrized test below would fail with a confusing FileNotFoundError.
    """
    missing: list[str] = []
    for cmd_path in _slash_command_files():
        text = cmd_path.read_text()
        for skill_name, _ in SKILL_CALL_RE.findall(text):
            try:
                _resolve_skill(skill_name)
            except FileNotFoundError:
                missing.append(f"{cmd_path.name}:{skill_name}")
    assert not missing, (
        f"Slash commands reference unresolved skills:\n  "
        + "\n  ".join(missing)
    )


def test_every_skill_dispatch_flag_is_documented():
    """Every --flag referenced in a Skill(...) args string must be in the
    target skill's documented arg list.

    This is the bug-class guard: catches re-introductions of the PR #93
    `--mode` / `--phase-until` decoration.
    """
    violations: list[str] = []
    for cmd_path in _slash_command_files():
        text = cmd_path.read_text()
        for skill_name, args_str in SKILL_CALL_RE.findall(text):
            try:
                skill_md = _resolve_skill(skill_name)
            except FileNotFoundError:
                # The pre-check test above reports unresolved skills;
                # skip here to avoid duplicate failures.
                continue
            documented = _documented_flags(skill_md)
            referenced = _referenced_flags(args_str)
            missing = referenced - documented
            if missing:
                violations.append(
                    f"{cmd_path.name} -> Skill('{skill_name}'): "
                    f"references --{', --'.join(sorted(missing))} "
                    f"but {skill_md.relative_to(ROOT)} does not document them. "
                    f"Documented: {sorted(documented) or '(none)'}."
                )
    assert not violations, (
        "Slash commands dispatch undocumented flags to Skills:\n  "
        + "\n  ".join(violations)
    )
