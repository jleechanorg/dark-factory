"""Tests for the .githooks/pre-push-beads-guard.sh guard.

df-160 (2026-07-17, dark-factory MiniMax-M3 session) hit a `br` error
`Duplicate external_ref: jleechanorg/worldarchitect.ai#8227` and **edited
`/home/jleechan/projects/dark-factory/.beads/issues.jsonl` directly** to file
a follow-up bead. Direct edits silently diverge from the SQLite DB and corrupt
the next flush.

The guard prevents recurrence on `factory/*` branches by diffing
`.beads/issues.jsonl` against the remote tip. Mirrors the design of
jleechanorg/worldarchitect.ai PR #8426 (logic ported to dark-factory's own
.githooks chain rather than Husky, because this repo uses `core.hooksPath`
.githooks/).

Test matrix (TDD: RED first proves the bypass worked, then GREEN proves the
guard blocks it without false-positives on legit paths):
  - RED:   factory/* branch + .beads/issues.jsonl changed vs origin -> hook exits 1
  - GREEN: factory/* branch + no JSONL change                          -> hook exits 0
  - GREEN: non-factory branch + .beads/issues.jsonl changed            -> hook exits 0
  - GREEN: factory/* branch + remote tip missing (first push)          -> hook exits 0
  - regression: real .githooks/pre-push shim wires the new guard script
  - regression: real .githooks/pre-push-beads-guard.sh contains the
                factory/* + .beads/issues.jsonl guard block
"""

from __future__ import annotations

import os
import shutil
import stat
import subprocess
import textwrap
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
GUARD_SCRIPT = REPO_ROOT / ".githooks" / "pre-push-beads-guard.sh"
PRE_PUSH_SHIM = REPO_ROOT / ".githooks" / "pre-push"


def _have_git() -> bool:
    return shutil.which("git") is not None


# Standalone driver: extract the factory-guard block from the real hook so
# tests don't shell out to git lfs / graph-audit / repro-guard on every run.
GUARD_DRIVER_TEMPLATE = textwrap.dedent(
    """\
    #!/usr/bin/env bash
    # Standalone driver for the .beads/issues.jsonl factory-branch guard.
    # Mirrors .githooks/pre-push-beads-guard.sh; if you change the guard,
    # mirror the change here.

    set -e

    CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
    case "$CURRENT_BRANCH" in
        factory/*)
            REMOTE_REF="origin/${CURRENT_BRANCH}"
            if git rev-parse --verify --quiet "$REMOTE_REF" >/dev/null 2>&1; then
                if git diff --name-only "$REMOTE_REF"...HEAD -- '.beads/issues.jsonl' | grep -q '^'; then
                    echo "FORBIDDEN: .beads/issues.jsonl changed on factory/* branch ($CURRENT_BRANCH)."
                    echo "   Use \\`br\\` for all bead writes. Never edit .beads/issues.jsonl directly."
                    echo "   Bypass only with: git push --no-verify"
                    exit 1
                fi
            fi
            ;;
    esac

    exit 0
    """
)


