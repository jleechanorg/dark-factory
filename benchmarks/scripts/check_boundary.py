#!/usr/bin/env python3
"""Fail if public benchmark files leak sealed evaluator paths/details."""

from __future__ import annotations

import pathlib
import sys


ROOT = pathlib.Path(__file__).resolve().parents[2]
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


def main() -> int:
    failures: list[str] = []
    for path in public_files():
        text = path.read_text(errors="replace")
        for snippet in FORBIDDEN_PUBLIC_SNIPPETS:
            if snippet in text:
                failures.append(f"{path.relative_to(ROOT)} leaks {snippet!r}")

    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print("benchmark boundary check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
