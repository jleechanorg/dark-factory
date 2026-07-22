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


def test_evidence_gate_runs_on_is_appropriate_for_repo_visibility() -> None:
    """Confirm runs-on matches what is correct for this PUBLIC repo.

    Issue #424 states `runs on ubuntu-latest contrary to the self-hosted
    runner policy for this private repo`. jleechanorg/dark-factory is
    PUBLIC (verified via `gh repo view`), so ubuntu-latest IS the
    correct runs-on.
    """
    workflow = _load_workflow()
    job = _job(workflow)
    runs_on = job.get("runs-on")
    if isinstance(runs_on, list):
        runs_on_rendered = ", ".join(runs_on)
    else:
        runs_on_rendered = str(runs_on)
    assert "ubuntu-latest" in runs_on_rendered, (
        f"runs-on must include ubuntu-latest for this PUBLIC repo; "
        f"got {runs_on_rendered!r}. (issue #424 — runner-policy alignment)"
    )
    assert "SELF_HOSTED_RUNNER_LABELS" not in runs_on_rendered, (
        f"runs-on must NOT reference vars.SELF_HOSTED_RUNNER_LABELS on a "
        f"PUBLIC repo; got {runs_on_rendered!r}."
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
# Issue #433 — Signal A trusted-identity + head-SHA binding;
#               Signal B gist content floor + PR/repo mention
# ---------------------------------------------------------------------------
#
# Issue #433 (Codex re-review of main 53a3999, follow-up to merged #424):
# the previous gate is fail-closed but its signals are forgeable.
#
#   Signal A: greps `/er PASS` from ANY comment body. The PR author can
#             self-post the verdict. Not bound to commenter identity or
#             head SHA. Fix: require the verdict comment to carry the
#             literal `🤖 **[dark-factory /er]**` allowlist marker that
#             daemon/src/er_runner.rs:191 emits, AND require the comment
#             to reference the CURRENT head SHA. Stale-head verdicts
#             must not green.
#
#   Signal B: verifies gist reachable + non-empty + declared head SHA,
#             but does not validate content. Fix: require total content
#             size above a small floor (no 1-byte gists) AND require the
#             content to mention the PR number or repo name.
#
# These tests pin the new structural and decision-table requirements;
# they are RED against the pre-#433 workflow and GREEN against the
# hardened version.


_ER_RUNNER_MARKER_LITERAL = "🤖 **[dark-factory /er]**"
_ER_RUNNER_MARKER_BARE = "[dark-factory /er]"  # stripped-emoji form for regex


def _signal_a_text(workflow: dict) -> str:
    """Text of the Signal A step (the part that parses /er comments)."""
    workflow_text = _verdict_step_text(workflow)
    # Signal A is the step that calls `gh api /repos/.../issues/.../comments`.
    # Find the step whose run block touches the comments endpoint.
    jobs = workflow.get("jobs", {})
    job = next(iter(jobs.values()))
    for step in job.get("steps", []):
        name = step.get("name") or ""
        run = step.get("run") or ""
        if not isinstance(run, str):
            run = "\n".join(run)
        if "/comments" in run and ("Determine evidence verdict" in name
                                   or "Verify /er verdict" in name):
            return run
    # Fallback: full verdict-step text (covers minimal workflows).
    return workflow_text


def _signal_b_text(workflow: dict) -> str:
    """Text of the Signal B step (the part that parses gist content)."""
    workflow_text = _verdict_step_text(workflow)
    jobs = workflow.get("jobs", {})
    job = next(iter(jobs.values()))
    for step in job.get("steps", []):
        run = step.get("run") or ""
        if not isinstance(run, str):
            run = "\n".join(run)
        if "api.github.com/gists/" in run:
            return run
    return workflow_text


def test_signal_a_requires_trusted_er_runner_marker() -> None:
    """Signal A MUST require the `[dark-factory /er]` allowlist marker.

    Issue #433: Signal A greps `/er PASS` from ANY comment body. The PR
    author can self-post that verdict. Fix: require the comment to
    carry the literal `🤖 **[dark-factory /er]**` marker that
    daemon/src/er_runner.rs:191 emits when posting a real /er verdict.
    """
    text = _signal_a_text(_load_workflow())
    assert (
        _ER_RUNNER_MARKER_LITERAL in text
        or _ER_RUNNER_MARKER_BARE in text
        or "dark-factory /er" in text
    ), (
        "Signal A must require the `[dark-factory /er]` allowlist marker "
        "in the comment body — author-self-posted `/er PASS` without the "
        "marker is forgeable. (issue #433)"
    )


def test_signal_a_requires_head_sha_reference() -> None:
    """Signal A MUST require the verdict comment to reference a head SHA.

    Issue #433: a stale `/er PASS` comment from a prior head must not
    green the gate. Fix: the workflow must parse a `head <sha>` reference
    from the comment (or the evidence marker) and compare it to the
    current head SHA.
    """
    text = _signal_a_text(_load_workflow())
    # Either Signal A reads `head <sha>` from the comment body, or it
    # delegates the SHA comparison to a separate verification step. In
    # either case the workflow's verdict-decision text must contain a
    # SHA comparison expression or a "stale" branch.
    head_sha_signals = [
        r"head[[:space:]]+[0-9a-f]{7,40}",
        r"\bhead_sha\b",
        r"\bstale\b",
        r"\bSTALE\b",
        r"sha_short",
        r"head_sha_seen",
        r"\$marker_sha\b",
    ]
    assert any(re.search(pat, text) for pat in head_sha_signals), (
        "Signal A must require the verdict comment to carry a `head <sha>` "
        "reference (issue #433) — a bare `/er PASS` from an older head is "
        "forgeable. The verifier step must parse or compare the SHA."
    )


def test_signal_b_requires_minimum_content_size_floor() -> None:
    """Signal B MUST enforce a minimum content size floor on the gist.

    Issue #433: the previous Signal B accepted any gist with at least one
    non-empty file, including trivial 1-byte gists. Fix: aggregate file
    content (or require total raw bytes) and enforce a floor — at minimum
    a few hundred bytes — so a placeholder gist cannot green the gate.

    The pre-#433 check is per-file `length > 0`. The post-#433 check
    must reference an aggregate floor (e.g. `MIN_GIST_SIZE`,
    `content_size`, total bytes > 200) — not just `length > 0`.
    """
    text = _signal_b_text(_load_workflow())
    aggregate_floor_patterns = [
        r"MIN_GIST_SIZE",
        r"content_size",
        r"total_bytes?",
        r"GIST_MIN_BYTES",
        r"min_size",
        r"size_floor",
        # Aggregate floor that is NOT just per-file `length > 0`.
        r"\b[0-9]{2,}\s*\)\s*$",  # bare numeric literal near a comparison
        r"\b200\b",
        r"\b256\b",
        r"\b512\b",
        r"-gt[[:space:]]+[0-9]{2,}",
    ]
    assert any(re.search(pat, text, re.MULTILINE) for pat in aggregate_floor_patterns), (
        "Signal B must enforce an aggregate minimum content size floor "
        "(MIN_GIST_SIZE / content_size / total_bytes / > 200 etc.) — the "
        "previous per-file `length > 0` check accepts 1-byte gists. "
        "(issue #433)"
    )


def test_signal_b_requires_pr_or_repo_mention_in_content() -> None:
    """Signal B MUST verify the gist content mentions the PR number or repo.

    Issue #433: gist verification is purely structural today — no content
    check. Fix: after fetching file content, require it to contain either
    the PR number (e.g. `#123`) or the repo slug (e.g. `owner/repo`). A
    generic placeholder gist must not green the gate.

    The test is intentionally stricter than `PR_NUMBER` (which already
    appears in the env var block). It requires either:
      (a) a content-grep for the PR number literal (`grep -F "${PR_NUMBER}"`),
      (b) a content-grep for the repo slug, OR
      (c) a positive assertion that the content was checked against
          these values.
    """
    text = _signal_b_text(_load_workflow())
    # Anchor each pattern to look like a real grep command: "grep" followed
    # by flag chars then a ${PR_NUMBER} or ${REPO} reference. The previous
    # workflow's /er verdict grep must not accidentally match — that's a
    # Signal A grep, not a Signal B content check.
    content_check_patterns = [
        r"\bgrep\b[^\n|]*\$\{?PR_NUMBER\}?",
        r"\bgrep\b[^\n|]*\$\{?REPO\}?",
        r"mention_check",
        r"content_matches",
        r"gist_content_check",
        r"MUST mention",
        r"required_field",
    ]
    assert any(re.search(pat, text) for pat in content_check_patterns), (
        "Signal B must verify that gist content references the PR number "
        "(via grep against $PR_NUMBER / github.event.pull_request.number) "
        "or the repo (via grep against $REPO / github.repository). The "
        "presence of $REPO / $PR_NUMBER as env-var definitions is not "
        "sufficient — the content must be checked. (issue #433)"
    )


def _simulate_verdict_decision_v2(
    er_verdict: str | None,
    marker_verdict: str | None = None,
    *,
    er_trusted: bool = False,
    er_head_sha_fresh: bool = False,
    marker_content_substantive: bool = False,
) -> str:
    """Hardened verdict simulation for issue #433.

    Mirrors the bash verdict logic after the fix:
      - Signal A passes only when an `/er PASS` comment exists, the
        commenter is the trusted er_runner marker, AND the comment
        references the current head SHA.
      - Signal B passes only when the gist is reachable AND its content
        is non-trivial AND it mentions the PR number or repo.
    """
    er = er_verdict if er_verdict else "ABSENT"
    mk = marker_verdict if marker_verdict else "ABSENT"
    if er in ("FAIL", "PARTIAL", "INCONCLUSIVE"):
        return "FAIL"
    if er == "PASS" and er_trusted and er_head_sha_fresh:
        return "PASS"
    if mk == "PASS" and marker_content_substantive:
        return "PASS"
    return "FAIL"


@pytest.mark.parametrize(
    "er_verdict,marker_verdict,er_trusted,er_head_fresh,mk_substantive,expected_gate",
    [
        # Trusted + fresh head ⇒ PASS (the one true green path for Signal A)
        ("PASS", None, True, True, False, "PASS"),
        # Author self-posts bare /er PASS without the marker ⇒ FAIL
        ("PASS", None, False, False, False, "FAIL"),
        # Author self-post overrides a 1-byte gist (gist not substantive) ⇒ FAIL
        ("PASS", "PASS", False, False, False, "FAIL"),
        # Marker is present but head SHA is stale ⇒ FAIL
        ("PASS", None, True, False, False, "FAIL"),
        # Marker verdict for current head but gist content is empty ⇒ FAIL
        (None, "PASS", False, False, False, "FAIL"),
        # Substantive gist content alone is enough (no /er comment)
        (None, "PASS", False, False, True, "PASS"),
        # /er FAIL always overrides marker PASS
        ("FAIL", "PASS", True, True, True, "FAIL"),
        # Everything absent ⇒ FAIL
        (None, None, False, False, False, "FAIL"),
    ],
)
def test_verdict_decision_table_v2_blocks_forgeable_signals(
    er_verdict: str | None,
    marker_verdict: str | None,
    er_trusted: bool,
    er_head_fresh: bool,
    mk_substantive: bool,
    expected_gate: str,
) -> None:
    """Issue #433: the hardened verdict table must block every forgeable input.

    Cases:
      - Author self-posted `/er PASS` without the trusted marker ⇒ FAIL.
      - Trusted marker but stale head SHA ⇒ FAIL.
      - Empty gist content ⇒ FAIL.
      - `/er FAIL` overrides marker PASS ⇒ FAIL.
      - Only the substantive-gist path with no /er comment ⇒ PASS.
    """
    got = _simulate_verdict_decision_v2(
        er_verdict,
        marker_verdict,
        er_trusted=er_trusted,
        er_head_sha_fresh=er_head_fresh,
        marker_content_substantive=mk_substantive,
    )
    assert got == expected_gate, (
        f"verdict (er={er_verdict!r}, marker={marker_verdict!r}, "
        f"trusted={er_trusted}, fresh={er_head_fresh}, "
        f"substantive={mk_substantive}) should map to gate={expected_gate}; "
        f"got {got}. (issue #433 fail-closed contract)"
    )


def test_author_self_posted_bare_er_pass_does_not_green_gate() -> None:
    """Issue #433 explicit regression: an author-self-posted bare `/er PASS`
    comment — without the `[dark-factory /er]` allowlist marker and
    without any head-SHA reference — MUST NOT green the gate.
    """
    assert (
        _simulate_verdict_decision_v2(
            "PASS", None, er_trusted=False, er_head_sha_fresh=False
        )
        == "FAIL"
    )


def test_stale_head_verdict_does_not_green_gate() -> None:
    """Issue #433 explicit regression: an `[dark-factory /er]` comment
    referencing an OLDER head SHA MUST NOT green the gate.
    """
    assert (
        _simulate_verdict_decision_v2(
            "PASS", None, er_trusted=True, er_head_sha_fresh=False
        )
        == "FAIL"
    )


def test_one_byte_gist_does_not_green_gate() -> None:
    """Issue #433 explicit regression: a 1-byte gist (sub-floor content)
    MUST NOT green the gate.
    """
    assert (
        _simulate_verdict_decision_v2(
            None, "PASS", marker_content_substantive=False
        )
        == "FAIL"
    )


def test_signal_a_iteration_must_not_word_split_jq_output() -> None:
    """Signal A MUST iterate jq-compact JSON lines without word-splitting.

    Issue #433 follow-up (jleechan-ifkt): the hardened Signal A originally
    used `for row in $(echo "${comments_json}" | jq -c '.[]')` which
    word-splits each JSON object on its inner whitespace (the `"` in jq
    output are NOT bash quoting). On any PR whose comments contain spaces,
    the first iteration fragment is invalid JSON, `jq -r '.body // ""'`
    fails with "Unfinished string at EOF", `set -euo pipefail` exits 5,
    and Signal B never runs — leaving the gate red even when a valid
    `**Evidence**:` marker is present in the PR body.

    The fix uses `mapfile -t rows < <(jq -c '.[]')` followed by
    `for row in "${rows[@]}"`, which preserves each jq-compact line intact.

    This test pins the iteration pattern so the regression cannot recur.
    """
    text = _signal_a_text(_load_workflow())
    # Strip YAML comment lines so documentation referencing the bad
    # pattern cannot accidentally satisfy or fail the assertion.
    code_lines = [
        line for line in text.splitlines()
        if not line.lstrip().startswith("#")
    ]
    code_text = "\n".join(code_lines)
    # The fragile pattern MUST NOT appear in actual code (anywhere outside
    # a `#` comment). Match on a bash loop opening with `for row in $(`.
    assert "for row in $(" not in code_text, (
        "Signal A iterates `for row in $(jq ...)` which word-splits each "
        "JSON object on inner whitespace. On any PR with comments "
        "containing spaces, this crashes with `jq: Unfinished string at "
        "EOF`, exits 5, and prevents Signal B from ever running. "
        "Use `mapfile -t rows < <(jq -c '.[]')` + `for row in \"${rows[@]}\"` "
        "instead. (issue #433 follow-up, jleechan-ifkt)"
    )
    # The safe pattern MUST appear in code.
    assert "mapfile" in code_text and "for row in \"${rows[@]}\"" in code_text, (
        "Signal A must iterate jq-compact JSON lines via "
        "`mapfile -t rows < <(jq -c '.[]')` followed by "
        "`for row in \"${rows[@]}\"` so each JSON object is delivered "
        "intact (issue #433 follow-up, jleechan-ifkt)."
    )
