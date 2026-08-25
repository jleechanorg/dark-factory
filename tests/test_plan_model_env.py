"""Tests for the pinned MiniMax plan model in the model stylesheet.

The production stylesheet pins the plan tier to ``MiniMax-M3``.  Ambient
``DARK_FACTORY_MINIMAX_MODEL`` state must not silently change that policy.
The parser's generic environment-substitution helper remains covered below
because other prompt/style inputs may use it.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.parser import (  # noqa: E402
    _substitute_env_in_value,
    parse,
)

CSS_PATH = ROOT / "pipelines" / "slim" / "minimal_feature.model.css"


def test_plan_model_is_pinned_even_with_ambient_override(monkeypatch):
    """The plan stays on MiniMax-M3 regardless of ambient model state."""
    monkeypatch.setenv("DARK_FACTORY_MINIMAX_MODEL", "MiniMax-M2")
    raw = CSS_PATH.read_text(encoding="utf-8")
    assert "model_name: MiniMax-M3;" in raw
    assert "DARK_FACTORY_MINIMAX_MODEL" not in raw

    g = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    plan = g.nodes["plan"]
    assert plan.attrs["model_name"] == "MiniMax-M3"
    assert plan.attrs["backend"] == "minimax"


def test_env_var_does_not_leak_into_review(monkeypatch):
    """The review rule remains independent of the pinned plan model."""
    monkeypatch.setenv("DARK_FACTORY_MINIMAX_MODEL", "MiniMax-M2")
    g = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    # review is added via include from _base.dot.
    review = g.nodes["review"]
    assert review.attrs["backend"] == "agy"
    # No model_name should be set by the stylesheet on review (the .review
    # rule doesn't define one).
    assert "model_name" not in review.attrs or review.attrs["model_name"] != "MiniMax-M2"


def test_substitute_env_plain_reference(monkeypatch):
    """``${VAR}`` resolves to env value; unset resolves to empty string."""
    monkeypatch.setenv("MY_MODEL", "gpt-test")
    assert _substitute_env_in_value("${MY_MODEL}") == "gpt-test"

    monkeypatch.delenv("MY_MODEL", raising=False)
    assert _substitute_env_in_value("${MY_MODEL}") == ""


def test_substitute_env_with_default(monkeypatch):
    """``${VAR:-default}`` uses default when VAR is unset or empty."""
    # Set + empty value: bash treats empty as unset; our impl follows that.
    monkeypatch.setenv("MY_MODEL", "")
    assert _substitute_env_in_value("${MY_MODEL:-fallback}") == "fallback"

    monkeypatch.delenv("MY_MODEL", raising=False)
    assert _substitute_env_in_value("${MY_MODEL:-fallback}") == "fallback"

    monkeypatch.setenv("MY_MODEL", "explicit")
    assert _substitute_env_in_value("${MY_MODEL:-fallback}") == "explicit"


def test_substitute_env_no_reference_passthrough(monkeypatch):
    """Plain values without ``${`` are returned unchanged."""
    monkeypatch.setenv("DARK_FACTORY_MINIMAX_MODEL", "MiniMax-M2")
    assert _substitute_env_in_value("MiniMax-M3") == "MiniMax-M3"
    assert _substitute_env_in_value("agy") == "agy"


def test_substitute_env_does_not_match_invalid_names(monkeypatch):
    """``${1foo}`` (starts with digit) is NOT substituted."""
    monkeypatch.setenv("DARK_FACTORY_MINIMAX_MODEL", "should-not-leak")
    # The regex requires the name to start with a letter or underscore.
    assert _substitute_env_in_value("${1FOO}") == "${1FOO}"
