"""Tests for the SHA-bound Skeptic gate (issue #278).

These tests cover the deterministic verdict-binding logic only.
Reviewer invocation and GitHub side effects are injected so the
contract is testable in isolation, per the issue's ZFC rule:
"deterministic code only validates the structured verdict and SHA binding."
"""

from __future__ import annotations

import pytest

from runner.skeptic_gate import (
    MARKER,
    ParsedVerdict,
    ValidationResult,
    bind_to_pr,
    build_prompt,
    comment_marker,
    evaluate,
    format_comment,
    parse_verdict,
)


# ---- parse_verdict ------------------------------------------------------------


def test_parse_verdict_pass_extracts_all_fields():
    out = (
        "Some preamble text the reviewer may produce.\n"
        "VERDICT: PASS\n"
        "HEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
        "REASON: Diff is small and well-scoped.\n"
    )
    parsed = parse_verdict(out)
    assert parsed is not None
    assert parsed.verdict == "PASS"
    assert parsed.head_sha == "abcdef1234567890abcdef1234567890abcdef12"
    assert parsed.repo == "jleechanorg/dark-factory"
    assert parsed.pr_number == 278


def test_parse_verdict_fail_extracts_minimal_fields():
    out = "VERDICT: FAIL\nHEAD_SHA: 1234567\n"
    parsed = parse_verdict(out)
    assert parsed is not None
    assert parsed.verdict == "FAIL"
    assert parsed.head_sha == "1234567"
    assert parsed.repo is None
    assert parsed.pr_number is None


def test_parse_verdict_handles_short_sha():
    """Short SHAs (7-12 hex) are valid."""
    out = "VERDICT: PASS\nHEAD_SHA: abc1234\n"
    parsed = parse_verdict(out)
    assert parsed is not None
    assert parsed.head_sha == "abc1234"


def test_parse_verdict_case_insensitive_verdict():
    out = "verdict: pass\nhead_sha: deadbeef\n"
    parsed = parse_verdict(out)
    assert parsed is not None
    assert parsed.verdict == "PASS"


def test_parse_verdict_rejects_missing_verdict():
    """No VERDICT line → unparseable, fail-closed."""
    out = "HEAD_SHA: abcdef\nREPO: jleechanorg/dark-factory\n"
    assert parse_verdict(out) is None


def test_parse_verdict_rejects_missing_sha():
    """No HEAD_SHA line → unparseable, fail-closed."""
    out = "VERDICT: PASS\nREPO: jleechanorg/dark-factory\n"
    assert parse_verdict(out) is None


def test_parse_verdict_rejects_invalid_sha_format():
    """Non-hex SHA → unparseable."""
    out = "VERDICT: PASS\nHEAD_SHA: not-a-real-sha\n"
    assert parse_verdict(out) is None


def test_parse_verdict_rejects_unknown_verdict_token():
    """WARN or other tokens are not allowed — only PASS|FAIL."""
    out = "VERDICT: WARN\nHEAD_SHA: abc1234\n"
    assert parse_verdict(out) is None


def test_parse_verdict_rejects_non_string_input():
    """Defensive: non-string types must return None, not crash."""
    assert parse_verdict(None) is None  # type: ignore[arg-type]
    assert parse_verdict(42) is None  # type: ignore[arg-type]
    assert parse_verdict(["VERDICT: PASS"]) is None  # type: ignore[arg-type]


def test_parse_verdict_handles_extra_whitespace():
    out = "  VERDICT:   PASS  \n\tHEAD_SHA:\tabcdef0\n"  # 7 hex chars (min SHA length)
    parsed = parse_verdict(out)
    assert parsed is not None
    assert parsed.verdict == "PASS"
    assert parsed.head_sha == "abcdef0"


# ---- bind_to_pr ---------------------------------------------------------------


def test_bind_to_pr_accepts_matching_context():
    """All three fields match → ok=True, verdict is propagated."""
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="newsha9999",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="newsha9999",
    )
    assert isinstance(result, ValidationResult)
    assert result.ok is True
    assert result.verdict == "PASS"


