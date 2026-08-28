"""Tests for the jleechan-ok8 delete-first template and LOC/dead-code gates."""

from __future__ import annotations

import pathlib
import subprocess as _sp
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
from runner.handlers import (  # noqa: E402
    Context,
    Result,
    _gate_net_loc,
    _gate_dead_code,
)


def test_gate_net_loc_success(tmp_path, monkeypatch):
    """gate_net_loc succeeds when additions <= deletions (net LOC <= 0)."""
    node = make_node(name="gate_net_loc")
    ctx = Context(goal="test", workdir=tmp_path, backend="echo")
    
    # Mock git diff --numstat to return additions=10, deletions=15
    def mock_run(cmd, **kwargs):
        if "diff" in cmd:
            return _sp.CompletedProcess(cmd, 0, stdout="10\t15\tfile.py\n", stderr="")
        return _sp.CompletedProcess(cmd, 0, stdout="", stderr="")

    monkeypatch.setattr("subprocess.run", mock_run)

    result = _gate_net_loc(node, ctx)
    assert result.outcome == "success"
    assert "Net LOC: -5" in result.output
    assert "Total Additions: 10" in result.output
    assert "Total Deletions: 15" in result.output


def test_gate_net_loc_failure(tmp_path, monkeypatch):
    """gate_net_loc fails when additions > deletions (net LOC > 0)."""
    node = make_node(name="gate_net_loc")
    ctx = Context(goal="test", workdir=tmp_path, backend="echo")

    # Mock git diff --numstat to return additions=20, deletions=5
    def mock_run(cmd, **kwargs):
        if "diff" in cmd:
            return _sp.CompletedProcess(cmd, 0, stdout="20\t5\tfile.py\n", stderr="")
        return _sp.CompletedProcess(cmd, 0, stdout="", stderr="")

    monkeypatch.setattr("subprocess.run", mock_run)

    result = _gate_net_loc(node, ctx)
    assert result.outcome == "failure"
    assert "Net LOC: 15" in result.output
    assert "Total Additions: 20" in result.output
    assert "Total Deletions: 5" in result.output


def test_gate_net_loc_binary_and_empty(tmp_path, monkeypatch):
    """gate_net_loc handles binary files (- -) and empty diffs correctly."""
    node = make_node(name="gate_net_loc")
    ctx = Context(goal="test", workdir=tmp_path, backend="echo")

    # Mock git diff --numstat to return additions=- deletions=- for binary, and blank lines
    def mock_run(cmd, **kwargs):
        if "diff" in cmd:
            return _sp.CompletedProcess(cmd, 0, stdout="-\t-\tfile.bin\n\n", stderr="")
        return _sp.CompletedProcess(cmd, 0, stdout="", stderr="")

    monkeypatch.setattr("subprocess.run", mock_run)

    result = _gate_net_loc(node, ctx)
    assert result.outcome == "success"
    assert "Net LOC: 0" in result.output


def test_gate_dead_code_pass(tmp_path, monkeypatch, project_scoped_claude_config):
    """gate_dead_code succeeds when the LLM reviews and returns a pass verdict."""
    node = make_node(name="gate_dead_code")
    ctx = Context(goal="test", workdir=tmp_path, backend="claude")

    fake_sha = "a" * 40
    called_prompts = []

    def mock_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", mock_run)

    result = _gate_dead_code(node, ctx)
    assert result.outcome == "success"
    assert called_prompts
    assert "Dead Code & Cleanliness Review" in called_prompts[0]


def test_gate_dead_code_fail(tmp_path, monkeypatch, project_scoped_claude_config):
    """gate_dead_code fails when the LLM reviews and returns a fail verdict."""
    node = make_node(name="gate_dead_code")
    ctx = Context(goal="test", workdir=tmp_path, backend="claude")

    fake_sha = "b" * 40

    def mock_run(cmd, **kwargs):
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: fail\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", mock_run)

    result = _gate_dead_code(node, ctx)
    assert result.outcome == "failure"
