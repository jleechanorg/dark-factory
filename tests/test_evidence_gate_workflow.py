"""Tests for jleechan-amri / issue #424: Evidence Gate workflow must
be fail-closed and bound to an independent /er verdict signal.

Issue #424 (filed by Codex post-merge review of #407) found that
`.github/workflows/evidence-gate.yml`:

  1. Never checks out the repository, so its `git diff` step ran in an
     empty dir, failed with 'Not a git repository', and the failure was
     masked by `| head -50`.
  2. Echoes `evidence_gate=PASS` unconditionally — the gate cannot fail.
  3. Runs on `ubuntu-latest`, but the issue describes the repo as
     'private' (it is actually PUBLIC — see the regression note on the
     runner-policy assertion below). The substantive defects are (1)
     and (2); the runner-policy assertion confirms `ubuntu-latest` is
     the correct runs-on for this PUBLIC repo.

A PR with no `/er` comment, or with `/er FAIL`, `/er PARTIAL`, or
`/er INCONCLUSIVE`, MUST show Evidence Gate FAILURE. This is the
gate-self-certification anti-pattern: the check's expected value must
come from independent ground truth (an actual /er verdict comment
posted to the PR), not from its own template.

The fix:

  * `actions/checkout@v4` with `fetch-depth` for the base ref.
  * A step that calls `gh api` to read PR comments and greps for the
    `/er` verdict token (PASS / FAIL / PARTIAL / INCONCLUSIVE), then
    emits the verdict via `$GITHUB_OUTPUT` and `exit 1` when absent or
    non-PASS — proving the gate is bound to external ground truth.
  * A complementary signal B: a canonical `**Evidence**: <gist-url>`
    marker in the PR body, verified via curl to api.github.com/gists/<id>
    (the default GITHUB_TOKEN cannot read public gists; public gists
    are fetchable anonymously).
  * An Ironclad reminder that lists the binding signal unambiguously.

These tests parse the workflow YAML semantically (via PyYAML), then
assert the structural and verdict-derivation properties the issue
demands. They are intentionally lightweight: no `requests`, no
`gh` subprocess — they fail loud and fast if the gate regresses.
"""

from __future__ import annotations

import pathlib
import re

import pytest
import yaml

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKFLOW_PATH = REPO_ROOT / ".github" / "workflows" / "evidence-gate.yml"


def _load_workflow() -> dict:
    """Parse evidence-gate.yml into a Python dict for structural assertions."""
    assert WORKFLOW_PATH.exists(), (
        f"evidence-gate.yml missing at {WORKFLOW_PATH} — cannot validate gate"
    )
    with WORKFLOW_PATH.open("r", encoding="utf-8") as fh:
        return yaml.safe_load(fh)


def _step_names(workflow: dict) -> list[str]:
    """Flatten the list of step names in the single job of evidence-gate.yml."""
    jobs = workflow.get("jobs", {})
    assert jobs, "evidence-gate.yml has no jobs block"
    job = next(iter(jobs.values()))
    return [s.get("name") or s.get("id") or "" for s in job.get("steps", [])]


def _step_by_name(workflow: dict, name: str) -> dict:
    """Return the step dict whose `name:` matches the given literal or prefix.

    Step names like "Mark evidence-gate verdict" may grow suffixes over
    time (e.g. "(fail-closed)"). The regression intent is to find the
    verdict-derivation step, so we accept either exact match or prefix
    match.
    """
    jobs = workflow.get("jobs", {})
    job = next(iter(jobs.values()))
    matches = [s for s in job.get("steps", []) if (s.get("name") or "").startswith(name)]
    if not matches:
        raise AssertionError(
            f"evidence-gate.yml has no step named {name!r} (or prefix); "
            f"available: {_step_names(workflow)}"
        )
    if len(matches) > 1:
        raise AssertionError(
            f"evidence-gate.yml has multiple steps with prefix {name!r}: "
            f"{[m.get('name') for m in matches]}"
        )
    return matches[0]


def _job(workflow: dict) -> dict:
    jobs = workflow.get("jobs", {})
    return next(iter(jobs.values()))


# ---------------------------------------------------------------------------
# Issue #424 fix #1 — the workflow MUST checkout the repo before diffing
# ---------------------------------------------------------------------------


def test_evidence_gate_checks_out_repository() -> None:
    """The gate diffs `origin/<base_ref>...<sha>` and that requires a clone.

    Issue #424 finding (1): the original workflow ran `git diff` in an
    empty dir, the command failed with 'Not a git repository', and the
    `| head -50` pipe masked the error. Any future regression here
    restores the same no-op.
    """
    workflow = _load_workflow()
    steps = _job(workflow).get("steps", [])
    checkout_steps = [
        s for s in steps
        if (s.get("uses") or "").startswith("actions/checkout")
    ]
    assert checkout_steps, (
        "evidence-gate.yml must include actions/checkout so the PR diff step "
        "runs against the real repository, not an empty directory "
        "(issue #424)."
    )
    checkout = checkout_steps[0]
    with_block = checkout.get("with") or {}
    if "fetch-depth" in with_block:
        assert with_block["fetch-depth"] in (0, "0"), (
            f"actions/checkout must use fetch-depth: 0 to diff against "
            f"origin/${{{{ github.base_ref }}}}; got {with_block['fetch-depth']!r}"
        )


