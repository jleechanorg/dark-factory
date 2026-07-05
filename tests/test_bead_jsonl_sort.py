"""Bead JSONL ordering invariant.

Background
----------
`.beads/issues.jsonl` is the canonical durable store of beads, committed to git.
`br sync --flush-only` rewrites the whole file from the SQLite DB. If the writer
does NOT emit records in a stable, id-ascending order, then a single bead
change produces a wholesale rewrite (~+1686/-1685) — every line shifts position
even when only one record changed. This drowns real signal in PR diffs.

Worldai symptom (2026-06-23, PR #7848): +1683/-1682 on a totally unrelated
"fix(latency): revert PR #7817 sync FastEmbed preload" PR. The whole JSONL
re-sorted because a different br version (or another tool) wrote it with a
different secondary sort key.

This test is RED when the JSONL is not id-sorted. With br 0.2.16 (which sorts
by id ascending) the test is GREEN — but the test catches future regressions
if anyone upgrades br or writes the JSONL by hand with a different order.
"""

import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).parent.parent
JSONL = ROOT / ".beads" / "issues.jsonl"


def _read_ids() -> list[str]:
    text = JSONL.read_text(encoding="utf-8")
    return [
        json.loads(line)["id"]
        for line in text.splitlines()
        if line.strip()
    ]


def test_jsonl_is_sorted_by_id() -> None:
    """RED: JSONL must be sorted by id ascending.

    Any out-of-order record means a wholesale re-sort — the source of the
    +1686/-1685 noise seen in worldai PRs.
    """
    if not JSONL.exists():
        pytest.skip(f"{JSONL} not present (not a beads-enabled repo)")
    ids = _read_ids()
    assert ids, "JSONL has no records"
    sorted_ids = sorted(ids)
    if ids != sorted_ids:
        # Show first few inversions for easy debugging
        inversions = [
            (ids[i], ids[i + 1])
            for i in range(len(ids) - 1)
            if ids[i] > ids[i + 1]
        ][:5]
        pytest.fail(
            "Beads must be sorted by id ascending (first 5 inversions: "
            + ", ".join(f"{a!r} > {b!r}" for a, b in inversions)
            + f"). Total records: {len(ids)}. "
            "Fix: run `br sync --flush-only` on br >= 0.2.16 (sorts by id) "
            "OR sort the JSONL in-place by id before committing."
        )


def test_jsonl_no_duplicate_ids() -> None:
    """Sanity: the union merge driver only works if every line is uniquely keyed."""
    if not JSONL.exists():
        pytest.skip(f"{JSONL} not present")
    ids = _read_ids()
    dupes = sorted({i for i in ids if ids.count(i) > 1})
    assert not dupes, f"Duplicate bead IDs in JSONL: {dupes[:5]}"


def test_br_sync_preserves_id_sort(tmp_path: Path) -> None:
    """RED: a br operation + sync must not re-sort the JSONL.

    Builds a tiny in-place fixture, makes a 1-bead change, runs
    `br sync --flush-only`, and asserts the resulting JSONL is still
    id-sorted. If br's writer ever stops sorting by id, this test catches it.
    """
    # The `br` CLI is installed locally (~/projects/.cargo/bin/br) but not in
    # GitHub Actions CI runners — skip gracefully when it's absent so the
    # test isn't a hard requirement for green CI. The pre-flight CI contract
    # is "binary present on PATH"; when it's not, the invariant under test
    # is unreachable on this host, not violated.
    if shutil.which("br") is None:
        pytest.skip("`br` CLI not on PATH (not a beads-enabled runner)")
    fixture = tmp_path / ".beads"
    fixture.mkdir(parents=True)
    (fixture / "config.yaml").write_text("issue_prefix: jsonlsort-test\n")
    # Required minimum fields per br 0.2.x schema:
    # id, title, status, priority, issue_type, created_at, updated_at
    # We write the two records in REVERSE id order on purpose, so the writer
    # has to sort them on flush to prove the invariant.
    record = {
        "title": "second",
        "status": "open",
        "priority": 2,
        "issue_type": "task",
        "created_at": "2026-01-01T00:00:00.000000Z",
        "updated_at": "2026-01-01T00:00:00.000000Z",
    }
    a = {"id": "jsonlsort-test-a", **record, "title": "first"}
    b = {"id": "jsonlsort-test-b", **record, "title": "second"}
    (fixture / "issues.jsonl").write_text(
        json.dumps(b) + "\n" + json.dumps(a) + "\n",
        encoding="utf-8",
    )
    # Init + import the deliberately-out-of-order JSONL
    subprocess.run(["br", "init"], cwd=tmp_path, check=True, capture_output=True)
    subprocess.run(
        ["br", "sync", "--import-only"], cwd=tmp_path, check=True, capture_output=True
    )
    # No-op edit to trigger a flush
    subprocess.run(
        ["br", "update", "jsonlsort-test-a", "--title", "first (edited)"],
        cwd=tmp_path,
        check=True,
        capture_output=True,
    )
    subprocess.run(
        ["br", "sync", "--flush-only"], cwd=tmp_path, check=True, capture_output=True
    )
    out_ids = _read_ids()
    assert out_ids == sorted(out_ids), (
        f"br sync did not preserve id-sort: got {out_ids}, expected {sorted(out_ids)}"
    )


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))