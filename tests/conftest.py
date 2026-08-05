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
    if sub == "run":
        import tempfile
        scratch = pathlib.Path(tempfile.mkdtemp(prefix="run_conformance_"))
        # The sealed holdout evaluator resolves `implementation` relative to
        # ctx.workdir. Mirror the repo's `impl/` tree into the scratch workdir
        # so the hello-dot holdout can find greet.py and pass.
        impl_link = scratch / "impl"
        if not impl_link.exists():
            impl_link.symlink_to(ROOT / "impl")
        # Inject --workdir AFTER the subcommand since `bin/conformance` uses
        # subparsers(required=True); top-level flags are rejected.
        full_args = (sub, "--workdir", str(scratch), *args[1:])
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
