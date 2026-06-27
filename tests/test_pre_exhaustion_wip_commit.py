"""Regression tests for P-A: pre-exhaustion WIP commit (proposal P-A).

The 2026-06-22 PR-B'' incident (run 7aa7695b1cf6) saw dark-factory exhaust
on a fix loop and never commit the worktree's working-tree changes. Work
survived only by luck (worktree was never reset). P-A guarantees a WIP
commit lands on the branch ref BEFORE `run()` returns on an `exhausted`
outcome, so `git fsck --lost-found` is unnecessary for recovery.

These tests pin the new behavior using `tmp_path` + `git init` so the
real `~/.dark-factory/` and real worktrees are never touched.
"""
from __future__ import annotations

import pathlib
import subprocess
import sys

import os
os.environ["DARK_FACTORY_ALLOW_WIP_TEST"] = "1"

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402

from runner.engine import run  # noqa: E402
from runner.engine_run import _auto_wip_commit_on_exhaustion  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.parser import parse  # noqa: E402


def _init_git_repo(path: pathlib.Path) -> None:
    """Set up a minimal git repo at `path` so _auto_wip_commit_on_exhaustion
    treats it as committable.

    Persists `user.name`/`user.email`/`commit.gpgsign=false` in the repo's
    local config so subsequent `git commit` calls (including those inside
    the helper under test) succeed regardless of whether the host has a
    `~/.gitconfig`. CI runners have no global git identity; locally the
    user has one. Setting it per-repo keeps the test environment hermetic.

    Also sets `safe.directory=*` so the repo is committable regardless of
    the host's `safe.directory` whitelist (matters when pytest's tmp_path
    lives under a path git would otherwise refuse)."""
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=str(path), check=True)
    for key, value in [
        ("user.name", "test"),
        ("user.email", "jleechan2015@users.noreply.github.com"),
        ("commit.gpgsign", "false"),
    ]:
        subprocess.run(
            ["git", "config", key, value],
            cwd=str(path), check=True,
        )
    subprocess.run(
        ["git", "config", "--add", "safe.directory", str(path)],
        cwd=str(path), check=True,
    )
    (path / "README.md").write_text("seed\n")
    subprocess.run(["git", "add", "README.md"], cwd=str(path), check=True)
    subprocess.run(["git", "commit", "-q", "-m", "init"], cwd=str(path), check=True)


def test_auto_wip_commit_fires_on_exhaustion_with_uncommitted(tmp_path):
    """If the workdir is a git repo with uncommitted files when an
    exhausted outcome lands, the helper writes a WIP commit onto the
    branch ref with the expected message."""
    _init_git_repo(tmp_path)

    # Drop an uncommitted file so `git status --porcelain` is non-empty.
    (tmp_path / "WIP_uncommitted.txt").write_text("uncommitted work product\n")

    head_before = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout.strip()

    ctx = Context(goal="t", workdir=tmp_path, backend="echo", run_id="abc123def456")
    _auto_wip_commit_on_exhaustion(ctx, "test reason")

    head_after = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout.strip()

    assert head_after != head_before, "WIP commit should have moved HEAD forward"

    log = subprocess.run(
        ["git", "log", "-1", "--pretty=%s%n%b"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout
    assert "WIP: dark-factory exhausted at abc123def456" in log
    assert "test reason" in log
    # HEAD short-SHA substring should be embedded in the message.
    assert head_before[:12] in log


def test_auto_wip_commit_skips_when_worktree_clean(tmp_path):
    """If the worktree is clean, the helper must NOT create a commit."""
    _init_git_repo(tmp_path)

    head_before = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout.strip()

    ctx = Context(goal="t", workdir=tmp_path, backend="echo", run_id="cleanrun01")
    _auto_wip_commit_on_exhaustion(ctx, "test reason")

    head_after = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout.strip()

    assert head_after == head_before, (
        "Helper must NOT commit when the worktree is already clean."
    )


def test_auto_wip_commit_skips_when_not_git_repo(tmp_path):
    """If workdir has no `.git`, the helper is a no-op (no exception)."""
    # tmp_path has no .git because pytest does not init one here.
    assert not (tmp_path / ".git").exists()

    ctx = Context(goal="t", workdir=tmp_path, backend="echo", run_id="nongit0001")
    # Should not raise.
    _auto_wip_commit_on_exhaustion(ctx, "test reason")


def test_no_progress_max_triggers_wip_commit_via_run_loop(tmp_path, monkeypatch):
    """End-to-end: drive the engine through `no_progress_max` exhaustion
    on a real git workdir and assert a WIP commit lands on the branch.
    Mirrors the pattern in tests/test_fix_loop_test_awareness.py but
    verifies the WIP-commit side effect, not just the `exhausted` outcome."""
    _init_git_repo(tmp_path)
    (tmp_path / "WIP_file.py").write_text("x = 1\n")

    pipeline_dot = """\
digraph NoProgressWIP {
  graph [goal="test no_progress_max WIP commit"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare,  label="Exit"]

  test [type="tool", label="Always Fail Test",
        command="false", goal_gate="true", retry_target="fix"]

  fix [type="codergen", label="Blind Fix",
       prompt="@prompts/slim/fix.md",
       max_visits="5", no_progress_max="2"]

  start -> test
  test -> fix [condition="outcome!=success"]
  test -> exit [condition="outcome=success"]
  fix -> test
}
"""
    pipeline_path = tmp_path / "no_progress_wip.dot"
    pipeline_path.write_text(pipeline_dot)

    head_before = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout.strip()

    def fake_tool(node, ctx):
        return Result(outcome="failure", output="forced fail", metadata={})

    def fake_fix(node, ctx):
        return Result(outcome="success", output="identical blind output", metadata={})

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_tool)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_fix)

    g = parse(pipeline_path, require_start_exit=True)
    ctx = Context(goal="t", workdir=tmp_path, backend="echo", run_id="e2ewip0001")
    history = run(g, ctx, max_steps=50)

    assert history[-1].outcome == "exhausted"

    head_after = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout.strip()
    assert head_after != head_before, (
        "Exhaustion via run() should have triggered a WIP commit, "
        "but HEAD did not advance."
    )

    log = subprocess.run(
        ["git", "log", "-1", "--pretty=%s"],
        cwd=str(tmp_path), capture_output=True, text=True, check=True,
    ).stdout.strip()
    assert "WIP: dark-factory exhausted at e2ewip0001" in log
