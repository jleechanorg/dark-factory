"""Regression guard: every ``timeout=`` attribute in any .dot pipeline is
in a sensible range (60..1800 seconds).

Companion to the 5 family-scoped timeout-attr tests:

- ``test_gates_dot_timeouts.py`` (factory/)
- ``test_slim_pipelines_timeouts.py`` (slim/)
- ``test_airbnb_clone_pipelines_timeouts.py`` (airbnb-clone/)
- ``test_amazon_clone_pipelines_timeouts.py`` (amazon-clone/)
- ``test_remaining_pipelines_timeouts.py`` (all-nodes-coverage + attractor-spec-review + fibonacci/)

The 5 sibling tests pin the contract on a per-family basis: subprocess-spawning
nodes declare a ``timeout``, and codergen nodes are pinned to 600s. This test
adds a cross-family **value-pinning** dimension: every ``timeout=`` value
across every .dot file in the repo must be in the range ``[60, 1800]`` seconds.

The range is intentionally wide enough to accommodate every existing value
in the repo (canonical 600, fix-loop 300, smasher holdout 900, sealed-eval
180, airbnb-clone S3 900) and tight enough to catch:

- A typo: ``timeout=6`` instead of ``timeout=600`` (catches it at 6s).
- A runaway value: ``timeout=999999`` (catches it at 1801s).
- A unit mismatch: ``timeout=60`` (60s is acceptable, 60ms is not — but the
  units are seconds in every existing usage, so a 60ms value would be a
  different bug class).

The 60s lower bound is the smallest unit that survives the longest-running
public acceptance script in the benchmark. The 1800s upper bound is 3x the
canonical 600s and 2x the longest intentional 900s value — anything above
1800s is a "we have a hanging subprocess" symptom in production.

**Cross-family discovery**: this test is intentionally global (scans every
.dot file in the repo) rather than per-family. The 5 sibling tests are
file-disjoint, but the value-pinning contract is cross-cutting: a typo on
any single .dot file should fail the same test.

**File-disjoint**: this test is a new file, only reads .dot pipelines
through the existing parser and pydot. Does not touch any WIP-touched
file. The 5 sibling test files are WIP-touched, so this test cannot
share helpers with them — it implements the parser walk inline.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.parser import parse  # noqa: E402


# Range bounds. Documented in the module docstring.
_MIN_TIMEOUT_S = 60
_MAX_TIMEOUT_S = 1800

# Skip test for include-only .dot files (start with underscore) — they
# are fragment imports, not runnable pipelines. The convention is
# `pipelines/_base.dot`, `pipelines/_review_pr.dot`, etc.
_SKIP_DOT_BASENAMES = frozenset()


def _all_dot_files() -> list[pathlib.Path]:
    """Return every .dot file in the repo, excluding worktree copies."""
    out: list[pathlib.Path] = []
    for path in ROOT.rglob("*.dot"):
        # Exclude per-agent worktree copies under .claude/worktrees/.
        parts = path.parts
        if ".claude" in parts and "worktrees" in parts:
            continue
        # Exclude include-only fragment .dot files (e.g. `_base.dot`).
        if path.stem.startswith("_"):
            continue
        out.append(path)
    return sorted(out)


def _normalise_timeout(value: object) -> int | None:
    """Coerce a DOT timeout attribute to an int, or None if missing/unparseable.

    DOT allows ``timeout=600`` (int) or ``timeout="600"`` (string).
    pydot returns whatever was written, so both forms reach us.
    """
    if value is None:
        return None
    try:
        return int(value)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return None


def test_every_timeout_value_in_every_dot_file_is_in_range() -> None:
    """Every ``timeout=`` attribute in any .dot file is in [60, 1800] seconds.

    Iterates every .dot file in the repo and asserts that any node with
    a ``timeout`` attribute has a value in the canonical range. Catches
    typos (``timeout=6``), runaway values (``timeout=999999``), and
    accidental unit changes (a future maintainer writing milliseconds
    by mistake).

    Test is global rather than per-family because a typo on any single
    .dot file should fail the same test, and a single global test is
    cheaper to maintain than 7 family-scoped variants.
    """
    offenders: list[tuple[str, str, int, str]] = []
    for path in _all_dot_files():
        rel = str(path.relative_to(ROOT))
        try:
            g = parse(path)
        except Exception as exc:  # noqa: BLE001
            offenders.append((rel, "<parse>", 0, f"parse error: {exc}"))
            continue
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            if "timeout" not in node.attrs:
                continue
            actual = _normalise_timeout(node.attrs.get("timeout"))
            if actual is None:
                offenders.append(
                    (rel, name, 0, f"unparseable timeout: {node.attrs.get('timeout')!r}")
                )
                continue
            if actual < _MIN_TIMEOUT_S or actual > _MAX_TIMEOUT_S:
                offenders.append(
                    (rel, name, actual, f"out of range [{_MIN_TIMEOUT_S}, {_MAX_TIMEOUT_S}]")
                )
    assert not offenders, (
        f"every timeout= value in every .dot file must be in "
        f"[{_MIN_TIMEOUT_S}, {_MAX_TIMEOUT_S}] seconds. Offenders: {offenders}."
    )


def test_timeout_value_range_bounds_are_sane() -> None:
    """The range bounds themselves are physically sensible.

    Lower bound must be > 0 (negative or zero timeouts are nonsense).
    Upper bound must be > lower bound (an empty range would mean \"no
    timeout is acceptable,\" which is the bug class this test exists
    to prevent).
    """
    assert _MIN_TIMEOUT_S > 0, f"_MIN_TIMEOUT_S must be positive: {_MIN_TIMEOUT_S}"
    assert _MAX_TIMEOUT_S > _MIN_TIMEOUT_S, (
        f"_MAX_TIMEOUT_S ({_MAX_TIMEOUT_S}) must be greater than "
        f"_MIN_TIMEOUT_S ({_MIN_TIMEOUT_S})"
    )
    # Guard against accidental swap of the bounds (60 <-> 1800).
    assert _MIN_TIMEOUT_S < _MAX_TIMEOUT_S, (
        "_MIN_TIMEOUT_S and _MAX_TIMEOUT_S appear to be swapped"
    )
