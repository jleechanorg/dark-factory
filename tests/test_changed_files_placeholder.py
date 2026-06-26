"""Test suite verifying changed_files placeholder resolution (G5)."""

from __future__ import annotations

import pathlib
import sys
import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from conftest import make_node  # noqa: E402
from runner.handlers import Context  # noqa: E402
from runner.handler_render import _substitute_placeholders  # noqa: E402

def test_changed_files_placeholder_substitution(tmp_path):
    ctx = Context(goal="test", workdir=tmp_path, backend="echo")
    
    # 1. When '_last_changed_files' is missing, it should default to '(none)'
    text = "Changed files:\n${changed_files}"
    rendered = _substitute_placeholders(text, ctx)
    assert "(none)" in rendered
    
    # 2. When '_last_changed_files' is present, it should substitute it
    ctx.state["_last_changed_files"] = "- file_a.py\n- file_b.py"
    rendered2 = _substitute_placeholders(text, ctx)
    assert "- file_a.py" in rendered2
    assert "- file_b.py" in rendered2
    assert "(none)" not in rendered2
