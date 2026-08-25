"""Structural tests for `.github/workflows/skeptic-gate-caller.yml`.

Bead: jleechan-5n0o — make the Skeptic PASS gate mandatory on every
dark-factory PR.

The SHA-bound skeptic gate (`.github/workflows/skeptic-gate.yml`)
already exists; the bootstrap caller (`.github/workflows/skeptic-gate-caller.yml`)
ships as a 71-line YAML with only the `on:` trigger block, but **no
`jobs:` section** — so opening a PR runs the trigger but never invokes
the gate. Without a `jobs.<name>.uses:` reusable-workflow invocation
that pins the gate to the caller's commit SHA on `main`, the gate is
advisory, not mandatory: a PR with the gate workflow YAML missing the
jobs section would still pass CI.

These tests enforce the invariant the bootstrap depends on:

1. The caller file MUST define at least one job (no jobs → no gate run).
2. The job's `uses:` MUST be the reusable-workflow form pinned to a
   specific 40-hex SHA on the default branch — never `@main`, never
   `@v1`, never a moving ref. This is the immutable-code-ref invariant
   the gate itself enforces (see skeptic-gate.yml lines 244-311).
3. The reusable workflow path MUST reference `skeptic-gate.yml` (the
   SHA-bound gate), not some other workflow.
4. `pull_request_target` MUST remain the trigger (the only surface that
   lets a same-target-repo bot post comments + commit statuses; PR-head
   `pull_request` is forgeable, workflow_dispatch cannot satisfy gate 7).
5. `trusted_code_sha` MUST be forwarded to the gate as an input,
   derived from `github.sha` (the caller's commit SHA on main) — this
   is the only way the gate can verify the checkout matches the
   caller-pinned ref.

If any of these invariants is violated, the gate is no longer
"mandatory" — a PR can merge without a Skeptic verdict.
"""

from __future__ import annotations

import pathlib
import re

import pytest
import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
CALLER_PATH = REPO_ROOT / ".github" / "workflows" / "skeptic-gate-caller.yml"
SHA_PIN_RE = re.compile(r"@([0-9a-f]{40})")
SHA_HEX_RE = re.compile(r"^[0-9a-f]{40}$")


def _caller_text() -> str:
    assert CALLER_PATH.exists(), (
        f"caller workflow missing at {CALLER_PATH} — bead jleechan-5n0o "
        f"requires this file to exist and bootstrap the gate"
    )
    return CALLER_PATH.read_text()


def test_caller_has_jobs_section() -> None:
    """No jobs → the trigger fires but the gate never runs.

    Regression class: a workflow with only `on:` and no `jobs:` is a
    trigger that does nothing. The pre-fix caller file had exactly this
    shape (71 lines, only `name`/`on`/commentary).
    """
    text = _caller_text()
    assert re.search(r"(?m)^jobs:\s*$", text), (
        "skeptic-gate-caller.yml is missing a top-level `jobs:` block — "
        "the gate cannot run without one. Bead jleechan-5n0o requires the "
        "caller to invoke the SHA-bound gate via `uses: ...@<SHA>`."
    )


def test_caller_pins_uses_ref_to_40_hex_sha() -> None:
    """The reusable-workflow `uses:` MUST be SHA-pinned.

    GitHub Actions allows `uses: foo/bar/.github/workflows/x.yml@main`
    which silently pulls the moving default branch on every run — a
    forgeable posture that violates the immutable-code-ref invariant.
    The gate (skeptic-gate.yml lines 244-275) refuses to run unless
    its checkout resolves to the SAME SHA the caller pinned; this is
    enforced via `inputs.trusted_code_sha`. A non-SHA `uses:` ref
    cannot be matched to a single commit, so the gate's invariant
    is unsatisfiable → it fails closed → every PR is blocked. That's
    worse than no gate. The fix: pin `uses: ...@<40hexSHA>`.
    """
    text = _caller_text()
    uses_matches = re.findall(r"(?m)^\s*uses:\s*[^\n]+", text)
    assert uses_matches, (
        "skeptic-gate-caller.yml has no `uses:` reference — without one "
        "the gate is not invoked at all (jobs section may exist but do nothing)"
    )
    pinned = [u for u in uses_matches if SHA_PIN_RE.search(u)]
    assert pinned, (
        f"caller `uses:` ref MUST be pinned to a 40-hex SHA; found: {uses_matches}. "
        f"A moving ref (`@main`, `@v1`, `@HEAD`) violates the "
        f"immutable-code-ref invariant the gate enforces (skeptic-gate.yml "
        f"lines 244-311). Format: `uses: jleechanorg/dark-factory/.github/"
        f"workflows/skeptic-gate.yml@<40hexSHA>`."
    )
    # Belt + braces: every captured SHA must be a valid 40-hex string.
    for u in pinned:
        sha = SHA_PIN_RE.search(u).group(1)
        assert SHA_HEX_RE.match(sha), (
            f"uses-ref SHA `{sha}` is not a 40-hex string in: {u}"
        )


