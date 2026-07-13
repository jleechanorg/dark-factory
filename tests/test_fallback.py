"""Tests for agent fallback handlers."""

from __future__ import annotations

import os
import pytest
from unittest.mock import MagicMock, patch

from runner.parser import Node
from runner.handlers import Context, Result
from runner.handler_fallback import (
    _parse_agent_chain,
    _agent_spawn,
    _agent_watchdog,
    _agent_swap,
)


def test_parse_agent_chain():
    # Happy path
    res = _parse_agent_chain("minimax:MiniMax-M3,antigravity:claude-sonnet-4-5")
    assert res == [("minimax", "MiniMax-M3"), ("antigravity", "claude-sonnet-4-5")]

    # Single agent with no model
    res = _parse_agent_chain("claude-code")
    assert res == [("claude-code", "")]

    # Spaces and empty elements
    res = _parse_agent_chain("  minimax : MiniMax-M3 , ,  antigravity:gemini ")
    assert res == [("minimax", "MiniMax-M3"), ("antigravity", "gemini")]


def test_agent_spawn():
    node = Node(name="spawn", attrs={
        "agent_chain": "minimax:MiniMax-M3,antigravity:claude-sonnet-4-5"
    })
    ctx = Context(workdir=None, goal="test", state={})

    res = _agent_spawn(node, ctx)
    assert res.outcome == "success"
    assert ctx.state["fallback_index"] == "0"
    assert ctx.state["agent_chain"] == "minimax:MiniMax-M3,antigravity:claude-sonnet-4-5"
    assert ctx.state["ao.agent"] == "minimax"
    assert os.environ.get("MINIMAX_MODEL") == "MiniMax-M3"


@patch("subprocess.run")
def test_agent_watchdog_rate_limit(mock_run):
    node = Node(name="watchdog", attrs={"stuck_threshold": "600"})
    ctx = Context(workdir=None, goal="test", state={"ao.session": "wa-1234"})

    # Mock list-sessions to return our session name
    mock_list = MagicMock()
    mock_list.returncode = 0
    mock_list.stdout = "ed3dd2670551-wa-1234\n"

    # Mock capture-pane to return a rate-limit matched string
    mock_capture = MagicMock()
    mock_capture.returncode = 0
    mock_capture.stdout = "Hello\nYou've hit your weekly limit · resets Jul 13\nWorld"

    mock_run.side_effect = [mock_list, mock_capture]

    res = _agent_watchdog(node, ctx)
    assert res.outcome == "partial"
    assert "weekly limit" in res.output


@patch("subprocess.run")
@patch("urllib.request.urlopen")
def test_agent_watchdog_active_healthy(mock_urlopen, mock_run):
    node = Node(name="watchdog", attrs={"stuck_threshold": "600"})
    ctx = Context(workdir=None, goal="test", state={"ao.session": "wa-1234"})

    # Mock list-sessions
    mock_list = MagicMock()
    mock_list.returncode = 0
    mock_list.stdout = "ed3dd2670551-wa-1234\n"

    # Mock capture-pane (no rate limit keywords)
    mock_capture = MagicMock()
    mock_capture.returncode = 0
    mock_capture.stdout = "running tests\n10 passed"

    mock_run.side_effect = [mock_list, mock_capture]

    # Mock AO API response: status is active
    mock_resp = MagicMock()
    mock_resp.read.return_value = b'{"status": "active", "activity": "working"}'
    mock_urlopen.return_value.__enter__.return_value = mock_resp

    res = _agent_watchdog(node, ctx)
    assert res.outcome == "success"


@patch("subprocess.run")
def test_agent_swap(mock_run):
    node = Node(name="swap", attrs={"max_swaps": "2", "ao_bin": "~/bin/ao"})
    ctx = Context(workdir=None, goal="test", state={
        "ao.session": "wa-1234",
        "ao.worktree": "/tmp/wt-1234",
        "agent_chain": "minimax:MiniMax-M3,antigravity:claude-sonnet-4-5",
        "fallback_index": "0",
    })

    # Mock killing session
    mock_kill = MagicMock()
    mock_kill.returncode = 0
    mock_run.return_value = mock_kill

    res = _agent_swap(node, ctx)
    assert res.outcome == "success"
    assert "ao.session" not in ctx.state
    assert "ao.worktree" not in ctx.state
    assert ctx.state["ao.agent"] == "antigravity"
    assert ctx.state["fallback_index"] == "1"
    assert os.environ.get("ANTIGRAVITY_MODEL") == "claude-sonnet-4-5"


@patch("subprocess.run")
def test_agent_swap_exhausted(mock_run):
    node = Node(name="swap", attrs={"max_swaps": "2", "ao_bin": "~/bin/ao"})
    ctx = Context(workdir=None, goal="test", state={
        "ao.session": "wa-1234",
        "agent_chain": "minimax:MiniMax-M3,antigravity:claude-sonnet-4-5",
        "fallback_index": "1",
    })



    # Mock killing session
    mock_kill = MagicMock()
    mock_kill.returncode = 0
    mock_run.return_value = mock_kill

    res = _agent_swap(node, ctx)
    assert res.outcome == "exhausted"
