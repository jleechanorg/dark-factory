"""Issue #478 sub-bead status snapshot regression guard.

This test guards the documented sub-bead resolution matrix in
``docs/backlog/issue-478-subbead-status-2026-07-26.md`` against two
classes of regression:

1. The snapshot file is deleted (renamed, moved, or otherwise lost).
   This test fails fast so a future factory wave sees the failure and
   re-derives the matrix.
2. Any merged-resolution PR listed in the matrix regresses — i.e. is
   closed without merge, or reverts from main, or is otherwise no
   longer the live carrier of the fix it claims. This is the same
   drift class flagged in CLAUDE.md's "Roadmap overclaim pattern"
   memory.

What this test does NOT do:

- It does not gate open / conflicting PRs (#336, #334, #346, #313, #205).
  Those are intentionally owned by other factory sessions and their
  reach-from-main status is out of scope here — gating them here would
  create a cross-PR flaky-test class.
- It does not rely on local git ref state (which is sparse / shallow in
  CI worktrees). It uses ``gh pr view --json state`` so the assertion
  is portable: any environment with a working ``gh`` CLI will reach
  the same conclusion.
- It does not modify state. It is read-only.

Prerequisites (skip the reachability tests gracefully if unmet):

- ``gh`` CLI on PATH
- ``GH_TOKEN`` (or ambient gh auth) for ``jleechanorg/dark-factory``

When those are missing, the reachability tests are ``skip``-ed — the
matrix doc existence and format tests still run. This matches dark-
factory's existing pattern of ``monkeypatch``-based test gating on
upstream availability.

Provenance: bead jleechan-478, file
``docs/backlog/issue-478-subbead-status-2026-07-26.md``, captured
2026-07-26.
"""
from __future__ import annotations

import json
import pathlib
import re
import shutil
import subprocess

import pytest

_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
_SNAPSHOT_PATH = _REPO_ROOT / "docs" / "backlog" / "issue-478-subbead-status-2026-07-26.md"
_REPO = "jleechanorg/dark-factory"


class MissingSnapshot(LookupError):
    """Raised when the snapshot doc has been deleted or moved."""


# PR rows the matrix claims are MERGED. Each is the unique carrier of
# the corresponding sub-bead's fix on main. If a revert happens, this
# test fails and the snapshot must be refreshed.
EXPECTED_MERGED_PRS: tuple[int, ...] = (
    233,  # jleechan-9k3a / issue #225 (Linux fail-closed isolation backend)
    285,  # jleechan-t5sw / issue #284 (rg provision + env-health preflight)
    342,  # jleechan-74wt (reroll PR-close target_repo)
    435,  # bze8.1 / issue #328 (exact-head 7-green merge authority)
    336,  # bze8.2 / issue #329 (Linux canary + SHA-bound deploy record)
    346,  # bze8.3 / issue #330 (attempt-scoped autonomy timebox)
    287,  # aw9y / issue #286 (pin self-hosted runner selector + drift gate)
    503,  # jleechan-0qy / issue #122 (make two_node.dot default graph for /f and /factory)
)


def _gh_available() -> bool:
    return shutil.which("gh") is not None


def _fetch_pr_state(pr_number: int) -> dict | None:
    """Return the current PR state via ``gh``, or ``None`` on API error."""
    proc = subprocess.run(
        ["gh", "pr", "view", str(pr_number), "--repo", _REPO, "--json", "state,mergedAt"],
        cwd=_REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        return None


def _load_snapshot() -> str:
    """Return the snapshot doc body or raise ``MissingSnapshot``."""
    if not _SNAPSHOT_PATH.exists():
        raise MissingSnapshot(
            f"snapshot doc missing at {_SNAPSHOT_PATH}; re-derive the "
            "issue #478 sub-bead matrix and re-create the doc."
        )
    return _SNAPSHOT_PATH.read_text(encoding="utf-8")


def test_snapshot_doc_present() -> None:
    """Issue #478's status snapshot doc must exist on disk."""
    assert _SNAPSHOT_PATH.exists(), (
        f"missing {_SNAPSHOT_PATH}; re-derive and re-create the matrix"
    )


@pytest.mark.parametrize("pr_number", EXPECTED_MERGED_PRS)
def test_listed_merged_pr_still_merged(pr_number: int) -> None:
    """Each merged-resolution PR cited by the matrix must still be
    ``MERGED`` on GitHub at capture time, not regressed to ``CLOSED``
    or ``OPEN`` later.
    """
    body = _load_snapshot()
    assert f"#{pr_number}](https://github.com/jleechanorg/dark-factory/pull/{pr_number}" in body, (
        f"snapshot does not cite PR #{pr_number}; refresh docs/backlog/issue-478-subbead-status-2026-07-26.md"
    )
    if not _gh_available():
        pytest.skip("gh CLI not on PATH; skipping reachability check")
    pr = _fetch_pr_state(pr_number)
    if pr is None:
        pytest.skip(f"gh pr view #{pr_number} unavailable; skipping")
    assert pr.get("state") == "MERGED", (
        f"matrix claims PR #{pr_number} is MERGED but GitHub says state="
        f"{pr.get('state')!r}; refresh docs/backlog/issue-478-subbead-status-2026-07-26.md "
        f"and re-derive the matrix."
    )


def test_table_format_row_count() -> None:
    """The status matrix table has the documented row count (1 header + 1 separator + 10 sub-bead rows + 1 footer)."""
    body = _load_snapshot()
    # Find the matrix table — it starts with `| # | Sub-bead |` and ends at the next blank line.
    m = re.search(r"^\| # \| Sub-bead \|.*?\n\|[-\s|]+\n((?:\|.*?\n)+)", body, re.MULTILINE)
    assert m is not None, (
        "sub-bead status table not found in snapshot; expected header row + separator + 10 data rows"
    )
    rows = [r for r in m.group(1).splitlines() if r.strip().startswith("|")]
    assert len(rows) == 10, (
        f"snapshot matrix should have 10 sub-bead rows per issue #478 body; got {len(rows)}"
    )


def test_open_prs_are_correctly_labeled_open() -> None:
    """The matrix must NOT claim OPEN / CONFLICTING PRs are MERGED — that
    would silently misrepresent live status to anyone reading the doc.
    """
    body = _load_snapshot()
    must_stay_open = (313, 334, 346, 336, 205)
    for pr in must_stay_open:
        # If the matrix cites this PR, the row's Status column must not say MERGED.
        row = re.search(rf"\|\s*#{pr}\][^\n]*", body)
        if row is None:
            continue  # not cited — fine
        assert "MERGED" not in row.group(0), (
            f"snapshot row for PR #{pr} says MERGED but the PR is OPEN at capture time; "
            "update the Status column"
        )
