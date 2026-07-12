"""Tests for the SHA-bound Skeptic gate (issue #278, mandatory redesign).

Covers:
  - Strict `parse_verdict` (six fields, each EXACTLY ONCE, full 40-char
    SHA, anti-injection / anti-code-block-spoofing)
  - `bind_to_pr` headline invariant (stale-SHA PASS never satisfies)
  - `verify_provenance` (refuse self-review)
  - `format_comment` (idempotent upsert via MARKER)
  - `evaluate` (single-reviewer deterministic verdict binding)
  - `aggregate_results` (ALL reviewers must PASS)
  - `verify_published_comment` (read-back gates the publish)
  - `build_prompt` (mentions all six required fields, IDENTITY)
  - CLI: `_reviewer_env` sanitizer, reviewer sandbox flags,
    `_extract_codex_message`, forced PASS/FAIL acceptance via
    `aggregate_results`, dual-reviewer contract.

These tests are the ironclad exit criteria: passing all of them is a
hard precondition for claiming the gate is green.
"""

from __future__ import annotations

import json
import os
import re
from typing import List
from unittest import mock

import pytest

from runner.skeptic_gate import (
    MARKER,
    ParsedVerdict,
    ReadBackCheck,
    SkepticResult,
    ValidationResult,
    aggregate_results,
    bind_to_pr,
    build_prompt,
    comment_marker,
    evaluate,
    format_comment,
    parse_verdict,
    verify_published_comment,
    verify_provenance,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _valid_output(
    *,
    verdict: str = "PASS",
    head_sha: str = "abcdef1234567890abcdef1234567890abcdef12",
    repo: str = "jleechanorg/dark-factory",
    pr_number: int = 278,
    reason: str = "diff is small and well-scoped",
    identity: str = "codex",
) -> str:
    """A canonical 6-line, 1-of-each reviewer output.

    `head_sha` MUST be 40 hex chars by default — that's the strict
    contract. Tests that exercise the short-SHA rejection path use
    override it.
    """
    return (
        f"VERDICT: {verdict}\n"
        f"HEAD_SHA: {head_sha}\n"
        f"REPO: {repo}\n"
        f"PR_NUMBER: {pr_number}\n"
        f"REASON: {reason}\n"
        f"IDENTITY: {identity}\n"
    )


def _ctx(**overrides):
    base = dict(
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        base_sha="0000000000000000000000000000000000000000",
        diff="diff --git a/foo b/foo\n+hello",
        reviewer="codex",
    )
    base.update(overrides)
    return base


# ===========================================================================
# parse_verdict — strict 6-field contract
# ===========================================================================


def test_parse_verdict_full_contract_passes():
    parsed = parse_verdict(_valid_output())
    assert parsed is not None
    assert parsed.verdict == "PASS"
    assert parsed.head_sha == "abcdef1234567890abcdef1234567890abcdef12"
    assert parsed.repo == "jleechanorg/dark-factory"
    assert parsed.pr_number == 278
    assert parsed.reviewr_identity if False else parsed.reviewer_identity == "codex"


def test_parse_verdict_rejects_short_sha():
    """A reviewer that emits only a 7-char SHA has not fully bound its
    verdict — the deterministic side rejects the verdict."""
    out = "VERDICT: PASS\nHEAD_SHA: abc1234\nREPO: x/y\nPR_NUMBER: 1\nREASON: ok\nIDENTITY: codex\n"
    assert parse_verdict(out) is None


def test_parse_verdict_rejects_short_sha_minimum_seven():
    """The 7-hex minimum is what GitHub displays; we require the full
    40-hex SHA so a sibling review cannot bind to a short ID."""
    out = "VERDICT: PASS\nHEAD_SHA: abc1234\nREPO: x/y\nPR_NUMBER: 1\nREASON: ok\nIDENTITY: codex\n"
    assert parse_verdict(out) is None


def test_parse_verdict_rejects_missing_field():
    out = "VERDICT: PASS\nHEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\nREPO: x/y\nPR_NUMBER: 1\nREASON: ok\n"
    assert parse_verdict(out) is None  # IDENTITY missing → not "exactly one OR zero" misbehavior; we still require


def test_parse_verdict_rejects_duplicate_verdict_lines():
    """Anti-injection: code block with VERDICT: PASS plus a top-level
    VERDICT: FAIL. findall returns 2 → reject."""
    out = (
        "VERDICT: FAIL\n"
        "HEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
        "REASON: looks good\n"
        "IDENTITY: codex\n"
        "\n"
        "```\n"
        "VERDICT: PASS\n"
        "```\n"
    )
    assert parse_verdict(out) is None


def test_parse_verdict_rejects_duplicate_identity_lines():
    """Two IDENTITY lines → reject. Otherwise an attacker could claim
    one identity for the structured parse and another for the audit
    trail."""
    out = (
        "VERDICT: PASS\n"
        "HEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
        "REASON: ok\n"
        "IDENTITY: codex\n"
        "IDENTITY: claude\n"
    )
    assert parse_verdict(out) is None


def test_parse_verdict_unknown_identity_maps_to_unknown():
    out = _valid_output(identity="banana")
    assert parse_verdict(out) is None  # not in {claude, codex, gemini, unknown}


def test_parse_verdict_rejects_non_string_input():
    assert parse_verdict(None) is None
    assert parse_verdict(42) is None
    assert parse_verdict(["VERDICT: PASS"]) is None


def test_parse_verdict_handles_case_insensitive_field_lines():
    out = (
        "verdict: pass\n"
        "head_sha: abcdef1234567890abcdef1234567890abcdef12\n"
        "repo: jleechanorg/dark-factory\n"
        "pr_number: 278\n"
        "reason: ok\n"
        "identity: codex\n"
    )
    parsed = parse_verdict(out)
    assert parsed is not None
    assert parsed.verdict == "PASS"
    assert parsed.reviewer_identity == "codex"


def test_parse_verdict_rejects_unknown_verdict_token():
    out = "VERDICT: WARN\nHEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\nREPO: x/y\nPR_NUMBER: 1\nREASON: ok\nIDENTITY: codex\n"
    assert parse_verdict(out) is None


# ===========================================================================
# bind_to_pr — the headline invariant
# ===========================================================================


def test_bind_to_pr_accepts_matching_context():
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reason="ok",
        reviewer_identity="codex",
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="abcdef1234567890abcdef1234567890abcdef12",
    )
    assert isinstance(result, ValidationResult)
    assert result.ok is True
    assert result.verdict == "PASS"


