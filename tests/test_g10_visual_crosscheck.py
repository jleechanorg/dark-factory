"""Prompt contract test: visual evidence cross-check (G10).

Verifies that reviewer and evidence-review prompts contain the
mandatory visual cross-check instruction added to close the G10 gap.

Background: PR #250 (worldai_claw) passed all gates despite three
visible UX bugs in every captured frame. The evidence_review.md and
review.md prompts checked metadata (SHA, pass rates, file existence)
but never told the LLM reviewer to open and view .png/.mp4 artifacts.
The /es skill (user-scope, line 209) already said "extract frames and
look" — it was never wired into the factory prompts.

The fix: both prompts now contain a visual cross-check step that
mandates opening and viewing representative frames when image/video
artifacts exist. This test pins that contract.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

# The two prompts that must contain the G10 visual cross-check:
_EVIDENCE_REVIEW = ROOT / "prompts" / "slim" / "evidence_review.md"
_REVIEW = ROOT / "prompts" / "slim" / "review.md"

# Required phrases that pin the contract — if any of these are removed,
# the G10 gap reopens and evidence review falls back to metadata-only.
_REQUIRED_PHRASES_EVIDENCE_REVIEW = [
    "Visual evidence cross-check",     # section header
    "must open and view",              # mandatory action
    ".png",                            # file type trigger
    ".mp4",                            # file type trigger
    "G10 anti-pattern",                # names the gap
]

_REQUIRED_PHRASES_REVIEW = [
    "Visual cross-check",              # inline instruction
    "G10 anti-pattern",                # names the gap
]


def test_evidence_review_prompt_contains_visual_crosscheck() -> None:
    """evidence_review.md must contain the G10 visual cross-check step.

    This prompt is used by the gate_er pipeline node. Without the visual
    cross-check, the reviewer validates metadata (SHA, pass rates, file
    counts) without ever opening frame artifacts.
    """
    assert _EVIDENCE_REVIEW.is_file(), (
        f"evidence_review.md not found at {_EVIDENCE_REVIEW}"
    )
    content = _EVIDENCE_REVIEW.read_text()
    missing = [p for p in _REQUIRED_PHRASES_EVIDENCE_REVIEW if p not in content]
    assert not missing, (
        f"evidence_review.md is missing G10 visual cross-check phrases: {missing}. "
        f"Without these, the reviewer falls back to metadata-only evidence "
        f"review (the G10 anti-pattern that missed 3 bugs in PR #250)."
    )


def test_review_prompt_contains_visual_crosscheck() -> None:
    """review.md must contain the G10 visual cross-check in the evidence step.

    This prompt is used by the codergen review node. Its step 3 (evidence
    quality check) must include the visual cross-check inline.
    """
    assert _REVIEW.is_file(), f"review.md not found at {_REVIEW}"
    content = _REVIEW.read_text()
    missing = [p for p in _REQUIRED_PHRASES_REVIEW if p not in content]
    assert not missing, (
        f"review.md is missing G10 visual cross-check phrases: {missing}. "
        f"Without these, the evidence quality check in step 3 validates "
        f"metadata only (the G10 anti-pattern)."
    )


def test_taxonomy_includes_g10() -> None:
    """The reviewer-gap-taxonomy.md must document G10.

    The taxonomy is the canonical reference for /factory-evolve gap
    analysis. G10 must be present so future /fe runs can detect and
    flag visual-evidence-not-inspected gaps.
    """
    taxonomy = ROOT / "docs" / "factory-evolve-research" / "reviewer-gap-taxonomy.md"
    assert taxonomy.is_file(), f"taxonomy not found at {taxonomy}"
    content = taxonomy.read_text()
    assert "G10" in content, (
        "reviewer-gap-taxonomy.md must contain G10 "
        "(visual-evidence-not-inspected). Without it, /factory-evolve "
        "cannot detect the gap category."
    )
    assert "visual-evidence-not-inspected" in content, (
        "G10 must be named 'visual-evidence-not-inspected' in the taxonomy."
    )