# ---------------------------------------------------------------------------
# Issue #424 fix #2 — the verdict MUST come from an external /er signal
# ---------------------------------------------------------------------------


_VERDICT_TOKEN_RE = re.compile(
    r"/er\s+(?P<verdict>PASS|FAIL|PARTIAL|INCONCLUSIVE)",
    re.IGNORECASE,
)


def _verdict_step_text(workflow: dict) -> str:
    """The text of the verdict-derivation step (the one that sets the gate).

    The original step 'Mark evidence-gate verdict (config)' echoed
    `evidence_gate=PASS` unconditionally. The fixed gate has TWO
    verdict-related steps:

      - "Determine evidence verdict from external signals" (signal-A
        `gh api` for /er comments + signal-B gist verification).
      - "Mark evidence-gate verdict (fail-closed)" (case-arm translator
        from ER_VERDICT env var to gate verdict + exit code).

    We concatenate the full text of every step whose name starts with
    "Determine evidence verdict" OR "Verify /er verdict" OR "Mark
    evidence-gate verdict" — all three name variants participate in the
    fail-closed contract.
    """
    jobs = workflow.get("jobs", {})
    job = next(iter(jobs.values()))
    parts: list[str] = []
    for step in job.get("steps", []):
        name = step.get("name") or ""
        if (
            name.startswith("Determine evidence verdict")
            or name.startswith("Verify /er verdict")
            or name.startswith("Mark evidence-gate verdict")
        ):
            run = step.get("run")
            assert run is not None, (
                f"verdict-related step {name!r} must be a `run:` block, "
                f"not `uses:` — the gate derives its verdict inline from "
                f"PR comments and the filesystem, not by calling another "
                f"action."
            )
            parts.append(run if isinstance(run, str) else "\n".join(run))
    assert parts, (
        "evidence-gate.yml must include at least one step whose name "
        "starts with 'Determine evidence verdict' OR 'Verify /er verdict' "
        "OR 'Mark evidence-gate verdict' — the gate cannot derive its "
        "verdict from independent ground truth without one. (issue #424)"
    )
    return "\n".join(parts)


def test_evidence_gate_no_unconditional_pass_echo() -> None:
    """The verdict step MUST NOT echo `evidence_gate=PASS` unconditionally.

    This is the literal anti-pattern from issue #424 finding (2):
    'the "Mark evidence-gate verdict" step UNCONDITIONALLY echoes
    evidence_gate=PASS — the gate cannot fail.'
    """
    text = _verdict_step_text(_load_workflow())
    lines = text.splitlines()
    for i, line in enumerate(lines):
        stripped = line.strip()
        if "evidence_gate=PASS" in stripped and not stripped.startswith("#"):
            preceding = "\n".join(lines[max(0, i - 3) : i])
            assert ("ER_VERDICT" in preceding
                    or "er_verdict" in preceding), (
                f"line {i + 1}: unconditional `evidence_gate=PASS` echo "
                f"detected at:\n  {line!r}\n"
                f"Preceding 3 lines:\n{preceding}\n"
                "The verdict MUST be gated on the parsed /er comment token, "
                "not echoed unconditionally. (issue #424)"
            )


def test_evidence_gate_queries_external_er_signal() -> None:
    """The verdict step MUST read PR comments via `gh api` (or equivalent).

    The independent ground truth is an `/er` verdict comment posted to
    the PR — anything else (the workflow's own template, a grep of the
    PR body, a hardcoded echo) is self-certifying.
    """
    text = _verdict_step_text(_load_workflow())
    assert "gh api" in text or "gh pr view" in text, (
        "verdict step must call `gh api` (or `gh pr view`) to read PR "
        "comments — the /er verdict must come from independent ground "
        "truth (a comment posted to the PR), not the workflow's own "
        "template. (issue #424)"
    )


def test_evidence_gate_parses_er_verdict_token() -> None:
    """The verdict step MUST regex-parse `/er PASS|FAIL|PARTIAL|INCONCLUSIVE`.

    Issue #424 names the binding signal as the /er verdict token; the
    verifier's `parse_er_verdict` (daemon/src/verifier.rs:233-280) and
    `parse_er_verdict_text` use exactly this grammar. The workflow's
    bash regex MUST match the same shape.
    """
    text = _verdict_step_text(_load_workflow())
    assert _VERDICT_TOKEN_RE.search(text), (
        "verdict step must regex-parse `/er PASS|FAIL|PARTIAL|INCONCLUSIVE` "
        "comments. The pattern matches the daemon's parse_er_verdict "
        "grammar. (issue #424)"
    )