@pytest.fixture
def synthetic_repo(tmp_path):
    """Build a synthetic git repo with origin remote + factory/* branch.

    Mirrors the df-160 reproduction: a `main` branch with a baseline
    `.beads/issues.jsonl`, plus a `factory/test` branch off main.
    """
    if not _have_git():
        pytest.skip("git not available")

    repo = tmp_path / "repo"
    bare = tmp_path / "bare.git"
    repo.mkdir()
    bare.mkdir()

    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "Test",
        "GIT_AUTHOR_EMAIL": "t@example.com",
        "GIT_COMMITTER_NAME": "Test",
        "GIT_COMMITTER_EMAIL": "t@example.com",
    }

    def run(cwd, *args, check=True):
        result = subprocess.run(  # noqa: S603 - intentional in tests
            ["git", "-C", str(cwd), *args],  # noqa: S607 - intentional in tests
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        if check and result.returncode != 0:
            raise RuntimeError(
                f"git {' '.join(args)} failed:\nstdout={result.stdout}\nstderr={result.stderr}"
            )
        return result

    def shell(*args, check=False):
        """Run a non-git shell command (e.g. the guard driver)."""
        result = subprocess.run(  # noqa: S603 - intentional in tests
            list(args),
            capture_output=True,
            text=True,
            env=env,
            cwd=str(repo),
            check=False,
        )
        if check and result.returncode != 0:
            raise RuntimeError(
                f"{' '.join(args)} failed:\nstdout={result.stdout}\nstderr={result.stderr}"
            )
        return result

    # 1. Init bare remote + working repo
    run(bare, "init", "--bare", "--quiet", "--initial-branch=main")
    run(repo, "init", "--quiet", "--initial-branch=main")
    run(repo, "remote", "add", "origin", str(bare))
    run(repo, "config", "user.email", "t@example.com")
    run(repo, "config", "user.name", "Test")

    # 2. Seed main with a baseline .beads/issues.jsonl
    (repo / ".beads").mkdir()
    (repo / ".beads" / "issues.jsonl").write_text(
        '{"id":"baseline-1","title":"seed","status":"open"}\n'
    )
    run(repo, "add", ".beads/issues.jsonl")
    run(repo, "commit", "-m", "seed beads", "--quiet")
    run(repo, "push", "-u", "origin", "main", "--quiet")

    # 3. Branch off as factory/test and push it to origin so the guard has
    #    a remote tip to diff against.
    run(repo, "checkout", "-b", "factory/test", "--quiet")
    run(repo, "push", "-u", "origin", "factory/test", "--quiet")

    # 4. Write driver script
    driver = repo / "_guard_driver.sh"
    driver.write_text(GUARD_DRIVER_TEMPLATE)
    driver.chmod(0o755)

    return {
        "repo": repo,
        "bare": bare,
        "driver": driver,
        "run": run,
        "shell": shell,
    }


def _run_driver(synthetic, expect_success: bool):
    """Execute the guard driver; assert exit code matches expectation."""
    result = synthetic["shell"]("sh", str(synthetic["driver"]))
    if expect_success:
        assert result.returncode == 0, (
            f"Expected guard to ALLOW push, but it rejected.\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
    else:
        assert result.returncode != 0, (
            f"Expected guard to REJECT push (df-160 reproduction), but it allowed.\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
        assert "FORBIDDEN" in result.stdout, (
            "Expected FORBIDDEN message in stdout, got: "
            f"{result.stdout!r} / {result.stderr!r}"
        )


# ----------------- RED: df-160 reproduction -----------------
def test_red_factory_branch_with_jsonl_change_is_rejected(synthetic_repo):
    """df-160 (2026-07-17) bypass scenario: on a factory/* branch the coder
    edited .beads/issues.jsonl directly. The guard MUST reject this push."""
    run = synthetic_repo["run"]
    repo = synthetic_repo["repo"]

    # Simulate the df-160 violation: append a bead directly to JSONL.
    jsonl = repo / ".beads" / "issues.jsonl"
    with jsonl.open("a") as f:
        f.write('{"id":"df-160-bypass","title":"hand-edited","status":"open"}\n')

    run(repo, "add", ".beads/issues.jsonl")
    run(
        repo,
        "commit",
        "-m",
        "df-160 reproduction: edit JSONL on factory branch",
        "--quiet",
    )

    _run_driver(synthetic_repo, expect_success=False)


# ----------------- GREEN paths -----------------
def test_green_factory_branch_no_jsonl_change_is_allowed(synthetic_repo):
    """A legitimate code-only commit on factory/* must still push."""
    run = synthetic_repo["run"]
    repo = synthetic_repo["repo"]

    (repo / "src.txt").write_text("hello\n")
    run(repo, "add", "src.txt")
    run(repo, "commit", "-m", "code change, no beads edit", "--quiet")

    _run_driver(synthetic_repo, expect_success=True)


def test_green_non_factory_branch_with_jsonl_change_is_allowed(synthetic_repo):
    """The guard is scoped to factory/*. A `main` branch that legitimately
    flushes a JSONL delta (e.g. via br sync) must NOT be blocked."""
    run = synthetic_repo["run"]
    repo = synthetic_repo["repo"]

    run(repo, "checkout", "main", "--quiet")
    jsonl = repo / ".beads" / "issues.jsonl"
    with jsonl.open("a") as f:
        f.write('{"id":"main-flush-1","title":"br flush","status":"open"}\n')
    run(repo, "add", ".beads/issues.jsonl")
    run(repo, "commit", "-m", "br flush on main", "--quiet")

    _run_driver(synthetic_repo, expect_success=True)


def test_green_factory_branch_first_push_no_remote_tip(synthetic_repo):
    """If origin/<branch> doesn't exist yet (first push), the guard must
    not block — there's nothing to diff against."""
    run = synthetic_repo["run"]
    repo = synthetic_repo["repo"]

    # Use a separate branch that was never pushed to origin.
    run(repo, "checkout", "-b", "factory/first-push", "--quiet")
    (repo / "first.txt").write_text("first push content\n")
    run(repo, "add", "first.txt")
    run(repo, "commit", "-m", "first commit on factory branch", "--quiet")

    out = run(
        repo,
        "rev-parse",
        "--verify",
        "--quiet",
        "origin/factory/first-push",
        check=False,
    ).stdout.strip()
    assert out == "", (
        "precondition violated: origin/factory/first-push unexpectedly exists"
    )

    _run_driver(synthetic_repo, expect_success=True)


# ----------------- Hook integrity checks -----------------
def test_guard_script_exists_and_is_executable():
    """The guard script must exist and be executable (the pre-push shim
    invokes it as `sh` actually — but a chmod 0644 still trips some
    installers; we just assert the script is present and not empty)."""
    if not GUARD_SCRIPT.exists():
        pytest.skip(f"Guard script not found at {GUARD_SCRIPT}")
    st = GUARD_SCRIPT.stat()
    assert st.st_size > 0, f"{GUARD_SCRIPT} is empty"
    # Should be readable by owner
    assert st.st_mode & stat.S_IRUSR, f"{GUARD_SCRIPT} is not readable by owner"


def test_guard_script_contains_guard_block():
    """If someone rewrites the guard and drops the block, this fails."""
    if not GUARD_SCRIPT.exists():
        pytest.skip(f"Guard script not found at {GUARD_SCRIPT}")
    content = GUARD_SCRIPT.read_text()
    assert "factory/*" in content, (
        "Guard script is missing the factory/* guard block — "
        "the df-160 bypass protection has regressed."
    )
    assert ".beads/issues.jsonl" in content, (
        "Guard script is missing the .beads/issues.jsonl guard — "
        "the df-160 bypass protection has regressed."
    )


def test_pre_push_shim_wires_guard_script():
    """The .githooks/pre-push shim must invoke the new guard. Without this
    wire-up, the script is dead code (matches the AGENTS.md "scripts must
    have callers" rule)."""
    if not PRE_PUSH_SHIM.exists():
        pytest.skip(f"pre-push shim not found at {PRE_PUSH_SHIM}")
    content = PRE_PUSH_SHIM.read_text()
    assert "pre-push-beads-guard.sh" in content, (
        "pre-push shim is missing the pre-push-beads-guard.sh invocation — "
        "the df-160 bypass protection is not wired in."
    )