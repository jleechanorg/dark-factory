"""dark-factory#828 item (c): --allow-push defaults to false + push guard.

Real incident: a `fix` node with full repo write+push authority pushed to
a LIVE PR branch with nothing actionable to act on. Two structural layers:
1. Every subprocess launched through runner.handler_sandbox._sanitized_env()
   is ALWAYS blocked from `git push` directly (a PATH shim, not a runtime
   flag the node could ignore).
2. --allow-push gates a SEPARATE, centralized push the runner itself may
   perform at run end, only when final_outcome is a genuine success.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner import push_guard  # noqa: E402
from runner.handler_sandbox import _sanitized_env  # noqa: E402

_TEST_GIT_EMAIL = "darkfactory-sidekick-test@users.noreply.github.com"


def _git(*args: str, cwd: pathlib.Path, env: dict | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=str(cwd), capture_output=True, text=True, check=False,
        env=env if env is not None else os.environ.copy(),
    )


def _init_repo(workdir: pathlib.Path) -> None:
    workdir.mkdir(parents=True, exist_ok=True)
    _git("init", "-q", cwd=workdir)
    _git("config", "user.email", _TEST_GIT_EMAIL, cwd=workdir)
    _git("config", "user.name", "Test", cwd=workdir)
    (workdir / "README.md").write_text("original\n", encoding="utf-8")
    _git("add", "-A", cwd=workdir)
    _git("commit", "-q", "-m", "initial", cwd=workdir)


def test_push_guard_env_blocks_push_but_allows_other_git_commands(tmp_path):
    workdir = tmp_path / "repo"
    _init_repo(workdir)

    guarded_env = push_guard.push_guard_env(dict(os.environ))
    assert str(push_guard._SHIM_DIR) in guarded_env["PATH"]
    # The shim dir must come FIRST so it's found before the real git.
    assert guarded_env["PATH"].split(os.pathsep)[0] == str(push_guard._SHIM_DIR)

    # Non-push commands still work through the shim.
    status = _git("status", "--short", cwd=workdir, env=guarded_env)
    assert status.returncode == 0

    # git push is blocked with a clear message, non-zero exit.
    blocked = _git("push", cwd=workdir, env=guarded_env)
    assert blocked.returncode != 0
    assert "blocked" in blocked.stderr.lower()
    assert "828" in blocked.stderr


def test_sanitized_env_always_includes_push_guard():
    """_sanitized_env() blocks push unconditionally, regardless of
    --allow-push — that flag only gates the SEPARATE centralized push."""
    push_guard.set_allow_push(True)
    try:
        env = _sanitized_env()
        assert str(push_guard._SHIM_DIR) in env["PATH"]
    finally:
        push_guard.set_allow_push(False)

    env2 = _sanitized_env()
    assert str(push_guard._SHIM_DIR) in env2["PATH"]


class TestMaybePushAtRunEnd:
    def setup_method(self):
        push_guard.set_allow_push(False)

    def teardown_method(self):
        push_guard.set_allow_push(False)

    def test_skips_when_allow_push_not_set(self, tmp_path):
        workdir = tmp_path / "repo"
        _init_repo(workdir)
        result = push_guard.maybe_push_at_run_end(workdir, "success")
        assert result["push_attempted"] == "0"
        assert result["push_succeeded"] == "0"
        assert result["push_skip_reason"] == "allow_push_not_set"

    def test_skips_on_exhausted_even_with_allow_push(self, tmp_path):
        """dark-factory#828: never push when final_outcome == exhausted."""
        push_guard.set_allow_push(True)
        workdir = tmp_path / "repo"
        _init_repo(workdir)
        result = push_guard.maybe_push_at_run_end(workdir, "exhausted")
        assert result["push_attempted"] == "0"
        assert result["push_succeeded"] == "0"
        assert "exhausted" in result["push_skip_reason"]

    def test_skips_on_failure_and_error(self, tmp_path):
        push_guard.set_allow_push(True)
        workdir = tmp_path / "repo"
        _init_repo(workdir)
        for outcome in ("failure", "error", "stuck", ""):
            result = push_guard.maybe_push_at_run_end(workdir, outcome)
            assert result["push_attempted"] == "0", outcome
            assert result["push_succeeded"] == "0", outcome

    def test_pushes_on_real_success_with_allow_push(self, tmp_path):
        """Positive case: allow_push=True + final_outcome=success against
        a real local bare remote actually pushes."""
        remote = tmp_path / "remote.git"
        remote.mkdir(parents=True, exist_ok=True)
        _git("init", "-q", "--bare", cwd=remote)

        workdir = tmp_path / "repo"
        _init_repo(workdir)
        _git("remote", "add", "origin", str(remote), cwd=workdir)
        setup = _git("push", "-q", "-u", "origin", "HEAD:refs/heads/main", cwd=workdir)
        assert setup.returncode == 0, setup.stderr

        # New local commit that hasn't been pushed yet.
        (workdir / "new.txt").write_text("hi\n", encoding="utf-8")
        _git("add", "-A", cwd=workdir)
        _git("commit", "-q", "-m", "second", cwd=workdir)

        push_guard.set_allow_push(True)
        result = push_guard.maybe_push_at_run_end(workdir, "success")
        assert result["push_attempted"] == "1"
        assert result["push_succeeded"] == "1", result

        # Verify the remote actually received the commit.
        remote_head = _git("rev-parse", "refs/heads/main", cwd=remote)
        local_head = _git("rev-parse", "HEAD", cwd=workdir)
        assert remote_head.stdout.strip() == local_head.stdout.strip()

    def test_no_workdir_skips(self):
        push_guard.set_allow_push(True)
        result = push_guard.maybe_push_at_run_end(None, "success")
        assert result["push_attempted"] == "0"
        assert result["push_skip_reason"] == "no_workdir"
