"""TDD tests for repository-agnostic gate dispatch logic.

These tests verify:
1. _gate_code_standards dispatches to /code-standards (hyphen) not /code_standards
2. _gate_evidence_review dispatches to the correct command based on which file exists
3. _gate_es and _gate_er are distinct and dispatch to /es and /er respectively
4. gate_evidence_review is in _VALIDATION_TYPES
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

import pytest  # noqa: E402

from runner.handlers import (  # noqa: E402
    Context,
    Result,
    _gate_code_standards,
    _gate_evidence_review,
    _gate_es,
    _gate_er,
)
from runner.parser import Node  # noqa: E402


def _make_node(name: str = "test_gate") -> Node:
    return Node(name=name, attrs={})


def _make_ctx(workdir: pathlib.Path) -> Context:
    return Context(goal="test", workdir=workdir, backend="echo")


# ---------------------------------------------------------------------------
# Issue 2: _gate_code_standards must use "code-standards" (hyphen)
# ---------------------------------------------------------------------------

def test_gate_code_standards_uses_hyphenated_command_when_local_cmd_exists(
    tmp_path, monkeypatch
):
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "code-standards.md").write_text("# code standards")

    captured_cmds: list[str] = []

    import runner.handlers as handlers_mod

    original_slash_gate = handlers_mod._slash_gate

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)
    # Re-read the function under test so it uses the monkeypatched _slash_gate
    from runner.handlers import _gate_code_standards as gcs  # noqa: F401

    gcs(_make_node(), _make_ctx(tmp_path))

    assert "code-standards" in captured_cmds, (
        f"Expected 'code-standards' but got: {captured_cmds}"
    )
    assert "code_standards" not in captured_cmds, (
        "Should not use underscore 'code_standards' — filename is 'code-standards.md'"
    )


def test_gate_code_standards_uses_hyphenated_command_when_local_skill_exists(
    tmp_path, monkeypatch
):
    skill_dir = tmp_path / ".claude" / "skills" / "code-standards"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text("# code standards skill")

    captured_cmds: list[str] = []
    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)

    from runner.handlers import _gate_code_standards as gcs  # noqa: F401

    gcs(_make_node(), _make_ctx(tmp_path))

    assert "code-standards" in captured_cmds, (
        f"Expected 'code-standards' but got: {captured_cmds}"
    )


# ---------------------------------------------------------------------------
# Issue 3: _gate_evidence_review must dispatch to the correct slash command
# ---------------------------------------------------------------------------

def test_gate_evidence_review_uses_es_when_es_md_exists(tmp_path, monkeypatch):
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "es.md").write_text("# es command")

    captured_cmds: list[str] = []
    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)

    from runner.handlers import _gate_evidence_review as ger  # noqa: F401

    ger(_make_node(), _make_ctx(tmp_path))

    assert "es" in captured_cmds, f"Expected 'es' but got: {captured_cmds}"


def test_gate_evidence_review_uses_evidence_review_when_evidence_review_md_exists_no_es(
    tmp_path, monkeypatch
):
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    # Only evidence_review.md exists — NOT es.md
    (cmd_dir / "evidence_review.md").write_text("# evidence review command")

    captured_cmds: list[str] = []
    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)

    from runner.handlers import _gate_evidence_review as ger  # noqa: F401

    ger(_make_node(), _make_ctx(tmp_path))

    assert "evidence_review" in captured_cmds, (
        f"Expected 'evidence_review' but got: {captured_cmds}"
    )
    assert "es" not in captured_cmds, (
        "Should not invoke /es when only evidence_review.md exists"
    )


# ---------------------------------------------------------------------------
# Issue 1 (P1): _gate_es uses /es, _gate_er uses /er (distinct handlers)
# ---------------------------------------------------------------------------

def test_gate_es_and_gate_er_are_distinct_handlers():
    assert _gate_es is not _gate_er, (
        "_gate_es and _gate_er must be distinct functions, not aliases"
    )


def test_gate_er_dispatches_to_er_command_when_er_md_exists(tmp_path, monkeypatch):
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "er.md").write_text("# er command")

    captured_cmds: list[str] = []
    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)

    from runner.handlers import _gate_er as ger_handler  # noqa: F401

    ger_handler(_make_node(), _make_ctx(tmp_path))

    assert "er" in captured_cmds, f"Expected 'er' but got: {captured_cmds}"
    assert "es" not in captured_cmds, (
        "_gate_er should dispatch to /er not /es"
    )


def test_gate_es_dispatches_to_es_command_when_es_md_exists(tmp_path, monkeypatch):
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "es.md").write_text("# es command")

    captured_cmds: list[str] = []
    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)

    from runner.handlers import _gate_es as ges_handler  # noqa: F401

    ges_handler(_make_node(), _make_ctx(tmp_path))

    assert "es" in captured_cmds, f"Expected 'es' but got: {captured_cmds}"
    assert "er" not in captured_cmds, (
        "_gate_es should dispatch to /es not /er"
    )


# ---------------------------------------------------------------------------
# Issue 5: gate_evidence_review must be in _VALIDATION_TYPES
# ---------------------------------------------------------------------------

def test_gate_evidence_review_in_validation_types():
    from runner.engine import _VALIDATION_TYPES  # noqa: E402

    assert "gate_evidence_review" in _VALIDATION_TYPES, (
        "gate_evidence_review must be in _VALIDATION_TYPES to clear "
        "_unresolved_failure on success, same as gate_es and gate_er"
    )
