"""Tests for _gate_es / _gate_code_standards universal-prompt fallback.

Regression for PR #39. Extracted from tests/test_gates.py per
docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402


def test_gate_es_uses_universal_prompt_when_local_es_md_absent(
    tmp_path, monkeypatch, project_scoped_claude_config
):
    """_gate_es must fall back to the embedded universal prompt when
    .claude/commands/es.md is absent from the workdir.

    RED: current code is `_gate_es = _slash_gate("es")` which always builds
    a "/es ..." prompt regardless of whether es.md exists locally.

    GREEN: _gate_es checks for local es.md; when absent it calls
    _run_universal_prompt_gate with UNIVERSAL_EVIDENCE_REVIEW_PROMPT.
    """
    import subprocess as _sp
    from runner.handlers import _gate_es, Context as HCtx

    node = make_node(name="gate_es")
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    # tmp_path has no .claude/commands/es.md
    assert not (tmp_path / ".claude" / "commands" / "es.md").exists()

    called_prompts: list[str] = []

    fake_sha = "a" * 40

    def _fake_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_es(node, ctx)
    assert result.outcome == "success"
    assert called_prompts, "subprocess.run must have been called"

    prompt_used = called_prompts[0]
    # Universal prompt path: starts with "You are performing..." not "/es "
    assert not prompt_used.startswith("/es "), (
        f"When es.md is absent, _gate_es must use universal prompt, not /es slash. "
        f"Got prompt starting with: {prompt_used[:60]!r}"
    )


def test_gate_code_standards_uses_universal_prompt_when_local_file_absent(
    tmp_path, monkeypatch, project_scoped_claude_config
):
    """_gate_code_standards must fall back to embedded prompt when
    .claude/commands/code-standards.md is absent from workdir.

    RED: current code is `_gate_code_standards = _slash_gate("code-standards")`
    which always invokes /code-standards regardless of file presence.

    GREEN: _gate_code_standards checks for local code-standards.md and falls
    back to UNIVERSAL_CODE_STANDARDS_PROMPT when absent.
    """
    import subprocess as _sp
    from runner.handlers import _gate_code_standards, Context as HCtx

    node = make_node(name="gate_cs")
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    assert not (tmp_path / ".claude" / "commands" / "code-standards.md").exists()

    called_prompts: list[str] = []
    fake_sha = "b" * 40

    def _fake_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_code_standards(node, ctx)
    assert result.outcome == "success"
    assert called_prompts

    prompt_used = called_prompts[0]
    assert not prompt_used.startswith("/code-standards "), (
        f"When code-standards.md is absent, _gate_code_standards must use "
        f"universal prompt. Got: {prompt_used[:60]!r}"
    )


def test_universal_gate_prompts_require_coder_handoff() -> None:
    from runner.handler_universal_prompts import (
        CODER_HANDOFF_FORMAT,
        UNIVERSAL_CODE_STANDARDS_PROMPT,
        UNIVERSAL_EVIDENCE_REVIEW_PROMPT,
    )

    for prompt in (UNIVERSAL_CODE_STANDARDS_PROMPT, UNIVERSAL_EVIDENCE_REVIEW_PROMPT):
        assert "## Coder Handoff" in prompt
        assert "Blocking findings" in prompt
        assert "Required fix" in prompt
        assert "Verification to rerun" in prompt

    assert "## Coder Handoff" in CODER_HANDOFF_FORMAT
    assert "Blocking findings" in CODER_HANDOFF_FORMAT


def test_custom_prompt_gate_appends_coder_handoff_contract(
    tmp_path, monkeypatch, project_scoped_claude_config
):
    import subprocess as _sp
    from runner.handlers import _run_custom_prompt_gate, Context as HCtx

    prompt_dir = tmp_path / "prompts"
    prompt_dir.mkdir()
    (prompt_dir / "custom.md").write_text("Custom review body for ${goal}\n", encoding="utf-8")

    node = make_node(name="gate_custom", type="gate_er", prompt="@prompts/custom.md")
    ctx = HCtx(goal="handoff goal", workdir=tmp_path, backend="claude")

    fake_sha = "c" * 40
    called_prompts: list[str] = []

    def _fake_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _run_custom_prompt_gate(node, ctx, "gate_er")

    assert result.outcome == "success"
    assert called_prompts
    prompt_used = called_prompts[0]
    assert "Custom review body for handoff goal" in prompt_used
    assert "## Coder Handoff" in prompt_used
    assert "Required fix" in prompt_used