def test_caller_uses_skeptic_gate_workflow() -> None:
    """The `uses:` ref MUST target `skeptic-gate.yml` specifically.

    Calling some other workflow (e.g. an audit log re-run) does not
    produce a `skeptic` commit status, so the gate is effectively
    optional again.
    """
    text = _caller_text()
    assert "skeptic-gate.yml" in text, (
        "caller MUST invoke `skeptic-gate.yml` (the SHA-bound gate) "
        "to publish the `skeptic` commit status that satisfies gate 7 "
        "of the 8-green contract. A different workflow target does not "
        "produce a Skeptic verdict."
    )


def test_caller_triggers_pull_request_target() -> None:
    """`pull_request_target` is the only PR-trigger that lets a
    same-target-repo bot post comments + commit statuses.

    `pull_request` cannot post statuses (fork limitation); `pull_request`
    also checks out PR head (forgeable). The `workflow_call` form of
    skeptic-gate.yml is intentionally absent on the bootstrap caller
    so a PR-head YAML edit cannot become a forgeable trigger.

    The caller MAY also expose `workflow_dispatch` for manual operator
    re-runs; that's allowed (diagnostic-only — read-back refuses PASS).
    """
    text = _caller_text()
    assert re.search(r"(?m)^\s*pull_request_target:\s*$", text), (
        "caller MUST trigger on `pull_request_target` — only this PR "
        "trigger grants same-target-repo bot API access for posting the "
        "`skeptic` commit status. Other triggers (`pull_request` from "
        "forks cannot post; `workflow_dispatch` cannot satisfy gate 7) "
        "leave the gate advisory."
    )


def test_caller_forwards_trusted_code_sha() -> None:
    """The caller MUST forward `trusted_code_sha` to the gate.

    The gate's immutable-code-ref invariant depends on receiving a
    40-hex SHA it can verify the checkout against (skeptic-gate.yml
    lines 248-275). The caller's own `github.sha` is the correct
    source — at workflow time it is the caller's commit SHA on the
    default branch (the SHA pinned in the `uses:` ref).
    """
    text = _caller_text()
    assert "trusted_code_sha" in text, (
        "caller MUST forward `trusted_code_sha` as an input to the "
        "gate — without it the gate's immutable-code-ref check fails "
        "closed and the gate cannot PASS."
    )
    # Pin derivation must come from github.sha, not a free-form string.
    assert re.search(r"trusted_code_sha\s*[:=]\s*\${{\s*github\.sha\s*}}", text), (
        "caller MUST derive `trusted_code_sha` from `${{ github.sha }}` "
        "— any other source lets a PR-head dispatch redirect the gate "
        "to execute untrusted code."
    )


def test_caller_uses_ref_is_same_target_repo() -> None:
    """The `uses:` ref MUST target `jleechanorg/dark-factory` (the
    target repo). Cross-repo callers fail closed because their
    GITHUB_TOKEN scope cannot post comments into the target repo
    (see skeptic-gate.md 'Trust posture' table).
    """
    text = _caller_text()
    uses_lines = re.findall(r"(?m)^\s*uses:\s*([^\s@]+)", text)
    assert uses_lines, "caller has no `uses:` references (no gate invocation)"
    for u in uses_lines:
        assert u.startswith("jleechanorg/dark-factory/"), (
            f"caller `uses: {u}` targets a different repo — cross-repo "
            f"callers cannot post the `skeptic` commit status into "
            f"`jleechanorg/dark-factory`. The ref MUST start with "
            f"`jleechanorg/dark-factory/`."
        )


# ---------------------------------------------------------------------------
# Pin extraction helper for callers + downstream tests.
# ---------------------------------------------------------------------------


def _extract_pinned_sha() -> str | None:
    """Return the SHA from the first `uses:` line that has a 40-hex pin.

    Used by other tests / future callers to verify the pin matches the
    caller's own commit SHA on `main`. Returns None if no pin is present.
    """
    text = _caller_text()
    for line in text.splitlines():
        if line.strip().startswith("uses:"):
            m = SHA_PIN_RE.search(line)
            if m:
                return m.group(1)
    return None