def test_bind_to_pr_rejects_stale_sha():
    """Stale-SHA PASS must never satisfy — the headline invariant of issue #278."""
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="oldsha1234",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="newsha5678",
    )
    assert result.ok is False
    assert "stale" in result.reason.lower()
    assert result.verdict is None  # binding failed → no propagated verdict


def test_bind_to_pr_rejects_repo_mismatch_when_present():
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="sha1234",
        repo="attacker/repo",
        pr_number=278,
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="sha1234",
    )
    assert result.ok is False
    assert "repo" in result.reason.lower()


def test_bind_to_pr_rejects_pr_number_mismatch_when_present():
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="sha1234",
        repo=None,
        pr_number=999,
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="sha1234",
    )
    assert result.ok is False
    assert "pr number" in result.reason.lower()


def test_bind_to_pr_treats_missing_optional_fields_as_non_binding():
    """A reviewer that omits REPO/PR_NUMBER still binds via SHA only."""
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="newsha9999",
        repo=None,
        pr_number=None,
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="newsha9999",
    )
    assert result.ok is True
    assert result.verdict == "PASS"


# ---- format_comment -----------------------------------------------------------


def test_format_comment_contains_marker_for_idempotent_upsert():
    """The HTML marker must be present so the upsert can find the prior comment."""
    body = format_comment(
        verdict="PASS",
        head_sha="abcdef1234",
        expected_head_sha="abcdef1234",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="gemini",
    )
    assert MARKER in body
    assert comment_marker() == MARKER


def test_format_comment_pass_contains_verdict_and_sha_lines():
    body = format_comment(
        verdict="PASS",
        head_sha="abcdef1234567890",
        expected_head_sha="abcdef1234567890",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="gemini",
    )
    assert "VERDICT: PASS" in body
    assert "HEAD_SHA: abcdef1234567890" in body
    assert "PR_NUMBER: 278" in body
    assert "REPO: jleechanorg/dark-factory" in body
    assert "STALE" not in body  # current SHA, not stale


def test_format_comment_fail_contains_verdict_and_sha_lines():
    body = format_comment(
        verdict="FAIL",
        head_sha="abcdef1234567890",
        expected_head_sha="abcdef1234567890",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="gemini",
        reason="introduces unscoped file deletions",
    )
    assert "VERDICT: FAIL" in body
    assert "introduces unscoped file deletions" in body


def test_format_comment_marks_stale_pass_as_warning():
    """A PASS verdict whose SHA doesn't match the current head must be flagged STALE."""
    body = format_comment(
        verdict="PASS",
        head_sha="oldsha1234",
        expected_head_sha="newsha5678",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="gemini",
    )
    assert "VERDICT: PASS" in body  # what the reviewer said
    assert "STALE" in body  # but it's stale
    assert "oldsha1234" in body
    assert "newsha5678" in body


# ---- evaluate (the orchestrator's deterministic side) -------------------------


def _ctx(**overrides):
    """Standard PR context for evaluate() calls."""
    base = dict(
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha="abcdef1234567890",
        base_sha="0000000000000000",
        diff="diff --git a/foo b/foo\n+hello",
        reviewer="gemini",
    )
    base.update(overrides)
    return base


def test_evaluate_pass_path_yields_success_state():
    """Happy path: reviewer emits PASS with current SHA → success."""
    output = (
        "VERDICT: PASS\n"
        "HEAD_SHA: abcdef1234567890\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
    )
    result = evaluate(review_output=output, **_ctx())
    assert result.check_state == "success"
    assert result.verdict == "PASS"
    assert "VERDICT: PASS" in result.comment_body
    assert "abcdef1234567890" in result.comment_body


def test_evaluate_fail_path_yields_failure_state():
    """Reviewer emits FAIL with current SHA → failure state, comment posted."""
    output = (
        "VERDICT: FAIL\n"
        "HEAD_SHA: abcdef1234567890\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
        "REASON: destructive rm -rf in deploy.sh\n"
    )
    result = evaluate(review_output=output, **_ctx())
    assert result.check_state == "failure"
    assert result.verdict == "FAIL"
    assert "VERDICT: FAIL" in result.comment_body
    assert "destructive" in result.comment_body


