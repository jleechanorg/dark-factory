"""Regression coverage for safe factory defaults and pilot env isolation."""

from __future__ import annotations

import os
import pathlib
import re
import subprocess


ROOT = pathlib.Path(__file__).parents[1]
ACTIVE_REVIEW_ROUTING_DOCS = (
    ".claude/skills/dark-factory/SKILL.md",
    "docs/pipeline-selection.md",
    "docs/pipelines/bugfix_noholdout.md",
    "docs/refactor/file-ownership-map.handlers.md",
)


def _stale_claude_sonnet_queue_claims(text: str) -> list[str]:
    normalized = " ".join(text.split())
    return re.findall(
        r"(?i)(?:priority queue|default|fallback)[^.;]{0,180}claude[- ]sonnet"
        r"|claude[- ]sonnet[^.;]{0,180}(?:priority queue|default|fallback)",
        normalized,
    )


def _stale_direct_claude_fallback_claims(text: str) -> list[str]:
    normalized = " ".join(text.split())
    return re.findall(
        r"(?i)(?:agy|codex|minimax)\s*(?:→|->|to)\s*claude(?:[- ]sonnet)?"
        r"[^.;]{0,80}fallback|fallback[^.;]{0,80}(?:agy|codex|minimax)"
        r"\s*(?:→|->|to)\s*claude(?:[- ]sonnet)?",
        normalized,
    )


def test_workflow_defaults_to_runner_ao_backend() -> None:
    workflow = (ROOT / ".claude/workflows/dark-factory.md").read_text()

    assert 'export BACKEND="${BACKEND:-ao}"' in workflow
    assert "AO/Antigravity" in workflow
    assert 'export BACKEND="${BACKEND:-claude}"' not in workflow


def test_skill_detector_falls_back_to_ao_without_explicit_signal() -> None:
    skill = (ROOT / ".claude/skills/dark-factory/SKILL.md").read_text()

    detector = skill.split("detect_cli_backend() {", 1)[1].split(
        "DETECTED_BACKEND=", 1
    )[0]
    assert 'echo "ao"' in detector
    assert 'case "${ANTHROPIC_BASE_URL:-}"' not in detector
    assert "parent_comm" not in detector
    assert "default `ao`" in skill
    assert "hardcoded default (`ao`)" in skill


def test_skill_does_not_advertise_claude_sonnet_as_reviewer_queue_default() -> None:
    """The skill must track the live cross-vendor reviewer queue.

    This deliberately scans the prose rather than asserting only the two
    currently known lines, so a future reintroduction of the stale
    ``claude-sonnet`` queue tail fails at the source.
    """
    skill = (ROOT / ".claude/skills/dark-factory/SKILL.md").read_text()
    stale_claims = _stale_claude_sonnet_queue_claims(skill)
    assert not stale_claims, (
        "canonical dark-factory skill advertises a stale claude-sonnet "
        f"reviewer default/fallback: {stale_claims!r}"
    )


def test_active_review_routing_docs_match_current_queue() -> None:
    """Active operator docs must not drift from the three-vendor queue."""
    violations = {}
    for path in ACTIVE_REVIEW_ROUTING_DOCS:
        text = (ROOT / path).read_text()
        claims = _stale_claude_sonnet_queue_claims(text)
        claims.extend(_stale_direct_claude_fallback_claims(text))
        violations[path] = claims
    violations = {path: claims for path, claims in violations.items() if claims}
    assert not violations, f"stale reviewer queue claims: {violations!r}"


def test_pilot_minimax_scrubber_removes_inherited_routing_state() -> None:
    script = (ROOT / "daemon/qw5-pilot-dispatch.sh").read_text()
    match = re.search(
        r"configure_minimax_env\(\) \{(?P<body>.*?)\n\}\n\nconfigure_minimax_env",
        script,
        flags=re.DOTALL,
    )
    assert match, "pilot must expose a testable MiniMax environment scrubber"

    inherited = os.environ.copy()
    inherited.update(
        {
            "MINIMAX_API_KEY": "pilot-key",
            "MINIMAX_MODEL": "stale-model",
            "MINIMAX_BASE_URL": "https://stale.minimax.example",
            "DARK_FACTORY_MINIMAX_MODEL": "stale-factory-model",
            "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
            "ANTHROPIC_MODEL": "stale-model",
            "CLAUDE_CONFIG_DIR": "/Users/personal/.claude",
            "DARK_FACTORY_CLAUDE_CONFIG_DIR": "/Users/personal/.claude-config",
            "CLAUDEM_MODE": "personal",
            "ANTHROPIC_SMALL_FAST_MODEL": "stale-fast-model",
            "CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL": "1",
        }
    )
    probe = (
        "configure_minimax_env() {"
        + match.group("body")
        + "\n}\nconfigure_minimax_env\n"
        + "env\n"
    )
    result = subprocess.run(
        ["bash", "-c", probe],
        env=inherited,
        text=True,
        capture_output=True,
        check=True,
    )
    env_lines = set(result.stdout.splitlines())

    assert "CLAUDE_CONFIG_DIR=/Users/personal/.claude" not in env_lines
    assert "DARK_FACTORY_CLAUDE_CONFIG_DIR=/Users/personal/.claude-config" not in env_lines
    assert "CLAUDEM_MODE=personal" not in env_lines
    assert "MINIMAX_MODEL=stale-model" not in env_lines
    assert "MINIMAX_BASE_URL=https://stale.minimax.example" not in env_lines
    assert "DARK_FACTORY_MINIMAX_MODEL=stale-factory-model" not in env_lines
    assert "ANTHROPIC_BASE_URL=https://api.anthropic.com" not in env_lines
    assert "MINIMAX_API_KEY=pilot-key" in env_lines
    assert "ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic" in env_lines
    assert "ANTHROPIC_MODEL=MiniMax-M3" in env_lines
    assert "ANTHROPIC_SMALL_FAST_MODEL=MiniMax-M3" in env_lines
    assert "CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL=0" in env_lines
    assert "ANTHROPIC_API_KEY=pilot-key" in env_lines
    assert "ANTHROPIC_AUTH_TOKEN=pilot-key" in env_lines