def test_pinned_sha_matches_caller_self_pin() -> None:
    """Sanity: the SHA in the `uses:` ref must parse as a 40-hex string
    AND be non-empty (defense against a literal `@${{ github.sha }}`
    interpolation, which GitHub allows but does NOT pin at write time).

    GitHub only resolves `${{ github.sha }}` inside workflow_call
    `with:` inputs. In a `uses:` ref, expressions are not evaluated
    by the actions runner — the literal string is used. Therefore
    a `uses: ...@${{ github.sha }}` form would either fail or pin to
    the literal text `${{ github.sha }}`, not a SHA. The fix is a
    literal 40-hex SHA (the caller's commit on main at the time this
    PR was opened).
    """
    pinned = _extract_pinned_sha()
    assert pinned is not None, (
        "could not extract a 40-hex SHA from any `uses:` line in the caller"
    )
    assert SHA_HEX_RE.match(pinned), (
        f"pinned SHA `{pinned}` is not a 40-hex string — `uses:` refs "
        f"do not support expression interpolation, so this MUST be a "
        f"literal SHA, not `${{ github.sha }}` or another template."
    )

# ---------------------------------------------------------------------------
# Unconfigured-guard tests (bead rev-iqa9).
#
# Fleet forensics proved the reviewer CLIs (codex/gemini) are absent from
# ALL runner containers. `pull_request_target` has no `paths:` filter, so
# it fires for every PR event. Without a guard, invoking the mandatory
# gate unconditionally would put a RED "Skeptic" check on every future PR
# purely because the environment isn't provisioned yet -- strictly worse
# repo hygiene than a missing gate. Both skeptic-gate-caller.yml and
# skeptic-gate.yml add a `config-check` job that always succeeds and gates
# the real `skeptic` job via `needs:` + `if:`, so unrelated PRs show a
# green config-check + a SKIPPED gate, never a red one. An explicit human
# `workflow_dispatch` is an escape hatch that always reaches the gate.
#
# These tests enforce that guard shape survives future edits, AND that the
# guard never touches the gate's own fail-closed "Verify mandatory pin
# vars" step -- that step must keep hard-failing with zero defaulting
# whenever the gate actually runs (post-audit comment 4953116428).
# ---------------------------------------------------------------------------

GATE_PATH = REPO_ROOT / ".github" / "workflows" / "skeptic-gate.yml"
PINNED_CALLEE_FIXTURES = REPO_ROOT / "tests" / "fixtures"


def _assert_caller_permission_lattice(caller_text: str, callee_text: str) -> None:
    """Require caller permissions to retain every write scope the callee needs.

    GitHub intersects permissions across a reusable-workflow boundary; a
    caller cannot elevate a read grant to the callee's declared write grant.
    The gate's top-level contract therefore makes ``pull-requests`` and
    ``statuses`` write-capable in both files.  Contents remains read-only.
    """
    caller = yaml.safe_load(caller_text)
    callee = yaml.safe_load(callee_text)
    assert isinstance(caller, dict) and isinstance(callee, dict), (
        "caller and callee workflows must both parse as YAML mappings"
    )
    caller_permissions = caller.get("permissions") or {}
    callee_permissions = callee.get("permissions") or {}
    assert isinstance(caller_permissions, dict), "caller permissions must be a mapping"
    assert isinstance(callee_permissions, dict), "callee permissions must be a mapping"

    for scope, required in callee_permissions.items():
        if required != "write":
            continue
        assert caller_permissions.get(scope) == "write", (
            f"caller permission {scope!r} must be `write` because the reusable "
            f"callee declares `{scope}: write`; GitHub cannot elevate a caller "
            "token across the workflow boundary"
        )
    assert caller_permissions.get("contents") == "read", (
        "caller must keep contents read-only; the gate does not need write access"
    )


def _pinned_callee_contract(caller_text: str) -> str:
    """Load the permission contract for the literal SHA in the caller.

    The caller's ``uses:`` ref is immutable, but a normal checkout may not
    contain that historical commit (and fetching it would make this test
    network-dependent).  A SHA-named fixture records the audited contract at
    that exact ref.  Changing the pin therefore fails until its contract is
    deliberately re-audited and a matching fixture is added.
    """
    uses_pins = re.findall(
        r"(?m)^\s*uses:\s*[^\n]+@([0-9a-f]{40})\s*$", caller_text
    )
    assert uses_pins, "caller has no literal SHA pin for its callee"
    sha = uses_pins[0]
    fixture = PINNED_CALLEE_FIXTURES / f"skeptic-gate-callee-{sha}.yml"
    assert fixture.exists(), (
        f"no audited callee contract fixture for pinned SHA {sha}; refusing "
        "to fall back to the mutable working-tree skeptic-gate.yml"
    )
    return fixture.read_text(encoding="utf-8")

