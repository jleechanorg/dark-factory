"""Tests for :mod:`runner._slug`."""

from __future__ import annotations

import pytest

from runner._slug import safe_slug


# ---------------------------------------------------------------------------
# Replacement of unsafe characters
# ---------------------------------------------------------------------------


def test_safe_slug_replaces_slashes():
    """Slashes are collapsed to a single underscore."""
    assert safe_slug("feat/my-feature") == "feat_my-feature"


def test_safe_slug_replaces_spaces():
    """Spaces are collapsed to a single underscore."""
    assert safe_slug("hello world") == "hello_world"


def test_safe_slug_replaces_at_sign():
    """``@`` (and any other non-safe char) becomes a single underscore."""
    assert safe_slug("user@host") == "user_host"


def test_safe_slug_replaces_colons():
    """Colons (common in refs like ``main:path``) become underscores."""
    assert safe_slug("main:src/foo.py") == "main_src_foo.py"


# ---------------------------------------------------------------------------
# Preservation of safe characters
# ---------------------------------------------------------------------------


def test_safe_slug_preserves_safe_characters():
    """Dots, dashes, and underscores in the input pass through unchanged."""
    assert safe_slug("v1.2.3-rc_4") == "v1.2.3-rc_4"


def test_safe_slug_preserves_alphanumeric():
    """Plain alphanumeric input is returned verbatim."""
    assert safe_slug("main") == "main"
    assert safe_slug("dark-factory") == "dark-factory"


# ---------------------------------------------------------------------------
# Run collapsing
# ---------------------------------------------------------------------------


def test_safe_slug_collapses_runs_of_unsafe_to_one_underscore():
    """A run of N unsafe characters becomes exactly one underscore, not N."""
    assert safe_slug("a///b") == "a_b"
    assert safe_slug("a   b") == "a_b"
    assert safe_slug("a@!#$%b") == "a_b"


def test_safe_slug_strips_leading_and_trailing_unsafe():
    """Leading and trailing unsafe runs collapse to single underscores."""
    assert safe_slug("///hello///") == "_hello_"


# ---------------------------------------------------------------------------
# Truncation
# ---------------------------------------------------------------------------


def test_safe_slug_truncates_long_input_to_64_chars():
    """Inputs longer than 64 chars are truncated, not raised."""
    long_name = "a" * 200
    out = safe_slug(long_name)
    assert len(out) == 64
    assert out == "a" * 64


def test_safe_slug_truncates_after_collapsing():
    """Truncation happens AFTER regex substitution, not before."""
    # 60 'a's, then 20 unsafe chars, then 20 'b's — total input 100 chars
    # After regex: "a" * 60 + "_" + "b" * 20 = 81 chars, truncated to 64
    name = "a" * 60 + "/" * 20 + "b" * 20
    out = safe_slug(name)
    assert len(out) == 64
    assert out == ("a" * 60 + "_" + "b" * 3)  # 60 + 1 + 3 = 64


# ---------------------------------------------------------------------------
# Fallback
# ---------------------------------------------------------------------------


def test_safe_slug_uses_default_fallback_for_empty_input():
    """Empty string returns the default fallback ``"unknown"``."""
    assert safe_slug("") == "unknown"


def test_safe_slug_uses_explicit_fallback_for_empty_input():
    """Empty string with explicit ``fallback=...`` returns the supplied value."""
    assert safe_slug("", fallback="node") == "node"
    assert safe_slug("", fallback="custom") == "custom"


def test_safe_slug_fallback_does_not_fire_for_all_unsafe_input():
    """Subtle pre-existing behaviour: an all-unsafe input collapses to ``"_"``,
    which is truthy, so the fallback does NOT fire — the single ``"_"`` is
    returned as-is. This is the behaviour both original implementations
    shared; tests pin it down so a future refactor cannot silently change it.
    """
    assert safe_slug("///") == "_"
    assert safe_slug("@#$%") == "_"


def test_safe_slug_fallback_keyword_only():
    """``fallback`` is keyword-only; passing it positionally raises TypeError."""
    with pytest.raises(TypeError):
        safe_slug("foo", "bar")  # type: ignore[misc]


# ---------------------------------------------------------------------------
# Parametrized table of representative inputs
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "raw,expected",
    [
        ("main", "main"),
        ("feat/my-feature", "feat_my-feature"),
        ("v1.2.3", "v1.2.3"),
        ("user@host", "user_host"),
        ("foo bar baz", "foo_bar_baz"),
        ("///", "_"),
        ("", "unknown"),
        ("dark-factory", "dark-factory"),
        ("fix/issue-42", "fix_issue-42"),
        ("hotfix.release", "hotfix.release"),
    ],
)
def test_safe_slug_parametrized(raw, expected):
    assert safe_slug(raw) == expected


# ---------------------------------------------------------------------------
# Format invariant (one place to change if the format ever evolves)
# ---------------------------------------------------------------------------


def test_format_invariant_max_length_is_64():
    """The slug is always at most 64 characters — never 63, never 65.

    This is a load-bearing invariant for downstream tooling that
    splits on the slug boundary in log indexers. If this test ever
    fails, the format has changed and the change must be coordinated
    with the perf-log and evidence-bundle consumers.
    """
    out = safe_slug("a" * 10_000)
    assert 0 < len(out) <= 64
