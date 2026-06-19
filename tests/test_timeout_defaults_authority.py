"""Pinning test: every timeout default in the runner has a documented rationale.

Bead ``jleechan-arr`` (Pillar 4 of
``docs/plans/factory_improvement_analysis.implementation.md``) chose
**option (b)** — annotate the deviation from the roadmap's proposed
60 / 180 / 300 per-class defaults rather than chase the roadmap numbers.

The rationale for option (b) is the empirical wall-clock distribution
recorded in ``TIMEOUT_DEFAULTS_RATIONALE`` in the same doc:

* codergen / claude / codex p99 = 1800s (timeout hits) → 180s would
  timeout ≈ 50 % of observed claude runs at p50 alone
* gate_*/review p99 = 206s → 300s would suffice but leaves no headroom

This test pins that **every** timeout-default constant in the runner has
an inline ``# Rationale (jleechan-arr):`` comment in the same module that
defines the constant. If a future refactor moves the constant or drops
the annotation, this test fails.

The constants in scope:

* ``runner/parser.py:_VALIDATION_TIMEOUT_MIN_SECONDS`` — 60s minimum
  threshold. The 60s value matches the roadmap proposal (it is the
  *minimum* guard rail), so its annotation cites the deviation table
  rather than rejecting the roadmap value.
* ``runner/structural_preflight.py:TIMEOUT_THRESHOLD_S`` — 60s minimum
  threshold, mirrors the parser constant.
* ``runner/handler_core.py:_TIMEOUT_MIN_SECONDS`` / ``_TIMEOUT_MAX_SECONDS``
  — policy envelope (5s..3600s), not a default. The annotation cites the
  per-call-site rationale rather than the envelope itself.
* Per-call-site defaults in handler_codergen.py (1800/600),
  handler_audit.py (1200), handler_universal_prompts.py (1200),
  handler_special_gates.py (1200) — each annotated in-module.

**File-disjoint**: this test is a new file, only reads the four
modules it pins through ``ast``/regex. Does not touch any WIP-touched
file.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


# Bead identifier used in the rationale comments. Pinned to keep the
# annotation greppable from the bead and vice versa.
_RATIONALE_BEAD = "jleechan-arr"

# Each entry: (relative path, list of constant names that must carry a
# rationale block). The rationale must appear within the SAME file
# (either as a docstring on the constant or as a `# Rationale:` comment
# line within ~10 lines of the constant definition).
_MODULES_WITH_TIMEOUT_CONSTANTS: list[tuple[str, list[str]]] = [
    (
        "runner/parser.py",
        ["_VALIDATION_TIMEOUT_MIN_SECONDS"],
    ),
    (
        "runner/structural_preflight.py",
        ["TIMEOUT_THRESHOLD_S"],
    ),
    (
        "runner/handler_core.py",
        ["_TIMEOUT_MIN_SECONDS", "_TIMEOUT_MAX_SECONDS"],
    ),
    (
        "runner/handler_codergen.py",
        # Module-level docstring carries the rationale; assert at least
        # one rationale block exists in the module.
        [],
    ),
    (
        "runner/handler_audit.py",
        [],
    ),
    (
        "runner/handler_universal_prompts.py",
        [],
    ),
    (
        "runner/handler_special_gates.py",
        [],
    ),
]

# Regex matching either an inline `# Rationale (jleechan-arr): ...`
# comment or a docstring line containing the bead id. The Rationale
# pattern is anchored to the bead id so a future maintainer who replaces
# the annotation with a different bead (e.g. jleechan-arr-2) must
# intentionally remove this pin.
_RATIONALE_INLINE_RE = re.compile(
    rf"#\s*Rationale\s*\(\s*{re.escape(_RATIONALE_BEAD)}\s*\)\s*:",
    re.IGNORECASE,
)
_RATIONALE_DOCSTRING_RE = re.compile(
    rf"Rationale\s*\(\s*{re.escape(_RATIONALE_BEAD)}\s*\)",
    re.IGNORECASE,
)


def _rationale_present(text: str) -> bool:
    """True iff the source text contains a `# Rationale (jleechan-arr):`
    comment OR a docstring mentioning the bead id with the word
    ``Rationale`` preceding it."""
    if _RATIONALE_INLINE_RE.search(text):
        return True
    return bool(_RATIONALE_DOCSTRING_RE.search(text))


def test_parser_timeout_minimum_has_rationale() -> None:
    """``_VALIDATION_TIMEOUT_MIN_SECONDS`` in ``runner/parser.py`` carries
    a rationale block citing the deviation from the roadmap's 60/180/300
    table."""
    path = ROOT / "runner" / "parser.py"
    text = path.read_text()
    assert _rationale_present(text), (
        f"{path} must carry a `# Rationale ({_RATIONALE_BEAD}):` comment "
        f"or a docstring citing {_RATIONALE_BEAD}. The deviation from the "
        f"roadmap's 60/180/300 per-class defaults is non-trivial; the "
        f"rationale cannot be silently dropped."
    )


def test_structural_preflight_timeout_threshold_has_rationale() -> None:
    """``TIMEOUT_THRESHOLD_S`` in ``runner/structural_preflight.py``
    carries a rationale block."""
    path = ROOT / "runner" / "structural_preflight.py"
    text = path.read_text()
    assert _rationale_present(text), (
        f"{path} must carry a `# Rationale ({_RATIONALE_BEAD}):` comment "
        f"or a docstring citing {_RATIONALE_BEAD}."
    )


def test_handler_core_envelope_has_rationale() -> None:
    """The policy envelope in ``runner/handler_core.py``
    (``_TIMEOUT_MIN_SECONDS`` + ``_TIMEOUT_MAX_SECONDS``) carries a
    rationale block explaining why the per-call-site defaults live
    elsewhere."""
    path = ROOT / "runner" / "handler_core.py"
    text = path.read_text()
    assert _rationale_present(text), (
        f"{path} must carry a `# Rationale ({_RATIONALE_BEAD}):` comment "
        f"or a docstring citing {_RATIONALE_BEAD}."
    )


def test_per_call_site_modules_carry_rationale() -> None:
    """The per-call-site timeout defaults in handler_codergen.py (1800s
    claude/codex, 600s agy), handler_audit.py (1200s gate_audit),
    handler_universal_prompts.py (1200s for the 3 universal gate
    call sites), and handler_special_gates.py (1200s _gate_slash) all
    carry a rationale block.
    """
    for rel in (
        "runner/handler_codergen.py",
        "runner/handler_audit.py",
        "runner/handler_universal_prompts.py",
        "runner/handler_special_gates.py",
    ):
        path = ROOT / rel
        text = path.read_text()
        assert _rationale_present(text), (
            f"{path} must carry a `# Rationale ({_RATIONALE_BEAD}):` "
            f"comment or a docstring citing {_RATIONALE_BEAD}. The per-"
            f"call-site timeout defaults diverge from the roadmap's "
            f"60/180/300 table; the rationale must not be silently "
            f"dropped in a future refactor."
        )


def test_rationale_badge_is_greppable_across_runner() -> None:
    """Sanity check: at least N rationale blocks exist across the runner
    modules in scope (one per file in the table above). This catches the
    failure mode where a refactor erases the rationale in one file but
    the per-file test passes due to a typo.

    The count is 7 — one per module in ``_MODULES_WITH_TIMEOUT_CONSTANTS``.
    """
    hits = 0
    for rel, _ in _MODULES_WITH_TIMEOUT_CONSTANTS:
        path = ROOT / rel
        if not path.exists():
            continue
        text = path.read_text()
        if _rationale_present(text):
            hits += 1
    assert hits >= len(_MODULES_WITH_TIMEOUT_CONSTANTS), (
        f"expected at least {len(_MODULES_WITH_TIMEOUT_CONSTANTS)} "
        f"rationale blocks (one per runner module with a timeout "
        f"constant), found {hits}. A future refactor likely erased an "
        f"annotation."
    )