def test_bind_to_pr_rejects_stale_sha():
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reason="ok",
        reviewer_identity="codex",
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="0000000000000000000000000000000000000001",
    )
    assert result.ok is False
    assert "stale" in result.reason.lower()


def test_bind_to_pr_rejects_repo_mismatch():
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        repo="attacker/repo",
        pr_number=278,
        reason="ok",
        reviewer_identity="codex",
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="abcdef1234567890abcdef1234567890abcdef12",
    )
    assert result.ok is False
    assert "repo" in result.reason.lower()


def test_bind_to_pr_rejects_pr_number_mismatch():
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        repo="jleechanorg/dark-factory",
        pr_number=999,
        reason="ok",
        reviewer_identity="codex",
        raw_excerpt="",
    )
    result = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha="abcdef1234567890abcdef1234567890abcdef12",
    )
    assert result.ok is False
    assert "pr number" in result.reason.lower()


# ===========================================================================
# verify_provenance — refuses self-review
# ===========================================================================


def test_verify_provenance_accepts_independent():
    ok, why = verify_provenance("claude", "codex")
    assert ok is True
    assert "independent" in why.lower()


def test_verify_provenance_rejects_self_review_claude_codex():
    ok, why = verify_provenance("claude", "claude")
    assert ok is False
    assert "self-review" in why.lower()


def test_verify_provenance_rejects_unknown_reviewer():
    ok, why = verify_provenance("claude", "unknown")
    assert ok is False
    assert "identity is unknown" in why.lower() or "must declare" in why.lower()


def test_verify_provenance_rejects_unknown_implementer():
    """If we cannot prove the implementer was Claude, we cannot prove
    a reviewer is independent of it. Refuse."""
    ok, why = verify_provenance("unknown", "codex")
    assert ok is False
    assert "implementation identity is unknown" in why.lower()


def test_verify_provenance_gemini_implementer_with_codex_reviewer():
    """If a Gemini-authored PR is reviewed by Codex, the review is
    independent."""
    ok, _ = verify_provenance("gemini", "codex")
    assert ok is True


# ===========================================================================
# format_comment — idempotent upsert via MARKER
# ===========================================================================


def test_format_comment_contains_marker():
    body = format_comment(
        verdict="PASS",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_head_sha="abcdef1234567890abcdef1234567890abcdef12",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="codex",
    )
    assert MARKER in body
    assert comment_marker() == MARKER


def test_format_comment_marks_stale_pass_as_warning():
    body = format_comment(
        verdict="PASS",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_head_sha="0000000000000000000000000000000000000001",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="codex",
    )
    assert "VERDICT: PASS" in body
    assert "STALE" in body


def test_format_comment_extra_reviewer_lines_preserve_marker():
    body = format_comment(
        verdict="FAIL",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_head_sha="abcdef1234567890abcdef1234567890abcdef12",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="(aggregate)",
        reason="one reviewer failed",
        extra_reviewer_lines=[
            "- **codex** — ✅ PASS",
            "- **gemini** — ❌ FAIL — destructive rm -rf",
        ],
    )
    assert body.startswith(MARKER)
    assert "- **codex**" in body
    assert "- **gemini**" in body


# ===========================================================================
# evaluate — single reviewer
# ===========================================================================


def test_evaluate_pass_path_yields_success_state():
    out = _valid_output()
    result = evaluate(review_output=out, **_ctx())
    assert result.check_state == "success"
    assert result.verdict == "PASS"


def test_evaluate_stale_sha_yields_failure():
    out = _valid_output(
        head_sha="0000000000000000000000000000000000000001"
    )
    result = evaluate(review_output=out, **_ctx())
    assert result.check_state == "failure"
    assert "stale" in result.reason.lower()


def test_evaluate_missing_reviewer_fails_closed():
    result = evaluate(review_output=None, review_error="codex: not found",
                      **_ctx())
    assert result.check_state == "failure"


def test_evaluate_malformed_output_fails_closed():
    result = evaluate(
        review_output="looks good, no extra text", **_ctx()
    )
    assert result.check_state == "failure"


# ===========================================================================
# aggregate_results — multi-reviewer independence
# ===========================================================================


def _reviewer_result(*, reviewer: str, ok: bool, identity: str = "codex") -> SkepticResult:
    sha = "abcdef1234567890abcdef1234567890abcdef12"
    if ok:
        body = f"{MARKER}\nverdict pass from {reviewer}\n"
        return SkepticResult(
            check_state="success",
            verdict="PASS",
            reason=f"{reviewer} ok",
            comment_body=body,
            parsed=ParsedVerdict(
                verdict="PASS",
                head_sha=sha,
                repo="jleechanorg/dark-factory",
                pr_number=278,
                reason="ok",
                reviewer_identity=identity,
                raw_excerpt="",
            ),
            reviewer=reviewer,
        )
    body = f"{MARKER}\nverdict fail from {reviewer}\n"
    return SkepticResult(
        check_state="failure",
        verdict=None,
        reason=f"{reviewer} failed",
        comment_body=body,
        parsed=None,
        reviewer=reviewer,
    )


