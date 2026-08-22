"""Regression test for the libpython3.13.so.1.0 fix in `.github/workflows/ci.yml`.

The PR #287 libpython symlink step historically called `python3 -c 'import
sys; print(sys.prefix)'` to discover `sys.prefix`. When the versioned soname
(`libpython3.13.so.1.0`) was missing on a self-hosted runner, that `python3`
invocation itself returned rc=127, so `PY_PREFIX` was empty, the symlink fix
never fired, and the test step blew up later with the same rc=127.

This test parses `.github/workflows/ci.yml` and asserts the structural
properties that prevent the regression:

  1. The step that handles Python 3.13 shared-library path MUST NOT call
     `python3` (or `python`) for self-discovery. It may fall back to
     ``command -v python3`` (a shell builtin) or ``readlink -f`` (a binary
     that exists on the runner image independently of python's shared libs).
  2. The step must add a versioned soname symlink for libpython3.13.so to
     allow subprocess python3 to start.
  3. The step must self-test the fix by invoking the toolchain `python3`
     explicitly with rc=0 checks so a remaining rc=127 surfaces as a
     step-level error (not as a whole-test-suite failure).

These assertions run without `libpython` available because the test reads
only the YAML text and pattern-matches the embedded shell script.
"""

from __future__ import annotations

import pathlib
import re

import yaml


ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


def _get_workflow_doc(workflow_text: str) -> dict:
    """Return the single parsed document for ci.yml."""
    docs = list(yaml.safe_load_all(workflow_text))
    if len(docs) != 1:
        raise AssertionError(
            f"ci.yml should be a single YAML document; got {len(docs)} docs"
        )
    return docs[0]


def _libpython_fix_step_dict(workflow_text: str) -> dict:
    """Return the step dict of the libpython-fix step."""
    doc = _get_workflow_doc(workflow_text)
    steps = doc["jobs"]["test"]["steps"]
    for step in steps:
        name = step.get("name", "")
        if "Python 3.13" in name and "shared-library path" in name:
            return step
    raise AssertionError("Could not find the Python 3.13 libpython fix step in ci.yml")


def _libpython_fix_step(workflow_text: str) -> str:
    """Return the embedded shell script of the libpython-fix step.

    Splits the multi-doc workflow YAML and finds the step whose name matches
    the libpython-fix intent. Returns its `run:` body verbatim so the shell
    assertions below are robust against future renames/refactors.
    """
    step = _libpython_fix_step_dict(workflow_text)
    return step.get("run", "")


def test_fix_step_avoids_python3_for_self_discovery():
    """The step MUST NOT call `python3 -c` or `python -c` for discovery.

    Background: actions/setup-python installs Python 3.13 into the GH
    toolcache without a versioned soname symlink. When subprocess python3
    is invoked under that toolchain, it returns rc=127 with
    `error while loading shared libraries: libpython3.13.so.1.0`. A
    self-discovery line like `PY_PREFIX="$(python3 -c 'import sys; print(sys.prefix)')"`
    then evaluates to an empty string and the remediation never fires.

    The fix uses `command -v python3` (a shell builtin) and `readlink -f`
    (a binary that exists on the runner image independently of libpython)
    to find the toolchain without going through the broken dynamic loader.

    A subprocess `python3 -c …` IS allowed AFTER the symlink is created,
    because at that point libpython is loadable. This test forbids only
    the discovery-time invocation: any line that runs `python3 -c/-` BEFORE
    the symlink is created.
    """
    run = _libpython_fix_step(WORKFLOW.read_text())
    # Strip shell comment lines (start with `#` after optional whitespace)
    # so that explanatory comments are not falsely matched as code.
    code_only = "\n".join(
        line for line in run.splitlines() if not line.lstrip().startswith("#")
    )
    lines = code_only.splitlines()
    # find the line index of the `ln -s` invocation that creates the
    # versioned soname symlink. The exact name may be in a variable or a
    # literal, so match the `ln -s` pattern and the `so.1.0` literal in
    # the surrounding 5 lines.
    symlink_line = None
    for i, line in enumerate(lines):
        if "ln -s" in line:
            window = "\n".join(lines[max(0, i - 3) : i + 5])
            if "libpython3.13.so.1.0" in window or "${VERSIONED}" in line:
                symlink_line = i
                break
    assert symlink_line is not None, (
        "Could not find the `ln -s …libpython3.13.so.1.0` line in the fix "
        "step — the symlink must exist before we can validate that no "
        "discovery-time python invocations come before it"
    )
    prefix = "\n".join(lines[:symlink_line])
    # Disallow any `python3 -c …` / `python -c …` / `python3 -` / etc. in the
    # prefix (i.e., before the symlink fix):
    forbidden = re.search(r"\bpython[23]?\b\s+-[cp]\b", prefix)
    assert forbidden is None, (
        "libpython fix step must not call `python -c/-p/- …` BEFORE the "
        "symlink fix is applied, because the broken libpython would "
        "reject the call (rc=127) before remediation runs. Use "
        "`command -v python3` and `readlink -f` for discovery instead. "
        f"Found: {prefix[forbidden.start():forbidden.end()]!r}"
    )


def test_fix_step_creates_versioned_soname_symlink():
    """The step must create the `libpython3.13.so.1.0` symlink.

    Without it the dynamic linker rejects `python3` with rc=127.
    """
    run = _libpython_fix_step(WORKFLOW.read_text())
    assert "ln -s" in run, (
        "libpython fix step must use `ln -s` to create the missing "
        "`libpython3.13.so.1.0` -> `libpython3.13.so` symlink"
    )
    assert "libpython3.13.so.1.0" in run, (
        "libpython fix step must explicitly create the versioned soname "
        "`libpython3.13.so.1.0`"
    )


