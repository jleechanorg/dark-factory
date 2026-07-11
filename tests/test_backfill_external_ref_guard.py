"""jleechan-mdgr regression test.

Reproduces the 2026-07-11T00:05:15Z escalation corruption: bead jleechan-8dyu
ended up with `external_ref = "jleechanorg/worldarchitect.ai#7888#local-8dyu"`
-- a real `<repo>#<pr>` ref with a `#local-<bead-id>` disambiguation suffix
(see `daemon/scripts/backfill_external_ref.py`'s convention) appended ON TOP
of it, producing two `#` characters instead of one. Escalation comment
posting (`daemon/src/adapters.rs::comment_external`) only accepts exactly one
`#`, so every escalation for that bead failed with
"parse: invalid external_ref format for comment: ...".

The corrupted value traced back to `daemon/scripts/backfill_external_ref.py`'s
`BACKFILL_MAP`, which mixed two disambiguation-base conventions: the
`#`-free full GitHub PR URL (used by every OTHER duplicate-target entry) and
the SHORT canonical `owner/repo#N` form (which already contains a `#`) for
jleechan-8dyu specifically. This test guards against that class of mistake
recurring in this exact map, at data-authoring time, before any `br update`
call runs.
"""
from __future__ import annotations

import importlib.util
import pathlib

import pytest

_MODULE_PATH = (
    pathlib.Path(__file__).resolve().parent.parent
    / "daemon"
    / "scripts"
    / "backfill_external_ref.py"
)
_spec = importlib.util.spec_from_file_location("backfill_external_ref", _MODULE_PATH)
assert _spec is not None and _spec.loader is not None
backfill_external_ref = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(backfill_external_ref)


def test_current_backfill_map_has_no_double_hash_entries():
    """The live BACKFILL_MAP must never regress into the jleechan-8dyu shape."""
    # Must not raise.
    backfill_external_ref._assert_no_double_hash_ambiguity(backfill_external_ref.BACKFILL_MAP)


def test_guard_rejects_double_hash_disambiguation_onto_canonical_ref():
    """Reproduces the exact corrupted shape and confirms the guard catches it."""
    bad_map = {
        "jleechan-8dyu": (
            "jleechanorg/worldarchitect.ai#7888#local-8dyu",
            "reproduction of the 2026-07-11T00:05:15Z incident",
        )
    }
    with pytest.raises(ValueError, match="double-suffix corruption risk"):
        backfill_external_ref._assert_no_double_hash_ambiguity(bad_map)


def test_guard_accepts_full_url_plus_local_suffix_convention():
    """The `#`-free full-URL base + `#local-<id>` suffix convention (used by
    every OTHER duplicate-target entry in BACKFILL_MAP) must remain accepted
    -- this test is NOT about jleechan-twa0's separate "parser only accepts
    one format" bug, only about this guard not over-rejecting the convention
    the script already relies on elsewhere.
    """
    good_map = {
        "jleechan-4dgx": (
            "https://github.com/jleechanorg/worldarchitect.ai/pull/8116#local-4dgx",
            "single '#' -- disambiguation-safe",
        )
    }
    # Must not raise.
    backfill_external_ref._assert_no_double_hash_ambiguity(good_map)
