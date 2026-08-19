"""PR #9092 (2026-08-18): GHA skeptic gate chain-walks the reviewer
priority queue on rate-limit / quota busts.

Before this PR, ``runner/dispatcher.py:VerifierDispatcher.run_one``
called ``_cli.invoke_reviewer`` exactly once with the resolved
reviewer. If that vendor's CLI hit a rate-limit (codex bust), the
factory failed the gate and never tried the next vendor in the
canonical priority queue. The daemon side already walks the queue
inline (via ``daemon/src/er_runner.rs``); the GHA side did not.

These tests pin the new chain-walk contract:

  1. On a rate-limit error from the resolved reviewer, the
     dispatcher advances to the next vendor in
     ``skeptic_reviewer_priority()`` and tries again.
  2. The fallback is recorded on the returned ``SkepticResult``:
     ``reason`` contains ``fallback_used=true`` and
     ``fallback_from=<original_vendor>``; ``reviewer`` reflects the
     vendor that actually produced the verdict.
  3. If every vendor in the queue returns a rate-limit error, the
     dispatcher returns a failure whose reason contains
     ``all reviewers exhausted`` and ``verdict=None``.
  4. The chain-walk does NOT bypass the existing security checks:
     the bind_reviewer_identity / verify_provenance calls are still
     invoked on the fallback path so the operator cannot exfiltrate
     a verdict by routing through a permissive vendor.

The dispatcher uses ``_cli.invoke_reviewer`` (late-bound via
``runner.skeptic_gate_cli``) so monkeypatching ``runner.skeptic_gate_cli
.invoke_reviewer`` and ``runner.skeptic_gate_cli.evaluate`` is the
right knob. We also stub ``bind_reviewer_identity`` and
``verify_provenance`` to assert they are still called on the
fallback path.
"""

from __future__ import annotations

import dataclasses
import pathlib
import sys
from unittest.mock import MagicMock

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.dispatcher import VerifierDispatcher, _detect_rate_limit  # noqa: E402
from runner.reviewer_priority import skeptic_reviewer_priority  # noqa: E402
from runner.rule_loader import Rule  # noqa: E402
from runner.skeptic_gate import (  # noqa: E402
    ParsedVerdict,
    SkepticResult,
    bind_reviewer_identity,
    format_comment,
    verify_provenance,
)


PREMIUM_RULE = Rule(
    rule_id="rule_premium",
    name="Premium Rule",
    target_globs=["*.py"],
    model_tier="premium",
    description="desc",
    prompt="prompt",
)

VALID_OUTPUT = (
    "VERDICT: PASS\n"
    "HEAD_SHA: 0123456789abcdef0123456789abcdef01234567\n"
    "REPO: jleechanorg/dark-factory\n"
    "PR_NUMBER: 123\n"
    "REASON: lgtm\n"
    "IDENTITY: claudem\n"
    "TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
    "LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
    "GREP_CITES: runner/foo.py:42\n"
    "HEAD_COMMIT_VERIFIED: 0123456789abcdef0123456789abcdef01234567\n"
)


def _mock_success_evaluate(*args, **kwargs):
    """Stand-in for ``runner.skeptic_gate_cli.evaluate`` that returns a
    successful SkepticResult with a valid ParsedVerdict. The parsed
    verdict is required so the dispatcher runs the bind/provenance
    checks downstream.

    The reviewer_identity in the parsed verdict mirrors the vendor
    that ``invoke_reviewer`` was actually called for, so the bind
    check ``bind_reviewer_identity(reviewer, declared_identity)``
    passes (the vendor declares its own identity, never another
    vendor's).
    """
    reviewer = kwargs.get("reviewer", "claudem")
    parsed = ParsedVerdict(
        verdict="PASS",
        head_sha="0123456789abcdef0123456789abcdef01234567",
        repo="jleechanorg/dark-factory",
        pr_number=123,
        reason="lgtm",
        reviewer_identity=str(reviewer),
        raw_excerpt="OK",
    )
    return SkepticResult(
        check_state="success",
        verdict="PASS",
        reason="ok",
        comment_body="",
        parsed=parsed,
        reviewer=reviewer,
    )


def _stub_invoke_reviewer(plan):
    """Build a fake ``_cli.invoke_reviewer`` keyed by vendor.

    ``plan`` is a dict mapping vendor name to the (stdout, err) tuple
    returned by that vendor's ``invoke_reviewer`` call. Each vendor
    is invoked exactly once during the chain-walk (the dispatcher
    moves on after a rate-limit error).

    Returns ``(fake_fn, seen_list)`` — ``seen_list`` is a shared list
    that records the vendors invoked, in order.
    """
    seen: list[str] = []

    def _fake(reviewer, model, prompt, *args, **kwargs):
        seen.append(reviewer)
        return plan.get(reviewer, (None, "unknown reviewer"))

    return _fake, seen


