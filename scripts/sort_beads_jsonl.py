#!/usr/bin/env python3
"""Canonicalize `.beads/issues.jsonl` — sort by id ascending.

This is the ROOT CAUSE fix for the +1686/-1685 noise pattern (worldai
PR #7848 etc.). Different br versions or non-br tools can write the
JSONL with different sort orders; every line position shifts even when
only one record changed, producing a wholesale diff in the next PR.

A pre-commit hook that calls this script before every commit guarantees
the JSONL is always sorted by id — the CI check (bead-jsonl-sort-check)
becomes a defense-in-depth backstop, not the only guard.

Standalone usage:
  python3 scripts/sort_beads_jsonl.py

Exit code 0 = sorted (or already sorted), exit code 1 = error.
Prints a one-line summary of how many records were touched.
"""

import json
import sys
from pathlib import Path

JSONL = Path(".beads") / "issues.jsonl"


def main() -> int:
    if not JSONL.exists():
        return 0
    text = JSONL.read_text(encoding="utf-8")
    beads = []
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            beads.append(json.loads(line))
        except json.JSONDecodeError as e:
            print(f"error: malformed JSONL at parse position: {e}", file=sys.stderr)
            return 1
    if not beads:
        return 0
    current_ids = [b["id"] for b in beads]
    sorted_ids = sorted(current_ids)
    if current_ids == sorted_ids:
        print(f"sort-beads-jsonl: {len(beads)} beads already sorted")
        return 0
    # Write atomically: temp file + rename
    tmp = JSONL.with_suffix(".jsonl.tmp")
    beads.sort(key=lambda b: b["id"])
    tmp.write_text(
        "".join(json.dumps(b, separators=(", ", ": ")) + "\n" for b in beads),
        encoding="utf-8",
    )
    tmp.replace(JSONL)
    print(f"sort-beads-jsonl: rewrote {len(beads)} beads in id-ascending order")
    return 0


if __name__ == "__main__":
    sys.exit(main())
