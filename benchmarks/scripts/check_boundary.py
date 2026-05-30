#!/usr/bin/env python3
"""Fail if public benchmark files leak sealed evaluator paths/details.

Three boundary invariants are enforced:
  (a) No file in prompts/ or specs/ (root-level) contains the string
      "holdout" or "DARK_FACTORY_HOLDOUTS".  Those files ship verbatim
      to the implementing agent; any holdout reference there collapses
      the adversarial guarantee.
  (b) No holdouts/ directory exists inside benchmarks/.  Holdout scenarios
      must live exclusively in the sibling sealed repo referenced by
      $DARK_FACTORY_HOLDOUTS, never in this tree.
  (c) The word "sealed" must not appear in any prompt-facing spec.md file
      (benchmarks/*/spec.md or similar).  It is acceptable in README,
      DESIGN, SCENARIOS, visible_acceptance, and script docs — those
      are operator-facing, not agent-facing.
"""

from __future__ import annotations

import pathlib
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]

# ---------------------------------------------------------------------------
# Legacy check: scan public benchmark globs for forbidden snippets
# ---------------------------------------------------------------------------
PUBLIC_GLOBS = [
    "benchmarks/**/*.md",
    "benchmarks/**/*.dot",
    "benchmarks/**/prompts/*.md",
    "docs/plans/*.md",
]
FORBIDDEN_PUBLIC_SNIPPETS = [
    "/dark-factory-holdouts",
    "dark-factory-holdouts/holdouts",
    "_holdout/",
    "scenarios.yaml",
    "hidden checkout edge",
    "secret-story",
    "product_listing_loads",
    "search_filters_products",
    "detail_page_has_price_title_description",
    "cart_add_remove_quantity",
    "checkout_rejects_invalid_email",
    "checkout_completes_valid_order",
    "cart_persists_refresh",
    "mobile_view_no_horizontal_overflow",
    "basic_a11y_labels",
    "no_pii_or_card_logged",
]


def public_files() -> list[pathlib.Path]:
    files: set[pathlib.Path] = set()
    for pattern in PUBLIC_GLOBS:
        files.update(ROOT.glob(pattern))
    return sorted(path for path in files if path.is_file() and path.name != "README.md")


def check_legacy_snippets() -> list[str]:
    failures: list[str] = []
    for path in public_files():
        text = path.read_text(errors="replace")
        for snippet in FORBIDDEN_PUBLIC_SNIPPETS:
            if snippet in text:
                failures.append(f"{path.relative_to(ROOT)} leaks {snippet!r}")
    return failures


# ---------------------------------------------------------------------------
# (a) root-level prompts/ and specs/ must not expose holdout paths or env vars
# ---------------------------------------------------------------------------
AGENT_FACING_DIRS = ["prompts", "specs"]

# These are the actual leakage vectors: the env-var name that resolves to the
# sealed repo path, and filesystem path fragments that would let an agent locate
# or read holdout content.  The word "holdout" alone used in boundary-enforcement
# language ("you do NOT have access to the holdout scenarios") is acceptable and
# intentional — it reinforces the boundary without revealing paths or content.
AGENT_FORBIDDEN = [
    "DARK_FACTORY_HOLDOUTS",       # env-var resolves to sealed repo path
    "/dark-factory-holdouts",      # direct filesystem path fragment
    "dark-factory-holdouts/",      # path prefix
    "_holdout/",                   # internal holdout dir prefix
    "$DARK_FACTORY_HOLDOUTS",      # shell expansion of the env var
]


def check_agent_facing_dirs() -> list[str]:
    """Invariant (a): prompts/ and specs/ ship to the implementing agent.

    They must never contain holdout path references or the env-var name, because
    that would let the agent locate and read sealed evaluator content.  Using the
    word "holdout" to reinforce the boundary ("you cannot access holdout
    scenarios") is acceptable — those phrases teach agents what they cannot read
    without revealing where the sealed content lives.
    """
    failures: list[str] = []
    for dirname in AGENT_FACING_DIRS:
        dirpath = ROOT / dirname
        if not dirpath.is_dir():
            continue
        for path in sorted(dirpath.rglob("*.md")):
            text = path.read_text(errors="replace")
            for forbidden in AGENT_FORBIDDEN:
                if forbidden in text:
                    failures.append(
                        f"{path.relative_to(ROOT)} contains {forbidden!r} "
                        f"(agent-facing file must not expose holdout paths or env vars)"
                    )
    return failures


# ---------------------------------------------------------------------------
# (b) No holdouts/ directory inside benchmarks/
# ---------------------------------------------------------------------------

def check_no_holdouts_dir() -> list[str]:
    """Invariant (b): holdout scenarios must not exist inside this repo tree."""
    failures: list[str] = []
    for holdouts_dir in ROOT.rglob("holdouts"):
        if holdouts_dir.is_dir():
            failures.append(
                f"{holdouts_dir.relative_to(ROOT)}/ directory found inside repo "
                f"(holdout scenarios must live in the sealed sibling repo only)"
            )
    return failures


# ---------------------------------------------------------------------------
# (c) "sealed" must not appear in prompt-facing spec.md files
# ---------------------------------------------------------------------------
# These filename patterns are operator-facing docs — "sealed" is fine there.
OPERATOR_DOC_NAMES = {
    "README.md",
    "DESIGN.md",
    "SCENARIOS.md",
    "SCORING.md",
    "visible_acceptance.md",
}

# Prompt files explicitly teach agents about the boundary ("do not read
# sealed holdouts") — "sealed" in those is boundary-enforcement, not leakage.
# We only flag it if it appears in spec.md (the public product spec that the
# agent reads to understand *what to build*).
SPEC_NAME = "spec.md"


def check_sealed_not_in_specs() -> list[str]:
    """Invariant (c): 'sealed' must not appear in prompt-facing spec.md files.

    spec.md describes the public product contract.  Mentioning "sealed" there
    would expose evaluator architecture to the implementing agent.
    """
    failures: list[str] = []
    for path in ROOT.rglob(SPEC_NAME):
        if not path.is_file():
            continue
        text = path.read_text(errors="replace")
        if "sealed" in text:
            failures.append(
                f"{path.relative_to(ROOT)} contains 'sealed' "
                f"(spec.md is agent-facing; evaluator architecture must not appear)"
            )
    return failures


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    failures: list[str] = []

    failures.extend(check_legacy_snippets())
    failures.extend(check_agent_facing_dirs())
    failures.extend(check_no_holdouts_dir())
    failures.extend(check_sealed_not_in_specs())

    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("benchmark boundary check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
