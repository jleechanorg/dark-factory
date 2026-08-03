"""Focused test for AGY codergen agent selection.

Ensures that handler_codergen passes explicit `--agent <agent_name>` when running
agy backend, defaulting safely to `gemini-3.6-flash-high`.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from unittest.mock import MagicMock, patch

from runner.handlers import _codergen
from runner.handler_core import Context
from runner.parser import Node


def _create_mock_popen(stdout_text="Success output", returncode=0):
    def _side_effect(args, **kwargs):
        proc = MagicMock()
        proc.communicate.return_value = (stdout_text, "")
        proc.returncode = returncode
        proc.stdout = stdout_text
        proc.stderr = ""
        return proc
    return _side_effect


def test_agy_codergen_default_agent(tmp_path: pathlib.Path) -> None:
    """When node/ctx specify no agy_agent, agy is launched with --agent gemini-3.6-flash-high."""
    (tmp_path / "prompts").mkdir(parents=True, exist_ok=True)
    (tmp_path / "prompts" / "test.md").write_text("Test prompt", encoding="utf-8")

    node = Node(
        name="test_node",
        attrs={"prompt": "@prompts/test.md", "backend": "agy"},
    )
    ctx = Context(goal="test goal", workdir=tmp_path, backend="agy")

    with patch("subprocess.Popen", side_effect=_create_mock_popen()) as popen_mock, \
         patch("subprocess.run", return_value=MagicMock(returncode=0, stdout="")), \
         patch("runner.handlers._sandboxed_args_for_workdir", side_effect=lambda args, wd: args):
        res = _codergen(node, ctx)

    assert res.outcome == "success"
    assert res.metadata.get("agy_agent") == "gemini-3.6-flash-high"
    assert popen_mock.called
    called_args = popen_mock.call_args[0][0]
    assert called_args[0] == "agy"
    assert called_args[1] == "--agent"
    assert called_args[2] == "gemini-3.6-flash-high"


def test_agy_codergen_custom_agent_override(tmp_path: pathlib.Path) -> None:
    """When node.attrs or ctx.state specifies an agent, agy is launched with that agent."""
    (tmp_path / "prompts").mkdir(parents=True, exist_ok=True)
    (tmp_path / "prompts" / "test.md").write_text("Test prompt", encoding="utf-8")

    node = Node(
        name="test_node",
        attrs={"prompt": "@prompts/test.md", "backend": "agy", "agy_agent": "custom-agent-v2"},
    )
    ctx = Context(goal="test goal", workdir=tmp_path, backend="agy")

    with patch("subprocess.Popen", side_effect=_create_mock_popen()) as popen_mock, \
         patch("subprocess.run", return_value=MagicMock(returncode=0, stdout="")), \
         patch("runner.handlers._sandboxed_args_for_workdir", side_effect=lambda args, wd: args):
        res = _codergen(node, ctx)

    assert res.outcome == "success"
    assert res.metadata.get("agy_agent") == "custom-agent-v2"
    called_args = popen_mock.call_args[0][0]
    assert called_args[1:3] == ["--agent", "custom-agent-v2"]