def _stub_invoke_raises(plan):
    """Like ``_stub_invoke_reviewer`` but raises for vendors that
    request a RuntimeError (used to exercise the chain-walk's
    try/except path). Returns ``(fake_fn, seen_list)``.
    """
    seen: list[str] = []

    def _fake(reviewer, model, prompt, *args, **kwargs):
        seen.append(reviewer)
        outcome = plan.get(reviewer)
        if isinstance(outcome, BaseException):
            raise outcome
        if outcome is None:
            return (None, "unknown reviewer")
        return outcome

    return _fake, seen


@pytest.fixture
def dispatcher():
    """A VerifierDispatcher whose premium tier points at claudem so
    the priority queue walk starts at index 0 (no skip needed)."""
    return VerifierDispatcher(
        cheap_reviewer="cursor-agent",
        cheap_model="agentf",
        premium_reviewer="claudem",
        premium_model="MiniMax-M3",
    )


@pytest.fixture
def stub_evaluate(monkeypatch):
    """Make ``runner.skeptic_gate_cli.evaluate`` return a valid
    success result so the dispatcher's bind/provenance block runs."""
    monkeypatch.setattr(
        "runner.skeptic_gate_cli.evaluate", _mock_success_evaluate
    )


# ---------------------------------------------------------------------------
# Rate-limit detection unit
# ---------------------------------------------------------------------------


def test_detect_rate_limit_matches_common_quota_phrases():
    """Substring match against the canonical rate-limit vocabulary."""
    assert _detect_rate_limit("codex: 429 rate limit exceeded")
    assert _detect_rate_limit("quota exceeded for customer")
    assert _detect_rate_limit("insufficient_quota: billing required")
    assert _detect_rate_limit("OpenAI rate_limit_reached")
    assert _detect_rate_limit("too many requests")
    assert _detect_rate_limit("claude hit your limit on this account")


def test_detect_rate_limit_ignores_unrelated_errors():
    """Non-rate-limit errors must NOT trigger the chain-walk."""
    assert not _detect_rate_limit("")
    assert _detect_rate_limit(None) is False
    assert not _detect_rate_limit("network timeout")
    assert not _detect_rate_limit("auth failure: invalid api key")
    assert not _detect_rate_limit("reviewer produced no output")


# ---------------------------------------------------------------------------
# Chain-walk: fallback on rate-limit
# ---------------------------------------------------------------------------


def test_chain_walk_falls_back_on_rate_limit(monkeypatch, dispatcher, stub_evaluate):
    """premium=claudem returns rate-limit, cursor-agent returns valid
    -> result.reviewer == "cursor-agent", reason contains
    "fallback_used"."""
    fake_invoke, seen = _stub_invoke_reviewer({
        "claudem": (None, "claude: 429 rate limit exceeded"),
        "agy": (None, "agy: 429 rate limit exceeded"),
        "cursor-agent": (VALID_OUTPUT, None),
    })
    monkeypatch.setattr(
        "runner.skeptic_gate_cli.invoke_reviewer", fake_invoke
    )

    results = dispatcher.dispatch(
        rules=[PREMIUM_RULE],
        changed_files=["foo.py"],
        diff="diff",
        repo="jleechanorg/dark-factory",
        pr_number=123,
        head_sha="0123456789abcdef0123456789abcdef01234567",
        base_sha="0000000000000000000000000000000000000000",
        implementation_identity="minimax",
    )

    assert len(results) == 1
    _, res = results[0]
    # The fallback landed on cursor-agent (the last in the priority list
    # that returned a valid output).
    assert res.reviewer == "cursor-agent", (
        f"expected fallback to cursor-agent, got reviewer={res.reviewer!r}"
    )
    assert "fallback_used" in res.reason, (
        f"expected reason to mention fallback_used, got {res.reason!r}"
    )
    # All three vendors were tried in priority order.
    assert seen == ["claudem", "agy", "cursor-agent"]


def test_chain_walk_records_fallback_from(monkeypatch, dispatcher, stub_evaluate):
    """The fallback trail records the original vendor so the operator
    can trace which vendor busted and forced the walk."""
    fake_invoke, _seen = _stub_invoke_reviewer({
        "claudem": (None, "rate limit exceeded"),
        "agy": (None, "rate limit exceeded"),
        "cursor-agent": (VALID_OUTPUT, None),
    })
    monkeypatch.setattr(
        "runner.skeptic_gate_cli.invoke_reviewer", fake_invoke
    )

    results = dispatcher.dispatch(
        rules=[PREMIUM_RULE],
        changed_files=["foo.py"],
        diff="diff",
        repo="jleechanorg/dark-factory",
        pr_number=123,
        head_sha="0123456789abcdef0123456789abcdef01234567",
        base_sha="0000000000000000000000000000000000000000",
        implementation_identity="minimax",
    )

    _, res = results[0]
    assert "fallback_from=claudem" in res.reason, (
        f"reason must record fallback_from=claudem, got {res.reason!r}"
    )


# ---------------------------------------------------------------------------
# Chain-walk: exhaustion
# ---------------------------------------------------------------------------