SIX_PIN_VARS = (
    "SKEPTIC_CODEX_BIN",
    "SKEPTIC_CODEX_VERSION",
    "SKEPTIC_CODEX_SHA256",
    "SKEPTIC_GEMINI_BIN",
    "SKEPTIC_GEMINI_VERSION",
    "SKEPTIC_GEMINI_SHA256",
)

# The exact if-condition the `skeptic` job must carry in BOTH files: gated
# on the config-check output, with an explicit workflow_dispatch escape
# hatch so a human manual re-run always reaches the real gate.
_SKEPTIC_IF_RE = re.compile(
    r"(?m)^\s*if:\s*needs\.config-check\.outputs\.configured\s*==\s*'true'"
    r"\s*\|\|\s*github\.event_name\s*==\s*'workflow_dispatch'\s*$"
)


def _gate_text() -> str:
    assert GATE_PATH.exists(), (
        f"gate workflow missing at {GATE_PATH} — bead rev-iqa9 "
        f"unconfigured-guard tests require this file to exist"
    )
    return GATE_PATH.read_text()


def _assert_skeptic_job_gated_by_config_check(text: str, filename: str) -> None:
    """Shared assertion: the `skeptic` job must declare
    `needs: [config-check]` and the exact if-condition documented above.

    Factored out so the SAME check runs against the real files below AND
    against synthetic mutated copies in the RED-first meta-test, proving
    the assertion actually bites instead of passing on anything.
    """
    assert re.search(r"(?m)^\s*needs:\s*\[config-check\]\s*$", text), (
        f"{filename}: the `skeptic` job MUST declare `needs: [config-check]` "
        f"— without it the gate runs unconditionally on every PR/dispatch "
        f"event even when reviewer CLIs are unprovisioned (bead rev-iqa9)."
    )
    assert _SKEPTIC_IF_RE.search(text), (
        f"{filename}: the `skeptic` job's `if:` MUST be exactly "
        f"\"needs.config-check.outputs.configured == 'true' || "
        f"github.event_name == 'workflow_dispatch'\" — the first clause "
        f"gates on config-check so unconfigured PRs show a SKIPPED gate, "
        f"not RED; the second clause is the explicit human-dispatch "
        f"escape hatch that always reaches the gate's own fail-closed "
        f"pin-var check (bead rev-iqa9)."
    )


def test_caller_skeptic_job_gated_by_config_check() -> None:
    _assert_skeptic_job_gated_by_config_check(
        _caller_text(), "skeptic-gate-caller.yml"
    )


def test_gate_skeptic_job_gated_by_config_check() -> None:
    _assert_skeptic_job_gated_by_config_check(_gate_text(), "skeptic-gate.yml")


def test_red_first_gate_condition_check_catches_missing_escape_hatch_or_needs() -> None:
    """RED-first proof for `_assert_skeptic_job_gated_by_config_check`.

    A synthetic *good* snippet must pass; snippets missing the
    `workflow_dispatch` escape hatch, or missing `needs: [config-check]`
    entirely, must raise. This proves the regex above actually bites on a
    regression rather than matching anything with "config-check" in it
    somewhere in the file.
    """
    good = (
        "  skeptic:\n"
        "    needs: [config-check]\n"
        "    if: needs.config-check.outputs.configured == 'true'"
        " || github.event_name == 'workflow_dispatch'\n"
    )
    _assert_skeptic_job_gated_by_config_check(good, "synthetic-good")

    missing_escape_hatch = (
        "  skeptic:\n"
        "    needs: [config-check]\n"
        "    if: needs.config-check.outputs.configured == 'true'\n"
    )
    with pytest.raises(AssertionError):
        _assert_skeptic_job_gated_by_config_check(
            missing_escape_hatch, "synthetic-mutated-no-escape-hatch"
        )

    missing_needs = (
        "  skeptic:\n"
        "    if: needs.config-check.outputs.configured == 'true'"
        " || github.event_name == 'workflow_dispatch'\n"
    )
    with pytest.raises(AssertionError):
        _assert_skeptic_job_gated_by_config_check(
            missing_needs, "synthetic-mutated-no-needs"
        )


