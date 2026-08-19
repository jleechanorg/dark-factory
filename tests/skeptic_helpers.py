"""Shared helpers for skeptic gate tests (default reviewer set from config)."""

from __future__ import annotations

import json

from runner.reviewer_priority import default_reviewers_json, mandatory_reviewers

DEFAULT_REVIEWERS_JSON = default_reviewers_json()
MANDATORY_REVIEWER_CLI_NAMES = mandatory_reviewers()


def reviewer_identity_for_cli(reviewer: str) -> str:
    """Declared IDENTITY line for a reviewer CLI name."""
    mapping = {
        "claudem": "minimax",
        "minimax": "minimax",
        "agy": "agy",
        "cursor-agent": "cursor-agent",
        "cursor": "cursor-agent",
        "agentf": "cursor-agent",
        "codex": "codex",
        "gemini": "gemini",
    }
    return mapping.get(reviewer, reviewer)


def parse_default_reviewers() -> list[tuple[str, str]]:
    parsed = json.loads(DEFAULT_REVIEWERS_JSON)
    return [(str(a), str(b)) for a, b in parsed]
