import pathlib
import pytest
from runner.engine_run import _extract_coder_handoff
from runner.handlers import Context, Result, Node
from runner.handler_core import _exit

def test_extract_coder_handoff():
    # Test case 1: normal extraction
    text1 = """
Some review text here.
## Coder Handoff
- Summary: fixed bug.
- Required fix: update code.
- Verification to rerun: pytest.

## Next Section
Other stuff.
"""
    handoff1 = _extract_coder_handoff(text1)
    assert "- Summary: fixed bug." in handoff1
    assert "Other stuff" not in handoff1

    # Test case 2: no section
    text2 = "verdict: pass"
    assert _extract_coder_handoff(text2) == ""

    # Test case 3: case insensitivity
    text3 = "## coder handoff\nhello world"
    assert _extract_coder_handoff(text3) == "hello world"


def test_exit_node_sha_pinning(tmp_path, monkeypatch):
    # Mock _worktree_head_sha to return a consistent SHA
    monkeypatch.setattr("runner.handler_verdict._worktree_head_sha", lambda wd: "abcd1234abcd1234abcd1234abcd1234abcd1234")

    # Node & Context setup
    node = Node(name="exit", attrs={"type": "exit"})
    ctx = Context(goal="test exit SHA", workdir=tmp_path)
    
    # 1. No last validated head SHA -> success
    res = _exit(node, ctx)
    assert res.outcome == "success"

    # 2. Matching last validated SHA -> success
    ctx.state["_last_validated_head_sha"] = "abcd1234abcd1234abcd1234abcd1234abcd1234"
    res = _exit(node, ctx)
    assert res.outcome == "success"

    # 3. Mismatched last validated SHA -> error
    ctx.state["_last_validated_head_sha"] = "differentdifferentdifferentdifferentdifferent"
    res = _exit(node, ctx)
    assert res.outcome == "error"
    assert "exit blocked: HEAD SHA changed" in res.output
    assert res.metadata["exit_sha_status"] == "mismatched"