def test_aggregate_results_both_pass_yields_success():
    results = [
        _reviewer_result(reviewer="codex", ok=True, identity="codex"),
        _reviewer_result(reviewer="gemini", ok=True, identity="gemini"),
    ]
    agg = aggregate_results(
        results, repo="jleechanorg/dark-factory", pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
    )
    assert agg.check_state == "success"
    assert agg.verdict == "PASS"


def test_aggregate_results_one_fail_yields_failure():
    """Forced FAIL: one reviewer fails → aggregate fails closed even if
    the other reviewer PASSed."""
    results = [
        _reviewer_result(reviewer="codex", ok=True, identity="codex"),
        _reviewer_result(reviewer="gemini", ok=False, identity="gemini"),
    ]
    agg = aggregate_results(
        results, repo="jleechanorg/dark-factory", pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
    )
    assert agg.check_state == "failure"
    assert agg.verdict is None


def test_aggregate_results_empty_list_yields_failure():
    agg = aggregate_results(
        [], repo="jleechanorg/dark-factory", pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
    )
    assert agg.check_state == "failure"


# ===========================================================================
# Forced PASS/FAIL acceptance (the ironclad acceptance test)
# ===========================================================================
#
# These two tests prove the deterministic binding actually accepts a
# well-formed PASS and actually rejects a well-formed FAIL. Without
# them, the gate could silently treat any output as PASS.


def test_forced_pass_acceptance_full_pipeline_binds_to_current_head():
    """Forced PASS: the deterministic side must recognize this output
    as a PASS bound to the current head SHA. The test asserts:
      - parse_verdict accepts it
      - bind_to_pr accepts it
      - evaluate returns success
      - aggregate_results with two such results returns success
    No credentials, no network, no reviewer CLI."""
    sha = "abcdef1234567890abcdef1234567890abcdef12"
    out_codex = _valid_output(identity="codex")
    out_gemini = _valid_output(identity="gemini")
    parsed_codex = parse_verdict(out_codex)
    parsed_gemini = parse_verdict(out_gemini)
    assert parsed_codex is not None
    assert parsed_gemini is not None
    assert parsed_codex.head_sha == sha
    assert parsed_gemini.head_sha == sha
    binding_codex = bind_to_pr(
        parsed_codex,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha=sha,
    )
    assert binding_codex.ok is True
    binding_gemini = bind_to_pr(
        parsed_gemini,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha=sha,
    )
    assert binding_gemini.ok is True

    r1 = evaluate(review_output=out_codex, **_ctx(reviewer="codex"))
    r2 = evaluate(review_output=out_gemini, **_ctx(reviewer="gemini"))
    assert r1.check_state == "success" and r1.verdict == "PASS"
    assert r2.check_state == "success" and r2.verdict == "PASS"

    agg = aggregate_results(
        [r1, r2],
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha=sha,
    )
    assert agg.check_state == "success"
    assert agg.verdict == "PASS"
    assert "VERDICT: PASS" in agg.comment_body
    assert sha in agg.comment_body


def test_forced_fail_acceptance_full_pipeline_propagates_failure():
    """Forced FAIL: the reviewer emits a clean FAIL with a current SHA.
    The deterministic side must:
      - parse it
      - bind it
      - propagate `verdict=FAIL` (not None — the reviewer SAID FAIL)
      - aggregate with another PASS still yields FAIL."""
    sha = "abcdef1234567890abcdef1234567890abcdef12"
    out_fail = _valid_output(verdict="FAIL", identity="codex", reason="destructive")
    parsed = parse_verdict(out_fail)
    assert parsed is not None
    assert parsed.verdict == "FAIL"
    binding = bind_to_pr(
        parsed,
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_head_sha=sha,
    )
    assert binding.ok is True
    r_fail = evaluate(review_output=out_fail, **_ctx(reviewer="codex"))
    assert r_fail.check_state == "failure"
    assert r_fail.verdict == "FAIL"

    out_pass = _valid_output(identity="gemini")
    r_pass = evaluate(review_output=out_pass, **_ctx(reviewer="gemini"))
    assert r_pass.check_state == "success"

    agg = aggregate_results(
        [r_fail, r_pass],
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha=sha,
    )
    # ANY FAIL → aggregate FAIL.
    assert agg.check_state == "failure"
    assert agg.verdict is None
    assert "FAIL" in agg.comment_body


# ===========================================================================
# verify_published_comment — read-back gates the publish
# ===========================================================================


def test_verify_published_comment_accepts_correct_readback():
    rb = ReadBackCheck(
        actor="github-actions[bot]",
        body_contains_marker=True,
        body_sha="abcdef1234567890abcdef1234567890abcdef12",
        body_repo="jleechanorg/dark-factory",
        body_pr_number=278,
        body_verdict="PASS",
        body_reviewer="codex",
        body_implementation_provenance="claude",
    )
    ok, why = verify_published_comment(
        rb,
        expected_actor="github-actions[bot]",
        expected_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_verdict="PASS",
        expected_reviewer="codex",
        expected_implementation_provenance="claude",
    )
    assert ok is True


def test_verify_published_comment_rejects_wrong_actor():
    """If the comment was posted by `some-other-actor`, fail closed."""
    rb = ReadBackCheck(
        actor="malicious-bot",
        body_contains_marker=True,
        body_sha="abcdef1234567890abcdef1234567890abcdef12",
        body_repo="jleechanorg/dark-factory",
        body_pr_number=278,
        body_verdict="PASS",
        body_reviewer="codex",
        body_implementation_provenance="claude",
    )
    ok, _ = verify_published_comment(
        rb,
        expected_actor="github-actions[bot]",
        expected_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_verdict="PASS",
        expected_reviewer="codex",
        expected_implementation_provenance="claude",
    )
    assert ok is False


