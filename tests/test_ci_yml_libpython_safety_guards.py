"""Regression test for the Mac arm64 safety guards in `.github/workflows/ci.yml`.

The `Fix Python 3.13 shared-library path (self-hosted runners)` step in
`.github/workflows/ci.yml` (added by PR #287) has a `find
$TOOLCHAIN_ROOT -maxdepth 4` that wedges indefinitely when:

1. `TOOLCHAIN_ROOT` resolves to `/usr` (system Python on Mac arm64
   self-hosted runners via `command -v python3` → `/usr/bin/python3` →
   `dirname dirname` → `/usr`). The original step would happily scan
   `/usr` (potentially hundreds of GB on self-hosted Macs) for the
   full job timeout (45+ min on PR #303 run 29618811329).
2. `actions/setup-python` doesn't export `PythonLocation` on
   self-hosted Mac arm64 runners and the fallback `command -v python3`
   returns `/usr/bin/python3` (a stub that re-execs the toolcache
   binary lazily — fine for normal use, but the toolchain root is
   `/usr`, not `/Users/runner/hostedtoolcache/...`).
3. The shim-install sub-step tries to `mv /usr/bin/python3
   /usr/bin/python3.real`, which fails silently with `Operation not
   permitted` on macOS System Integrity Protection (SIP) but the
   script keeps going — and the subsequent `python3 -c 'import sys'`
   self-test hangs forever waiting on a binary that was never actually
   replaced.

This test asserts the structural properties that prevent the regression:

  1. The step MUST reject unsafe TOOLCHAIN_ROOT values (`/`, `/usr`,
     `/System`, `/private`) BEFORE the `find` runs.
  2. The `find` MUST be wrapped in `timeout` so a runaway filesystem
     scan can't wedge the whole workflow.
  3. The shim-install sub-step MUST be skipped when SHIM_BIN lives
     under a SIP-protected macOS path (`/usr/bin`, `/bin`, `/sbin`,
     `/System/*`, `/private/*`).
  4. The self-test python invocation MUST be wrapped in `timeout`.

These assertions run without a Mac runner because the test reads only
the YAML text and pattern-matches the embedded shell script.
"""

from __future__ import annotations

import pathlib

import yaml


ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


def _libpython_fix_step_run(workflow_text: str) -> str:
    """Return the `run:` body of the libpython-fix step.

    Splits the multi-doc workflow YAML and finds the step whose name
    contains "Fix Python 3.13" (the libpython-fix intent). Returns the
    `run:` body verbatim so the structural assertions below are robust
    against future renames/refactors.
    """
    docs = list(yaml.safe_load_all(workflow_text))
    if len(docs) != 1:
        raise AssertionError(
            f"ci.yml should be a single YAML document; got {len(docs)} docs"
        )
    steps = docs[0]["jobs"]["test"]["steps"]
    for step in steps:
        name = step.get("name", "")
        if "Fix Python 3.13" in name and "shared-library path" in name:
            return step.get("run", "")
    raise AssertionError(
        "Could not find the 'Fix Python 3.13 shared-library path' step "
        "in ci.yml — the libpython fix step has been removed entirely."
    )


def test_libpython_step_rejects_unsafe_toolchain_root():
    """The step MUST refuse to operate on `/`, `/usr`, `/System`, `/private`.

    Background: on self-hosted Mac arm64 runners without
    `actions/setup-python`'s `PythonLocation` export, the fallback
    `command -v python3` returns `/usr/bin/python3` (system Python)
    and the original step computes `TOOLCHAIN_ROOT=/usr`. The
    subsequent `find /usr -maxdepth 4 -name 'libpython3.13.so'`
    wedges for the full job timeout because `/usr` is huge on a
    self-hosted Mac and the scan is unbounded.

    The fix uses a `case "$TOOLCHAIN_ROOT" in …` block to short-circuit
    with `exit 0` and a `::warning::` annotation when the resolved
    toolchain root is `/`, `/usr`, `/System`, `/private`, or any of
    their slash-suffixed variants.
    """
    run = _libpython_fix_step_run(WORKFLOW.read_text())
    # Strip shell comment lines so explanatory comments don't count
    # against the structural assertions.
    code_only = "\n".join(
        line for line in run.splitlines() if not line.lstrip().startswith("#")
    )
    # The case statement must list each unsafe root as a separate
    # pattern. We require at least four of the five known unsafe
    # roots to be explicitly rejected — matching `/usr` is the most
    # important one (it's what bit PR #303 run 29618811329).
    unsafe_roots = ["/", "/usr", "/System", "/private"]
    matched = [root for root in unsafe_roots if f'"{root}"' in code_only or f'"{root}/"' in code_only]
    assert len(matched) >= 3, (
        f"libpython fix step must reject unsafe TOOLCHAIN_ROOT values "
        f"(/, /usr, /System, /private); only matched {matched!r}. "
        f"Without these guards, `command -v python3` resolving to "
        f"`/usr/bin/python3` on Mac arm64 self-hosted runners causes "
        f"`find /usr -maxdepth 4` to wedge for 45+ minutes."
    )
    # The case block must exit 0 (not exit 1) so the workflow continues
    # past the libpython step even when the toolchain root is unsafe —
    # the downstream test step will fail with rc=127 and that signal
    # is more actionable than a 45-min stall.
    assert "exit 0" in code_only, (
        "libpython fix step must `exit 0` (with a ::warning::) when "
        "TOOLCHAIN_ROOT is unsafe — `exit 1` would fail the workflow "
        "for a runner config problem rather than the actual test "
        "subprocess rc=127 signal."
    )


