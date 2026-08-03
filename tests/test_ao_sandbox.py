"""Regression tests for AO worker sandbox isolation (orch-7z3e).

The `--backend ao` branch of `_codergen` spawns the AO CLI (`ao spawn`,
`ao send`) as a subprocess. Without mechanical isolation, that subprocess
inherits the parent's filesystem access and can read the sealed holdouts
repo at `$DARK_FACTORY_HOLDOUTS`.

These tests lock in two layers of protection:

  1. `_codergen` wraps `ao spawn` and `ao send` argv in `_sandboxed_args`,
     so the AO CLI itself runs under `sandbox-exec` with a deny rule on the
     holdouts subpath. Verified by mocking `subprocess.run`.

  2. With a real fake `ao` shim on PATH, attempts to read from the
     holdouts subpath inside the AO subprocess actually fail at the kernel
     level (sandbox-exec returns non-zero). Verified by exec'ing the real
     pipeline path against a fake shim.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import stat
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.handlers import (  # noqa: E402
    Context,
    _codergen,
    _holdouts_repo_path,
    _sandboxed_args,
    _sanitized_env,
)
from runner.parser import Node  # noqa: E402


# ---------------------------------------------------------------------------
# Mock-based: verify `ao spawn` / `ao send` argv are wrapped in sandbox-exec
# ---------------------------------------------------------------------------


def _make_ao_node() -> Node:
    return Node(name="ao_step", attrs={"type": "codergen"})


def test_codergen_ao_spawn_args_are_sandboxed(monkeypatch, tmp_path):
    """First AO call must wrap `ao spawn` in sandbox-exec.

    We intercept subprocess.run, inspect the argv it would have invoked,
    and confirm it begins with `sandbox-exec -p <profile>`.
    """
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec unavailable (non-macOS host)")

    captured: dict[str, list[str]] = {}

    class _FakeCompleted:
        def __init__(self) -> None:
            self.returncode = 0
            # Provide a valid SESSION= line so the handler proceeds past parsing.
            self.stdout = "SESSION=fake-session\nWorktree: /tmp/fake-worktree\n"
            self.stderr = ""
            self.timed_out = False

    def _fake_run(args, **kwargs):
        captured.setdefault("calls", []).append(list(args))
        return _FakeCompleted()

    # Short-circuit the post-spawn idle wait so the test doesn't hang.
    monkeypatch.setattr(
        "runner.handlers._ao_wait_idle",
        lambda *a, **kw: "ready",
    )
    monkeypatch.setattr(subprocess, "run", _fake_run)
    monkeypatch.setattr("runner.handlers.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_codergen.run_bounded_process", _fake_run)
    fake_holdouts = tmp_path / "dark-factory-holdouts"
    fake_holdouts.mkdir()
    monkeypatch.setattr("runner.handler_sandbox._holdouts_repo_path", lambda: fake_holdouts)

    node = _make_ao_node()
    ctx = Context(goal="test", workdir=tmp_path, backend="ao")
    ctx.state["ao.project"] = "fake-project"

    result = _codergen(node, ctx)

    assert "calls" in captured and captured["calls"], "subprocess.run was not invoked"
    spawn_argv = next(call for call in captured["calls"] if "ao" in call and "spawn" in call)
    assert spawn_argv[0].endswith("sandbox-exec"), (
        f"first arg should be sandbox-exec, got {spawn_argv[:3]!r}"
    )
    assert spawn_argv[1] == "-p", f"expected -p flag after sandbox-exec, got {spawn_argv[:3]!r}"
    # The deny rule for the holdouts subpath must appear in the profile.
    assert "dark-factory-holdouts" in spawn_argv[2], (
        "sandbox profile is missing the holdouts deny rule"
    )
    assert "(deny file-read*" in spawn_argv[2]
    # The real `ao spawn` argv must follow the sandbox wrapper.
    assert "ao" in spawn_argv and "spawn" in spawn_argv
    assert result.outcome == "success"


def test_codergen_ao_send_args_are_sandboxed(monkeypatch, tmp_path):
    """Subsequent AO calls must wrap `ao send` in sandbox-exec too."""
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec unavailable (non-macOS host)")

    captured: dict[str, list[str]] = {}

    class _FakeCompleted:
        def __init__(self) -> None:
            self.returncode = 0
            self.stdout = ""
            self.stderr = ""
            self.timed_out = False

    def _fake_run(args, **kwargs):
        captured.setdefault("calls", []).append(list(args))
        return _FakeCompleted()

    monkeypatch.setattr(
        "runner.handlers._ao_wait_idle",
        lambda *a, **kw: "ready",
    )
    monkeypatch.setattr("runner.handlers.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_codergen.run_bounded_process", _fake_run)
    fake_holdouts = tmp_path / "dark-factory-holdouts"
    fake_holdouts.mkdir()
    monkeypatch.setattr("runner.handler_sandbox._holdouts_repo_path", lambda: fake_holdouts)

    node = _make_ao_node()
    ctx = Context(goal="test", workdir=tmp_path, backend="ao")
    ctx.state["ao.project"] = "fake-project"
    # Pre-seed an existing session so _codergen takes the `ao send` branch.
    ctx.state["ao.session"] = "existing-session"

    result = _codergen(node, ctx)

    assert captured["calls"], "subprocess.run was not invoked"
    send_argv = next(call for call in captured["calls"] if "ao" in call and "send" in call)
    assert send_argv[0].endswith("sandbox-exec")
    assert send_argv[1] == "-p"
    assert "dark-factory-holdouts" in send_argv[2]
    assert "(deny file-read*" in send_argv[2]
    assert "ao" in send_argv and "send" in send_argv
    assert result.outcome == "success"


# ---------------------------------------------------------------------------
# Integration: a real fake `ao` shim cannot read $DARK_FACTORY_HOLDOUTS
# ---------------------------------------------------------------------------


def _write_fake_ao_shim(shim_dir: pathlib.Path, target_path: str) -> pathlib.Path:
    """Write an `ao` executable that tries to read `target_path` and emits a
    valid SESSION= line if (and only if) the read succeeds.

    Under sandbox-exec with a deny rule on `target_path`, the read should
    fail and the script should exit non-zero before printing SESSION=.
    """
    shim = shim_dir / "ao"
    shim.write_text(
        "#!/bin/sh\n"
        # Try to read the sealed holdouts path. Under sandbox-exec deny, this
        # fails with EPERM and the script exits non-zero.
        f"contents=$(cat {target_path!s} 2>&1) || {{ echo \"AO_LEAK_BLOCKED $contents\" 1>&2; exit 7; }}\n"
        "echo \"AO_LEAK_LEAKED $contents\"\n"
        "echo SESSION=fake\n"
    )
    shim.chmod(shim.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return shim


def test_fake_ao_shim_cannot_read_holdouts_under_sandbox(monkeypatch, tmp_path):
    """End-to-end: a fake `ao` on PATH that tries to read the real holdouts
    file MUST fail when invoked through `_codergen` (because sandbox-exec
    denies the read).
    """
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec unavailable (non-macOS host)")

    # Target a real file inside the sealed holdouts repo (resolved via the
    # canonical path so the deny rule actually applies). We don't read it
    # ourselves — we only ask the sandboxed child to try.
    real_holdouts = pathlib.Path.home() / "projects" / "dark-factory-holdouts"
    candidate = real_holdouts / "holdouts" / "hello" / "scenarios.yaml"
    if not candidate.exists():
        pytest.skip(f"real holdouts target missing: {candidate}")

    shim_dir = tmp_path / "fake-bin"
    shim_dir.mkdir()
    _write_fake_ao_shim(shim_dir, str(candidate))

    # Put the shim FIRST on PATH so `ao` resolves to it.
    monkeypatch.setenv("PATH", f"{shim_dir}:{os.environ.get('PATH', '')}")
    # Short-circuit idle waiting (the shim never produces a real session).
    monkeypatch.setattr(
        "runner.handlers._ao_wait_idle",
        lambda *a, **kw: "ready",
    )

    node = _make_ao_node()
    ctx = Context(goal="test", workdir=tmp_path, backend="ao")
    ctx.state["ao.project"] = "fake-project"

    result = _codergen(node, ctx)

    # The shim's read must be blocked → exit 7 → handler reports failure.
    assert result.outcome == "failure", (
        f"expected failure (sandbox should block holdout read), got "
        f"outcome={result.outcome!r} output={result.output!r}"
    )
    # Holdout content must NOT appear in the captured output.
    assert "AO_LEAK_LEAKED" not in result.output, (
        "sandbox failed to block holdout read — content leaked into AO output"
    )


def test_ao_subprocess_inherits_sanitized_env(monkeypatch, tmp_path):
    """The AO subprocess must receive `_sanitized_env()` (no DARK_FACTORY_HOLDOUTS,
    no *HOLDOUT* vars), even when sandboxed."""
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec unavailable (non-macOS host)")

    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", "/secret/holdouts")
    monkeypatch.setenv("HOLDOUT_TOKEN", "secret-token")

    # The fake /secret/holdouts path must not raise on sandbox setup; mock
    # the holdout-path resolver to a real-looking directory so the
    # `_sandboxed_args` call inside `_codergen` completes and the test can
    # observe the sanitized env vars that reach the subprocess.
    fake_holdouts = tmp_path / "fake-holdouts"
    fake_holdouts.mkdir()
    monkeypatch.setattr(
        "runner.handler_sandbox._holdouts_repo_path",
        lambda: fake_holdouts,
    )

    captured: dict[str, dict[str, str]] = {}

    class _FakeCompleted:
        def __init__(self) -> None:
            self.returncode = 0
            self.stdout = "SESSION=fake\n"
            self.stderr = ""
            self.timed_out = False

    def _fake_run(args, **kwargs):
        captured["env"] = dict(kwargs.get("env") or {})
        return _FakeCompleted()

    monkeypatch.setattr(
        "runner.handlers._ao_wait_idle",
        lambda *a, **kw: "ready",
    )
    monkeypatch.setattr("runner.handlers.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_codergen.run_bounded_process", _fake_run)

    node = _make_ao_node()
    ctx = Context(goal="test", workdir=tmp_path, backend="ao")
    ctx.state["ao.project"] = "fake-project"

    _codergen(node, ctx)

    env = captured.get("env", {})
    assert "DARK_FACTORY_HOLDOUTS" not in env, (
        "AO subprocess received DARK_FACTORY_HOLDOUTS — holdout leak vector"
    )
    assert "HOLDOUT_TOKEN" not in env, (
        "AO subprocess received *HOLDOUT* env var — holdout leak vector"
    )


# ---------------------------------------------------------------------------
# Portability: _holdouts_repo_path must fail loud, not silently no-op,
# when the sealed sibling repo cannot be located (bd portability audit).
# ---------------------------------------------------------------------------


def test_holdouts_repo_path_fails_loud_when_no_env_and_default_missing(
    monkeypatch, tmp_path
):
    """No DARK_FACTORY_HOLDOUTS + no sibling checkout at the default location
    must raise, not silently return a nonexistent path. A nonexistent path
    fed into `_build_sandbox_profile` produces a deny rule on a directory
    that will never be hit — which looks safe but actually means the sealed
    holdouts, wherever they really live on this machine, are NOT covered by
    the sandbox deny-list. The isolation guarantee this whole repo's
    CRITICAL Agent Isolation section depends on must not degrade silently.
    """
    monkeypatch.delenv("DARK_FACTORY_HOLDOUTS", raising=False)
    fake_home = tmp_path / "no_holdouts_here"
    fake_home.mkdir()
    monkeypatch.setattr(pathlib.Path, "home", lambda: fake_home)

    with pytest.raises(RuntimeError, match="DARK_FACTORY_HOLDOUTS"):
        _holdouts_repo_path()


def test_holdouts_repo_path_fails_loud_when_env_set_but_missing(monkeypatch, tmp_path):
    """DARK_FACTORY_HOLDOUTS explicitly set to a path that doesn't exist is a
    misconfiguration, not a valid signal to silently no-op — must also raise.
    """
    monkeypatch.setenv(
        "DARK_FACTORY_HOLDOUTS", str(tmp_path / "does_not_exist_at_all")
    )

    with pytest.raises(RuntimeError, match="DARK_FACTORY_HOLDOUTS"):
        _holdouts_repo_path()


def test_holdouts_repo_path_succeeds_when_env_points_at_real_dir(monkeypatch, tmp_path):
    """Regression guard: a valid, existing DARK_FACTORY_HOLDOUTS must still
    resolve normally (this must not become fail-loud for the happy path).
    """
    real_dir = tmp_path / "real_holdouts"
    real_dir.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(real_dir))

    resolved = _holdouts_repo_path()

    assert resolved == real_dir.resolve()