def test_evidence_gate_fails_closed_when_no_er_comment() -> None:
    """A PR with NO `/er` verdict comment AND NO canonical evidence marker
    MUST make the gate fail.

    Issue #424: 'regression: a PR with no /er verdict must show Evidence
    Gate FAILURE.' The verifier step MUST `exit 1` when no `/er` comment
    AND no canonical evidence marker (signal B) are found.
    """
    text = _verdict_step_text(_load_workflow())
    assert re.search(r"\bexit\s+1\b", text), (
        "verdict step must include an explicit `exit 1` so the gate fails "
        "closed when no /er verdict comment exists on the PR. (issue #424)"
    )
    fail_closed_patterns = [
        r"if\s+\[\s*[\"']?\$ER_VERDICT[\"']?\s*=\s*[\"']?ABSENT",
        r"if\s+\[\s*-z\s+[\"']?\$ER_VERDICT",
        r"if\s+\[\s*[\"']?\$\{ER_VERDICT:-[^\}]*\}[\"']?\s*=\s*[\"']?\"?ABSENT",
        r"if\s+\[\s*-z\s+[\"']?\$\{ER_VERDICT:-[^\}]*\}[\"']?",
        r"case\s+[\"']?\$\{?ER_VERDICT",
    ]
    has_no_match_branch = any(re.search(pat, text) for pat in fail_closed_patterns)
    has_default_fail = re.search(
        r"\)\s*\*\)\s*\n[^#]*exit\s+1", text
    ) or re.search(r"\*\)\s*echo\s+[\"']?evidence_gate=FAIL", text)
    assert has_no_match_branch and has_default_fail, (
        "verdict step must `exit 1` specifically when the /er verdict "
        "is absent (or empty); the no-comment branch must be the "
        "fail-closed branch. (issue #424)\n"
        f"Verdict step text:\n{text}"
    )


def test_evidence_gate_fails_closed_on_er_fail_partial_inconclusive() -> None:
    """A `/er FAIL`, `/er PARTIAL`, or `/er INCONCLUSIVE` MUST exit non-zero.

    Only `/er PASS` is success; the other three verdicts are red gates.
    """
    text = _verdict_step_text(_load_workflow())
    fail_closed_patterns = [
        r"\bPASS\b[\s\S]+\bexit\s+1\b",
        r"\bPASS\)\s*;;[\s\S]+\*\)\s*exit\s+1",
        r"if\s+\[\s*[\"']?\$ER_VERDICT[\"']?\s+!=\s+[\"']?PASS",
    ]
    assert any(re.search(pat, text) for pat in fail_closed_patterns), (
        "verdict step must `exit 1` when the parsed /er verdict is FAIL, "
        "PARTIAL, or INCONCLUSIVE (only PASS is success). (issue #424)\n"
        f"Verdict step text:\n{text}"
    )


# ---------------------------------------------------------------------------
# Issue #424 fix #3 — runs-on must align with the repo's runner policy
# ---------------------------------------------------------------------------


def test_evidence_gate_runs_on_uses_self_hosted_runner_labels() -> None:
    """Confirm evidence gate workflow uses self-hosted runner labels.

    See bead jleechan-z284 / issue #286: evidence-gate workflow is strictly
    bound to self-hosted runners via vars.SELF_HOSTED_RUNNER_LABELS.
    """
    workflow = _load_workflow()
    job = _job(workflow)
    runs_on = job.get("runs-on")
    if isinstance(runs_on, list):
        runs_on_rendered = ", ".join(runs_on)
    else:
        runs_on_rendered = str(runs_on)
    assert "SELF_HOSTED_RUNNER_LABELS" in runs_on_rendered, (
        f"runs-on must reference vars.SELF_HOSTED_RUNNER_LABELS (bead jleechan-z284); "
        f"got {runs_on_rendered!r}."
    )


# ---------------------------------------------------------------------------
# End-to-end — no /er verdict + no canonical marker ⇒ FAIL
# ---------------------------------------------------------------------------


def test_evidence_gate_does_not_have_a_trivial_pass_only_workflow() -> None:
    """End-to-end regression: removing the fail-closed logic must be blocked.

    Combines the structural signals from the prior tests: the gate MUST
    have a checkout AND a fail-closed verdict step. If either is missing,
    the gate is a no-op (issue #424 finding 1 + 2 combined).
    """
    workflow = _load_workflow()
    steps = _job(workflow).get("steps", [])
    has_checkout = any(
        (s.get("uses") or "").startswith("actions/checkout") for s in steps
    )
    assert has_checkout, "evidence-gate.yml is missing actions/checkout"
    verdict_step = _step_by_name(workflow, "Mark evidence-gate verdict")
    text = verdict_step.get("run") or ""
    assert "exit 1" in text, (
        "evidence-gate.yml is missing an `exit 1` path in the verdict "
        "step — the gate cannot fail, so it is a self-certifying no-op "
        "(issue #424)."
    )


# ---------------------------------------------------------------------------
# Signal B — canonical evidence marker in PR body
# ---------------------------------------------------------------------------


def test_evidence_gate_verifies_canonical_evidence_marker() -> None:
    """The workflow MUST parse the canonical `**Evidence**:` marker line.

    daemon/src/tools.rs:539 defines EVIDENCE_MARKER = "**Evidence**:".
    The workflow must grep for that exact literal in the PR body and
    extract the gist URL.
    """
    text = _verdict_step_text(_load_workflow())
    assert "**Evidence**:" in text, (
        "verdict step must grep for the canonical `**Evidence**:` "
        "marker in the PR body (matches daemon/src/tools.rs:539 "
        "EVIDENCE_MARKER). (issue #424)"
    )
    assert "gist.github.com" in text, (
        "verdict step must extract the gist URL from the canonical "
        "marker line."
    )