def test_verify_published_comment_rejects_missing_marker():
    rb = ReadBackCheck(
        actor="github-actions[bot]",
        body_contains_marker=False,
        body_sha="abcdef1234567890abcdef1234567890abcdef12",
        body_repo="jleechanorg/dark-factory",
        body_pr_number=278,
        body_verdict="PASS",
        body_reviewer="codex",
        body_implementation_provenance="claude",
    )
    ok, _ = verify_published_comment(
        rb,
        expected_actor="github-actions[bot]",
        expected_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_verdict="PASS",
        expected_reviewer="codex",
        expected_implementation_provenance="claude",
    )
    assert ok is False


# ===========================================================================
# build_prompt — full contract
# ===========================================================================


def test_build_prompt_mentions_all_required_fields_and_identity():
    prompt = build_prompt(
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        base_sha="0000000000000000000000000000000000000000",
        diff="+x",
        implementation_identity="claude",
    )
    assert "VERDICT" in prompt
    assert "HEAD_SHA" in prompt
    assert "REPO" in prompt
    assert "PR_NUMBER" in prompt
    assert "REASON" in prompt
    assert "IDENTITY" in prompt
    assert "EXACTLY ONCE" in prompt
    assert "implementation identity" in prompt.lower()
    assert "claude" in prompt  # the implementation_identity is mentioned


# ===========================================================================
# CLI-level: sandbox flags, env sanitization, codex JSONL extraction
# ===========================================================================


def test_build_reviewer_cmd_codex_uses_sandbox_readonly():
    """codex must run with `--sandbox=read-only` and NOT with
    `--dangerously-bypass-approvals-and-sandbox`."""
    from runner.skeptic_gate_cli import _build_reviewer_cmd

    cmd = _build_reviewer_cmd("codex", "")
    assert "--sandbox" in cmd
    s_idx = cmd.index("--sandbox")
    assert cmd[s_idx + 1] == "read-only"
    assert "--dangerously-bypass-approvals-and-sandbox" not in cmd
    # Last arg is "-" (stdin prompt) — same convention as before
    assert cmd[-1] == "-"


def test_build_reviewer_cmd_gemini_uses_sandbox():
    """gemini must run sandboxed (`-s`) with `--approval-mode=default`,
    NOT `yolo`."""
    from runner.skeptic_gate_cli import _build_reviewer_cmd

    cmd = _build_reviewer_cmd("gemini", "gemini-2.5-pro")
    assert "-s" in cmd
    # default approval mode, not yolo
    assert "default" in cmd
    assert "yolo" not in cmd


def test_reviewer_env_strips_secrets():
    """The sanitizer must NOT leak GITHUB_TOKEN, GH_TOKEN, HOME,
    OPENCLAW_*, HERMES_*, SLACK_*, SSH agent socket, or cloud
    credentials. Per post-audit comment 4953064910, HOME is stripped
    so the reviewer process cannot read user-level credentials."""
    from runner.skeptic_gate_cli import _reviewer_env

    parent = {
        "GITHUB_TOKEN": "secret",
        "GH_TOKEN": "secret",
        "OPENCLAW_GATEWAY_TOKEN": "secret",
        "SLACK_BOT_TOKEN": "secret",
        "HERMES_SLACK_WEBHOOK_URL": "secret",
        "PATH": "/usr/bin",
        "HOME": "/root",
        "USER": "jleechan",
        "SSH_AUTH_SOCK": "/tmp/ssh-XXXX",
        "AWS_ACCESS_KEY_ID": "AKIA...",
        "OPENAI_API_KEY": "sk-xxx",
        "FOO_BAR": "user-set",
    }
    sanitized = _reviewer_env(parent)
    assert "GITHUB_TOKEN" not in sanitized
    assert "GH_TOKEN" not in sanitized
    assert "OPENCLAW_GATEWAY_TOKEN" not in sanitized
    assert "SLACK_BOT_TOKEN" not in sanitized
    assert "HERMES_SLACK_WEBHOOK_URL" not in sanitized
    # HOME/USER are stripped (defense against shell-rc reads).
    assert "HOME" not in sanitized
    assert "USER" not in sanitized
    assert "SSH_AUTH_SOCK" not in sanitized
    assert "AWS_ACCESS_KEY_ID" not in sanitized
    # Allowlist passes through (PATH, TMPDIR, OPENAI_API_KEY).
    assert sanitized["PATH"] == "/usr/bin"
    assert sanitized["OPENAI_API_KEY"] == "sk-xxx"
    # Non-allowlisted, non-secret is dropped (conservative).
    assert "FOO_BAR" not in sanitized


def test_extract_codex_message_pulls_last_agent_message():
    from runner.skeptic_gate_cli import _extract_codex_message

    jsonl = (
        '{"type":"thread.started","thread_id":"abc"}\n'
        '{"type":"turn.started"}\n'
        '{"type":"item.completed","item":{"type":"reasoning","text":"thinking..."}}\n'
        '{"type":"item.completed","item":{"type":"agent_message",'
        '"text":"VERDICT: PASS\\nHEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\\n'
        'REPO: jleechanorg/dark-factory\\nPR_NUMBER: 278\\nREASON: ok\\nIDENTITY: codex"}}\n'
        '{"type":"turn.completed","usage":{"input_tokens":100}}\n'
    )
    text = _extract_codex_message(jsonl)
    assert "VERDICT: PASS" in text
    assert "IDENTITY: codex" in text


def test_extract_codex_message_no_agent_message_returns_empty():
    from runner.skeptic_gate_cli import _extract_codex_message

    assert _extract_codex_message('{"type":"thread.started"}\n') == ""


def test_extract_codex_message_takes_last_when_multiple():
    from runner.skeptic_gate_cli import _extract_codex_message

    jsonl = (
        '{"type":"item.completed","item":{"type":"agent_message","text":"first draft"}}\n'
        '{"type":"item.completed","item":{"type":"agent_message",'
        '"text":"VERDICT: PASS\\nHEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\\n'
        'REPO: x/y\\nPR_NUMBER: 2\\nREASON: final\\nIDENTITY: gemini"}}\n'
    )
    text = _extract_codex_message(jsonl)
    assert "final" in text
    assert "first draft" not in text


