"""Tests for TYPE_REGISTRY smoke checks.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.handlers import TYPE_REGISTRY as REG  # noqa: E402


def test_gate_slash_registered_in_type_registry():
    assert "gate_slash" in REG