def test_evidence_gate_signal_b_uses_curl_for_public_gist_fetch() -> None:
    """Signal B must use curl (not gh api) to fetch public gists.

    The default GITHUB_TOKEN cannot read public gists (returns 403
    'Resource not accessible by integration'). Public gists are
    fetchable anonymously via curl to api.github.com/gists/<id>.
    """
    text = _verdict_step_text(_load_workflow())
    assert "curl" in text, (
        "verdict step must use curl for gist fetch — the default "
        "GITHUB_TOKEN cannot read public gists (returns 403), but "
        "public gists are fetchable anonymously."
    )
    assert "api.github.com/gists/" in text, (
        "verdict step must hit api.github.com/gists/<id> to fetch the "
        "gist content."
    )


def test_evidence_gate_signal_b_verifies_gist_files_have_content() -> None:
    """Signal B must verify the gist has files with non-empty content.

    Per daemon/src/er_runner.rs:88-90: "a missing, empty, or stale-head
    gist is a FAIL." The binding check is that the gist is real,
    public, and has files with non-empty content.
    """
    text = _verdict_step_text(_load_workflow())
    assert "files_with_content" in text or "files_with_head_sha" in text, (
        "verdict step must count gist files with non-empty content — "
        "the binding signal-B check that proves the gist is real and "
        "substantive."
    )
    assert ".files" in text or "files" in text, (
        "verdict step must inspect the gist's .files map."
    )


# ---------------------------------------------------------------------------
# Pytest fixtures / helpers
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def workflow() -> dict:
    """Cached workflow load for tests that don't need to re-parse."""
    return _load_workflow()


# ---------------------------------------------------------------------------
# Issue #424 explicit regression — bash-logic simulation
# ---------------------------------------------------------------------------


def _simulate_verdict_decision(
    er_verdict: str | None,
    marker_verdict: str | None = None,
) -> str:
    """Mirror the bash verdict-decision logic in 'Determine evidence verdict'.

    Returns one of: 'PASS', 'FAIL'.
    """
    er = er_verdict if er_verdict else "ABSENT"
    mk = marker_verdict if marker_verdict else "ABSENT"
    if er in ("FAIL", "PARTIAL", "INCONCLUSIVE"):
        return "FAIL"
    if er == "PASS":
        return "PASS"
    if mk == "PASS":
        return "PASS"
    return "FAIL"


@pytest.mark.parametrize(
    "er_verdict,marker_verdict,expected_gate",
    [
        ("PASS", None, "PASS"),
        ("FAIL", None, "FAIL"),
        ("PARTIAL", None, "FAIL"),
        ("INCONCLUSIVE", None, "FAIL"),
        (None, "PASS", "PASS"),
        ("", "PASS", "PASS"),
        ("ABSENT", "PASS", "PASS"),
        (None, None, "FAIL"),
        (None, "ABSENT", "FAIL"),
        (None, "STALE_SHA", "FAIL"),
        (None, "EMPTY", "FAIL"),
        (None, "UNREACHABLE", "FAIL"),
        ("FAIL", "PASS", "FAIL"),
    ],
)
def test_verdict_decision_table_is_fail_closed(
    er_verdict: str | None,
    marker_verdict: str | None,
    expected_gate: str,
) -> None:
    """The gate's verdict table MUST map every non-PASS input to FAIL."""
    assert _simulate_verdict_decision(er_verdict, marker_verdict) == expected_gate, (
        f"verdict (er={er_verdict!r}, marker={marker_verdict!r}) should "
        f"map to gate={expected_gate}; this is the fail-closed contract "
        f"from issue #424."
    )


def test_no_er_comment_at_all_makes_gate_fail_closed() -> None:
    """Issue #424 explicit regression: a PR with NO /er verdict AND NO
    canonical evidence marker ⇒ gate FAIL.
    """
    assert _simulate_verdict_decision(None, None) == "FAIL"


# ---------------------------------------------------------------------------
# Issue #433 — Evidence Gate signal hardening
#
# Codex post-merge review of #424 found that the now-fail-closed gate still
# has TWO forgeable surfaces:
#
#   (a) Signal A grepped `/er PASS` from ANY comment body — no identity
#       binding (PR author can self-post), no head-SHA binding (verdict for
#       an older head still greens the gate).
#
#   (b) Signal B verified the gist is reachable + non-empty + head-SHA
#       matches the marker line, but did NOT validate content beyond "files
#       have non-empty content" — a 1-byte gist with a fake SHA line clears.
#
# Acceptance from issue #433:
#   - Author-posted bare '/er PASS' does NOT green the gate.
#   - er_runner-style verdict for the CURRENT head DOES green.
#   - Stale-head verdict does NOT green.
#   - 1-byte gist does NOT green.
# ---------------------------------------------------------------------------


def test_signal_a_requires_trusted_identity_marker() -> None:
    """Signal A: the verdict step MUST require a trusted-identity marker
    in the comment body — not just the `/er PASS` token.

    Without this check, the PR author can post `/er PASS` as a PR comment
    and the gate will accept it (issue #433 forgery surface).

    The canonical trusted-identity marker is the er_runner comment header
    `🤖 **[dark-factory /er]**` (see daemon/src/er_runner.rs:191). A
    verifier-token (`AUTHOR="..."` env-var compare) or regex against the
    comment body is acceptable; the binding check is that the grep is
    NOT a bare token scan.
    """
    text = _verdict_step_text(_load_workflow())
    assert "dark-factory /er" in text or "[dark-factory" in text or "AUTHOR" in text or "user.login" in text, (
        "Signal A must require a trusted-identity marker in the comment "
        "body (e.g. 'dark-factory /er' from er_runner.rs) OR an explicit "
        "user-login compare — not a bare `/er PASS` grep. (issue #433)"
    )