def test_invoke_reviewer_missing_binary_returns_error():
    """Forced FAIL: reviewer binary is absent → (None, error_msg)."""
    import runner.skeptic_gate_cli as cli_mod

    original = cli_mod._build_reviewer_cmd

    def fake_cmd(reviewer, model, *, codex_bin="", gemini_bin=""):
        return ["definitely-not-a-real-binary-xyz"]

    cli_mod._build_reviewer_cmd = fake_cmd
    try:
        out, err = cli_mod.invoke_reviewer(
            "anything", "any-model", "prompt", parent_env={"PATH": "/bin"}
        )
    finally:
        cli_mod._build_reviewer_cmd = original
    assert out is None
    assert err is not None
    assert "not found" in err.lower() or "no such file" in err.lower()


def test_invoke_reviewer_nonzero_exit_returns_error():
    """Forced FAIL: reviewer exits non-zero → (stdout, error_msg)."""
    import runner.skeptic_gate_cli as cli_mod

    original = cli_mod._build_reviewer_cmd

    def fake_cmd(reviewer, model, *, codex_bin="", gemini_bin=""):
        return ["false"]  # always exits 1

    cli_mod._build_reviewer_cmd = fake_cmd
    try:
        out, err = cli_mod.invoke_reviewer(
            "anything", "any-model", "prompt", parent_env={"PATH": "/bin"}
        )
    finally:
        cli_mod._build_reviewer_cmd = original
    assert err is not None
    assert "rc=1" in err


# ===========================================================================
# End-to-end: forced PASS via the CLI with both reviewers' outputs mocked
# ===========================================================================


def _cli_argv(**overrides):
    base = [
        "--repo", "jleechanorg/dark-factory",
        "--pr-number", "278",
        "--pr-sha", "abcdef1234567890abcdef1234567890abcdef12",
        "--reviewers-json", '[["codex",""],["gemini","gemini-2.5-pro"]]',
        "--dry-run",
    ]
    for k, v in overrides.items():
        base.extend([f"--{k.replace('_', '-')}", str(v)])
    return base


def _inline_structured_verdict(
    *,
    verdict: str = "PASS",
    head_sha: str = "abcdef1234567890abcdef1234567890abcdef12",
    identity: str = "codex",
    reason: str = "ok",
) -> str:
    """Return a 6-line structured verdict string."""
    return (
        f"VERDICT: {verdict}\n"
        f"HEAD_SHA: {head_sha}\n"
        f"REPO: jleechanorg/dark-factory\n"
        f"PR_NUMBER: 278\n"
        f"REASON: {reason}\n"
        f"IDENTITY: {identity}\n"
    )


def _reviewer_stdout(reviewer: str, *, verdict: str = "PASS",
                     identity: str = "codex",
                     head_sha: str = "abcdef1234567890abcdef1234567890abcdef12") -> str:
    """Emit the stdout that a real CLI would produce.

    codex `--json` → JSONL events. The agent_message.text contains the
    structured verdict.
    gemini → plain text with the structured verdict.
    """
    body = _inline_structured_verdict(
        verdict=verdict, identity=identity, head_sha=head_sha
    )
    if reviewer == "codex":
        event = {
            "type": "item.completed",
            "item": {"type": "agent_message", "text": body},
        }
        return json.dumps(event) + "\n"
    return body


def test_cli_forced_pass_with_both_reviewers(monkeypatch, capsys):
    """Both reviewers emit valid structured PASS — aggregate must be
    PASS. Forced acceptance path."""
    import runner.skeptic_gate_cli as cli_mod

    monkeypatch.setattr(
        cli_mod, "get_pr_head_sha_via_api",
        lambda repo, pr: "abcdef1234567890abcdef1234567890abcdef12",
    )
    monkeypatch.setattr(
        cli_mod, "get_pr_diff",
        lambda repo, pr: "diff --git a/foo b/foo\n+hello\n",
    )
    monkeypatch.setattr(
        cli_mod, "get_implementation_identity",
        lambda repo, pr: "claude",
    )

    def fake_cmd(reviewer, model, *, codex_bin="", gemini_bin=""):
        # Emit the stdout a real reviewer would produce.
        return [
            "python3", "-c",
            "import sys; sys.stdout.write(" +
            repr(_reviewer_stdout(reviewer, identity=reviewer)) +
            ")",
        ]

    monkeypatch.setattr(cli_mod, "_build_reviewer_cmd", fake_cmd)

    rc = cli_mod.main(_cli_argv())
    captured = capsys.readouterr()
    assert rc == 0, f"expected PASS rc=0, got rc={rc}\n{captured.err}"
    assert "AGGREGATE verdict=PASS" in captured.err


def test_cli_forced_fail_with_missing_reviewer(monkeypatch, capsys):
    """One reviewer unavailable → aggregate FAIL. Forced FAIL path."""
    import runner.skeptic_gate_cli as cli_mod

    monkeypatch.setattr(
        cli_mod, "get_pr_head_sha_via_api",
        lambda repo, pr: "abcdef1234567890abcdef1234567890abcdef12",
    )
    monkeypatch.setattr(
        cli_mod, "get_pr_diff",
        lambda repo, pr: "+x\n",
    )
    monkeypatch.setattr(
        cli_mod, "get_implementation_identity",
        lambda repo, pr: "claude",
    )

    def fake_cmd(reviewer, model, *, codex_bin="", gemini_bin=""):
        if reviewer == "codex":
            return ["definitely-not-a-real-binary-xyz"]
        return [
            "python3", "-c",
            "import sys; sys.stdout.write(" +
            repr(_reviewer_stdout("gemini", identity="gemini")) +
            ")",
        ]

    monkeypatch.setattr(cli_mod, "_build_reviewer_cmd", fake_cmd)

    rc = cli_mod.main(_cli_argv())
    captured = capsys.readouterr()
    assert rc == 1, f"expected FAIL rc=1, got rc={rc}\n{captured.err}"
    assert "AGGREGATE verdict=None" in captured.err


