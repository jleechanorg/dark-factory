"""Tests for _branch_context read-only passthrough.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.engine import _branch_context  # noqa: E402
from runner.handlers import Context as HCtx  # noqa: E402


def test_branch_context_keeps_parent_workdir_for_readonly_gates(tmp_path):
    """Parallel branch isolation must NOT apply to read-only reviewer gates:
    they need the real repo (SHA binding, .claude/commands/, the diff).
    File-writing branch types still get tempdir isolation."""
    ctx = HCtx(goal="t", workdir=tmp_path, backend="claude")

    for gate_type in ("gate_slash", "gate_es", "gate_er", "gate_code_standards", "holdout_eval"):
        cloned = _branch_context(ctx, "lane", gate_type)
        assert pathlib.Path(cloned.workdir) == tmp_path, (
            f"{gate_type} branch must keep the parent workdir"
        )

    isolated = _branch_context(ctx, "impl", "codergen")
    assert pathlib.Path(isolated.workdir) != tmp_path
    assert str(isolated.workdir).startswith(str(tmp_path)), (
        "codergen branch keeps tempdir isolation under the parent"
    )