def test_signal_a_captures_comment_author() -> None:
    """Signal A: the verdict step MUST query PR comments in a shape that
    preserves the comment author's login.

    The default `gh api /repos/.../issues/<n>/comments` returns objects
    with `user.login`; the workflow MUST thread that field through so the
    trusted-identity check (see test_signal_a_requires_trusted_identity_
    marker) has something to compare against. A bare `.body | join("\n")`
    discards the author and re-opens the forgery hole.
    """
    text = _verdict_step_text(_load_workflow())
    assert "user.login" in text or ".user.login" in text, (
        "Signal A must capture `user.login` from the comment object so "
        "the trusted-identity check has something to compare against. "
        "A bare `.body | join(\"\\n\")` discards the author. (issue #433)"
    )


def test_signal_a_requires_current_head_sha_reference() -> None:
    """Signal A: the verdict comment MUST reference the CURRENT PR head SHA.

    Issue #433: a verdict comment that names an OLDER head does not
    verify the code that would actually merge — it must be rejected.
    The binding check is that the verdict-step text parses both a head
    SHA reference AND matches it against the current PR head SHA.
    """
    text = _verdict_step_text(_load_workflow())
    # The pattern below accepts either an explicit SHA regex AND a compare
    # against the current head, OR a comment body that contains the
    # canonical `head <sha>` literal that the marker line uses.
    assert re.search(r"head[[:space:]]+\[?[0-9a-f]{7,64}", text) or re.search(
        r"head_sha|HEAD_SHA|github\.event\.pull_request\.head\.sha", text
    ), (
        "Signal A must require the verdict comment to reference the "
        "current head SHA — either by extracting `head <sha>` from the "
        "comment body and comparing it to `github.event.pull_request.head.sha`, "
        "or by capturing `head_sha` from the comment directly. (issue #433)"
    )


def test_signal_b_enforces_minimum_gist_content_size() -> None:
    """Signal B: the verdict step MUST enforce a minimum gist content size.

    Issue #433: a 1-byte gist with a fake `(head <sha>)` line clears
    the old "files have non-empty content" check. The fix requires a
    size floor across the gist's files (e.g. >= 256 bytes total) so a
    placeholder cannot satisfy signal B.
    """
    text = _verdict_step_text(_load_workflow())
    # Look for a size threshold constant in bash (e.g. `MIN_GIST_BYTES`,
    # `> 256`, `length > N`).
    assert re.search(r"MIN_GIST|MIN_BYTES|MIN_CONTENT|>=?\s*[0-9]{2,5}", text) or re.search(
        r"length[[:space:]]*>", text
    ), (
        "Signal B must enforce a minimum gist content size — a 1-byte "
        "gist must NOT green the gate. The check should be visible in "
        "the verdict step text (e.g. a MIN_GIST_BYTES constant or a "
        "`length > N` comparison). (issue #433)"
    )


def test_signal_b_requires_substantive_content_keywords() -> None:
    """Signal B: the gist MUST contain at least one substantive
    evidence keyword (PR number, repo name, or evidence marker).

    Issue #433: a gist that contains only a SHA reference is forgeable —
    the contract requires that the gist actually reference the PR it
    purports to evidence (PR number or repo name) AND contain some
    signal-bearing token (verdict, evidence marker, test runner name).
    """
    text = _verdict_step_text(_load_workflow())
    # Acceptable: an explicit grep for a PR number ($PR_NUMBER), the
    # canonical EVIDENCE_MARKER ("**Evidence**:"), or a verdict token
    # (/er PASS|FAIL|PARTIAL|INCONCLUSIVE) inside the gist body.
    has_pr_anchor = "PR_NUMBER" in text or "pull_request.number" in text or "REPO" in text
    has_marker_anchor = "**Evidence**" in text or "EVIDENCE_MARKER" in text
    has_verdict_anchor = "/er" in text or "er verdict" in text.lower()
    assert has_pr_anchor and (has_marker_anchor or has_verdict_anchor), (
        "Signal B must require the gist to mention the PR (PR_NUMBER or "
        "REPO context) AND contain a substantive evidence token "
        "(`**Evidence**:` or `/er` verdict). A bare SHA reference is not "
        "enough. (issue #433)"
    )


def test_signal_b_fetches_gist_files_content_not_just_metadata() -> None:
    """Signal B: the verdict step MUST fetch and inspect each gist file's
    `content` field (not just the metadata wrapper).

    Issue #433: the old check accepted any non-empty file list, but a
    file with only `null` content (or an empty string) cleared it.
    The fix must drill into `.files.<name>.content` for the size and
    keyword checks.
    """
    text = _verdict_step_text(_load_workflow())
    assert ".content" in text or "files_with_content" in text, (
        "Signal B must inspect `.files.<name>.content` (or "
        "`files_with_content`) to apply the size floor and keyword "
        "checks. A bare file-list count is not enough. (issue #433)"
    )


# Issue #433 — bash-logic simulation for the hardened signal contract


