"""Tests for runner.cxdb hash normalisation (orch-pzqg)."""
from __future__ import annotations

from runner.cxdb import _hash, _normalise_for_hash


def test_pytest_timing_value_stripped_before_hash() -> None:
    """Two identical failures with different pytest wall times must share a hash."""
    a = "\nno tests ran in 0.10s\n\nSTDERR:\nERROR: file or directory not found\n"
    b = "\nno tests ran in 0.13s\n\nSTDERR:\nERROR: file or directory not found\n"
    assert a != b
    assert _hash(a) == _hash(b)


def test_pytest_passing_summary_timing_stripped() -> None:
    """The '21 passed in 0.10s' summary's timing must not affect hash."""
    a = ".....................                                                    [100%]\n21 passed in 0.09s\n"
    b = ".....................                                                    [100%]\n21 passed in 0.13s\n"
    assert _hash(a) == _hash(b)


def test_non_timing_difference_still_changes_hash() -> None:
    """Real differences (different error messages) must still produce different hashes."""
    a = "FAILED tests/foo.py::test_x - AssertionError: missing __version__\n"
    b = "FAILED tests/foo.py::test_y - AssertionError: wrong conversion\n"
    assert _hash(a) != _hash(b)


def test_normalise_preserves_shape() -> None:
    """The normaliser strips the time value but keeps the surrounding shape."""
    norm = _normalise_for_hash("2 passed in 0.10s\n")
    assert "passed" in norm
    assert "0.10s" not in norm
    assert "<TIME>" in norm


def test_normalise_empty_safe() -> None:
    assert _normalise_for_hash("") == ""
    assert _normalise_for_hash(None) == ""  # type: ignore[arg-type]
