"""G4 — reviewer-prompt diff injection.

The implementing agent's real diff used to be invisible to reviewer
prompts; the runner now captures ``git diff`` + ``git diff --staged``
after each successful codergen step and stashes it in ``ctx.state`` for
``${diff}`` substitution. These tests pin the contract on three axes:

1. **Capture** — the echo backend (no LLM, deterministic) actually
   stashes a diff into ``ctx.state["<node>.diff"]`` + ``ctx.state["_last_diff"]``
   when the workdir is a git repo with a tracked modification.
2. **Staged-only** — a tracked file modified AND ``git add``'d shows up
   in the captured diff (the runner captures unstaged + staged separately
   and concatenates, so a fully-staged change must still appear).
3. **Render** — ``_render_prompt`` substitutes ``${diff}`` to
   ``ctx.state["_last_diff"]`` when set, and to ``"(no diff captured)"``
   when not.
4. **Truncation** — a diff over 50 000 chars is hard-truncated with the
   ``... (truncated, full diff is N bytes)`` marker so reviewer prompts
   never blow past LLM context windows.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT as _ROOT  # noqa: E402, F811

from runner.handler_core import Context  # noqa: E402
from runner.handlers import _codergen, _capture_diff, _DIFF_MAX_CHARS, _render_prompt  # noqa: E402
from runner.parser import Node  # noqa: E402


# Use a real identity. The repo's pre-commit hook blocks the
# ``test@example.com`` RFC 2606 placeholder on every commit, including
# ephemeral ones in tmp dirs, so we use the operator's noreply email
# to satisfy the identity guard on this codebase.
_TEST_EMAIL = "jleechan2015@users.noreply.github.com"
_TEST_NAME = "jleechan2015"


def _git(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(list(args), capture_output=True, text=True, check=check)


def _init_git_repo(path: pathlib.Path) -> None:
    """Create an empty git repo with a real user/email identity (so commits work).

    The repo's global pre-commit hook (``~/.config/git/hooks/pre-commit``)
    rejects RFC 2606 placeholder emails like ``test@example.com`` — they
    have caused 3 real production identity-leak commits. We use the
    real noreply identity so the test commits are accepted; the tmp
    repo's identity is local-only and never escapes tmp_path.
    """
    path.mkdir(parents=True, exist_ok=True)
    _git("git", "init", str(path))
    _git("git", "-C", str(path), "config", "user.email", _TEST_EMAIL)
    _git("git", "-C", str(path), "config", "user.name", _TEST_NAME)


def _make_node(name: str = "impl") -> Node:
    return Node(name=name, attrs={"backend": "echo"})


def test_capture_diff_for_tracked_modification(tmp_path: pathlib.Path) -> None:
    """A tracked file with a new uncommitted modification is captured.

    Sets up a git repo, commits an empty file, modifies the file, runs the
    echo backend's codergen via the public path, then asserts that the
    diff is stashed both in the per-node key and the rolling
    ``_last_diff`` slot.
    """
    _init_git_repo(tmp_path)
    target = tmp_path / "tracked.txt"
    target.write_text("line one\n")
    _git("git", "-C", str(tmp_path), "add", "tracked.txt")
    _git("git", "-C", str(tmp_path), "commit", "-m", "init")
    # Uncommitted modification after the commit.
    target.write_text("line one\nline two added by implementing agent\n")

    node = _make_node("impl")
    ctx = Context(goal="diff capture test", workdir=tmp_path, backend="echo")
    # Drive the echo backend deterministically via the same outcome
    # seed the engine would inject on a successful run; protects the
    # test from any future change that gates echo on the state slot.
    ctx.state["impl.outcome"] = "success"
    result = _codergen(node, ctx)

    assert result.outcome == "success", f"echo backend should succeed; got {result.outcome}: {result.output}"
    assert ctx.state.get("impl.diff"), "per-node diff must be stashed"
    assert "line two added by implementing agent" in ctx.state["impl.diff"]
    assert ctx.state.get("_last_diff") == ctx.state["impl.diff"], (
        "_last_diff must mirror the per-node diff so ${diff} substitutes correctly"
    )


def test_capture_diff_includes_staged_changes(tmp_path: pathlib.Path) -> None:
    """A modification that was ``git add``'d (fully staged) appears in the diff.

    The runner calls ``git diff`` (unstaged) AND ``git diff --staged``
    and concatenates both, so a fully-staged change must still surface
    via the staged leg. A naive runner that only ran `git diff` (which
    compares workdir to index) would miss a fully-staged change because
    the index matches the workdir but differs from HEAD.
    """
    _init_git_repo(tmp_path)
    target = tmp_path / "staged.txt"
    target.write_text("baseline\n")
    _git("git", "-C", str(tmp_path), "add", "staged.txt")
    _git("git", "-C", str(tmp_path), "commit", "-m", "init")
    target.write_text("baseline\nstaged-only delta\n")
    _git("git", "-C", str(tmp_path), "add", "staged.txt")

    diff = _capture_diff(tmp_path)
    assert "staged-only delta" in diff, (
        f"fully-staged change must appear in the captured diff; got: {diff!r}"
    )


def test_render_prompt_substitutes_diff_when_set(tmp_path: pathlib.Path) -> None:
    """``${diff}`` in a template resolves to ``ctx.state["_last_diff"]`` when set."""
    (tmp_path / "prompt.md").write_text(
        "Goal: ${goal}\n\nImplementing agent's diff:\n```\n${diff}\n```\n"
    )
    node = Node(name="reviewer", attrs={"prompt": "@prompt.md"})
    ctx = Context(goal="review the change", workdir=tmp_path, backend="echo")
    ctx.state["_last_diff"] = "diff --git a/foo b/foo\n+hello"

    rendered = _render_prompt(node, ctx)
    assert "diff --git a/foo b/foo" in rendered
    assert "+hello" in rendered
    assert "${diff}" not in rendered
    assert "Goal: review the change" in rendered


def test_render_prompt_uses_placeholder_when_no_diff_captured(tmp_path: pathlib.Path) -> None:
    """``${diff}`` falls back to ``(no diff captured)`` when no codergen ran yet.

    A reviewer that runs before any implementing agent (or in a smoke
    pipeline where the coder is an echo and the workdir is not a git
    repo) must still see SOMETHING in the diff slot — an empty cell
    would silently degrade the reviewer's behavior.
    """
    (tmp_path / "prompt.md").write_text("Diff block:\n```\n${diff}\n```\n")
    node = Node(name="reviewer", attrs={"prompt": "@prompt.md"})
    ctx = Context(goal="g", workdir=tmp_path, backend="echo")
    # Deliberately do NOT set _last_diff.

    rendered = _render_prompt(node, ctx)
    assert "(no diff captured)" in rendered
    assert "${diff}" not in rendered


def test_capture_diff_truncates_at_max_chars(tmp_path: pathlib.Path) -> None:
    """A diff > 50 000 chars is hard-truncated with the size note."""
    _init_git_repo(tmp_path)
    target = tmp_path / "big.txt"
    # Commit a small file, then add a HUGE modification that overflows the cap.
    target.write_text("x\n")
    _git("git", "-C", str(tmp_path), "add", "big.txt")
    _git("git", "-C", str(tmp_path), "commit", "-m", "init")
    # Add ~60 000 added characters in a single line so the diff itself exceeds
    # the cap. The diff header alone is fine; the +added lines are what we need.
    target.write_text("x\n" + ("A" * 60_000) + "\n")

    diff = _capture_diff(tmp_path)
    assert len(diff) <= _DIFF_MAX_CHARS + 100, (
        f"captured diff must respect the cap; got len={len(diff)}"
    )
    assert "(truncated, full diff is" in diff, (
        "truncation marker must surface so reviewer knows the diff was lossy"
    )
    assert "bytes)" in diff


def test_capture_diff_handles_non_git_workdir(tmp_path: pathlib.Path) -> None:
    """A non-git workdir returns an empty string (best-effort, no failure).

    The runner must NOT raise when the workdir is not a git repo —
    reviewers still need a (no diff captured) message, not a crash.
    """
    non_git = tmp_path / "not_a_repo"
    non_git.mkdir()
    # No git init here — this is the no-repo path.
    result = _capture_diff(non_git)
    assert result == "", f"non-git workdir should yield empty diff; got {result!r}"