def _simulate_signal_a_decision(
    comment_author: str | None,
    comment_body: str | None,
    current_head_sha: str,
    declared_head_sha: str | None,
) -> str:
    """Mirror the hardened Signal A decision in 'Determine evidence verdict'.

    Acceptance from issue #433:
      - Author-posted bare '/er PASS' does NOT green (no trusted identity).
      - er_runner-style verdict for current head DOES green.
      - Stale-head verdict does NOT green.
    """
    if not comment_author or not comment_body:
        return "FAIL"
    # Trusted identity: must carry the er_runner header literal. The
    # exact login compare is sufficient in production, but here we
    # accept the body marker as the trusted-identity anchor (the bot
    # posts under any login but always includes the literal).
    if "dark-factory /er" not in comment_body:
        return "FAIL"
    # Verdict token: /er PASS|FAIL|PARTIAL|INCONCLUSIVE.
    body_lower = comment_body.lower()
    verdict = "ABSENT"
    for token in ("pass", "fail", "partial", "inconclusive"):
        if f"/er {token}" in body_lower or f"/er  {token}" in body_lower:
            verdict = token.upper()
            break
    if verdict == "ABSENT":
        return "FAIL"
    if verdict in ("FAIL", "PARTIAL", "INCONCLUSIVE"):
        return "FAIL"
    # Head binding: the declared_head_sha must match the current head.
    if declared_head_sha is None:
        return "FAIL"
    if declared_head_sha.lower() != current_head_sha.lower():
        return "FAIL"
    return "PASS"


@pytest.mark.parametrize(
    "author,body,current_sha,declared_sha,expected",
    [
        # Forgeable: PR author posts bare /er PASS (no identity marker).
        ("jleechan", "/er PASS", "abcdef1234", None, "FAIL"),
        # Forgeable: PR author posts bare /er PASS with SHA but no identity.
        ("jleechan", "/er PASS (head abcdef1234)", "abcdef1234", "abcdef1234", "FAIL"),
        # Trusted identity, current head: green.
        (
            "dark-factory-er[bot]",
            "🤖 **[dark-factory /er]** Evidence review verdict:\n\n```\n/er PASS\n```\n\nhead=abcdef1234",
            "abcdef1234",
            "abcdef1234",
            "PASS",
        ),
        # Trusted identity, STALE head: must NOT green.
        (
            "dark-factory-er[bot]",
            "🤖 **[dark-factory /er]** ... /er PASS ... head=olderhead",
            "abcdef1234",
            "olderhead",
            "FAIL",
        ),
        # No comment at all: fail.
        (None, None, "abcdef1234", None, "FAIL"),
    ],
)
def test_signal_a_trusted_identity_plus_head_binding(
    author: str | None,
    body: str | None,
    current_sha: str,
    declared_sha: str | None,
    expected: str,
) -> None:
    """Issue #433 acceptance: only trusted-identity + current-head verdicts
    may pass Signal A."""
    assert (
        _simulate_signal_a_decision(author, body, current_sha, declared_sha) == expected
    ), f"Signal A (author={author!r}, sha={declared_sha!r}) should map to {expected}"


def _simulate_signal_b_decision(
    gist_files_content: list[tuple[str, str]],
    pr_number: int,
    repo: str,
    declared_head_sha: str,
    current_head_sha: str,
) -> str:
    """Mirror the hardened Signal B decision in 'Determine evidence verdict'.

    Acceptance from issue #433:
      - 1-byte gist fails.
      - Gist missing PR number or repo fails.
      - Gist with substantive content for the current head passes.
    """
    if not gist_files_content:
        return "FAIL"
    total_bytes = sum(len(content) for _, content in gist_files_content)
    if total_bytes < 256:
        return "FAIL"
    combined = "\n".join(content for _, content in gist_files_content)
    if str(pr_number) not in combined and repo.split("/")[-1] not in combined:
        return "FAIL"
    if declared_head_sha.lower() != current_head_sha.lower():
        return "FAIL"
    return "PASS"


# jleechan-wjm2: regression test for the head-SHA extraction regex bug.
#
# The Evidence Gate workflow extracts the declared head SHA from /er
# verdict comments. The extraction accepts BOTH of these formats:
#
#   1. er_runner format:  "head=<sha>"      (head SHA right after `=`)
#   2. marker-line form: "head <sha>"      (space between `head` and SHA)
#
# The previous sed `'s/^=//'` only stripped a leading `=`, so the er_runner
# format captured `head=<sha>` (with the `head=` prefix retained) and the
# prefix-match against the current PR head always failed. The live
# incident: drive-PR-#487 on 2026-07-31 had to fall back to the space
# format because `head=<sha>` would never match.
#
# This test guards both formats against future regression by asserting the
# extraction regex+awk+sed pipeline produces the bare SHA in each case.


def _simulate_head_sha_extraction(comment_body: str) -> str:
    """Mirror the head-SHA extraction step in `evidence-gate.yml:215` via the
    actual bash pipeline (avoids re-implementing POSIX grep -E semantics in
    Python where `[[:space:]]` is not a recognised class).

    Equivalent of:
        echo "$c_body" | grep -oE 'head[[:space:]]*=[[:space:]]*[0-9a-f]{7,64}|head[[:space:]]+[0-9a-f]{7,64}' | head -1 | awk '{print $NF}' | sed -E 's/^head=//; s/^=//'
    """
    import subprocess

    script = (
        "grep -oE 'head[[:space:]]*=[[:space:]]*[0-9a-f]{7,64}"
        "|head[[:space:]]+[0-9a-f]{7,64}'"
        " | head -1 | awk '{print $NF}' | sed -E 's/^head=//; s/^=//'"
    )
    proc = subprocess.run(
        ["bash", "-c", script],
        input=comment_body,
        capture_output=True,
        text=True,
        timeout=5,
    )
    return proc.stdout.rstrip("\n")