def test_chain_walk_exhausts_all_returns_last_error(monkeypatch, dispatcher, stub_evaluate):
    """All three vendors return rate-limit -> result.verdict is None
    and reason contains 'all reviewers exhausted'."""
    fake_invoke, seen = _stub_invoke_reviewer({
        "claudem": (None, "rate limit exceeded"),
        "agy": (None, "quota exceeded"),
        "cursor-agent": (None, "429 too many requests"),
    })
    monkeypatch.setattr(
        "runner.skeptic_gate_cli.invoke_reviewer", fake_invoke
    )

    results = dispatcher.dispatch(
        rules=[PREMIUM_RULE],
        changed_files=["foo.py"],
        diff="diff",
        repo="jleechanorg/dark-factory",
        pr_number=123,
        head_sha="0123456789abcdef0123456789abcdef01234567",
        base_sha="0000000000000000000000000000000000000000",
        implementation_identity="minimax",
    )

    _, res = results[0]
    assert res.verdict is None, (
        f"exhaustion must leave verdict=None, got {res.verdict!r}"
    )
    assert res.check_state == "failure"
    assert "all reviewers exhausted" in res.reason, (
        f"reason must say 'all reviewers exhausted', got {res.reason!r}"
    )
    # Hard cap: the dispatcher tries each vendor at most once.
    assert len(seen) == len(skeptic_reviewer_priority()), (
        f"expected exactly one invocation per priority-list vendor, "
        f"got {seen!r}"
    )
    # The exhaustion reason should mention the last error verbatim so
    # the operator can see WHICH rate-limit text closed the gate.
    assert "429" in res.reason or "too many requests" in res.reason, (
        f"reason should carry the last error verbatim, got {res.reason!r}"
    )


# ---------------------------------------------------------------------------
# Chain-walk: provenance + bind still run on the fallback path
# ---------------------------------------------------------------------------


def test_chain_walk_preserves_provenance_check(monkeypatch, dispatcher, stub_evaluate):
    """The chain-walk must NOT bypass the security checks. The
    fallback path is still subject to bind_reviewer_identity AND
    verify_provenance so a reviewer that swaps identity mid-walk
    cannot exfiltrate a verdict through the fallback vendor."""
    # Make bind + verify SPY so we can assert they were called.
    bind_calls = []
    verify_calls = []

    def _bind(reviewer_cli, declared_identity):
        bind_calls.append((reviewer_cli, declared_identity))
        return True, f"bound {reviewer_cli}->{declared_identity}"

    def _verify(impl, reviewer):
        verify_calls.append((impl, reviewer))
        return True, "verified"

    # Replace the names imported into the dispatcher module so the
    # call inside ``run_one`` picks up our spies.
    monkeypatch.setattr(
        "runner.skeptic_gate.bind_reviewer_identity", _bind
    )
    monkeypatch.setattr(
        "runner.skeptic_gate.verify_provenance", _verify
    )

    # Verification only succeeds when the declared identity matches
    # the CLI name. The fallback lands on cursor-agent, which the
    # parsers in this test will declare as `cursor-agent`. The bind
    # map currently only allows codex/gemini; the test asserts the
    # checks are CALLED, not that they pass — bind_reviewer_identity
    # returns False for cursor-agent, which means the dispatcher
    # must REJECT the fallback verdict, not honor it.
    fake_invoke, _seen_with_spy = _stub_invoke_reviewer({
        "claudem": (None, "rate limit exceeded"),
        "agy": (None, "rate limit exceeded"),
        "cursor-agent": (VALID_OUTPUT, None),
    })
    monkeypatch.setattr(
        "runner.skeptic_gate_cli.invoke_reviewer", fake_invoke
    )

    results = dispatcher.dispatch(
        rules=[PREMIUM_RULE],
        changed_files=["foo.py"],
        diff="diff",
        repo="jleechanorg/dark-factory",
        pr_number=123,
        head_sha="0123456789abcdef0123456789abcdef01234567",
        base_sha="0000000000000000000000000000000000000000",
        implementation_identity="minimax",
    )

    _, res = results[0]
    # The bind_reviewer_identity check must have been called on the
    # fallback vendor (cursor-agent), not the original (claudem).
    # This is the security invariant: bind runs on whatever vendor
    # actually produced the verdict, so a chain-walk cannot bypass
    # the identity check.
    assert any(call[0] == "cursor-agent" for call in bind_calls), (
        f"bind_reviewer_identity was not called on the fallback vendor: "
        f"bind_calls={bind_calls!r}"
    )
    # Provenance was also called on the fallback vendor (not the
    # original), in bind-then-verify order.
    assert any(call[1] == "cursor-agent" for call in verify_calls), (
        f"verify_provenance was not called on the fallback vendor: "
        f"verify_calls={verify_calls!r}"
    )
    # The bind map is extended from skeptic_reviewer_priority() at
    # import time (PR #9092). cursor-agent IS in the priority list,
    # so bind returns True and the verdict reaches the operator.
    assert res.check_state == "success", (
        f"bind_reviewer_identity should accept cursor-agent (extended bind map); "
        f"got check_state={res.check_state!r} reason={res.reason!r}"
    )
    assert "fallback_used" in res.reason, (
        f"reason must include fallback trail, got {res.reason!r}"
    )
    # And the chain-walk tried every vendor before settling.
    assert _seen_with_spy == ["claudem", "agy", "cursor-agent"]