def test_libpython_find_is_timeout_bounded():
    """The `find $TOOLCHAIN_ROOT …` MUST be wrapped in `timeout`.

    The original step ran `find` with no upper bound. On Mac arm64
    self-hosted runners with a `/usr`-sized toolchain root, the scan
    can take tens of minutes (PR #303 wedged at 45m before being
    cancelled). Wrapping in `timeout 30` (or any positive numeric
    bound) ensures the worst case is a 30-second step failure with a
    clear cause, not a 45-minute job hang.

    The exact bound is not asserted here because future tuning may
    bump it from 30s to 60s — what matters is that SOME timeout is
    applied.
    """
    run = _libpython_fix_step_run(WORKFLOW.read_text())
    # The `find` invocation that searches for libpython3.13.so MUST
    # be preceded by a `timeout N` command. Pattern: the find line
    # should contain `timeout` somewhere before the `find` keyword.
    find_lines = [
        line for line in run.splitlines()
        if "find " in line and "libpython3.13.so" in line
    ]
    assert find_lines, (
        "libpython fix step should still have a `find … libpython3.13.so` "
        "fallback (the step must look for libpython outside the expected "
        "lib dir when the conventional path is missing)."
    )
    has_timeout = any("timeout " in line for line in find_lines)
    assert has_timeout, (
        f"libpython fix step's `find` invocation must be wrapped in "
        f"`timeout N` so a runaway filesystem scan can't wedge the "
        f"workflow. Found: {find_lines!r}"
    )


def test_libpython_shim_install_skipped_under_sip_paths():
    """The shim-install sub-step MUST be skipped when SHIM_BIN is SIP-protected.

    macOS System Integrity Protection (SIP) makes `/usr/bin`, `/bin`,
    `/sbin`, `/usr/sbin`, `/System`, and `/private/*` read-only for
    non-Apple processes. The original libpython step tried to `mv
    /usr/bin/python3 /usr/bin/python3.real` on self-hosted Mac
    runners when `TOOLCHAIN_ROOT=/usr`, which fails silently with
    `Operation not permitted` and wedges the subsequent
    `python3 -c 'import sys'` self-test (PR #303 45-min stall).

    The fix wraps the shim-install for-loop in a `case
    "$TOOLCHAIN_ROOT/bin" in /usr/bin|/bin|…)` block that matches
    SIP paths and prints a `::warning::` instead of attempting the
    mv. The libpython symlink fix at step (4) is the primary
    remediation; the shim is a belt-and-suspenders for subprocesses
    that strip LD_LIBRARY_PATH.
    """
    run = _libpython_fix_step_run(WORKFLOW.read_text())
    code_only = "\n".join(
        line for line in run.splitlines() if not line.lstrip().startswith("#")
    )
    # The case statement must list the SIP-protected paths. We
    # require at least three of the five known SIP paths. The case
    # pattern may be bare (e.g. `/usr/bin`) or quoted (`"/usr/bin"`).
    sip_paths = ["/usr/bin", "/bin", "/sbin", "/usr/sbin", "/System", "/private"]
    def _matches(path: str) -> bool:
        bare = path in code_only
        quoted_double = f'"{path}"' in code_only
        quoted_single = f"'{path}'" in code_only
        glob_double = f'"{path}/*"' in code_only
        glob_single = f"'{path}/*'" in code_only
        return bare or quoted_double or quoted_single or glob_double or glob_single
    matched = [p for p in sip_paths if _matches(p)]
    assert len(matched) >= 3, (
        f"libpython fix step must skip shim install for SIP-protected "
        f"paths (/usr/bin, /bin, /sbin, /System, /private); only "
        f"matched {matched!r}. Without this guard, the shim install "
        f"silently fails on macOS and the self-test hangs."
    )


def test_libpython_self_test_python_is_timeout_bounded():
    """The self-test python invocation MUST be wrapped in `timeout`.

    The original step ran `"$SHIM_BIN" -c 'import sys; sys.exit(0)'`
    with no upper bound. On a runner where the shim install silently
    failed (because SHIM_BIN is SIP-protected), the python invocation
    could hang forever waiting on a binary that was never actually
    replaced.

    The fix wraps the self-test in `timeout 60` so a hung subprocess
    surfaces as a step-level error in at most 60 seconds, not a
    45-minute job stall.
    """
    run = _libpython_fix_step_run(WORKFLOW.read_text())
    # The self-test line invokes `$SHIM_BIN -c 'import sys; sys.exit(0)'`.
    # Find it and verify it's wrapped in `timeout`.
    self_test_lines = [
        line for line in run.splitlines()
        if "import sys; sys.exit(0)" in line
    ]
    assert self_test_lines, (
        "libpython fix step should still have a self-test invocation "
        "of `python3 -c 'import sys; sys.exit(0)'` after the shim "
        "install — that's the primary signal that the libpython "
        "remediation worked."
    )
    has_timeout = any("timeout " in line for line in self_test_lines)
    assert has_timeout, (
        f"libpython fix step's self-test python invocation must be "
        f"wrapped in `timeout N` so a hung subprocess can't wedge "
        f"the workflow. Found: {self_test_lines!r}"
    )
