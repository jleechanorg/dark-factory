"""Real leakage tests for the Linux agent-isolation backend (jleechan-haux).

macOS's `sandbox-exec` (see `tests/test_sealed_paths.py` and
`tests/test_ao_sandbox.py`, which already exercise real leakage under
sandbox-exec and skip themselves on non-macOS hosts) has no portable Linux
equivalent that works without elevated privilege. This file locks in the
Linux backend added in `runner/handler_sandbox.py`:

  1. `_linux_preload_lib_path` builds (and caches) the LD_PRELOAD
     deny-path shim from `scripts/agent-isolation/deny_paths_preload.c`.
  2. `_verify_linux_preload_denies` is a REAL behavioral canary — it does
     not trust a subprocess's exit code alone (that trap is exactly how
     `systemd-run --user --scope -p InaccessiblePaths=...` silently
     no-ops on a host without unprivileged user namespaces; see the
     `runner/handler_sandbox.py` module docstring for the empirical
     evidence this design decision is based on).
  3. `_sandboxed_args` / `_sandboxed_args_for_workdir` fail CLOSED
     (return `None`) when the backend can't be built or verified —
     never silently fall back to running the coder subprocess
     unsandboxed.
  4. End-to-end: a real subprocess cannot read a holdout-path file when
     wrapped by `_sandboxed_args_for_workdir`, AND a real subprocess CAN
     still read a normal in-workdir file through the same wrapper (this
     is the "verify a real pipeline node runs while holdout reads stay
     denied" acceptance bar from bead jleechan-haux / issue #225).

All tests in this file are Linux-only (they exercise the platform-specific
backend directly) and skip themselves on any other platform.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent.parent
sys.path.insert(0, str(ROOT))

from runner.handlers import (  # noqa: E402
    Context,
    _codergen,
    _linux_preload_lib_path,
    _verify_linux_preload_denies,
    _linux_sandbox_prefix,
    _reset_linux_preload_verification_cache_for_tests,
    _sandboxed_args,
    _sandboxed_args_for_workdir,
    _sanitized_env,
)
from runner.parser import Node

linux_only = pytest.mark.skipif(
    not sys.platform.startswith("linux"),
    reason="Linux-only agent-isolation backend (macOS uses sandbox-exec; "
    "see tests/test_sealed_paths.py and tests/test_ao_sandbox.py)",
)


@pytest.fixture(autouse=True)
def _reset_preload_cache(monkeypatch):
    """Each test gets a fresh verification result (the real function caches
    for the life of the process; tests need to force re-verification when
    they monkeypatch away the compiler or the canary).

    Also force-clears `DISABLE_SANDBOX` — CI's workflow sets it globally
    (`.github/workflows/ci.yml`) because CI historically ran on Linux where
    `sandbox-exec` was simply absent and there was no Linux backend to test.
    Now that there IS one, these tests must exercise it for real; the one
    test that specifically checks the `DISABLE_SANDBOX=1` escape hatch sets
    it back itself.
    """
    monkeypatch.delenv("DISABLE_SANDBOX", raising=False)
    _reset_linux_preload_verification_cache_for_tests()
    yield
    _reset_linux_preload_verification_cache_for_tests()


# ---------------------------------------------------------------------------
# (1) The shim builds and is cached.
# ---------------------------------------------------------------------------


@linux_only
def test_linux_preload_lib_builds():
    lib = _linux_preload_lib_path()
    assert lib is not None, (
        "LD_PRELOAD deny-path shim failed to build — is a C compiler "
        "(cc/gcc) installed? See scripts/agent-isolation/deny_paths_preload.c"
    )
    assert lib.is_file()
    assert lib.suffix == ".so"


@linux_only
def test_linux_preload_lib_path_is_cached():
    """Second call returns the identical path (content-hash cache hit), not
    a fresh temp file — avoids recompiling on every codergen node."""
    lib1 = _linux_preload_lib_path()
    lib2 = _linux_preload_lib_path()
    assert lib1 == lib2


@linux_only
def test_linux_preload_lib_path_none_when_compiler_missing(monkeypatch):
    """Fail closed: no cc/gcc on PATH -> None, not a stale/partial build."""
    import runner.handler_sandbox as hs

    monkeypatch.setattr(hs.shutil, "which", lambda name: None)
    assert hs._linux_preload_lib_path() is None


# ---------------------------------------------------------------------------
# (2) Behavioral verification — not exit-code trust.
# ---------------------------------------------------------------------------


@linux_only
def test_verify_linux_preload_denies_real_canary():
    """The shim really denies a freshly-created canary file when loaded.

    This is the same category of check that `systemd-run --user --scope -p
    InaccessiblePaths=...` FAILS on a locked-down host (exits 0, applies
    nothing). This test proves our replacement backend doesn't have that
    failure mode: it does a real read attempt and checks the outcome.
    """
    lib = _linux_preload_lib_path()
    assert lib is not None
    assert _verify_linux_preload_denies(lib) is True


@linux_only
def test_verify_linux_preload_denies_false_for_bogus_lib(tmp_path):
    """A library that isn't the real shim (or doesn't exist) must not be
    treated as verified — canary failure fails closed, not open."""
    bogus = tmp_path / "not-a-real-shim.so"
    bogus.write_bytes(b"not an ELF shared object")
    assert _verify_linux_preload_denies(bogus) is False


# ---------------------------------------------------------------------------
# (3) Fail-closed contract: None propagates, no silent unsandboxed fallback.
# ---------------------------------------------------------------------------


@linux_only
def test_sandboxed_args_none_when_verification_fails(monkeypatch):
    import runner.handler_sandbox as hs

    monkeypatch.setattr(hs, "_verify_linux_preload_denies", lambda lib: False)
    assert hs._linux_sandbox_prefix([pathlib.Path("/tmp/whatever")]) is None
    assert _sandboxed_args(["cat", "/etc/hostname"]) is None
    assert _sandboxed_args_for_workdir(["cat", "/etc/hostname"], None) is None


@linux_only
def test_sandboxed_args_disabled_when_disable_sandbox_set(monkeypatch):
    """DISABLE_SANDBOX=1 is the testing escape hatch — same contract as the
    macOS backend in tests/test_sealed_paths.py."""
    monkeypatch.setenv("DISABLE_SANDBOX", "1")
    assert _sandboxed_args(["cat", "/etc/hostname"]) == ["cat", "/etc/hostname"]
    assert _sandboxed_args_for_workdir(["cat", "/etc/hostname"], None) == [
        "cat",
        "/etc/hostname",
    ]


# ---------------------------------------------------------------------------
# (4) End-to-end real leakage test: holdout denied, normal read allowed.
# ---------------------------------------------------------------------------


@linux_only
def test_real_subprocess_cannot_read_holdout_path(tmp_path, monkeypatch):
    """A real subprocess, wrapped by `_sandboxed_args_for_workdir`, cannot
    read a file inside `$DARK_FACTORY_HOLDOUTS`.

    This is the direct Linux analogue of
    `test_sealed_paths.py::test_fake_claude_shim_cannot_read_sealed_readme_under_sandbox`.
    """
    holdouts_repo = tmp_path / "dark-factory-holdouts"
    holdouts_repo.mkdir()
    secret = holdouts_repo / "scenario.txt"
    secret.write_text("TOP-SECRET-HOLDOUT-CONTENT")
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts_repo))

    args = _sandboxed_args_for_workdir(["cat", str(secret)], tmp_path)
    assert args is not None, "isolation backend unavailable on this host"

    proc = subprocess.run(
        args,
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
        env=_sanitized_env(),
    )
    assert proc.returncode != 0, (
        f"holdout read SUCCEEDED — isolation failed to deny it:\n{proc.stdout!r}"
    )
    assert "TOP-SECRET-HOLDOUT-CONTENT" not in proc.stdout, (
        "holdout content leaked into stdout despite non-zero exit"
    )


@linux_only
def test_real_subprocess_can_still_read_workdir_file(tmp_path, monkeypatch):
    """The isolation wrapper does NOT break a real pipeline node: reading a
    normal file inside the implementing agent's own workdir still works.

    Together with the previous test, this is the acceptance-criteria pair:
    "verify a real pipeline node runs while holdout reads stay denied."
    """
    holdouts_repo = tmp_path / "dark-factory-holdouts"
    holdouts_repo.mkdir()
    (holdouts_repo / "scenario.txt").write_text("secret")
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts_repo))

    workdir = tmp_path / "worktree"
    workdir.mkdir()
    normal_file = workdir / "impl.py"
    normal_file.write_text("print('hello from the real workdir')")

    args = _sandboxed_args_for_workdir(["cat", str(normal_file)], workdir)
    assert args is not None

    proc = subprocess.run(
        args,
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
        env=_sanitized_env(),
    )
    assert proc.returncode == 0, f"normal in-workdir read broke: {proc.stderr}"
    assert "hello from the real workdir" in proc.stdout


@linux_only
def test_real_subprocess_cannot_read_sealed_benchmark_doc_via_workdir(tmp_path, monkeypatch):
    """`_sandboxed_args_for_workdir` also denies sealed benchmark docs
    (jleechan-113 contract) via the Linux backend, matching the macOS
    coverage in tests/test_sealed_paths.py."""
    holdouts_repo = tmp_path / "dark-factory-holdouts"
    holdouts_repo.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts_repo))

    workdir = tmp_path / "worktree"
    bench_dir = workdir / "benchmarks" / "fibonacci"
    bench_dir.mkdir(parents=True)
    sealed_readme = bench_dir / "README.md"
    sealed_readme.write_text("SEALED_PROBE: operator-only scoring rubric content")

    args = _sandboxed_args_for_workdir(["cat", str(sealed_readme)], workdir)
    assert args is not None

    proc = subprocess.run(
        args,
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
        env=_sanitized_env(),
    )
    assert proc.returncode != 0
    assert "SEALED_PROBE" not in proc.stdout


@linux_only
def test_relative_path_after_chdir_is_also_denied(tmp_path, monkeypatch):
    """Deny-by-prefix must survive `chdir` + relative-path opens (the most
    common way a Python/shell coder tool actually reads files), not just
    literal absolute-path opens."""
    holdouts_repo = tmp_path / "dark-factory-holdouts"
    holdouts_repo.mkdir()
    (holdouts_repo / "scenario.txt").write_text("secret-via-relative-path")
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts_repo))

    args = _sandboxed_args_for_workdir(
        [sys.executable, "-c", "print(open('scenario.txt').read())"],
        tmp_path,
    )
    assert args is not None

    proc = subprocess.run(
        args,
        cwd=holdouts_repo,
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
        env=_sanitized_env(),
    )
    assert proc.returncode != 0
    assert "secret-via-relative-path" not in proc.stdout


# ---------------------------------------------------------------------------
# (5) Full-stack: through the real `_codergen` handler, not just the helper.
#
# This is the strongest form of the "verify a real pipeline node runs while
# holdout reads stay denied" acceptance bar: a fake `claude` binary on PATH,
# invoked exactly the way `_codergen`'s claude branch invokes the real CLI
# (same argv construction, same env, same sandbox wrapper), attempts to leak
# a holdout file's content into its own stdout. A pipeline node calling this
# same backend for a *normal* task must still succeed end to end.
# ---------------------------------------------------------------------------


def _write_fake_claude_binary(bin_dir: pathlib.Path, marker_env_var: str) -> None:
    """A fake `claude` that tries to `cat` the path in `$<marker_env_var>`
    and reports the outcome on stdout, then always exits 0 (so `_codergen`'s
    success/failure branch reflects *containment*, not accidental process
    failure) -- the test asserts on the reported leak status, not on the
    handler's outcome field.
    """
    shim = bin_dir / "claude"
    shim.write_text(
        "#!/bin/sh\n"
        f'target="${{{marker_env_var}}}"\n'
        'if [ -z "$target" ]; then\n'
        '  echo "RESULT: no-marker-configured"\n'
        "  exit 0\n"
        "fi\n"
        'contents=$(cat "$target" 2>/dev/null)\n'
        'if [ -n "$contents" ]; then\n'
        '  echo "RESULT: LEAKED $contents"\n'
        "else\n"
        '  echo "RESULT: DENIED"\n'
        "fi\n"
        "exit 0\n"
    )
    shim.chmod(0o755)


@linux_only
def test_real_pipeline_node_runs_while_holdout_reads_stay_denied(tmp_path, monkeypatch):
    """End-to-end through `_codergen(backend="claude")`.

    Two calls through the exact same handler code path used by a real
    pipeline node:
      (a) the fake coder tries to read a holdout-path file -> DENIED.
      (b) the fake coder does a normal task with no holdout access -> the
          node still completes (`outcome == "success"`), proving the
          isolation wrapper doesn't break real pipeline execution.
    """
    holdouts_repo = tmp_path / "dark-factory-holdouts"
    holdouts_repo.mkdir()
    secret = holdouts_repo / "answer.txt"
    secret.write_text("THE-REAL-HOLDOUT-ANSWER")
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts_repo))

    bin_dir = tmp_path / "fake-bin"
    bin_dir.mkdir()
    _write_fake_claude_binary(bin_dir, marker_env_var="TEST_LEAK_TARGET")
    monkeypatch.setenv("PATH", f"{bin_dir}:{os.environ.get('PATH', '')}")

    workdir = tmp_path / "worktree"
    workdir.mkdir()
    # Direct Claude is an explicit, scoped backend. Give this end-to-end
    # isolation test a project config directory so it exercises the real
    # subprocess + LD_PRELOAD path rather than the fail-closed config check.
    claude_config_dir = tmp_path / "project-claude-config"
    claude_config_dir.mkdir()
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(claude_config_dir))

    # (a) Attempt to leak the holdout file.
    monkeypatch.setenv("TEST_LEAK_TARGET", str(secret))
    node = Node(name="coder_step", attrs={"type": "codergen", "backend": "claude"})
    ctx = Context(goal="attempt holdout leak", workdir=workdir, backend="claude")
    result = _codergen(node, ctx)
    assert "LEAKED" not in result.output, (
        f"holdout content leaked through a real _codergen(backend='claude') node:\n{result.output}"
    )
    assert "THE-REAL-HOLDOUT-ANSWER" not in result.output
    assert "DENIED" in result.output

    # (b) Same handler, same wrapper, a normal task with no holdout target.
    monkeypatch.delenv("TEST_LEAK_TARGET", raising=False)
    node2 = Node(name="coder_step_2", attrs={"type": "codergen", "backend": "claude"})
    ctx2 = Context(goal="do a normal task", workdir=workdir, backend="claude")
    result2 = _codergen(node2, ctx2)
    assert result2.outcome == "success", (
        f"a normal (non-holdout) pipeline node failed under the isolation wrapper: {result2.output!r}"
    )
    assert "no-marker-configured" in result2.output
