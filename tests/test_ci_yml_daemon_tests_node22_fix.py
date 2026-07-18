"""Regression test for the Node 22.22.0 provisioning fix in `.github/workflows/ci.yml`.

PR #287's `daemon-tests` job calls `cargo test` which executes
`adapters::ao_spawn_contract_tests` — a Rust test suite that invokes
Node 22 via the `bridge_test_node()` helper in `daemon/src/adapters.rs`.

`bridge_test_node()` looks for `~/.nvm/versions/node/v22.22.0/bin/node`
(the global CLAUDE.md "Use nvm Node 22 (v22.22.0)" convention) and
falls back to `node` on PATH. Self-hosted runners don't ship with nvm
or any Node 22 preinstalled — verified on PR #287 CI run 29549724870
where 5 bridge tests panicked with:

    FileNotFoundError: [Errno 2] No such file or directory
    ... os.execvp(os.environ["AO_FAKE_NODE"], ...)

The AO bridge tests wrap `ao spawn` invocations in a Python shim that
`os.execvp`s the `AO_FAKE_NODE` path. When that path is missing, the
shim never executes the Rust assertions, so 5 cargo tests fail with
`SpawnFallbackExhausted`.

The daemon-tests job MUST provision Node 22.22.0 to the exact path
`bridge_test_node()` consults, otherwise every PR's `daemon-tests`
job is a roll-the-dice on the runner's pre-existing node install.

This test asserts the structural properties that prevent the
regression:

  1. The `daemon-tests` job MUST contain a step whose name or run body
     installs Node 22.22.0 (matches `node-v22.22.0-…` tarball OR
     a pre-existing nvm-style install).
  2. The provision step MUST place the `node` binary at
     `~/.nvm/versions/node/v22.22.0/bin/node` (the path
     `bridge_test_node()` consults before falling back to PATH).
  3. The provision step MUST cover both `linux-x64` AND `linux-arm64`
     because the org fleet includes both architectures (Linux-x64
     runners are gradually being retired in favor of aarch64).
  4. The provision step MUST short-circuit cleanly when the binary
     already exists, so cached/ephemeral runners don't re-download.

These assertions run without Node installed because the test reads
only the YAML text and pattern-matches the embedded shell script.
"""

from __future__ import annotations

import pathlib
import re

import yaml


ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


def _daemon_tests_job(workflow_text: str) -> dict:
    """Return the `daemon-tests` job dict from ci.yml.

    Splits the multi-doc workflow YAML and locates the job whose key
    matches `daemon-tests`. Raises AssertionError if the job is missing
    or the YAML is malformed (so a typo in the job name surfaces here
    instead of as an opaque Cargo failure on the runner).
    """
    docs = list(yaml.safe_load_all(workflow_text))
    if len(docs) != 1:
        raise AssertionError(
            f"ci.yml should be a single YAML document; got {len(docs)} docs"
        )
    jobs = docs[0].get("jobs") or {}
    if "daemon-tests" not in jobs:
        raise AssertionError(
            "ci.yml is missing the `daemon-tests` job — the AO bridge "
            "test suite cannot run without it"
        )
    return jobs["daemon-tests"]


def _node_provision_step(job: dict) -> str:
    """Return the `run:` body of the Node 22.22.0 provision step.

    Finds the step whose name OR run body mentions Node 22.22.0 (either
    `node-v22.22.0` tarball download, an nvm-style `versions/node/v22.22.0`
    path, or the literal string `Node 22.22.0` in the step name). Returns
    the run body verbatim so the structural assertions below are robust
    against future renames/refactors.
    """
    steps = job.get("steps") or []
    for step in steps:
        name = step.get("name", "") or ""
        run = step.get("run", "") or ""
        haystack = f"{name}\n{run}"
        if (
            "v22.22.0" in haystack
            or "Node 22.22.0" in haystack
            or "node-v22.22.0" in haystack
        ):
            return run
    raise AssertionError(
        "ci.yml daemon-tests job is missing a Node 22.22.0 provision "
        "step — bridge_test_node() requires the binary at "
        "~/.nvm/versions/node/v22.22.0/bin/node"
    )


