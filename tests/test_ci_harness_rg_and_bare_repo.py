"""Regression tests for CI bash harness ripgrep provisioning and bare-repo HEAD initialization.

Bead: jleechan-t5sw / issue #284.

All PRs can fail the bash integration job before feature evaluation on
self-hosted runners because:
  1. `test_factory_af_tick_structured_rc.sh` and `daemon/factory-ao-remediate.sh`
     (or `daemon/factory-af-tick.sh`) invoke `rg`, but self-hosted runner PATH
     has no `rg`.
  2. `test_deploy_af_tick.sh` and `test_factory_af_tick_drift_gate.sh` create empty
     bare remotes, then push main while remote HEAD/default branch is unset.

Acceptance:
  - provision and version-check a stable ripgrep on the self-hosted runner;
  - initialize bare test remotes and HEAD/main explicitly;
  - add an environment-health preflight that fails early with the missing dependency;
  - run the full bash integration lane on the same shared self-hosted selector;
  - keep the fix scoped and preserve existing assertions.
"""

from __future__ import annotations

import pathlib
import re

import yaml

ROOT = pathlib.Path(__file__).resolve().parent.parent
CI_YML = ROOT / ".github" / "workflows" / "ci.yml"
TEST_SCRIPTS = ROOT / "tests" / "scripts"


def _load_ci_workflow() -> dict:
    docs = list(yaml.safe_load_all(CI_YML.read_text()))
    if len(docs) != 1:
        raise AssertionError(f"ci.yml should be a single document; got {len(docs)}")
    return docs[0]


def test_ci_yml_contains_bootstrap_preflight():
    """`test` job must contain a Bootstrap-preflight step before dependencies."""
    doc = _load_ci_workflow()
    test_job = doc["jobs"]["test"]
    steps = test_job.get("steps", [])
    step_names = [s.get("name", "") for s in steps]
    assert any("Bootstrap-preflight" in name or "bootstrap preflight" in name.lower() for name in step_names), (
        "ci.yml `test` job must contain a Bootstrap-preflight step to fail early if core tools are missing"
    )


def test_ci_yml_provisions_ripgrep_multiarch():
    """`test` job must provision ripgrep across Linux and macOS (x86_64 and arm64)."""
    doc = _load_ci_workflow()
    test_job = doc["jobs"]["test"]
    env = test_job.get("env", {})
    assert "RG_VERSION" in env, "ci.yml `test` job env must define RG_VERSION"
    assert "RG_SHA_LINUX_X86_64" in env, "ci.yml `test` job env must define RG_SHA_LINUX_X86_64"
    assert "RG_SHA_LINUX_ARM64" in env, "ci.yml `test` job env must define RG_SHA_LINUX_ARM64"
    assert "RG_SHA_MACOS_X86_64" in env, "ci.yml `test` job env must define RG_SHA_MACOS_X86_64"
    assert "RG_SHA_MACOS_ARM64" in env, "ci.yml `test` job env must define RG_SHA_MACOS_ARM64"

    steps = test_job.get("steps", [])
    rg_step = None
    for step in steps:
        name = step.get("name", "")
        run = step.get("run", "")
        if "ripgrep" in name.lower() or "rg" in name.lower() or "RG_VERSION" in run:
            rg_step = step
            break
    assert rg_step is not None, "ci.yml `test` job must contain a ripgrep provisioning step"
    run_body = rg_step.get("run", "")
    assert "x86_64-unknown-linux-musl" in run_body, "ripgrep step must support Linux x86_64"
    assert "aarch64-unknown-linux-gnu" in run_body, "ripgrep step must support Linux ARM64"
    assert "x86_64-apple-darwin" in run_body, "ripgrep step must support macOS x86_64"
    assert "aarch64-apple-darwin" in run_body, "ripgrep step must support macOS ARM64"
    assert "GITHUB_PATH" in run_body, "ripgrep step must append directory to GITHUB_PATH"
    assert "export PATH=" in run_body, "ripgrep step must export PATH for the current shell step"


def test_ci_yml_bash_lane_preflight_verifies_rg():
    """`test` job preflight step must verify `rg`, `git`, `python3`, and `bash`."""
    doc = _load_ci_workflow()
    test_job = doc["jobs"]["test"]
    steps = test_job.get("steps", [])
    all_runs = "\n".join(s.get("run", "") for s in steps if s.get("run"))
    assert "for bin in bash rg git python3" in all_runs or (
        "command -v rg" in all_runs and "command -v git" in all_runs
    ), "ci.yml preflight must verify rg, git, python3, and bash on PATH before running tests"


def test_ci_yml_daemon_tests_bootstrap_preflight():
    """`daemon-tests` job must contain a bootstrap preflight verifying cargo and git."""
    doc = _load_ci_workflow()
    daemon_job = doc["jobs"]["daemon-tests"]
    steps = daemon_job.get("steps", [])
    step_names = [s.get("name", "") for s in steps]
    assert any("Bootstrap-preflight" in name or "bootstrap" in name.lower() for name in step_names), (
        "ci.yml `daemon-tests` job must contain a Bootstrap-preflight step"
    )
    all_runs = "\n".join(s.get("run", "") for s in steps if s.get("run"))
    assert "cargo" in all_runs and "git" in all_runs, (
        "ci.yml `daemon-tests` preflight must verify cargo and git"
    )


def test_bare_repo_fixtures_seed_initial_main_commit():
    """Bare test repos must be seeded with an initial commit to avoid empty clone warnings."""
    for script_name in [
        "test_deploy_af_tick.sh",
        "test_factory_af_tick_drift_gate.sh",
        "test_deploy_af_tick_extra_shas.sh",
    ]:
        script_path = TEST_SCRIPTS / script_name
        content = script_path.read_text()
        assert "git clone -q --bare" in content or "git clone --bare" in content, (
            f"{script_name} must seed the bare origin from an initialized repository with a main commit"
        )


def test_factory_af_tick_uses_stdlib_tomllib():
    """`daemon/factory-af-tick.sh` must use `tomllib` (with fallbacks), not raw `import toml`."""
    script_path = ROOT / "daemon" / "factory-af-tick.sh"
    content = script_path.read_text()
    assert "import toml\n" not in content and "import sys, toml\n" not in content, (
        "daemon/factory-af-tick.sh must not use bare `import toml` without fallback to stdlib `tomllib`"
    )
    assert "tomllib" in content, "daemon/factory-af-tick.sh must use stdlib `tomllib`"


if __name__ == "__main__":
    test_ci_yml_contains_bootstrap_preflight()
    test_ci_yml_provisions_ripgrep_multiarch()
    test_ci_yml_bash_lane_preflight_verifies_rg()
    test_ci_yml_daemon_tests_bootstrap_preflight()
    test_bare_repo_fixtures_seed_initial_main_commit()
    test_factory_af_tick_uses_stdlib_tomllib()
    print("All 6 test cases in test_ci_harness_rg_and_bare_repo.py passed!")

