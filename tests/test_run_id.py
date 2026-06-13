"""Tests for :mod:`runner._run_id`."""

from __future__ import annotations

import os
import re
import time

import pytest

from runner._run_id import generate, is_valid, parse


# ---------------------------------------------------------------------------
# generate
# ---------------------------------------------------------------------------


def test_generate_has_df_prefix():
    """A fresh run id starts with the literal ``df-`` prefix."""
    rid = generate()
    assert rid.startswith("df-")


def test_generate_includes_current_pid():
    """The trailing component of a run id is the current process id."""
    rid = generate()
    parts = rid.split("-")
    assert len(parts) == 3
    assert int(parts[2]) == os.getpid()


def test_generate_uses_overridden_time():
    """``now_ns`` is honored when supplied."""
    rid = generate(now_ns=1_781_333_039_990_123_456)
    assert rid == "df-1781333039990123456-{}".format(os.getpid())


def test_generate_zero_pads_to_19_digits():
    """The timestamp component is zero-padded to 19 digits."""
    rid = generate(now_ns=1)
    ns_str = rid.split("-")[1]
    assert len(ns_str) == 19
    assert ns_str == "0000000000000000001"


def test_generate_uniqueness_under_realistic_pacing():
    """Two generate() calls separated by 1us produce different ids.

    The realistic use case is "one generate() per bin/dark-factory
    invocation" — humans type fast but at >1us intervals, CI launches
    seconds apart. A tight loop of 100 calls in <1us is not the
    realistic case; the format is documented to be unique to the
    nanosecond, and one call per microsecond is well within the
    format's collision-free window.
    """
    ids = set()
    for _ in range(100):
        ids.add(generate())
        time.sleep(1e-6)  # 1us — well above time.time_ns() tick resolution
    assert len(ids) == 100


def test_generate_format_is_greppable_and_sortable():
    """The id is sortable by time-of-creation when grouped by pid."""
    a = generate(now_ns=1_000_000_000_000_000_000)
    b = generate(now_ns=2_000_000_000_000_000_000)
    assert a < b  # lexicographic == chronological for the timestamp part


# ---------------------------------------------------------------------------
# is_valid
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "rid",
    [
        "df-1781333039990123456-37132",
        "df-0000000000000000001-1",
        "df-9999999999999999999-999999",
    ],
)
def test_is_valid_accepts_well_formed_ids(rid):
    assert is_valid(rid) is True


@pytest.mark.parametrize(
    "rid",
    [
        "",  # empty
        "df",  # missing components
        "df-123-456",  # timestamp too short
        "df-1781333039990123456-",  # missing pid
        "df-1781333039990123456-37132-extra",  # too many components
        "DF-1781333039990123456-37132",  # wrong prefix case
        "x-1781333039990123456-37132",  # wrong prefix
        "df-not-a-number-37132",  # non-numeric timestamp
        "df-1781333039990123456-abc",  # non-numeric pid
        None,  # not a string
    ],
)
def test_is_valid_rejects_malformed_ids(rid):
    assert is_valid(rid) is False


# ---------------------------------------------------------------------------
# parse
# ---------------------------------------------------------------------------


def test_parse_round_trip():
    """A generated id parses back to its (timestamp, pid) pair."""
    rid = generate(now_ns=1_781_333_039_990_123_456)
    parsed = parse(rid)
    assert parsed == (1_781_333_039_990_123_456, os.getpid())


def test_parse_returns_none_for_malformed():
    assert parse("not-a-run-id") is None
    assert parse("") is None
    assert parse("df-123-456") is None
    assert parse(None) is None  # type: ignore[arg-type]


# ---------------------------------------------------------------------------
# Format invariants (one place to change if the format ever evolves)
# ---------------------------------------------------------------------------


def test_format_invariant_length_is_stable():
    """The timestamp component is always 19 digits — never 18, never 20.

    This is a load-bearing invariant for log readers that split on ``-``
    and expect the middle field to be a fixed width. If this test
    ever fails, the format has changed and downstream tooling (CXDB
    replay, log indexers) must be updated in lockstep.
    """
    rid = generate()
    ns_str = rid.split("-")[1]
    assert len(ns_str) == 19
    assert re.fullmatch(r"\d{19}", ns_str)


def test_format_invariant_time_ns_is_current():
    """The default ``time.time_ns()`` is used when ``now_ns`` is omitted."""
    before = time.time_ns()
    rid = generate()
    after = time.time_ns()
    parsed = parse(rid)
    assert parsed is not None
    ns, _ = parsed
    assert before <= ns <= after
