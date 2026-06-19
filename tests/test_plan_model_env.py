"""Tests for ``DARK_FACTORY_PLAN_MODEL`` env-var override in the model stylesheet.

Bead: jleechan-x57 (P3 feature) — honor ``DARK_FACTORY_PLAN_MODEL`` env var in
``pipelines/slim/minimal_feature.model.css`` instead of hard-coded
``claude-opus-4-6``. Default behavior is preserved when the env var is unset.

Implementation path: parser-side substitution. The CSS file uses
``${DARK_FACTORY_PLAN_MODEL:-claude-opus-4-6}`` and
``runner/parser.py:_substitute_env_in_value`` resolves ``${VAR}`` and
``${VAR:-default}`` references against ``os.environ`` at parse time.

Why parser-side: pydot's CSS handling is rudimentary (it doesn't read CSS at
all in this codebase — the CSS-like stylesheet is parsed by our own tiny
``_parse_model_style_rules``). Lifting substitution into the parser keeps the
mechanism general, stdlib-only, and testable without touching the CSS file
unnecessarily.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.parser import (  # noqa: E402
    _substitute_env_in_value,
    parse,
)

CSS_PATH = ROOT / "pipelines" / "slim" / "minimal_feature.model.css"


def test_default_model_when_env_unset(monkeypatch):
    """Without ``DARK_FACTORY_PLAN_MODEL`` set, plan resolves to ``claude-opus-4-6``."""
    monkeypatch.delenv("DARK_FACTORY_PLAN_MODEL", raising=False)
    raw = CSS_PATH.read_text(encoding="utf-8")
    # Sanity: the CSS file's default value matches the legacy hard-coded value.
    assert "claude-opus-4-6" in raw

    g = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    plan = g.nodes["plan"]
    assert plan.attrs["model_name"] == "claude-opus-4-6"
    assert plan.attrs["backend"] == "claude"


def test_env_var_overrides_default(monkeypatch):
    """``DARK_FACTORY_PLAN_MODEL=claude-sonnet-4-6`` swaps the plan model."""
    monkeypatch.setenv("DARK_FACTORY_PLAN_MODEL", "claude-sonnet-4-6")
    g = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    plan = g.nodes["plan"]
    assert plan.attrs["model_name"] == "claude-sonnet-4-6"
    assert plan.attrs["backend"] == "claude"


def test_env_var_does_not_leak_into_review(monkeypatch):
    """``.review`` rule has no env-var reference; backend is unchanged."""
    monkeypatch.setenv("DARK_FACTORY_PLAN_MODEL", "claude-sonnet-4-6")
    g = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    # review is added via include from _base.dot.
    review = g.nodes["review"]
    assert review.attrs["backend"] == "agy"
    # No model_name should be set by the stylesheet on review (the .review
    # rule doesn't define one).
    assert "model_name" not in review.attrs or review.attrs["model_name"] != "claude-sonnet-4-6"


def test_env_var_cleared_restores_default(monkeypatch):
    """Setting then clearing the env var restores the hard-coded default."""
    monkeypatch.setenv("DARK_FACTORY_PLAN_MODEL", "claude-sonnet-4-6")
    g1 = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    assert g1.nodes["plan"].attrs["model_name"] == "claude-sonnet-4-6"

    monkeypatch.delenv("DARK_FACTORY_PLAN_MODEL")
    g2 = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    assert g2.nodes["plan"].attrs["model_name"] == "claude-opus-4-6"


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
    monkeypatch.setenv("DARK_FACTORY_PLAN_MODEL", "claude-sonnet-4-6")
    assert _substitute_env_in_value("claude-opus-4-6") == "claude-opus-4-6"
    assert _substitute_env_in_value("agy") == "agy"


def test_substitute_env_does_not_match_invalid_names(monkeypatch):
    """``${1foo}`` (starts with digit) is NOT substituted."""
    monkeypatch.setenv("DARK_FACTORY_PLAN_MODEL", "should-not-leak")
    # The regex requires the name to start with a letter or underscore.
    assert _substitute_env_in_value("${1FOO}") == "${1FOO}"


@pytest.mark.parametrize("model_id", [
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
    "gpt-5",
    "gemini-2.5-pro",
])
def test_env_var_overrides_with_various_models(monkeypatch, model_id):
    """Substitution is purely textual — any value passes through."""
    monkeypatch.setenv("DARK_FACTORY_PLAN_MODEL", model_id)
    g = parse(ROOT / "pipelines" / "slim" / "minimal_feature.dot")
    assert g.nodes["plan"].attrs["model_name"] == model_id