def _assert_config_check_verifies_all_six_pin_vars(text: str, filename: str) -> None:
    assert "config-check:" in text, (
        f"{filename}: missing the `config-check` job entirely (bead rev-iqa9)"
    )
    names_array = re.search(r"names=\(([^)]*)\)", text)
    assert names_array is not None, (
        f"{filename}: config-check step has no `names=(...)` enumeration "
        f"to iterate over the pin vars — the six vars must be actively "
        f"evaluated in a loop, not merely referenced in `env:`."
    )
    enumerated = names_array.group(1)
    for var in SIX_PIN_VARS:
        assert var in enumerated, (
            f"{filename}: `names=(...)` array is missing {var} — this var "
            f"would never be evaluated by the unconfigured-guard, so a "
            f"missing pin for it would silently let the gate run instead "
            f"of being reported as unconfigured (bead rev-iqa9)."
        )


def test_caller_config_check_verifies_all_six_pin_vars() -> None:
    _assert_config_check_verifies_all_six_pin_vars(
        _caller_text(), "skeptic-gate-caller.yml"
    )


def test_gate_config_check_verifies_all_six_pin_vars() -> None:
    _assert_config_check_verifies_all_six_pin_vars(_gate_text(), "skeptic-gate.yml")


def test_red_first_pin_var_check_catches_dropped_var() -> None:
    """RED-first proof for `_assert_config_check_verifies_all_six_pin_vars`.

    Drop one var from an otherwise-good synthetic `names=(...)` array and
    confirm the helper raises — proves the per-var membership check bites
    rather than passing on any non-empty array.
    """
    good = (
        "config-check:\n"
        "    steps:\n"
        "      - run: |\n"
        "          names=(SKEPTIC_CODEX_BIN SKEPTIC_CODEX_VERSION "
        "SKEPTIC_CODEX_SHA256 SKEPTIC_GEMINI_BIN SKEPTIC_GEMINI_VERSION "
        "SKEPTIC_GEMINI_SHA256)\n"
    )
    _assert_config_check_verifies_all_six_pin_vars(good, "synthetic-good")

    dropped_gemini_sha = (
        "config-check:\n"
        "    steps:\n"
        "      - run: |\n"
        "          names=(SKEPTIC_CODEX_BIN SKEPTIC_CODEX_VERSION "
        "SKEPTIC_CODEX_SHA256 SKEPTIC_GEMINI_BIN SKEPTIC_GEMINI_VERSION)\n"
    )
    with pytest.raises(AssertionError):
        _assert_config_check_verifies_all_six_pin_vars(
            dropped_gemini_sha, "synthetic-mutated-dropped-var"
        )

    no_config_check_job = "skeptic:\n    runs-on: ubuntu-latest\n"
    with pytest.raises(AssertionError):
        _assert_config_check_verifies_all_six_pin_vars(
            no_config_check_job, "synthetic-mutated-no-config-check-job"
        )


def _extract_step_body(text: str, step_name_substr: str) -> str:
    """Return the YAML body of the first step whose `name:` contains
    `step_name_substr`, up to (not including) the next `- name:` step or
    end of file. Used to scope fail-closed-pattern assertions to exactly
    the pin-var verification step, not the whole file.
    """
    pattern = re.compile(
        r"- name:\s*[\"']?" + re.escape(step_name_substr) + r"[\"']?.*?"
        r"(?=\n\s*- name:|\Z)",
        re.DOTALL,
    )
    match = pattern.search(text)
    assert match is not None, (
        f"could not locate a step named like {step_name_substr!r} in the "
        f"provided text"
    )
    return match.group(0)


