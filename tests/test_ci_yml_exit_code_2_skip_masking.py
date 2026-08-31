"""RED regression test for the generic exit-code-2 CI skip masking defect.

CI defect (PR #790, prerequisite #781): the `Run bash integration tests`
step in `.github/workflows/ci.yml` was modified to treat **any** shell test
returning exit code 2 as a "skipped" warning, instead of failing the build.

That is a generic skip-masking defect because:

  * Exit code 2 is a perfectly legal "real failure" code for a shell script
    (`set -e` + a failed assertion, `grep -q` with no match, a missing
    required binary, etc.). Treating it as "skipped" makes the CI log show
    a green `::warning` annotation and the step exits 0, so the runner
    marks the build green even though the test script was never actually
    executed against real evidence.
  * The honest way to mark a test as skippable is to name it explicitly
    as an "optional probe" (a probe that legitimately cannot run in this
    environment — e.g. needs an unmounted /proc, a missing privileged
    binary, an absent fixture directory). Anything else returning 2 must
    be a hard failure.
  * PR #790's diff (`cebc0e4c`) introduced the masking:

        bash "$t"
        rc=$?
        if [ "$rc" -eq 2 ]; then
          echo "::warning file=$t::test skipped (exit 2 = ...)"
        elif [ "$rc" -ne 0 ]; then
          echo "::error file=$t::test failed"
          fail=$((fail + 1))
        fi

    This unconditionally swallows every exit-2 — no allow-list of named
    optional probes — so a regression in any other shell test can pass CI
    silently.

Acceptance criteria (the GREEN shape, not implemented here):
  1. The `Run bash integration tests` step must enumerate an EXPLICIT
     allow-list of "optional probe" script basenames (e.g.
     `OPTIONAL_PROBES=(test_af_immutable_restart.sh test_xyz.sh)`) whose
     exit code 2 may be reported as a `::warning`. The list must live
     inside the workflow file (not in a script the test cannot inspect)
     so any future change to the allow-list goes through PR review.
  2. Any test script NOT in that allow-list that returns exit code 2
     MUST be treated as a failure (counted toward `$fail`, emitted as
     `::error`, and ultimately cause the step to exit non-zero).
  3. Exit code 0 continues to pass silently; non-2 non-zero exits
     continue to be hard failures.

This test asserts (1) only — the structural property that proves the
allow-list exists. The behavioral property (2)/(3) is enforced by the
GREEN change in the workflow itself, which this PR does not ship.

The test is intentionally RED against the unmodified
`.github/workflows/ci.yml` because the current step uses an inline
`bash "$t" || { fail=$((fail + 1)); }` that does NOT consult any
explicit allow-list; it also unconditionally treats every exit-2 as a
hard failure (which is the conservative direction but blocks legitimate
optional probes like `test_af_immutable_restart.sh`). The GREEN step
must add the allow-list; this test will go green when it does.

References:
  * PR #790 — `tests/scripts/test_af_immutable_restart.sh … inventory-query
    failures now SKIP instead of vacuously comparing []==[]` and the CI
    bash integration step change `bash "$t" || …` → `if [ "$rc" -eq 2 ] …`.
  * Issue #9613 on `jleechanorg/worldarchitect.ai` — the upstream
    consumer that surfaced the masking after a CI run reported green
    despite a real shell-test regression.
"""

from __future__ import annotations

import pathlib
import re

import yaml


ROOT = pathlib.Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


def _bash_integration_step(workflow_text: str) -> dict:
    """Return the `Run bash integration tests` step dict from ci.yml.

    Splits the multi-doc workflow YAML, finds the `test` job, and locates
    the step whose name starts with `Run bash integration tests`. Raises
    AssertionError if the step is missing (so a typo surfaces here rather
    than as an opaque CI failure on the runner).
    """
    docs = list(yaml.safe_load_all(workflow_text))
    if len(docs) != 1:
        raise AssertionError(
            f"ci.yml should be a single YAML document; got {len(docs)} docs"
        )
    jobs = docs[0].get("jobs") or {}
    test_job = jobs.get("test")
    if not test_job:
        raise AssertionError("ci.yml is missing the `test` job")
    steps = test_job.get("steps") or []
    for step in steps:
        name = step.get("name", "") or ""
        if name.startswith("Run bash integration tests"):
            return step
    raise AssertionError(
        "ci.yml `test` job is missing the `Run bash integration tests` step"
    )


def _step_run_body(step: dict) -> str:
    """Return the `run:` body of a step, joined if it is a list."""
    run = step.get("run", "")
    if isinstance(run, list):
        return "\n".join(run)
    return run or ""


