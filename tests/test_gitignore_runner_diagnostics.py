"""Tests for .gitignore patterns covering generated runner diagnostics.

The dark-factory runner emits ephemeral diagnostics during failed runs:

- ``failed_run_log*.txt``  — human-readable failure log dropped at the repo root
  by the runner when a pipeline exhausts retries.
- ``branch_fail_step_*``   — per-step failure markers written under
  ``<workdir>/branch_fail_step_<id>`` so the operator can see which node
  stopped a branch lane.

Both are generated artifacts (not source) and must not be committed. This
test enforces the contract by calling ``git check-ignore`` (the same tool
the task brief specifies) on representative paths, and by parsing the
.gitignore to make sure both patterns are present literally.
"""

from __future__ import annotations

import pathlib
import re
import subprocess

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
GITIGNORE = ROOT / ".gitignore"


def _git_check_ignore(*paths: str) -> set[str]:
    """Return the subset of *paths* that ``git check-ignore`` reports as ignored.

    A path is "ignored" if ``git check-ignore -q -- <path>`` exits 0. Paths
    that exit 1 are not ignored. We compare against the worktree root so
    ``git`` treats them as relative.
    """
    ignored: set[str] = set()
    for path in paths:
        result = subprocess.run(
            ["git", "check-ignore", "--no-index", path],
            cwd=str(ROOT),
            capture_output=True,
            text=True,
            check=False,
        )
        # git check-ignore --no-index: 0 = ignored, 1 = not ignored
        if result.returncode == 0:
            ignored.add(path)
    return ignored


def test_gitignore_contains_failed_run_log_pattern():
    """The ``failed_run_log*.txt`` glob must appear literally in .gitignore."""
    text = GITIGNORE.read_text()
    assert re.search(r"^failed_run_log\*\.txt\s*$", text, re.MULTILINE), (
        "Expected an exact line `failed_run_log*.txt` in .gitignore so "
        "glob matches at any directory depth; got:\n" + text
    )


def test_gitignore_contains_branch_fail_step_pattern():
    """The ``branch_fail_step_*`` glob must appear literally in .gitignore."""
    text = GITIGNORE.read_text()
    assert re.search(r"^branch_fail_step_\*\s*$", text, re.MULTILINE), (
        "Expected an exact line `branch_fail_step_*` in .gitignore so "
        "the glob matches any branch_fail_step_<id> artifact; got:\n" + text
    )


@pytest.mark.parametrize(
    "sample_path",
    [
        "failed_run_log.txt",
        "failed_run_log2.txt",
        "failed_run_log_2026-07-15_run42.txt",
        "logs/failed_run_log_branch_a.txt",
        "branch_fail_step_a3k9",
        "branch_fail_step__ayz83rw",
        "branch_fail_step_hg0iohpa",
        "branch_fail_step_reproduce_42",
        "results/branch_fail_step_implement_3",
    ],
)
def test_gitignore_patterns_ignore_representative_paths(sample_path: str):
    """``git check-ignore`` must report each representative diagnostic path as ignored.

    Covers both at-root and nested placements because the runner drops logs
    in arbitrary workdir subdirectories.
    """
    ignored = _git_check_ignore(sample_path)
    assert sample_path in ignored, (
        f"Expected `git check-ignore {sample_path!r}` to match; "
        f"the .gitignore pattern(s) are not catching this generated diagnostic."
    )


@pytest.mark.parametrize(
    "source_path",
    [
        "runner/engine.py",
        "tests/test_gitignore_runner_diagnostics.py",
        "pipelines/factory/hello.dot",
        "README.md",
        "docs/security/ci-runner-log-hygiene.md",
    ],
)
def test_gitignore_patterns_do_not_match_tracked_source(source_path: str):
    """Sanity guard: the new patterns must NOT swallow tracked source files.

    The task brief explicitly says 'do not remove tracked source'. This is
    a regression net against a too-broad glob (e.g. ``branch_fail_*`` that
    also matches a real source path).
    """
    # Only meaningful for paths that actually exist in the worktree.
    full = ROOT / source_path
    if not full.exists():
        pytest.skip(f"{source_path} not present in this worktree")
    ignored = _git_check_ignore(source_path)
    assert source_path not in ignored, (
        f"Source file {source_path!r} is matched by .gitignore — "
        "the new diagnostic pattern is too broad."
    )


def test_ci_runner_log_hygiene_doc_exists_and_covers_destinations():
    """``docs/security/ci-runner-log-hygiene.md`` must exist and document log locations."""
    doc = ROOT / "docs" / "security" / "ci-runner-log-hygiene.md"
    assert doc.is_file(), (
        f"Expected doc {doc} explaining where CI and runner logs belong to exist."
    )
    content = doc.read_text(encoding="utf-8")
    assert "failed_run_log*.txt" in content
    assert "branch_fail_step_*" in content
    assert "Library/Logs/dark-factory" in content
    assert "cxdb" in content.lower()
