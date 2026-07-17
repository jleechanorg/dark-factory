"""Regression tests for state dict substitution (fixes 1 & 2).

Tests that _substitute_placeholders properly handles non-str ctx.state values
(dicts, lists, ints) and that _resolve_gate_backend stores its metadata
as a JSON string for cross-visit compatibility.
"""

from __future__ import annotations

import json
import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from conftest import make_node  # noqa: E402
from runner.handlers import Context  # noqa: E402
from runner.handler_render import _substitute_placeholders  # noqa: E402
from runner.handler_dispatch import _resolve_gate_backend  # noqa: E402
from runner.parser import Node  # noqa: E402


class TestSubstitutePlaceholdersWithNonStrValues:
    """Tests for fix 1: _substitute_placeholders must coerce non-str values."""

    def test_dict_value_renders_as_json(self):
        """When ctx.state contains a dict, substitution should render as JSON."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["review_main.resolved_backend_meta"] = {
            "reviewer_backend_resolution": "priority_queue",
            "adversarial_resolved": "codex",
        }

        text = "Backend: ${state.review_main.resolved_backend_meta}"
        result = _substitute_placeholders(text, ctx)

        # Should render as sorted JSON
        assert "reviewer_backend_resolution" in result
        assert "priority_queue" in result
        assert "codex" in result

    def test_dict_value_not_in_text_does_not_crash(self):
        """When ctx.state has a dict but text doesn't reference it, no crash."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["review_main.resolved_backend_meta"] = {"key": "value"}
        ctx.state["some_other"] = "present"

        # Text doesn't reference the dict key
        text = "Other: ${state.some_other}"
        result = _substitute_placeholders(text, ctx)

        assert "Other: present" == result

    def test_list_value_renders_without_error(self):
        """Lists should render as JSON without crashing."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["files_changed"] = ["a.py", "b.py", "c.py"]

        text = "Changed: ${state.files_changed}"
        result = _substitute_placeholders(text, ctx)

        assert "a.py" in result
        assert "b.py" in result

    def test_int_value_renders_without_error(self):
        """Integer values should render as string without crashing."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["attempt_number"] = 3

        text = "Attempt #${state.attempt_number}"
        result = _substitute_placeholders(text, ctx)

        assert "Attempt #3" == result


class TestResolveGateBackendMetaJSONString:
    """Tests for fix 2: _resolve_gate_backend stores meta as JSON string."""

    def test_resolved_backend_meta_stored_as_json_string(self, monkeypatch):
        """ctx.state[<node>.resolved_backend_meta] must be a JSON string, not dict."""
        # Mock backend probing to return deterministic result
        monkeypatch.setattr(
            "runner.handlers._probe_backend_installed",
            lambda name: name == "codex",
        )

        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")
        node = Node(
            name="review_main",
            attrs={"backend_priority": "codex,minimax"},
        )

        resolved, meta = _resolve_gate_backend(node, ctx)

        # The returned meta should be a real dict
        assert isinstance(meta, dict)
        assert meta.get("reviewer_backend_resolution") == "priority_queue"

        # But ctx.state should store it as a JSON STRING
        stored_meta = ctx.state.get("review_main.resolved_backend_meta")
        assert stored_meta is not None, "resolved_backend_meta should be in ctx.state"
        assert isinstance(stored_meta, str), (
            f"ctx.state should store meta as JSON string, got {type(stored_meta).__name__}"
        )

        # Verify it's valid JSON that decodes to the expected dict
        decoded = json.loads(stored_meta)
        assert decoded["reviewer_backend_resolution"] == "priority_queue"

    def test_cross_visit_read_back_tolerates_json_string(self, monkeypatch):
        """Second call to _resolve_gate_backend should read back from JSON string."""
        monkeypatch.setattr(
            "runner.handlers._probe_backend_installed",
            lambda name: name == "codex",
        )

        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")

        # First call - sets up the pinned backend
        node1 = Node(
            name="review_main",
            attrs={"backend_priority": "codex,minimax,agy,claude-sonnet"},
        )
        resolved1, meta1 = _resolve_gate_backend(node1, ctx)
        assert resolved1 == "codex"

        # ctx.state should have the JSON string
        stored = ctx.state.get("review_main.resolved_backend_meta")
        assert isinstance(stored, str)

        # Second call - should read back the pinned backend
        node2 = Node(
            name="review_main",
            attrs={"backend_priority": "codex,minimax,agy,claude-sonnet"},
        )
        resolved2, meta2 = _resolve_gate_backend(node2, ctx)

        # Should return the same pinned backend
        assert resolved2 == resolved1 == "codex"

    def test_cross_visit_tolerates_legacy_dict(self, monkeypatch):
        """Read-back should tolerate legacy dict (not just JSON string)."""
        monkeypatch.setattr(
            "runner.handlers._probe_backend_installed",
            lambda name: name == "minimax",
        )

        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")

        # Seed with legacy dict (pre-fix behavior)
        ctx.state["review_main.resolved_backend_meta"] = {
            "reviewer_backend_resolution": "priority_queue",
            "adversarial_resolved": "minimax",
        }

        # Call should tolerate the dict and return pinned backend
        node = Node(
            name="review_main",
            attrs={"backend_priority": "codex,minimax"},
        )
        resolved, meta = _resolve_gate_backend(node, ctx)

        # Should return the pinned backend from legacy dict
        assert resolved == "minimax"

    def test_cross_visit_tolerates_malformed_value(self, monkeypatch):
        """Read-back should tolerate malformed/empty value gracefully."""
        monkeypatch.setattr(
            "runner.handlers._probe_backend_installed",
            lambda name: name == "agy",
        )

        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")

        # Seed with malformed values
        ctx.state["review_main.resolved_backend_meta"] = None

        node = Node(
            name="review_main",
            attrs={"backend_priority": "agy,claude-sonnet"},
        )
        resolved, meta = _resolve_gate_backend(node, ctx)

        # Should fall through to probe and return agy
        assert resolved == "agy"

        # Now test with invalid JSON string
        ctx.state["review_main.resolved_backend_meta"] = "not valid json {"
        resolved2, meta2 = _resolve_gate_backend(node, ctx)

        # Should tolerate and re-resolve
        assert resolved2 == "agy"

    def test_substitute_placeholders_with_backend_meta_json(self):
        """Integration: _substitute_placeholders should handle the JSON string meta."""
        ctx = Context(goal="test", workdir=pathlib.Path("/tmp"), backend="echo")

        # Simulate what _resolve_gate_backend stores
        pq_meta = {
            "reviewer_backend_resolution": "priority_queue",
            "adversarial_resolved": "codex",
            "adversarial_priority": "codex,minimax,agy,claude-sonnet",
        }
        ctx.state["review_main.resolved_backend_meta"] = json.dumps(pq_meta, sort_keys=True)

        # This should NOT raise TypeError (the original bug)
        text = "Backend meta: ${state.review_main.resolved_backend_meta}"
        result = _substitute_placeholders(text, ctx)

        # Should render as JSON
        assert "priority_queue" in result
        assert "codex" in result