def test_cli_provenance_fails_self_review(monkeypatch, capsys):
    """Implementation identity matches a reviewer's declared identity
    → that reviewer's verdict is overridden to FAIL even if it said
    PASS. Aggregate fails. Forced self-review rejection."""
    import runner.skeptic_gate_cli as cli_mod

    monkeypatch.setattr(
        cli_mod, "get_pr_head_sha_via_api",
        lambda repo, pr: "abcdef1234567890abcdef1234567890abcdef12",
    )
    monkeypatch.setattr(
        cli_mod, "get_pr_diff",
        lambda repo, pr: "+x\n",
    )
    # Implementer is claude; both reviewers declare "claude" identity
    # → provenance fails for both.
    monkeypatch.setattr(
        cli_mod, "get_implementation_identity",
        lambda repo, pr: "claude",
    )

    def fake_cmd(reviewer, model, *, codex_bin="", gemini_bin=""):
        return [
            "python3", "-c",
            "import sys; sys.stdout.write(" +
            repr(_reviewer_stdout(reviewer, identity="claude")) +
            ")",
        ]

    monkeypatch.setattr(cli_mod, "_build_reviewer_cmd", fake_cmd)

    rc = cli_mod.main(_cli_argv())
    captured = capsys.readouterr()
    assert rc == 1, f"expected FAIL rc=1, got rc={rc}\n{captured.err}"
    assert "AGGREGATE verdict=None" in captured.err


# ===========================================================================
# Adversarial integration tests — post-audit comment 4953064910
# ===========================================================================
#
# Each test below corresponds to a specific attack described in the
# post-audit comment and the audit response. A PASS on the corresponding
# PR gate must satisfy ALL of them.


def test_adversarial_parse_rejects_code_block_injection():
    """A reviewer that wraps the verdict in a Markdown code block must
    be rejected — code-fence injection (the reviewer tries to hide a
    second VERDICT inside ```…```)."""
    out = (
        "```\n"
        "VERDICT: PASS\n"
        "HEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
        "REASON: ok\n"
        "IDENTITY: codex\n"
        "```\n"
    )
    assert parse_verdict(out) is None


def test_adversarial_parse_rejects_trailing_prose():
    """A reviewer that emits the verdict followed by free-form prose
    must be rejected — the deterministic side requires the strict
    no-prose contract."""
    out = (
        "VERDICT: PASS\n"
        "HEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
        "REASON: ok\n"
        "IDENTITY: codex\n"
        "\n"
        "Here's my reasoning in detail: ...\n"
    )
    assert parse_verdict(out) is None


def test_adversarial_parse_rejects_second_verdict_in_prose():
    """A reviewer that emits a second VERDICT line inside a free-form
    paragraph must be rejected (anti-injection)."""
    out = (
        "VERDICT: PASS\n"
        "HEAD_SHA: abcdef1234567890abcdef1234567890abcdef12\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 278\n"
        "REASON: ok\n"
        "IDENTITY: codex\n"
        "But I also wrote VERDICT: FAIL for archival\n"
    )
    assert parse_verdict(out) is None


def test_adversarial_commit_prefix_claudem_minimax_m3():
    """Commit-subject prefix `claudem/` maps to `claude`."""
    from runner.skeptic_gate import extract_implementation_identity_from_commit
    assert extract_implementation_identity_from_commit(
        "claudem/minimax-M3: feat(ci): skeptic gate redesign"
    ) == "claude"


def test_adversarial_commit_prefix_codexm_o3():
    """Commit-subject prefix `codexm/` maps to `codex`."""
    from runner.skeptic_gate import extract_implementation_identity_from_commit
    assert extract_implementation_identity_from_commit(
        "codexm/o3: fix: race"
    ) == "codex"


def test_adversarial_commit_prefix_unknown_for_unprefixed_subject():
    """A commit subject without a known prefix maps to `unknown`,
    which the gate refuses PASS on (conservative fail-closed)."""
    from runner.skeptic_gate import extract_implementation_identity_from_commit
    assert (
        extract_implementation_identity_from_commit("naked commit message") == "unknown"
    )
    assert extract_implementation_identity_from_commit("") == "unknown"
    assert extract_implementation_identity_from_commit(None) == "unknown"


def test_adversarial_bind_reviewer_identity_codex_must_declare_codex():
    """A codex CLI invocation that declares `gemini` is rejected."""
    from runner.skeptic_gate import bind_reviewer_identity
    ok, why = bind_reviewer_identity("codex", "gemini")
    assert ok is False
    assert "codex" in why and "gemini" in why


def test_adversarial_bind_reviewer_identity_gemini_must_declare_gemini():
    """A gemini CLI invocation that declares `codex` is rejected."""
    from runner.skeptic_gate import bind_reviewer_identity
    ok, why = bind_reviewer_identity("gemini", "codex")
    assert ok is False


def test_adversarial_bind_reviewer_identity_rejects_claude_or_unknown():
    """Reviewer identity must be `codex` or `gemini` (the two pinned
    CLIs). Declaring `claude` or `unknown` is refused — a codex
    invocation cannot impersonate Claude."""
    from runner.skeptic_gate import bind_reviewer_identity
    ok, _ = bind_reviewer_identity("codex", "claude")
    assert ok is False
    ok, _ = bind_reviewer_identity("codex", "unknown")
    assert ok is False