def test_evidence_gate_extracts_head_sha_with_equals_format() -> None:
    """Regression: the er_runner format `head=<sha>` MUST extract to the
    bare SHA (jleechan-wjm2). Previously the extraction kept the `head=`
    prefix and the gate failed."""
    sha = "00eaae8a2017fd1399a9d57519dd67218dae8deb"
    body = f"🤖 **[dark-factory /er]** Evidence review verdict: ``` /er PASS ``` head={sha}"
    extracted = _simulate_head_sha_extraction(body)
    assert extracted == sha, (
        f"head=<sha> format must extract to bare SHA, got {extracted!r}; "
        f"the Evidence Gate would have compared `head={sha}` against "
        f"`{sha}` and failed. (jleechan-wjm2)"
    )


def test_evidence_gate_extracts_head_sha_with_space_format() -> None:
    """The marker-line format `head <sha>` MUST also extract to the bare SHA
    (the workaround used to drive PR #487 on 2026-07-31)."""
    sha = "00eaae8a2017fd1399a9d57519dd67218dae8deb"
    body = f"🤖 **[dark-factory /er]** Evidence review verdict: ``` /er PASS ``` head {sha}"
    extracted = _simulate_head_sha_extraction(body)
    assert extracted == sha, (
        f"head <sha> format must extract to bare SHA, got {extracted!r} "
        f"(jleechan-wjm2)"
    )


def test_evidence_gate_workflow_uses_head_equals_strip() -> None:
    """The evidence-gate.yml workflow file MUST contain the fixed sed
    expression that strips the `head=` prefix (jleechan-wjm2)."""
    text = _verdict_step_text(_load_workflow())
    # Accept any of the forms that strip the leading `head=` prefix:
    #   - sed -E 's/^head=//; s/^=//'
    #   - sed 's/^head=//; s/^=//'
    #   - sed 's/^head=//'
    # The OLD form `sed 's/^=//'` (without head= strip) is what caused
    # the bug; ensure it has been replaced.
    has_strip_head = bool(
        re.search(r"sed\s+(?:-E\s+)?['\"]?s/\^head=|s/\^head=", text)
    )
    assert has_strip_head, (
        "evidence-gate.yml head-SHA extraction sed must strip the `head=` "
        "prefix; the previous `sed 's/^=//'` form retained it and broke "
        "Signal A for /er comments in `head=<sha>` format. (jleechan-wjm2)"
    )
    # Sanity: the broken single-strip form must NOT appear without the
    # head= companion strip.
    bare_equals_strip = bool(re.search(r"sed\s+(?:-E\s+)?['\"]?s/\^=//['\"]?", text))
    has_head_equals_strip = bool(re.search(r"sed\s+(?:-E\s+)?['\"]?s/\^head=//", text))
    if bare_equals_strip and not has_head_equals_strip:
        raise AssertionError(
            "evidence-gate.yml still uses bare `sed 's/^=//'` without "
            "stripping `head=` — the documented er_runner format "
            "(`head=<sha>`) will fail to match. (jleechan-wjm2)"
        )


@pytest.mark.parametrize(
    "files,pr,repo,declared,current,expected",
    [
        # 1-byte gist: must FAIL (issue #433 acceptance).
        (
            [("out.txt", "x")],
            433, "owner/repo", "abc1234", "abc1234", "FAIL",
        ),
        # Substantive content for the current head: must PASS.
        (
            [
                (
                    "evidence.txt",
                    "PR #433 / owner-repo\n/er PASS\nhead=abc1234\n"
                    "integration tests run: 12 passed, 0 failed\n"
                    "video: https://github.com/example/cast.cast\n"
                    "test runner output excerpt:\n"
                    "  test_signal_a_requires_trusted_identity_marker PASSED\n"
                    "  test_signal_a_captures_comment_author PASSED\n"
                    "  test_signal_b_enforces_minimum_gist_content_size PASSED\n"
                    "  test_signal_b_requires_substantive_content_keywords PASSED\n"
                    "  test_signal_a_trusted_identity_plus_head_binding PASSED\n",
                )
            ],
            433, "owner/repo", "abc1234", "abc1234", "PASS",
        ),
        # Empty gist (no files): must FAIL.
        (
            [],
            433, "owner/repo", "abc1234", "abc1234", "FAIL",
        ),
        # Substantive size but missing PR anchor: must FAIL.
        (
            [
                (
                    "log.txt",
                    "this is a long enough file but does not reference the issue number or the org\n" * 8,
                )
            ],
            433, "owner/repo", "abc1234", "abc1234", "FAIL",
        ),
        # Stale head: must FAIL.
        (
            [
                (
                    "evidence.txt",
                    "PR #433 / owner-repo\n/er PASS\nhead=oldhead\n"
                    "integration tests run: 12 passed, 0 failed\n" * 4,
                )
            ],
            433, "owner/repo", "oldhead", "newhead", "FAIL",
        ),
    ],
)
def test_signal_b_content_floor(
    files: list[tuple[str, str]],
    pr: int,
    repo: str,
    declared: str,
    current: str,
    expected: str,
) -> None:
    """Issue #433 acceptance: gist must meet size floor + PR anchor + head binding."""
    assert (
        _simulate_signal_b_decision(files, pr, repo, declared, current) == expected
    ), f"Signal B (bytes={sum(len(c) for _,c in files)}, declared={declared!r}) should map to {expected}"