def test_evaluate_stale_sha_pass_yields_failure_not_success():
    """Stale-SHA PASS must never satisfy. Even if the reviewer says PASS, the
    verdict's SHA doesn't match the current PR head, so the gate fails."""
    output = (
        "VERDICT: PASS\n"
        "HEAD_SHA: deadbeef00000000\n"  # different from expected abcdef1234567890
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
    )
    result = evaluate(review_output=output, **_ctx())
    assert result.check_state == "failure"
    assert "stale" in result.reason.lower()
    assert "STALE" in result.comment_body  # comment must mark this clearly


def test_evaluate_missing_reviewer_yields_error_state():
    """Reviewer errored out (binary missing, timeout, non-zero exit) → fail closed."""
    result = evaluate(
        review_output=None,
        review_error="codex: command not found",
        **_ctx(),
    )
    assert result.check_state == "failure"
    assert result.verdict is None
    assert "unavailable" in result.reason.lower() or "error" in result.reason.lower()


def test_evaluate_malformed_output_yields_failure_state():
    """Reviewer returned but produced no parseable VERDICT/SHA → fail closed."""
    result = evaluate(
        review_output="I think this looks good but I won't say VERDICT: PASS explicitly.",
        **_ctx(),
    )
    assert result.check_state == "failure"
    assert result.verdict is None
    assert "parse" in result.reason.lower() or "verdict" in result.reason.lower()


def test_evaluate_both_missing_yields_error_state():
    """Defensive: neither output nor error provided → fail closed."""
    result = evaluate(review_output=None, review_error=None, **_ctx())
    assert result.check_state == "failure"
    assert result.verdict is None


# ---- duplicate reruns (idempotent comment) ------------------------------------


def test_evaluate_duplicate_rerun_same_sha_produces_same_marker():
    """Re-running with the same SHA must produce a comment with the same
    upsert marker, so the workflow's update logic replaces the prior comment
    instead of creating a new one."""
    output = "VERDICT: PASS\nHEAD_SHA: abcdef1234567890\n"
    r1 = evaluate(review_output=output, **_ctx())
    r2 = evaluate(review_output=output, **_ctx())
    assert comment_marker() in r1.comment_body
    assert comment_marker() in r2.comment_body
    # And the verdict lines match
    assert "VERDICT: PASS" in r1.comment_body
    assert "VERDICT: PASS" in r2.comment_body


def test_evaluate_rerun_after_flip_updates_verdict_in_comment():
    """Re-running with the same SHA but a different verdict (PASS→FAIL) must
    produce a comment with the new verdict, ready to replace the old one."""
    pass_output = "VERDICT: PASS\nHEAD_SHA: abcdef1234567890\n"
    fail_output = "VERDICT: FAIL\nHEAD_SHA: abcdef1234567890\n"
    r1 = evaluate(review_output=pass_output, **_ctx())
    r2 = evaluate(review_output=fail_output, **_ctx())
    assert "VERDICT: PASS" in r1.comment_body
    assert "VERDICT: FAIL" in r2.comment_body
    # Both share the same marker, so the workflow's upsert finds the old one
    assert comment_marker() in r1.comment_body
    assert comment_marker() in r2.comment_body


# ---- build_prompt -------------------------------------------------------------


def test_build_prompt_includes_pr_context_and_diff():
    prompt = build_prompt(
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha="abcdef1234567890",
        base_sha="0000000000000000",
        diff="diff --git a/foo b/foo\n+hello\n",
    )
    assert "jleechanorg/dark-factory" in prompt
    assert "278" in prompt
    assert "abcdef1234567890" in prompt
    assert "0000000000000000" in prompt
    assert "+hello" in prompt
    # Output contract is in the prompt
    assert "VERDICT: PASS" in prompt or "VERDICT: <PASS|FAIL>" in prompt
    assert "HEAD_SHA:" in prompt


# ---- CLI-level tests (reviewer argv + dry-run main) --------------------------


def _cli_argv(**overrides):
    """Build a minimal argv for `main()`."""
    base = [
        "--repo", "jleechanorg/dark-factory",
        "--pr-number", "278",
        "--pr-sha", "abcdef1234567890",
        "--reviewer", "gemini",
        "--reviewer-model", "gemini-2.5-pro",
        "--dry-run",
    ]
    for k, v in overrides.items():
        base.extend([f"--{k.replace('_', '-')}", str(v)])
    return base