def test_adversarial_aggregate_rejects_duplicate_reviewer_identities():
    """A list of two codex results (or two gemini results) is rejected
    outright — a PR may not be reviewed twice by the same model."""
    codex_pass = evaluate(
        review_output=_valid_output(verdict="PASS", identity="codex"),
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        reviewer="codex",
        implementation_provenance="claude",
    )
    duplicate = evaluate(
        review_output=_valid_output(verdict="PASS", identity="codex"),
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        reviewer="codex",
        implementation_provenance="claude",
    )
    agg = aggregate_results(
        [codex_pass, duplicate],
        repo="jleechanorg/dark-factory",
        pr_number=278,
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
    )
    assert agg.check_state == "failure"
    assert "duplicate reviewer identities" in agg.reason


def test_adversarial_readback_rejects_wrong_sha():
    """Equality read-back fails when the published comment's HEAD_SHA
    differs from what we wrote."""
    rb = ReadBackCheck(
        actor="github-actions[bot]",
        body_contains_marker=True,
        body_sha="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        body_repo="jleechanorg/dark-factory",
        body_pr_number=278,
        body_verdict="PASS",
        body_reviewer="codex",
        body_implementation_provenance="claude",
    )
    ok, why = verify_published_comment(
        rb,
        expected_actor="github-actions[bot]",
        expected_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_verdict="PASS",
        expected_reviewer="codex",
        expected_implementation_provenance="claude",
    )
    assert ok is False
    assert "HEAD_SHA" in why


def test_adversarial_readback_rejects_wrong_implementation_provenance():
    """Equality read-back fails when the published comment's
    IMPLEMENTATION_PROVENANCE differs from what we wrote."""
    rb = ReadBackCheck(
        actor="github-actions[bot]",
        body_contains_marker=True,
        body_sha="abcdef1234567890abcdef1234567890abcdef12",
        body_repo="jleechanorg/dark-factory",
        body_pr_number=278,
        body_verdict="PASS",
        body_reviewer="codex",
        body_implementation_provenance="codex",  # claimed a non-claude implementer
    )
    ok, why = verify_published_comment(
        rb,
        expected_actor="github-actions[bot]",
        expected_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_verdict="PASS",
        expected_reviewer="codex",
        expected_implementation_provenance="claude",
    )
    assert ok is False
    assert "IMPLEMENTATION_PROVENANCE" in why


def test_adversarial_readback_rejects_empty_sha_field():
    """Per post-audit comment 4953064910, the previous read-back only
    checked non-empty; here we verify equality (empty ≠ expected)."""
    rb = ReadBackCheck(
        actor="github-actions[bot]",
        body_contains_marker=True,
        body_sha=None,
        body_repo="jleechanorg/dark-factory",
        body_pr_number=278,
        body_verdict="PASS",
        body_reviewer="codex",
        body_implementation_provenance="claude",
    )
    ok, why = verify_published_comment(
        rb,
        expected_actor="github-actions[bot]",
        expected_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_repo="jleechanorg/dark-factory",
        expected_pr_number=278,
        expected_verdict="PASS",
        expected_reviewer="codex",
        expected_implementation_provenance="claude",
    )
    assert ok is False


def test_adversarial_workflow_has_no_trusted_ref_input():
    """Per post-audit comment 4953064910, the workflow MUST NOT accept
    a caller-supplied `trusted_ref` (otherwise a PR-controlled caller
    can re-pin to PR head)."""
    import yaml
    with open(".github/workflows/skeptic-gate.yml") as f:
        wf = yaml.safe_load(f)
    inputs = ((wf.get("on") or {}).get("workflow_call") or {}).get("inputs") or {}
    dispatch_inputs = ((wf.get("on") or {}).get("workflow_dispatch") or {}).get("inputs") or {}
    assert "trusted_ref" not in inputs, (
        "workflow_call.inputs.trusted_ref must be removed "
        "(PR-controlled callers could otherwise re-pin to PR head)"
    )
    assert "trusted_ref" not in dispatch_inputs
    # Confirm the checkout step uses the trusted default branch, not
    # any caller-supplied ref.
    jobs = (wf.get("jobs") or {}).get("skeptic") or {}
    steps = jobs.get("steps") or []
    checkout = next(
        (s for s in steps if (s.get("name") or "").startswith("Checkout")), None
    )
    assert checkout is not None
    ref = (checkout.get("with") or {}).get("ref", "")
    assert "inputs.trusted_ref" not in ref, (
        f"checkout.ref must not interpolate inputs.trusted_ref; got {ref!r}"
    )
    assert "default_branch" in ref, (
        f"checkout.ref must pin to the trusted default branch; got {ref!r}"
    )


def test_adversarial_workflow_pins_reviewer_binaries():
    """The workflow must assert path/version/sha256 of each reviewer
    binary before invoking it (defense against mutable PATH installs)."""
    import yaml
    with open(".github/workflows/skeptic-gate.yml") as f:
        wf = yaml.safe_load(f)
    jobs = (wf.get("jobs") or {}).get("skeptic") or {}
    steps = jobs.get("steps") or []
    pin_step = next(
        (s for s in steps if "pinned" in (s.get("name") or "").lower()), None
    )
    assert pin_step is not None, "no reviewer-binary pinning step found"
    script = pin_step.get("run") or ""
    assert "sha256sum" in script, "sha256 verification missing"
    assert "check_binary" in script or "codex" in script and "gemini" in script
    # Env vars for pinned paths/versions/sha256 must be present.
    env = jobs.get("env") or {}
    for key in (
        "SKEPTIC_CODEX_BIN",
        "SKEPTIC_CODEX_VERSION",
        "SKEPTIC_CODEX_SHA256",
        "SKEPTIC_GEMINI_BIN",
        "SKEPTIC_GEMINI_VERSION",
        "SKEPTIC_GEMINI_SHA256",
    ):
        assert key in env, f"missing env var {key} for reviewer binary pinning"


