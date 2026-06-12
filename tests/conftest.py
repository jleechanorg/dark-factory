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


def run_conformance(*args: str, timeout: int = 60, env: dict | None = None) -> "subprocess.CompletedProcess[str]":
    """Run `bin/conformance` with the given args, return CompletedProcess."""
    return subprocess.run(
        [sys.executable, str(ROOT / "bin" / "conformance"), *args],
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