def test_daemon_tests_jobs_provisions_node_22_22_0():
    """The daemon-tests job MUST contain a Node 22.22.0 provision step.

    Background: PR #287 CI run 29549724870 panicked 5 tests in
    `adapters::ao_spawn_contract_tests` with `[Errno 2] No such file or
    directory` because `bridge_test_node()` returned a path that did not
    exist on the self-hosted runner. Without an explicit provision step
    the suite is permanently red on the org fleet.

    The fix downloads the NodeSource `node-v22.22.0-…` tarball and
    installs it under `~/.nvm/versions/node/v22.22.0/`, matching the
    fallback path `bridge_test_node()` consults.
    """
    workflow_text = WORKFLOW.read_text()
    job = _daemon_tests_job(workflow_text)
    run = _node_provision_step(job)
    # Confirm the step actually downloads Node 22.22.0 (the tarball name
    # is the strongest signal; the version literal is a weaker fallback
    # in case the implementation uses an actions/setup-node wrapper).
    has_tarball = bool(re.search(r"node-v22\.22\.0-[a-z0-9_-]+\.tar\.xz", run))
    has_version_literal = bool(re.search(r"\bv22\.22\.0\b", run))
    assert has_tarball or has_version_literal, (
        "Node 22.22.0 provision step must download a node-v22.22.0 tarball "
        "(or invoke an action that pins v22.22.0); neither pattern found. "
        "bridge_test_node() requires a v22.22.0 binary at the nvm path."
    )


def test_daemon_tests_provisions_to_nvm_22_22_0_path():
    """The provision step MUST install to `~/.nvm/versions/node/v22.22.0/bin/node`.

    This is the exact path `bridge_test_node()` consults in
    `daemon/src/adapters.rs:1942`:

        let node22 = std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
            .join(".nvm/versions/node/v22.22.0/bin/node");

    If the step installs anywhere else (e.g. `/usr/local/bin/node`,
    `/opt/node/bin/node`, a fresh `~/.nvm/versions/node/v22.23.1/`),
    the test still fails on the runner because `bridge_test_node()` will
    return either a missing path or `node` on PATH (which on a fresh
    self-hosted image is the Ubuntu-shipped Node 18 — incompatible with
    the v0.1.3 bridge).
    """
    workflow_text = WORKFLOW.read_text()
    job = _daemon_tests_job(workflow_text)
    run = _node_provision_step(job)
    nvm_path_patterns = [
        r"\$\{?HOME\}?/\.nvm/versions/node/v22\.22\.0",
        r"\$\{HOME\}/\.nvm/versions/node/v22\.22\.0",
        r"/\.nvm/versions/node/v22\.22\.0",
    ]
    matched = any(re.search(p, run) for p in nvm_path_patterns)
    assert matched, (
        "Node 22.22.0 provision step must install the binary to "
        "`~/.nvm/versions/node/v22.22.0/bin/node` (the path "
        "bridge_test_node() consults). Found neither the literal path "
        "nor an `nvm_path`-style variable assignment."
    )


def test_daemon_tests_provision_covers_linux_x64_and_arm64():
    """The provision step MUST handle linux-x64 AND linux-aarch64.

    The jleechanorg org self-hosted fleet is mixed: some hosts are
    `Linux-x86_64`, others are `Linux-aarch64` (M1-class ARM instances).
    A step that only handles `linux-x64` makes the ARM hosts red
    silently (no node binary → bridge tests panic with rc=127).

    The provision step must match both arches in a `case` statement
    (or use actions/setup-node with an `architecture:` matrix that
    covers both).
    """
    workflow_text = WORKFLOW.read_text()
    job = _daemon_tests_job(workflow_text)
    run = _node_provision_step(job)
    has_x64 = bool(re.search(r"linux-x64", run))
    has_arm64 = bool(re.search(r"linux-arm64", run))
    assert has_x64 and has_arm64, (
        f"Node 22.22.0 provision step must cover linux-x64 ({has_x64}) "
        f"AND linux-arm64 ({has_arm64}). The org fleet includes both "
        f"architectures; missing either arch makes that subset of "
        f"runners permanently red on the bridge tests."
    )


def test_daemon_tests_provision_short_circuits_when_present():
    """The provision step MUST short-circuit cleanly when the binary exists.

    Cached and ephemeral self-hosted runners may have Node 22.22.0
    preinstalled (or survive across runs). A provision step that
    unconditionally re-downloads wastes ~30s per CI run and depends on
    nodejs.org availability. The step MUST check `[ -x $NODE_BIN/node ]`
    first and `exit 0` when the binary already exists.
    """
    workflow_text = WORKFLOW.read_text()
    job = _daemon_tests_job(workflow_text)
    run = _node_provision_step(job)
    # Match common idempotency patterns: `[ -x … ]`, `if command -v`,
    # `test -x`. We don't pin to one form because the implementation
    # may legitimately vary; we only require that some short-circuit
    # guard exists before the curl/tar block.
    has_short_circuit = bool(
        re.search(r"\[\s*-x\s+\"\$\{?NODE_BIN\}?/node", run)
        or re.search(r"if\s+\[\[?\s*-x\s+", run)
        or re.search(r"command\s+-v\s+node\b", run)
    )
    assert has_short_circuit, (
        "Node 22.22.0 provision step must short-circuit when the binary "
        "already exists (e.g. `if [ -x \"${NODE_BIN}/node\" ]; then …`). "
        "Without an idempotency guard, every CI run re-downloads ~30MB "
        "even on cached runners."
    )