def test_adversarial_workflow_strips_secrets_before_reviewer_invocation():
    """The reviewer-invocation step must unset GITHUB_TOKEN, GH_TOKEN,
    HOME, OPENCLAW_*, HERMES_*, SLACK_* secrets before invoking the
    Python gate (defense in depth: the CLI also strips them)."""
    import yaml
    with open(".github/workflows/skeptic-gate.yml") as f:
        wf = yaml.safe_load(f)
    jobs = (wf.get("jobs") or {}).get("skeptic") or {}
    steps = jobs.get("steps") or []
    run_step = next(
        (
            s for s in steps
            if (s.get("name") or "").startswith("Run skeptic")
        ),
        None,
    )
    assert run_step is not None, "no Run skeptic step found"
    script = run_step.get("run") or ""
    for secret in (
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "OPENCLAW_GATEWAY_TOKEN",
        "OPENCLAW_SLACK_BOT_TOKEN",
        "SLACK_BOT_TOKEN",
        "HERMES_SLACK_WEBHOOK_URL",
    ):
        assert secret in script, (
            f"reviewer invocation does not unset {secret!r}"
        )


def test_adversarial_cli_rejects_duplicate_reviewer_json():
    """The CLI MUST refuse `--reviewers-json` with duplicate reviewers
    (e.g. two codex entries)."""
    import runner.skeptic_gate_cli as cli_mod
    import pytest
    with pytest.raises(SystemExit):
        cli_mod._parse_reviewers('[["codex",""],["codex","gpt-5"]]')


def test_adversarial_cli_reviewers_default_is_distinct():
    """The default reviewer list must be distinct (codex AND gemini)."""
    from runner.skeptic_gate_cli import DEFAULT_REVIEWERS_JSON
    parsed = json.loads(DEFAULT_REVIEWERS_JSON)
    ids = [item[0] for item in parsed]
    assert len(set(ids)) == len(ids), (
        f"DEFAULT_REVIEWERS_JSON contains duplicates: {ids}"
    )


def test_adversarial_status_failure_is_fail_closed(monkeypatch, capsys):
    """If `set_commit_status` raises, the gate returns 1 (fail-closed),
    not 0. Per post-audit comment 4953064910, the previous version
    swallowed the error and could let a stale status satisfy the
    read-back."""
    import runner.skeptic_gate_cli as cli_mod

    monkeypatch.setattr(
        cli_mod, "get_pr_head_sha_via_api",
        lambda repo, pr: "abcdef1234567890abcdef1234567890abcdef12",
    )
    monkeypatch.setattr(
        cli_mod, "get_pr_diff",
        lambda repo, pr: "+x\n",
    )
    monkeypatch.setattr(
        cli_mod, "get_implementation_identity",
        lambda repo, pr: "claude",
    )

    def fake_cmd(reviewer, model, *, codex_bin="", gemini_bin=""):
        return [
            "python3", "-c",
            "import sys; sys.stdout.write(" +
            repr(_reviewer_stdout(reviewer, identity=reviewer)) +
            ")",
        ]

    monkeypatch.setattr(cli_mod, "_build_reviewer_cmd", fake_cmd)
    monkeypatch.setattr(cli_mod, "post_or_update_comment", lambda *a, **k: 9999)

    def boom(*a, **k):
        raise RuntimeError("status API 5xx")

    monkeypatch.setattr(cli_mod, "set_commit_status", boom)

    rc = cli_mod.main(["--repo", "jleechanorg/dark-factory",
                       "--pr-number", "278",
                       "--pr-sha", "abcdef1234567890abcdef1234567890abcdef12",
                       "--reviewers-json", '[["codex",""],["gemini","gemini-2.5-pro"]]',
                       "--expected-actor", "github-actions[bot]"])
    captured = capsys.readouterr()
    assert rc == 1, f"status failure must fail closed, got rc={rc}\n{captured.err}"
    assert "status set failed" in captured.err or "status API" in captured.err


def test_adversarial_diff_oversize_fails_closed(monkeypatch, capsys):
    """A diff exceeding MAX_DIFF_BYTES must fail closed (no truncation,
    no PASS)."""
    import runner.skeptic_gate_cli as cli_mod

    monkeypatch.setattr(
        cli_mod, "get_pr_head_sha_via_api",
        lambda repo, pr: "abcdef1234567890abcdef1234567890abcdef12",
    )

    def huge_diff(repo, pr):
        return "x" * (cli_mod.MAX_DIFF_BYTES + 1)

    monkeypatch.setattr(cli_mod, "get_pr_diff", huge_diff)

    rc = cli_mod.main(_cli_argv())
    captured = capsys.readouterr()
    assert rc == 1, f"oversize diff must fail closed, got rc={rc}\n{captured.err}"
    assert (
        "diff capture failed" in captured.err
        or "too large" in captured.err
        or "MAX_DIFF_BYTES" in captured.err
    )


def test_adversarial_format_comment_includes_implementation_provenance():
    """The published comment must include IMPLEMENTATION_PROVENANCE so
    the read-back verifier can equality-check it."""
    body = format_comment(
        verdict="PASS",
        head_sha="abcdef1234567890abcdef1234567890abcdef12",
        expected_head_sha="abcdef1234567890abcdef1234567890abcdef12",
        repo="jleechanorg/dark-factory",
        pr_number=278,
        reviewer="codex",
        implementation_provenance="claude",
        reason="ok",
    )
    assert "IMPLEMENTATION_PROVENANCE: claude" in body
    assert "HEAD_SHA: abcdef1234567890abcdef1234567890abcdef12" in body
    assert "REPO: jleechanorg/dark-factory" in body
    assert "PR_NUMBER: 278" in body
    assert "VERDICT: PASS" in body
    assert "REVIEWER: codex" in body
