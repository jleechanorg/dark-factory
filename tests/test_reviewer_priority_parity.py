"""Parity: Rust daemon, Python gate, and JSON config share reviewer priority."""

from __future__ import annotations

import json
from pathlib import Path

from runner.reviewer_priority import (
    coder_fallback_chain,
    default_coder,
    default_reviewers_json,
    mandatory_reviewers,
    skeptic_reviewer_priority,
)

_CONFIG = (
    Path(__file__).resolve().parent.parent / "config" / "skeptic_reviewer_priority.json"
)


def test_json_file_matches_python_loader():
    raw = json.loads(_CONFIG.read_text(encoding="utf-8"))
    assert skeptic_reviewer_priority() == raw["reviewer_priority"]
    assert default_coder() == raw.get("default_coder", "agy")
    assert coder_fallback_chain() == raw.get("coder_fallback_chain", ["claudem"])


def test_mandatory_reviewers_equals_priority_list():
    assert mandatory_reviewers() == tuple(skeptic_reviewer_priority())


def test_default_reviewers_json_covers_all_vendors():
    parsed = json.loads(default_reviewers_json())
    ids = [pair[0] for pair in parsed]
    assert ids == skeptic_reviewer_priority()


def test_default_priority_excludes_legacy_vendors():
    priority = skeptic_reviewer_priority()
    assert priority == ["claudem", "agy", "cursor-agent"]
    assert "codex" not in priority
    assert "gemini" not in priority
