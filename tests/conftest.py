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
    """
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


def mock_pre_gate_reviewers(monkeypatch) -> None:
    """Stub the pre-gate reviewer nodes so gate/engine tests can seed outcomes.

    `gates.dot` / `pr_gates.dot` place `gate_skeptic` and
    `adversarial_reviewer` between `holdout` and `gate_es`. Neither honors
    the plain `ctx.state["<node>.outcome"]` seeding convention on its own:

      - `gate_skeptic` has no registered handler, so it falls through to
        `_codergen`, which only reads seeded state under the echo backend.
      - `adversarial_reviewer` is `type="parallel_reviewer"` and carries
        `review_contract="cold-review-v1"`. `_parallel_reviewer` refuses to
        take its echo shortcut for a contract-bound reviewer, so that a gate
        can never certify itself from its own fixture.

    Without this stub, a seeded `adversarial_reviewer.outcome="success"` is
    ignored, the node returns non-success, and
    `adversarial_reviewer -> fix [condition="outcome!=success"]` diverts the
    run into the bounded fix loop, breaking node-sequence assertions.

    Replacing the handler (rather than weakening it) is the supported way to
    drive graph topology in tests. Registered by handler *type*, so it works
    under any backend.
    """
    from runner.handlers import TYPE_REGISTRY, Result

    def _seeded(node, ctx):
        pre = ctx.state.get(f"{node.name}.outcome")
        return Result(outcome=pre or "success", output=f"stub_reviewer({node.name})")

    monkeypatch.setitem(TYPE_REGISTRY, "gate_skeptic", _seeded)
    monkeypatch.setitem(TYPE_REGISTRY, "parallel_reviewer", _seeded)


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
