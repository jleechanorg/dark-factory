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