def test_fix_step_emits_subprocess_python_rc_check():
    """The step must self-test the fix by rc-checking subprocess python.

    Critically, this self-test must happen INSIDE the libpython-fix step
    (not in `Run tests`), so that a remaining rc=127 surfaces as a
    descriptive step error here, not as an opaque test-suite crash later.
    """
    run = _libpython_fix_step(WORKFLOW.read_text())
    # The step must explicitly invoke a subprocess python binary AND check rc.
    # The PR head commit `202eddc` lacked this — it relied on the next test
    # step to surface the regression with no pre-flight check.
    # Match `<toolchain-dir>/python3 -c …` (the toolchain prefix may be a
    # shell variable like $TOOLCHAIN_BIN_DIR or ${PythonLocation}; YAML
    # may re-quote the single-quoted python -c argument). Look for any
    # execution line `<something>/python3 -c` and a corresponding rc check.
    assert re.search(r"python[23]?\s+-c\b", run), (
        "libpython fix step must self-test by executing "
        "`python3 -c '<import-expression>'` (the toolchain python3) and "
        "asserting rc=0, so a remaining rc=127 surfaces as a step-level "
        "error before the test step runs"
    )
    # rc check: explicit numeric compare on ${rc}.
    has_rc_compare = bool(re.search(r"-ne\s+0\b|\"-eq\s+0\b", run))
    has_set_e = bool(re.search(r"^\s*set\s+-[a-zA-Z]*e\b", run, re.MULTILINE))
    assert has_rc_compare or has_set_e, (
        "libpython fix step must check rc explicitly (or run with set -e) "
        "after invoking subprocess python3, so a remaining rc=127 surfaces "
        "as a hard error here"
    )


def test_fix_step_uses_action_aware_discovery():
    """The step must derive the toolchain dir from `PythonLocation` or fall
    back to `command -v python3` + `readlink -f` — both of which work even
    when the libpython linker is broken.
    """
    run = _libpython_fix_step(WORKFLOW.read_text())
    assert "PythonLocation" in run, (
        "libpython fix step must consult `PythonLocation` env var exported "
        "by actions/setup-python as the primary discovery source"
    )
    assert "command -v python3" in run or "readlink -f" in run, (
        "libpython fix step must include a fallback (`command -v python3` "
        "and/or `readlink -f`) to discover the toolchain without invoking "
        "python and crashing on the missing libpython soname"
    )


def test_ci_jobs_have_timeout_guards():
    """All jobs in ci.yml must define `timeout-minutes` to avoid wedging self-hosted runners.

    Without a job-level timeout, a hung setup step or test suite defaults to
    GitHub's 360-minute (6-hour) timeout, blocking the entire self-hosted
    runner fleet for hours (as occurred on ez-runner-c-8 in run 30776197538).
    """
    doc = _get_workflow_doc(WORKFLOW.read_text())
    jobs = doc.get("jobs", {})
    assert "test" in jobs, "ci.yml must define a `test` job"
    assert "daemon-tests" in jobs, "ci.yml must define a `daemon-tests` job"

    for job_name in ("test", "daemon-tests"):
        job = jobs[job_name]
        timeout = job.get("timeout-minutes")
        assert timeout is not None, (
            f"ci.yml job `{job_name}` must define `timeout-minutes` to prevent "
            "hung jobs from blocking self-hosted runners indefinitely"
        )
        assert isinstance(timeout, int) and 0 < timeout <= 30, (
            f"ci.yml job `{job_name}` timeout-minutes must be between 1 and 30 minutes; got {timeout}"
        )


def test_fix_step_has_timeout_guard():
    """The libpython fix step must define a step-level `timeout-minutes`.

    A dedicated step timeout (e.g. 2 minutes) ensures that any hang inside the
    step aborts immediately without waiting for the full job timeout.
    """
    step = _libpython_fix_step_dict(WORKFLOW.read_text())
    timeout = step.get("timeout-minutes")
    assert timeout is not None, (
        "libpython fix step in ci.yml must define `timeout-minutes` (e.g. 2) "
        "so any runner hang fails fast"
    )
    assert isinstance(timeout, int) and 0 < timeout <= 5, (
        f"libpython fix step timeout-minutes must be <= 5 minutes; got {timeout}"
    )


def test_fix_step_uses_timeout_guard_on_subprocess_python():
    """The libpython self-test and search commands must use command-level `timeout`.

    If the toolchain python3 binary or dynamic linker deadlocks or hangs
    on initialization, `timeout 15s "$PY_TEST"` guarantees the command
    exits with rc=124 and surfaces a step failure immediately.
    """
    run = _libpython_fix_step(WORKFLOW.read_text())
    # Assert timeout is used on the python self-test invocation
    assert re.search(r'timeout\s+\d+s?\s+"?\$PY_TEST"?', run) or re.search(
        r'timeout\s+\d+s?\s+"?\$TOOLCHAIN_ROOT/bin/python3"?', run
    ), (
        "libpython fix step self-test must wrap python3 invocation in "
        '`timeout <N>s "$PY_TEST" -c ...` to fail fast on deadlocks/hangs'
    )


def test_fix_step_uses_atomic_force_symlink():
    """The step must use `ln -sf` (or `ln -s -f`) to overwrite existing/broken symlinks.

    If a broken symlink already exists from a previous runner state, `ln -s`
    without force fails with 'File exists'.
    """
    run = _libpython_fix_step(WORKFLOW.read_text())
    assert re.search(r"ln\s+-[a-zA-Z]*f[a-zA-Z]*\s+", run) or re.search(
        r"ln\s+-[a-zA-Z]*s\s+-[a-zA-Z]*f", run
    ), (
        "libpython fix step must use `ln -sf` so existing or broken symlinks "
        "are safely overwritten without error"
    )