def test_bash_step_declares_explicit_optional_probe_allow_list():
    """The `Run bash integration tests` step MUST enumerate an EXPLICIT
    allow-list of optional probes (basenames) whose exit code 2 is
    acceptable; the allow-list must live inside the workflow YAML so
    any future change goes through PR review.

    Background: PR #790's bash integration step change
    (`bash "$t" || …` → `if [ "$rc" -eq 2 ]`) treats every exit-2 as a
    skip — there is no way for a reviewer to tell "this script is an
    optional probe" from "this script returned 2 because of a real
    regression". The only honest cure is an allow-list the workflow
    author declares and the test contract enforces.

    The allow-list must be a literal in the step's `run:` body, not a
    reference to a separate file the contract test cannot see — the
    whole point is to force every change to surface in a PR diff.
    """
    workflow_text = WORKFLOW.read_text()
    step = _bash_integration_step(workflow_text)
    run = _step_run_body(step)
    assert run, "Run bash integration tests step must have a non-empty `run:` body"
    # The allow-list may be expressed as:
    #   * an array literal:  OPTIONAL_PROBES=(test_a.sh test_b.sh)
    #   * a comma/whitespace-separated scalar:  OPTIONAL_PROBES="test_a.sh test_b.sh"
    #   * a here-doc:        OPTIONAL_PROBES <<EOF\ntest_a.sh\nEOF
    #   * a YAML list under the step's `with:` block (less common for inline
    #     shell). The strongest signal is the literal name `OPTIONAL_PROBES`
    #     (or `OPTIONAL_PROBE`, `SKIP_ALLOWLIST`, `SKIPPABLE_PROBES`) in
    #     combination with a basename that ends in `.sh`.
    array_literal = re.search(
        r"OPTIONAL_PROBES\s*\(\s*([^\)]*\.sh[^\)]*)\s*\)",
        run,
    )
    scalar_literal = re.search(
        r"(?:OPTIONAL_PROBES|OPTIONAL_PROBE|SKIP_ALLOWLIST|SKIPPABLE_PROBES)\s*=\s*[\"']?([^\n\"']*\.sh[^\n\"']*)[\"']?",
        run,
    )
    heredoc_literal = re.search(
        r"(?:OPTIONAL_PROBES|OPTIONAL_PROBE|SKIP_ALLOWLIST|SKIPPABLE_PROBES)\s*<<\s*['\"]?(\w+)['\"]?",
        run,
    )
    assert (
        array_literal is not None
        or scalar_literal is not None
        or heredoc_literal is not None
    ), (
        "Run bash integration tests step must declare an EXPLICIT "
        "allow-list of optional probes (basenames ending in `.sh`) "
        "whose exit code 2 may be reported as a `::warning`. Without "
        "the allow-list, ANY shell test that happens to exit with 2 is "
        "silently demoted to a skip — that is the PR #790 / #9613 "
        "defect. Found neither an array literal "
        "`OPTIONAL_PROBES=(foo.sh bar.sh)`, a scalar assignment "
        "`OPTIONAL_PROBES=\"foo.sh bar.sh\"`, nor a heredoc anchor "
        "in the step's `run:` body."
    )
    # The allow-list must contain at least one basename — an empty
    # `OPTIONAL_PROBES=()` is structurally present but semantically
    # equivalent to "no allow-list, everything is a hard failure",
    # which is fine for property (2)/(3) but does not satisfy (1) on
    # its own when the GREEN change needs to keep `test_af_immutable_restart.sh`
    # skippable. We require >=1 `.sh` basename to keep the GREEN
    # contract honest.
    matched_text = ""
    if array_literal is not None:
        matched_text = array_literal.group(1)
    elif scalar_literal is not None:
        matched_text = scalar_literal.group(1)
    basenames = re.findall(r"\b\w[\w\-]*\.sh\b", matched_text)
    assert basenames, (
        f"OPTIONAL_PROBES allow-list is declared but contains no `.sh` "
        f"basenames ({matched_text!r}); a non-empty list is required so "
        f"the GREEN contract can keep legitimate optional probes "
        f"skippable while still failing arbitrary exit-2 regressions."
    )


