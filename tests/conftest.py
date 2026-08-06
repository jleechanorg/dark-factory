"""Session-wide pytest configuration.

Declares the test environment contract that CI provides via the workflow
file (`DARK_FACTORY_HOLDOUTS=${{ github.workspace }}/tests/fixtures/holdout-eval`).
Without this, the 5 conformance/CLI tests that run the sealed holdout
evaluator against the impl tree silently fail: the real sealed repo (or
no env at all) returns `verdict=fail, scenarios=[]` for the `hello`
feature, which the fix loop then exhausts.

Five tests rely on this contract:
  - tests/test_conformance.py::test_conformance_run_uses_echo_backend_and_zero_cost
  - tests/test_conformance.py::test_conformance_run_supports_mock_url
  - tests/test_conformance.py::test_conformance_score_is_deterministic_mock_surface
  - tests/test_engine.py::test_cli_invocation_green
  - tests/test_evidence_bundle.py::test_cli_evidence_bundle_flag_creates_bundle

`DARK_FACTORY_HOLDOUTS` is the test fixture (`tests/fixtures/holdout-eval/`)
which contains a stub `evaluator/run.py` that checks `impl/greet.py` returns
"Hello, world!" for the `hello` feature.

Note: this conftest deliberately does NOT set `DISABLE_SANDBOX`. CI sets it
because CI runs on Linux where `sandbox-exec` is absent; on macOS local
dev `sandbox-exec` exists and tests like `test_ao_sandbox.py` rely on the
sandbox wrapper being active. Other tests (e.g. test_hardening) explicitly
set `DARK_FACTORY_HOLDOUTS` via `monkeypatch` to test env-stripping — those
overrides still work because monkeypatch restores on teardown.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

import pytest

from runner.parser import Node as _Node

FIXTURE_HOLDOUTS = pathlib.Path(__file__).resolve().parent / "fixtures" / "holdout-eval"
ROOT = pathlib.Path(__file__).resolve().parent.parent


def _pipeline(name: str) -> pathlib.Path:
    """Return the absolute path to a pipeline .dot file in pipelines/factory/."""
    return ROOT / "pipelines" / "factory" / name


def run_conformance(*args: str, timeout: int = 600, env: dict | None = None) -> "subprocess.CompletedProcess[str]":
    """Run `bin/conformance` with the given args, return CompletedProcess.

    The default 600s outer wrapper covers the `conformance score` chain
    (compileall + pytest + validate + run). It must be a strict superset of
    `bin/conformance:cmd_score`'s inner pytest cap (currently 240s) — when
    the warm suite is ~121s and the host is under CI load, an outer cap
    equal to the inner cap races the inner timeout and the wrapper fires
    first, returning `subprocess.TimeoutExpired` -> test red. Raising to
    600s gives a 360s margin and eliminates the flap-on-warm-run class.

    For the ``run`` subcommand (which directly invokes the runner), inject
    ``--workdir=<scratch>`` so fan-out branch isolation mkdtemp dirs land in
    the OS tempdir instead of leaking into the repo root. ``parse``,
    ``validate``, ``list-handlers``, and ``score`` do not accept
    ``--workdir`` so the scratch override is skipped for those. The
    ``score`` chain runs an inner pytest that itself uses test-local
    scratch workdirs via the per-file SCRATCH constants and the tests/
    fixtures we patched.
    """
    full_args = args
    sub = args[0] if args else ""
    scratch: pathlib.Path | None = None
    if sub == "run":
        import tempfile
        scratch = pathlib.Path(tempfile.mkdtemp(prefix="run_conformance_"))
        # The sealed holdout evaluator resolves `implementation` relative to
        # ctx.workdir. Mirror the repo's `impl/` tree into the scratch workdir
        # so the hello-dot holdout can find greet.py and pass.
        impl_link = scratch / "impl"
        if not impl_link.exists():
            impl_link.symlink_to(ROOT / "impl")
        # Strip any caller-supplied --workdir <path> so the scratch we inject
        # is authoritative. Otherwise a stray --workdir=<repo root> in a
        # fixture would re-introduce the branch_* disk leak this helper
        # exists to prevent.
        rest: list[str] = []
        skip_next = False
        for arg in args[1:]:
            if skip_next:
                skip_next = False
                continue
            if arg == "--workdir":
                skip_next = True
                continue
            if arg.startswith("--workdir="):
                continue
            rest.append(arg)
        full_args = (sub, "--workdir", str(scratch), *rest)
    if scratch is not None:
        # Clean up the scratch workdir after the subprocess exits or raises.
        # The runner is done with it once the subprocess returns, and the
        # OS will eventually reap it but explicit cleanup keeps the OS
        # tempdir from filling up under repeated invocations.
        import shutil
        try:
            return subprocess.run(
                [sys.executable, str(ROOT / "bin" / "conformance"), *full_args],
                cwd=ROOT,
                capture_output=True,
                text=True,
                timeout=timeout,
                env=env,
                check=False,
            )
        finally:
            shutil.rmtree(scratch, ignore_errors=True)
    return subprocess.run(
        [sys.executable, str(ROOT / "bin" / "conformance"), *full_args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
        check=False,
    )


def make_node(name: str = "test", **attrs) -> _Node:
    """Build a real runner.parser.Node instance for tests that need a stub.

    The audit found that test_gates.py and a few other tests used
    type("Node", (), {...})() ad-hoc stubs. Use this helper instead.
    """
    return _Node(name=name, attrs=attrs)


def hermetic_subprocess_env(**overrides) -> dict:
    """Build a minimal, explicit env for a `sys.executable` subprocess.

    Tests that shell out to `python -m runner...` deliberately pass a tiny
    hand-built env instead of inheriting `os.environ`, so the subprocess can't
    accidentally depend on the developer's shell. That hermeticity is correct
    and is preserved here — this helper only adds the one variable the
    interpreter needs in order to START.

    `LD_LIBRARY_PATH` is not a secret, it is a linker search path. GitHub's
    `setup-python` installs `libpython3.13.so.1.0` outside the default linker
    search path, so a `sys.executable` spawned WITHOUT `LD_LIBRARY_PATH` dies
    before running a single byte of our code:

        python: error while loading shared libraries: libpython3.13.so.1.0

    That failure is invisible locally (system python has libpython on the
    default path) and only appears on the hosted runner — which is why these
    tests passed on every dev box while `main` stayed red. Same rationale, same
    variable, as `runner/skeptic_gate_cli.py`'s `REVIEWER_ENV_BASE_ALLOWLIST`
    (see its LD_LIBRARY_PATH comment).

    Propagated only when actually set, so local hermetic runs are unchanged.
    """
    env = dict(overrides)
    ld = os.environ.get("LD_LIBRARY_PATH")
    if ld:
        env["LD_LIBRARY_PATH"] = ld
    return env


@pytest.fixture(autouse=True, scope="session")
def _declare_test_environment() -> None:
    """Set the env var every test in this directory expects."""
    os.environ.setdefault("DARK_FACTORY_HOLDOUTS", str(FIXTURE_HOLDOUTS))
    # DARK_FACTORY_HOME: lets the parser's `_resolve_includes` find
    # `@pipelines/_base.dot` from any cwd (e.g. a worktree). The bin/dark-factory
    # wrapper exports this by default; pytest doesn't, so tests that
    # `parse(pipelines/slim/*.dot)` from a worktree cwd raise "include not
    # found" for `@pipelines/_base.dot`. Set it to the install root (parent of
    # this conftest).
    os.environ.setdefault("DARK_FACTORY_HOME", str(ROOT))


# Registry of module-level SCRATCH workdirs created by tests that
# default `Context(workdir=...)` to a tempfile.mkdtemp. Tests register
# their SCRATCH path via `register_scratch_dir(SCRATCH)` at import time
# (after the mkdtemp call); the pytest_sessionfinish hook below removes
# them on session teardown so the OS tempdir doesn't accumulate
# branch_* sibling dirs across many pytest runs.
_SCRATCH_DIRS: list[pathlib.Path] = []


def register_scratch_dir(path: pathlib.Path) -> pathlib.Path:
    """Register a module-level SCRATCH workdir for session-end cleanup.

    Returns ``path`` unchanged so test files can write
    ``SCRATCH = register_scratch_dir(pathlib.Path(tempfile.mkdtemp(...)))``.
    """
    _SCRATCH_DIRS.append(path)
    return path


def pytest_sessionfinish(session, exitstatus):  # noqa: ARG001
    """Remove every registered module-level SCRATCH workdir at session end.

    Hook fires after the test session finishes; using a hook (not a fixture)
    guarantees cleanup runs even when no test requests the fixture.
    """
    import shutil
    for scratch in _SCRATCH_DIRS:
        shutil.rmtree(scratch, ignore_errors=True)
    _SCRATCH_DIRS.clear()
