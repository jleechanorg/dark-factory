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


def test_gate_code_standards_uses_universal_fallback_when_only_skill_exists(
    tmp_path, monkeypatch
):
    """Skill file alone cannot resolve a /code-standards slash command; use universal prompt."""
    skill_dir = tmp_path / ".claude" / "skills" / "code-standards"
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text("# code standards skill")

    captured_cmds: list[str] = []
    captured_universal: list[str] = []
    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    def mock_universal(prompt_template: str, name: str, node, ctx):
        captured_universal.append(name)
        return Result(outcome="success", output=f"universal:{name}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)
    monkeypatch.setattr(handlers_mod, "_run_universal_prompt_gate", mock_universal)

    from runner.handlers import _gate_code_standards as gcs  # noqa: F401

    gcs(_make_node(), _make_ctx(tmp_path))

    assert not captured_cmds, (
        f"_slash_gate must NOT be called when only skill file exists, got: {captured_cmds}"
    )
    assert "gate_code_standards" in captured_universal, (
        f"Expected universal prompt fallback, got: {captured_universal}"
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


# ---------------------------------------------------------------------------
# Bug: _gate_evidence_review must run BOTH /es AND /er when both files exist
# ---------------------------------------------------------------------------

def test_gate_evidence_review_runs_both_es_and_er_when_both_exist(tmp_path, monkeypatch):
    """Regression: when es.md AND er.md both exist, _gate_evidence_review must
    invoke both gates, not silently skip /er after /es succeeds."""
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "es.md").write_text("# es command")
    (cmd_dir / "er.md").write_text("# er command")

    captured_cmds: list[str] = []
    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        captured_cmds.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)

    from runner.handlers import _gate_evidence_review as ger  # noqa: F401

    result = ger(_make_node(), _make_ctx(tmp_path))

    assert "es" in captured_cmds, f"Expected /es to run but got: {captured_cmds}"
    assert "er" in captured_cmds, (
        f"Expected /er to also run when both es.md and er.md exist, but got: {captured_cmds}"
    )
    assert result.outcome == "success"


def test_gate_evidence_review_fails_when_er_fails_even_if_es_passes(tmp_path, monkeypatch):
    """Combined result should be 'failure' if /er fails even when /es passes."""
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "es.md").write_text("# es command")
    (cmd_dir / "er.md").write_text("# er command")

    import runner.handlers as handlers_mod

    def mock_slash_gate(cmd: str):
        outcome = "success" if cmd == "es" else "failure"
        return lambda node, ctx: Result(outcome=outcome, output=f"ran /{cmd}")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)

    from runner.handlers import _gate_evidence_review as ger

    result = ger(_make_node(), _make_ctx(tmp_path))

    assert result.outcome == "failure", (
        f"Expected failure when /er fails, got: {result.outcome}"
    )


# ---------------------------------------------------------------------------
# Bug: _gate_es must NOT dispatch to /es when only skill file exists
# ---------------------------------------------------------------------------

def test_gate_es_uses_universal_fallback_when_only_skill_file_exists(tmp_path, monkeypatch):
    """Regression: when evidence-standards.md skill exists but es.md does NOT,
    _gate_es must NOT call _slash_gate('es') — the /es command cannot be resolved
    from a skill file. It should fall back to the universal prompt gate."""
    skill_dir = tmp_path / ".claude" / "skills"
    skill_dir.mkdir(parents=True)
    (skill_dir / "evidence-standards.md").write_text("# skill")
    # Intentionally do NOT create es.md

    import runner.handlers as handlers_mod

    slash_gate_called: list[str] = []
    universal_called: list[str] = []

    def mock_slash_gate(cmd: str):
        slash_gate_called.append(cmd)
        return lambda node, ctx: Result(outcome="success", output=f"ran /{cmd}")

    def mock_universal(prompt: str, gate_name: str, node, ctx) -> Result:
        universal_called.append(gate_name)
        return Result(outcome="success", output="universal fallback")

    monkeypatch.setattr(handlers_mod, "_slash_gate", mock_slash_gate)
    monkeypatch.setattr(handlers_mod, "_run_universal_prompt_gate", mock_universal)

    from runner.handlers import _gate_es as ges

    ges(_make_node(), _make_ctx(tmp_path))

    assert "es" not in slash_gate_called, (
        f"_gate_es must NOT call _slash_gate('es') when only skill file exists, "
        f"got slash_gate calls: {slash_gate_called}"
    )
    assert len(universal_called) == 1, (
        f"Expected universal fallback to be called once, got: {universal_called}"
    )


# ---------------------------------------------------------------------------
# Bug: _run_universal_prompt_gate must catch TimeoutExpired
# ---------------------------------------------------------------------------

def test_universal_prompt_gate_returns_error_on_timeout(tmp_path, monkeypatch):
    """Regression: if subprocess.run raises TimeoutExpired, _run_universal_prompt_gate
    must return a Result(outcome='error') rather than propagating the exception."""
    import subprocess
    import runner.handlers as handlers_mod

    def _raise_timeout(*args, **kwargs):
        raise subprocess.TimeoutExpired(cmd="claude", timeout=1200)

    monkeypatch.setattr(subprocess, "run", _raise_timeout)
    monkeypatch.setattr(handlers_mod, "_worktree_head_sha", lambda _: "abc1234")

    from runner.handlers import _run_universal_prompt_gate

    ctx = _make_ctx(tmp_path)
    ctx.backend = "claude"  # non-echo so subprocess is invoked
    node = _make_node()
    result = _run_universal_prompt_gate("Review: {expected_sha}", "gate_test", node, ctx)

    assert result.outcome == "error", f"Expected 'error' outcome on timeout, got {result.outcome!r}"
    assert "timed out" in result.output.lower(), f"Expected timeout message, got: {result.output!r}"
