"""dark-factory#828 item (e): commits_created / refs_pushed run-summary fields.

Real incident: the run's JSON summary gave zero indication a write
occurred — `"uncommitted_files": "0"` was technically true because the
coder had already committed AND pushed by the time the run ended. These
tests prove the new fields would have caught that.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine_observability import (  # noqa: E402
    _capture_run_base_state,
    _collect_commit_push_state,
)

# See tests/test_gates_fix_node_actionable_gate.py's _GIT_ENV comment: the
# global pre-commit guard rejects @example.com committer emails
# unconditionally; use a non-placeholder-pattern email instead.
_GIT_ENV = dict(os.environ)
_TEST_GIT_EMAIL = "darkfactory-sidekick-test@users.noreply.github.com"


def _git(*args: str, cwd: pathlib.Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=str(cwd), capture_output=True, text=True, check=False,
        env=_GIT_ENV,
    )


def _init_repo(workdir: pathlib.Path) -> None:
    workdir.mkdir(parents=True, exist_ok=True)
    # Explicit -b main: see tests/test_push_guard.py::_init_repo for the
    # full explanation -- a CI runner without init.defaultBranch=main
    # produces a local branch name that doesn't match the explicit
    # `HEAD:refs/heads/main` push below, breaking a later bare `git push`
    # under push.default=simple. Found live in CI on PR #832.
    _git("init", "-q", "-b", "main", cwd=workdir)
    _git("config", "user.email", _TEST_GIT_EMAIL, cwd=workdir)
    _git("config", "user.name", "Test", cwd=workdir)
    (workdir / "README.md").write_text("original\n", encoding="utf-8")
    _git("add", "-A", cwd=workdir)
    _git("commit", "-q", "-m", "initial", cwd=workdir)


def test_capture_and_collect_zero_commits_zero_push(tmp_path):
    """No writes at all -> commits_created="0", refs_pushed determinable
    (no upstream configured -> "" not "0", since we genuinely can't tell)."""
    workdir = tmp_path / "repo"
    _init_repo(workdir)

    base = _capture_run_base_state(workdir)
    assert base["_df_run_base_sha"]
    # No upstream configured in this bare local repo.
    assert base["_df_run_base_upstream_sha"] == ""

    state = _collect_commit_push_state(
        workdir, base["_df_run_base_sha"], base["_df_run_base_upstream_sha"]
    )
    assert state["commits_created"] == "0"
    assert state["refs_pushed"] == ""  # undeterminable, not a confirmed "0"


def test_collect_detects_new_local_commit(tmp_path):
    """A real incident shape: base captured, then a coder commits — must
    be visible as commits_created >= 1, not silently "0"."""
    workdir = tmp_path / "repo"
    _init_repo(workdir)

    base = _capture_run_base_state(workdir)

    (workdir / "DESTRUCTIVE_CHANGE.txt").write_text("improvised\n", encoding="utf-8")
    _git("add", "-A", cwd=workdir)
    _git("commit", "-q", "-m", "improvised fix", cwd=workdir)

    state = _collect_commit_push_state(
        workdir, base["_df_run_base_sha"], base["_df_run_base_upstream_sha"]
    )
    assert state["commits_created"] == "1"


def test_collect_detects_a_real_push_to_upstream(tmp_path):
    """Real incident shape #2: the coder didn't just commit, it pushed to
    a live branch. Set up a real local bare remote + tracking branch, push
    to it, and confirm refs_pushed flips to "1"."""
    remote = tmp_path / "remote.git"
    remote.mkdir(parents=True, exist_ok=True)
    _git("init", "-q", "--bare", cwd=remote)

    workdir = tmp_path / "repo"
    _init_repo(workdir)
    _git("remote", "add", "origin", str(remote), cwd=workdir)
    push_setup = _git("push", "-q", "-u", "origin", "HEAD:refs/heads/main", cwd=workdir)
    assert push_setup.returncode == 0, push_setup.stderr

    base = _capture_run_base_state(workdir)
    assert base["_df_run_base_upstream_sha"]

    # No new push yet — refs_pushed must read "0", not "1".
    unchanged_state = _collect_commit_push_state(
        workdir, base["_df_run_base_sha"], base["_df_run_base_upstream_sha"]
    )
    assert unchanged_state["refs_pushed"] == "0"

    # Now actually commit + push, like an improvising fix node would.
    (workdir / "DESTRUCTIVE_CHANGE.txt").write_text("improvised\n", encoding="utf-8")
    _git("add", "-A", cwd=workdir)
    _git("commit", "-q", "-m", "improvised fix", cwd=workdir)
    push = _git("push", "-q", cwd=workdir)
    assert push.returncode == 0, push.stderr

    pushed_state = _collect_commit_push_state(
        workdir, base["_df_run_base_sha"], base["_df_run_base_upstream_sha"]
    )
    assert pushed_state["commits_created"] == "1"
    assert pushed_state["refs_pushed"] == "1"


def test_capture_handles_non_git_dir(tmp_path):
    not_a_repo = tmp_path / "plain_dir"
    not_a_repo.mkdir()
    base = _capture_run_base_state(not_a_repo)
    assert base == {"_df_run_base_sha": "", "_df_run_base_upstream_sha": ""}

    state = _collect_commit_push_state(not_a_repo, "", "")
    assert state == {"commits_created": "", "refs_pushed": ""}