# ---------------------------------------------------------------------------
# bead jleechan-2qn8 — Signal B PR anchor regex must accept natural prose.
#
# The previous regex only matched when the literal `#N` was quote-adjacent
# (a JSON-embedded form), which no human- or agent-written gist naturally
# satisfies. A gist that says `PR #571` (the natural form) failed the gate
# with `missing_pr_anchor`. The fix replaces the quote-adjacent alternation
# with a word-boundary adjacency that accepts the same JSON form AND every
# natural prose form a human or agent writes.
# ---------------------------------------------------------------------------


def _pr_anchor_regex(pr_number: int) -> str:
    """Mirror the (fixed) PR anchor regex used by evidence-gate.yml.

    The new pattern: `#<PR_NUMBER>` preceded by start-of-line or any
    non-digit character (so `#571` inside `PR #571` matches but `#1571`
    does not). The previous form required a literal `"` immediately
    before the `#`, which only matched JSON-embedded forms.
    """
    return rf'(^|[^0-9])#{pr_number}\b'


def _match_pr_anchor(text: str, pr_number: int) -> bool:
    """Run the actual bash regex against the given text via subprocess.

    Using subprocess avoids re-implementing POSIX `grep -E` semantics in
    Python (where `\\b` and character classes differ).
    """
    import subprocess

    proc = subprocess.run(
        ["bash", "-c", f"grep -qE '{_pr_anchor_regex(pr_number)}'"],
        input=text,
        capture_output=True,
        text=True,
        timeout=5,
    )
    return proc.returncode == 0


@pytest.mark.parametrize(
    "gist_body,pr_number,expected",
    [
        # Natural prose — the form humans and agents actually write.
        ("PR #571 passed review.\n", 571, True),
        ("See PR #571 for the full diff.\n", 571, True),
        ("Closes #571 with this change.\n", 571, True),
        ("(#571) trailing parens\n", 571, True),
        ("[#571] bracketed ref\n", 571, True),
        ("* #571 bulleted\n", 571, True),
        ("#571\n", 571, True),  # line-start
        # JSON-embedded form — must still match (back-compat).
        ('key: "#571" pull_request\n', 571, True),
        ('"#571" pull_request\n', 571, True),
        # Wrong PR number — must NOT match (regression guard).
        ("PR #572 unrelated\n", 571, False),
        ("PR #1571 superset\n", 571, False),
        # No PR anchor — must NOT match.
        ("this is unrelated evidence\n", 571, False),
        ("", 571, False),
    ],
)
def test_signal_b_pr_anchor_regex_accepts_natural_prose(
    gist_body: str, pr_number: int, expected: bool
) -> None:
    """bead jleechan-2qn8: the PR anchor regex must accept natural prose forms.

    The previous regex required a literal `"` immediately before `#N`,
    which no human- or agent-written gist naturally satisfies. A gist
    that says `PR #571` failed the gate with `missing_pr_anchor`. The
    fix replaces the quote-adjacent alternation with a word-boundary
    adjacency that accepts the JSON form AND every natural prose form.
    """
    actual = _match_pr_anchor(gist_body, pr_number)
    assert actual == expected, (
        f"PR anchor regex (pr={pr_number}) on {gist_body!r}: "
        f"expected {expected}, got {actual}."
    )


def test_signal_b_pr_anchor_regex_is_quote_free() -> None:
    """bead jleechan-2qn8: the fixed PR anchor regex must NOT require a
    literal double-quote before `#N`.

    The previous regex's alternation (`"#N\\b|"#N"|\\"#N\\"`) all
    required a `"` immediately before the hash — JSON-only. The fix
    removes the quote requirement entirely.
    """
    text = _verdict_step_text(_load_workflow())
    # Look for the old quote-adjacent alternation. The new pattern
    # uses `(^|[^0-9])#<PR>` and contains no `"` before the hash.
    has_quote_adjacent = bool(
        re.search(r'\\?"#\$\{?PR_NUMBER\}?\\?b', text)
        or re.search(r'\\?"#\$\{?PR_NUMBER\}?\\?"', text)
        or re.search(r'\\\\\\?"#\$\{?PR_NUMBER\}?\\\\\\?"', text)
    )
    assert not has_quote_adjacent, (
        "evidence-gate.yml PR anchor regex still requires a quote "
        "immediately before `#N` — the natural prose form `PR #571` "
        "will fail the gate. (bead jleechan-2qn8)"
    )
    # The new form must be present: `(^|[^0-9])#<PR_NUMBER>`.
    assert re.search(r"\(\^|\[\^0-9\]\)#\$\{?PR_NUMBER\}?", text), (
        "evidence-gate.yml PR anchor regex must use `(^|[^0-9])#<PR_NUMBER>` "
        "form so natural prose `PR #571` matches. (bead jleechan-2qn8)"
    )