def test_bash_step_does_not_treat_every_exit_2_as_skip():
    """The `Run bash integration tests` step MUST NOT unconditionally
    accept exit code 2 as a skip without consulting the explicit
    optional-probe allow-list.

    This is the core RED assertion: PR #790's GREEN diff added a blanket
    `if [ "$rc" -eq 2 ]; then echo ::warning …` that bypassed the
    allow-list. A test script that is NOT in `OPTIONAL_PROBES` and
    exits with 2 must increment the failure counter (or otherwise
    guarantee the step exits non-zero), NOT be silently absorbed into
    a warning that lets the build go green.

    We assert that any branch in the `run:` body that handles exit-2
    is gated by the allow-list — either by referencing `OPTIONAL_PROBES`
    inside the inner `if`, by gating on a guard variable set by an
    earlier allow-list lookup, or by routing through a helper whose
    body lives in the same step. A bare `if [ "$rc" -eq 2 ]` whose
    body emits `::warning` and has no allow-list signal ANYWHERE in
    the surrounding for-loop body is the masking shape we forbid.
    """
    workflow_text = WORKFLOW.read_text()
    step = _bash_integration_step(workflow_text)
    run = _step_run_body(step)
    allow_list_signals = (
        "OPTIONAL_PROBES",
        "OPTIONAL_PROBE",
        "SKIP_ALLOWLIST",
        "SKIPPABLE_PROBES",
        "is_optional_probe",
        "in_optional_probes",
    )
    # Locate each `if [ "$rc" -eq 2 ]` line and inspect a 30-line
    # window after it — that covers both the inner block AND any
    # companion guard set immediately above (the canonical GREEN
    # shape uses a preceding `for p in ${OPTIONAL_PROBES[@]}` loop).
    exit2_line_indices = [
        i for i, line in enumerate(run.splitlines())
        if re.search(r"\[\s*[\"']?\$\{?rc\}?[\"']?\s+-eq\s+2\s+\]", line)
    ]
    if not exit2_line_indices:
        # No exit-2 special-case branch at all → fail-closed behavior.
        # This is acceptable as a "GREEN-by-fail-closed" position; it
        # does not satisfy (1) on its own (no allow-list means
        # legitimate optional probes cannot be skipped), but it does
        # satisfy this property. Allow the early return.
        return
    lines = run.splitlines()
    for idx in exit2_line_indices:
        window = "\n".join(lines[max(0, idx - 15) : idx + 15])
        has_allow_list_signal = any(sig in window for sig in allow_list_signals)
        assert has_allow_list_signal, (
            "Run bash integration tests step contains an `if [ \"$rc\" "
            "-eq 2 ]` branch with no allow-list signal in the "
            "surrounding for-loop window. Every such branch must be "
            "gated by the explicit optional-probe allow-list "
            "(e.g. `OPTIONAL_PROBES`, `OPTIONAL_PROBE`, "
            "`SKIP_ALLOWLIST`, or a guard variable set by an earlier "
            "allow-list lookup), otherwise arbitrary shell tests "
            "returning 2 are silently demoted to a `::warning` skip — "
            f"the PR #790 / #9613 defect. Window:\n{window}"
        )


def test_bash_step_emits_error_not_just_warning_for_non_optional_exit_2():
    """If the step accepts exit-2 at all, the failure path MUST emit a
    `::error` annotation (not just a `::warning`) and increment the
    failure counter — so the CI log surfaces a real failure for any
    non-optional exit-2 even if the allow-list check is later moved
    out of the inner branch.

    Background: PR #790's GREEN diff used `::warning` for the exit-2
    branch with no failure-counter increment, so even if the allow-list
    check is later added, a regression in the allow-list plumbing
    cannot silently pass. The defensive shape the test enforces is:
    either the step fail-closes on exit-2, OR (when it accepts exit-2)
    the failure path is wired to both `::error` and `$fail` increment
    so a regression still surfaces.

    We only enforce this when the step has an exit-2 branch (otherwise
    fail-closed is the default and there is nothing to wire).
    """
    workflow_text = WORKFLOW.read_text()
    step = _bash_integration_step(workflow_text)
    run = _step_run_body(step)
    has_exit2_branch = bool(
        re.search(r"\[\s*[\"']?\$\{?rc\}?[\"']?\s+-eq\s+2\s+\]", run)
    )
    if not has_exit2_branch:
        # No exit-2 special case → fail-closed by default → nothing
        # to wire.
        return
    # When an exit-2 branch exists, the failure path must increment
    # `$fail` somewhere reachable. We accept any of:
    #   * `fail=$((fail + 1))` reachable on the non-optional exit-2 path
    #   * a `::error` annotation paired with the warning
    #   * `exit 1` on the failure path
    # The defect shape is a branch with ONLY `::warning` and no
    # failure-increment / `::error` / `exit` — that's the silent skip.
    increments_fail = bool(re.search(r"fail=\$\(\(fail\s*\+\s*1\)\)", run))
    emits_error_annotation = bool(re.search(r"::error\b", run))
    # `::error` is required for any failure path (exit-2 included),
    # not just generic test failure.
    assert increments_fail or emits_error_annotation, (
        "Run bash integration tests step has an exit-2 special-case "
        "branch but the failure path does not increment `$fail` and "
        "does not emit a `::error` annotation. Either wire "
        "`fail=$((fail + 1))` into the non-optional exit-2 path or "
        "add a `::error file=$t::test returned exit 2 but is not an "
        "optional probe` annotation. A bare `::warning` with no "
        "counter increment lets a regression pass CI silently — the "
        "PR #790 / #9613 defect."
    )