def _assert_fail_closed_pin_var_step_unmodified(text: str, filename: str) -> None:
    step_name = "Verify mandatory pin vars are set (no defaults)"
    assert step_name in text, (
        f"{filename}: the fail-closed \"{step_name}\" step is missing — "
        f"the unconfigured-guard (bead rev-iqa9) must never remove or "
        f"rename the gate's own fail-closed check; it only decides "
        f"whether to invoke the gate at all."
    )
    step_body = _extract_step_body(text, step_name)
    assert "exit 1" in step_body, (
        f"{filename}: the fail-closed pin-var step no longer contains "
        f"`exit 1` — missing pins would no longer hard-fail the gate run "
        f"(regression against post-audit comment 4953116428)."
    )
    assert "fail=1" in step_body and 'if [ "$fail" -ne 0 ]' in step_body, (
        f"{filename}: the accumulate-then-check fail-closed pattern "
        f"(`fail=1` ... `if [ \"$fail\" -ne 0 ]`) is missing from the "
        f"pin-var step — this is the exact mechanism that makes the "
        f"check fail-closed rather than fail-open."
    )
    for var in SIX_PIN_VARS:
        assert f"{var}:-" not in step_body, (
            f"{filename}: found a bash default-value operator "
            f"(`{var}:-...`) inside the fail-closed pin-var step — "
            f"post-audit comment 4953116428 requires NO defaulting; "
            f"empty pins must hard-fail, never silently default. The "
            f"config-check guard (bead rev-iqa9) only decides whether to "
            f"invoke the gate; it must never soften this step."
        )


def test_gate_fail_closed_pin_var_step_present_and_unmodified() -> None:
    _assert_fail_closed_pin_var_step_unmodified(_gate_text(), "skeptic-gate.yml")


def test_red_first_fail_closed_check_catches_removed_exit_or_reintroduced_default() -> None:
    """RED-first proof for `_assert_fail_closed_pin_var_step_unmodified`.

    Three synthetic mutations of an otherwise-good step body must each be
    caught: (1) `exit 1` removed (fail-open regression), (2) the
    accumulate-then-check pattern removed, (3) a bash default-value
    operator reintroduced on a guarded var. This proves the checks bite
    on the exact regression classes bead rev-iqa9 warns against, not just
    on total step-removal.
    """
    good = (
        "steps:\n"
        '      - name: "Verify mandatory pin vars are set (no defaults)"\n'
        "        run: |\n"
        "          fail=0\n"
        '          if [ -z "$SKEPTIC_CODEX_BIN" ]; then fail=1; fi\n'
        '          if [ "$fail" -ne 0 ]; then\n'
        "            exit 1\n"
        "          fi\n"
        "      - name: Next step\n"
        "        run: echo done\n"
    )
    _assert_fail_closed_pin_var_step_unmodified(good, "synthetic-good")

    no_exit = good.replace("            exit 1\n", "            echo skip\n")
    with pytest.raises(AssertionError):
        _assert_fail_closed_pin_var_step_unmodified(no_exit, "synthetic-mutated-no-exit")

    no_accumulate_pattern = good.replace("fail=0\n", "").replace(
        "fail=1; fi\n", "exit 1; fi\n"
    ).replace('if [ "$fail" -ne 0 ]; then\n            exit 1\n          fi\n', "")
    with pytest.raises(AssertionError):
        _assert_fail_closed_pin_var_step_unmodified(
            no_accumulate_pattern, "synthetic-mutated-no-accumulate-pattern"
        )

    reintroduced_default = good.replace(
        '"$SKEPTIC_CODEX_BIN"', '"${SKEPTIC_CODEX_BIN:-/opt/reviewers/codex/codex}"'
    )
    with pytest.raises(AssertionError):
        _assert_fail_closed_pin_var_step_unmodified(
            reintroduced_default, "synthetic-mutated-reintroduced-default"
        )


def test_caller_retains_callee_write_permission_lattice() -> None:
    """A reusable caller cannot downgrade the gate's API permissions.

    ``skeptic-gate.yml`` posts a pull-request comment and a commit status.
    GitHub rejects the workflow before jobs start when this caller grants
    either scope only ``read`` (the callee's ``write`` request cannot elevate
    the caller token).  Parse the caller plus a SHA-named snapshot of the
    exact immutable callee contract so working-tree callee drift cannot hide
    the startup failure.
    """
    caller = _caller_text()
    _assert_caller_permission_lattice(caller, _pinned_callee_contract(caller))


def test_red_first_permission_lattice_catches_read_downgrade() -> None:
    """The permission assertion must fail when either write scope regresses."""
    caller = _caller_text()
    callee = _pinned_callee_contract(caller)
    _assert_caller_permission_lattice(caller, callee)

    for scope in ("pull-requests", "statuses"):
        mutated = re.sub(
            rf"(?m)^(\s*{re.escape(scope)}:)\s*write\s*$",
            rf"\1 read",
            caller,
            count=1,
        )
        assert mutated != caller, f"test mutation failed to find {scope} permission"
        with pytest.raises(AssertionError, match=rf"{re.escape(scope)}.*write"):
            _assert_caller_permission_lattice(mutated, callee)