def test_build_reviewer_cmd_codex_uses_stdin():
    from runner.skeptic_gate_cli import _build_reviewer_cmd

    cmd = _build_reviewer_cmd("codex", "o3-mini")
    assert cmd[0] == "codex"
    assert "exec" in cmd
    # codex exec reads prompt from stdin when "-" is the trailing arg
    assert cmd[-1] == "-"
    # model flag is present
    idx = cmd.index("-m")
    assert cmd[idx + 1] == "o3-mini"


def test_build_reviewer_cmd_gemini_uses_prompt_placeholder():
    from runner.skeptic_gate_cli import _build_reviewer_cmd

    cmd = _build_reviewer_cmd("gemini", "gemini-2.5-pro")
    assert cmd[0] == "gemini"
    assert "__PROMPT_PLACEHOLDER__" in cmd
    idx = cmd.index("-m")
    assert cmd[idx + 1] == "gemini-2.5-pro"


def test_build_reviewer_cmd_rejects_unknown_reviewer():
    from runner.skeptic_gate_cli import _build_reviewer_cmd

    with pytest.raises(RuntimeError, match="unknown reviewer"):
        _build_reviewer_cmd("claude", "opus-4")  # claude is the implementing model — explicitly excluded


def test_invoke_reviewer_missing_binary_returns_error():
    """When the reviewer binary is absent, we get (None, error_msg) and
    the gate must fail closed. The test uses `codex-_-missing-binary-xyz`
    which definitely does not exist on PATH."""
    from runner.skeptic_gate_cli import _build_reviewer_cmd, invoke_reviewer

    # We can't monkey-patch the path inside the CLI without ugly mocks,
    # so we instead point _build_reviewer_cmd at a non-existent binary
    # by monkeypatching it for this test.
    import runner.skeptic_gate_cli as cli_mod

    original = cli_mod._build_reviewer_cmd

    def fake_cmd(reviewer, model):
        return ["definitely-not-a-real-binary-xyz", "-"]

    cli_mod._build_reviewer_cmd = fake_cmd
    try:
        out, err = invoke_reviewer("anything", "any-model", "prompt")
    finally:
        cli_mod._build_reviewer_cmd = original
    assert out is None
    assert err is not None
    assert "not found" in err.lower() or "no such file" in err.lower()


def test_invoke_reviewer_nonzero_exit_returns_error_and_stdout():
    """When the reviewer exits non-zero, the gate sees (stdout, error) so
    it can still surface a useful message in the failure comment."""
    from runner.skeptic_gate_cli import _build_reviewer_cmd, invoke_reviewer

    import runner.skeptic_gate_cli as cli_mod

    def fake_cmd(reviewer, model):
        return ["false"]  # /bin/false: exits 1 with no output

    original = cli_mod._build_reviewer_cmd
    cli_mod._build_reviewer_cmd = fake_cmd
    try:
        out, err = invoke_reviewer("anything", "any-model", "prompt")
    finally:
        cli_mod._build_reviewer_cmd = original
    assert out == ""  # /bin/false produces no stdout
    assert err is not None
    assert "rc=1" in err


def test_main_dry_run_missing_reviewer_returns_nonzero(monkeypatch, capsys):
    """End-to-end: dry-run main() with a missing reviewer binary → exit 1,
    deterministic verdict-binding still records the failure cleanly."""
    import runner.skeptic_gate_cli as cli_mod

    # Bypass `gh` — we only want to exercise the reviewer-arg + evaluate path.
    monkeypatch.setattr(cli_mod, "get_pr_context",
                        lambda repo, pr_number: ("abcdef1234567890",
                                                 "0000000000000000",
                                                 "diff --git a/foo b/foo\n+hello\n"))
    monkeypatch.setattr(cli_mod, "_build_reviewer_cmd",
                        lambda reviewer, model: ["definitely-not-a-real-binary-xyz"])
    rc = cli_mod.main(_cli_argv())
    assert rc == 1
    captured = capsys.readouterr()
    assert "diff capture failed" not in captured.err
    assert "unavailable" in captured.err.lower() or "reviewer" in captured.err.lower()
