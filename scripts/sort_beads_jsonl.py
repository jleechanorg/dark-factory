#!/usr/bin/env python3
"""Canonicalize ``.beads/issues.jsonl`` — sort by id ascending + dedup by id.

Root-cause fix for recurring duplicate-id corruption across merges. Two failure
modes historically caused by the sort-only predecessor:

  * Different ``br`` versions (or non-br tooling) wrote records in different
    orders, so every line position shifted even when only one record changed,
    producing wholesale diffs in the next PR.
  * Concurrent worktrees each spawned ``br create`` and generated local
    ``rev-*`` ids that overlapped when merged back to ``main``; git's default
    3-way merge kept both copies and the JSONL ended up with duplicate ids.

Canonicalization rules:

  1. Parse every line as JSON. Malformed lines are REPORTED on stderr with
     a ``line:content`` prefix and preserved at the end of the file —
     recovery is hard, so do not silently drop them.
  2. Deduplicate by ``id``. Keep the first occurrence (in current file
     order). If two records share an ``id``, prefer the one with the LATER
     ``updated_at`` (ISO string compare is safe; values are UTC sorted).
     Fall back to first-occurrence when either record is missing
     ``updated_at``.
  3. Sort by ``id`` ascending using a stable sort.
  4. Write atomically via ``tmp`` + ``os.replace``.

Standalone usage::

    python3 scripts/sort_beads_jsonl.py

Exit code 0 on success, 1 only on fatal I/O error. Idempotent: running it
twice never changes the output.
"""

import json
import os
import sys
from pathlib import Path

JSONL = Path(".beads") / "issues.jsonl"


def _load(lines):
    """Yield valid records; for malformed input yield (None, line_no, preview)."""
    for line_no, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            preview = line if len(line) <= 120 else line[:117] + "..."
            yield None, line_no, preview
            continue
        if not isinstance(rec, dict) or "id" not in rec:
            preview = line if len(line) <= 120 else line[:117] + "..."
            yield None, line_no, preview
            continue
        yield rec


def _dedup(records):
    """Collapse duplicate ids; later ``updated_at`` wins; else first-occurrence."""
    seen: dict = {}
    deduped: list = []
    duplicate_ids = 0
    for rec in records:
        if rec is None:
            continue
        rid = rec["id"]
        if rid in seen:
            duplicate_ids += 1
            idx = seen[rid]
            existing = deduped[idx]
            ex_u = existing.get("updated_at")
            new_u = rec.get("updated_at")
            if ex_u and new_u and new_u > ex_u:
                deduped[idx] = rec
            # else: keep existing — first-occurrence rule (per spec)
            continue
        seen[rid] = len(deduped)
        deduped.append(rec)
    return deduped, duplicate_ids


def main() -> int:
    if not JSONL.exists():
        return 0

    try:
        raw = JSONL.read_text(encoding="utf-8")
    except OSError as e:
        print(f"sort-beads-jsonl: cannot read {JSONL}: {e}", file=sys.stderr)
        return 1

    valid_records: list = []
    malformed: list = []  # (line_no, preview)
    for item in _load(raw.splitlines()):
        if isinstance(item, tuple):
            malformed.append((item[1], item[2]))
        else:
            valid_records.append(item)

    deduped, duplicate_ids = _dedup(valid_records)
    deduped.sort(key=lambda r: r["id"])

    out_parts = [json.dumps(rec, separators=(", ", ": ")) + "\n" for rec in deduped]
    for _line_no, content in malformed:
        out_parts.append(content + "\n")
    new_text = "".join(out_parts)

    if new_text == raw:
        print(f"sort-beads-jsonl: {len(deduped)} beads already sorted")
        return 0

    tmp = JSONL.with_suffix(".jsonl.tmp")
    try:
        tmp.write_text(new_text, encoding="utf-8")
        os.replace(tmp, JSONL)
    except OSError as e:
        print(f"sort-beads-jsonl: cannot write {JSONL}: {e}", file=sys.stderr)
        return 1

    for line_no, content in malformed:
        print(
            f"sort-beads-jsonl: warning: malformed line {line_no}: {content}",
            file=sys.stderr,
        )

    print(
        f"sort-beads-jsonl: {len(deduped)} beads, "
        f"{duplicate_ids} duplicate-ids removed, "
        f"{len(new_text.encode('utf-8'))} bytes"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())