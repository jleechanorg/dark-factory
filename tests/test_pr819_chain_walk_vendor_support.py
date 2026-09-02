"""PR #819 round-2 adversarial findings: the chain-walk priority queue in
``runner/dispatcher.py:VerifierDispatcher._chain_walk_reviewer`` reads
``skeptic_reviewer_priority()`` (``config/skeptic_reviewer_priority.json`` =
``["claudem", "agy", "cursor-agent"]``), but two pieces of the gate never
learned about those vendor names:

1. ``runner/skeptic_gate_cli._build_reviewer_cmd`` only builds argv for
   ``codex``/``gemini``. When the chain-walk advances to ``claudem`` (e.g.
   after a codex rate-limit bust), ``invoke_reviewer`` raises
   ``RuntimeError("unknown reviewer 'claudem'; expected 'codex' or
   'gemini'")``. That message does not match ``_detect_rate_limit``'s
   patterns, so the chain-walk's ``continue`` branch is never taken — the
   walk stops dead at the first configured fallback vendor instead of
   advancing past it, defeating the whole point of the priority queue.

2. ``runner.skeptic_gate.parse_verdict``'s ``_IDENTITY_RE`` and the
   ``identity_token not in (...)`` check hardcode
   ``claude|codex|gemini|human|unknown`` — so even a reviewer that
   correctly declares its own vendor name (``IDENTITY: claudem``, as
   ``tests/test_dispatcher_reviewer_chain_walk.py``'s own ``VALID_OUTPUT``
   fixture does) is rejected by the deterministic parser BEFORE
   ``bind_reviewer_identity`` (which already supports claudem/agy/
   cursor-agent via ``_extend_bind_table_from_priority()``) ever runs.

Both defects mean the chain-walk fallback added in PR #654 has never
actually worked end-to-end for the real configured vendors.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.reviewer_priority import skeptic_reviewer_priority  # noqa: E402
from runner.skeptic_gate import (  # noqa: E402
    REVIEWER_CLI_TO_IDENTITY,
    bind_reviewer_identity,
    parse_verdict,
)
from runner.skeptic_gate_cli import _build_reviewer_cmd  # noqa: E402


def _verdict_text(identity: str) -> str:
    return (
        "VERDICT: PASS\n"
        "HEAD_SHA: 0123456789abcdef0123456789abcdef01234567\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 819\n"
        "REASON: lgtm\n"
        f"IDENTITY: {identity}\n"
        "TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        "LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        "GREP_CITES: runner/foo.py:42\n"
        "HEAD_COMMIT_VERIFIED: 0123456789abcdef0123456789abcdef01234567\n"
    )


@pytest.mark.parametrize("vendor", list(skeptic_reviewer_priority()))
def test_build_reviewer_cmd_supports_every_configured_priority_vendor(vendor):
    """Defect 1: every vendor in the live priority config must build a
    real argv, not raise ``RuntimeError("unknown reviewer ...")``.

    This is the exact failure that stops the chain-walk cold: the
    RuntimeError text does not match ``_detect_rate_limit``'s patterns,
    so ``_chain_walk_reviewer`` treats it as a terminal (non-rate-limit)
    outcome instead of advancing to the next vendor in the queue.
    """
    cmd = _build_reviewer_cmd(vendor, "")
    assert isinstance(cmd, list) and cmd, (
        f"_build_reviewer_cmd({vendor!r}) must return a non-empty argv"
    )


def test_build_reviewer_cmd_still_rejects_a_truly_unknown_vendor():
    """A vendor NOT in the priority config (and not codex/gemini) should
    still fail loudly — this is not a request to accept arbitrary strings."""
    with pytest.raises(RuntimeError):
        _build_reviewer_cmd("totally-not-a-reviewer", "")


@pytest.mark.parametrize("vendor", list(skeptic_reviewer_priority()))
def test_parse_verdict_accepts_declared_identity_for_every_priority_vendor(vendor):
    """Defect 2: parse_verdict must not reject a verdict whose IDENTITY
    line is the reviewer's own configured vendor name.

    ``tests/test_dispatcher_reviewer_chain_walk.py``'s ``VALID_OUTPUT``
    fixture already declares ``IDENTITY: claudem`` — that fixture is only
    ever fed through a *mocked* ``evaluate()``, so this was never
    exercised against the real deterministic parser until now.
    """
    parsed = parse_verdict(_verdict_text(vendor))
    assert parsed is not None, (
        f"parse_verdict rejected a verdict declaring IDENTITY: {vendor} "
        "even though that is a live entry in skeptic_reviewer_priority()"
    )
    assert parsed.reviewer_identity == vendor.lower()


@pytest.mark.parametrize("vendor", list(skeptic_reviewer_priority()))
def test_bind_reviewer_identity_end_to_end_for_every_priority_vendor(vendor):
    """Full loop: parse_verdict's accepted identity must also satisfy
    bind_reviewer_identity (already extended via
    _extend_bind_table_from_priority — this asserts the two layers agree)."""
    parsed = parse_verdict(_verdict_text(vendor))
    assert parsed is not None
    ok, why = bind_reviewer_identity(vendor, parsed.reviewer_identity)
    assert ok, f"bind_reviewer_identity({vendor!r}, {parsed.reviewer_identity!r}) failed: {why}"
    assert REVIEWER_CLI_TO_IDENTITY.get(vendor) == vendor


# ===========================================================================
# Round-3 findings (advice quorum, 0/2 REQUEST_CHANGES): the prompt sent
# to the reviewer never told a claudem/agy/cursor-agent CLI what to
# declare — it hardcoded `IDENTITY: <gemini|codex|claude>`. A vendor
# following that literally would emit `IDENTITY: claude`, which then
# fails ``bind_reviewer_identity`` because the bind table expects the
# vendor's own configured name (e.g. `claudem`). The two prior tests
# above only fed hand-written, already-compliant IDENTITY lines into
# ``parse_verdict``/``bind_reviewer_identity`` directly — they never
# exercised the actual rendered prompt, so they could not catch this.
# ===========================================================================


def test_build_rule_prompt_never_hardcodes_a_fixed_identity_enum():
    """The rendered rule prompt must not tell every vendor to pick from
    a fixed {codex, gemini, claude} enum — that's exactly what made a
    claudem/agy/cursor-agent reviewer declare the wrong identity."""
    from runner.dispatcher import VerifierDispatcher
    from runner.rule_loader import Rule

    dispatcher = VerifierDispatcher()
    rule = Rule(
        rule_id="r1", name="R1", target_globs=["*"], model_tier="cheap",
        description="d", prompt="p",
    )
    prompt = dispatcher.build_rule_prompt(
        rule, "jleechanorg/dark-factory", 819,
        "0123456789abcdef0123456789abcdef01234567",
        "0000000000000000000000000000000000000000",
        "+x", "unknown",
    )
    assert "IDENTITY: <gemini|codex|claude>" not in prompt
    assert "IDENTITY: <codex|gemini|claude|unknown>" not in prompt


@pytest.mark.parametrize("vendor", list(skeptic_reviewer_priority()))
def test_retargeted_prompt_declares_the_correct_identity_for_every_vendor(vendor):
    """Round-3 fix: ``_retarget_identity`` (invoked per chain-walk
    attempt in ``_chain_walk_reviewer``) must produce a prompt whose
    IDENTITY line matches exactly what ``bind_reviewer_identity``
    will require for that same vendor — closing the gap the hand-
    written verdict-text tests above could not catch."""
    from runner.dispatcher import VerifierDispatcher, _retarget_identity
    from runner.rule_loader import Rule
    from runner.skeptic_gate import IDENTITY_TOKEN_PLACEHOLDER, expected_identity_for_vendor

    dispatcher = VerifierDispatcher()
    rule = Rule(
        rule_id="r1", name="R1", target_globs=["*"], model_tier="cheap",
        description="d", prompt="p",
    )
    base_prompt = dispatcher.build_rule_prompt(
        rule, "jleechanorg/dark-factory", 819,
        "0123456789abcdef0123456789abcdef01234567",
        "0000000000000000000000000000000000000000",
        "+x", "unknown",
    )
    assert IDENTITY_TOKEN_PLACEHOLDER in base_prompt, (
        "build_rule_prompt must leave the identity placeholder for "
        "_retarget_identity to fill in per chain-walk attempt"
    )

    vendor_prompt = _retarget_identity(base_prompt, vendor)

    expected = expected_identity_for_vendor(vendor)
    assert f"IDENTITY: {expected}" in vendor_prompt
    assert IDENTITY_TOKEN_PLACEHOLDER not in vendor_prompt

    # The instructed identity must itself satisfy bind_reviewer_identity
    # for this vendor — proves the prompt and the deterministic gate
    # can never drift apart (single source of truth).
    ok, why = bind_reviewer_identity(vendor, expected)
    assert ok, f"prompt tells {vendor!r} to declare {expected!r}, but bind rejects it: {why}"


def test_chain_walk_sends_each_vendor_a_prompt_declaring_its_own_identity(monkeypatch):
    """End-to-end: when the chain-walk advances through multiple
    vendors on rate-limit busts, each vendor must receive a prompt
    instructing it to declare ITS OWN identity, not the first vendor's
    or a shared fixed enum."""
    from runner.dispatcher import VerifierDispatcher
    from runner.rule_loader import Rule
    from runner.skeptic_gate import expected_identity_for_vendor
    from runner import skeptic_gate_cli as cli_mod

    priority = list(skeptic_reviewer_priority())
    assert len(priority) >= 2, "test needs at least 2 configured vendors"

    seen_prompts: dict[str, str] = {}

    def _fake_invoke(reviewer, model, prompt, *args, **kwargs):
        seen_prompts[reviewer] = prompt
        if reviewer == priority[0]:
            return (None, "429 rate limit exceeded")
        return (_verdict_text(reviewer), None)

    def _fake_evaluate(*args, **kwargs):
        reviewer = kwargs.get("reviewer")
        parsed = parse_verdict(_verdict_text(reviewer))
        from runner.skeptic_gate import SkepticResult
        return SkepticResult(
            check_state="success", verdict="PASS", reason="ok",
            comment_body="", parsed=parsed, reviewer=reviewer,
        )

    monkeypatch.setattr(cli_mod, "invoke_reviewer", _fake_invoke)
    monkeypatch.setattr(cli_mod, "evaluate", _fake_evaluate)

    dispatcher = VerifierDispatcher(
        cheap_reviewer=priority[0], cheap_model="",
        premium_reviewer=priority[0], premium_model="",
    )
    rule = Rule(
        rule_id="r1", name="R1", target_globs=["*"], model_tier="cheap",
        description="d", prompt="p",
    )
    results = dispatcher.dispatch(
        [rule], ["x.py"], "+x", "jleechanorg/dark-factory", 819,
        "0123456789abcdef0123456789abcdef01234567",
        "0000000000000000000000000000000000000000",
        "codex",
    )
    assert len(results) == 1
    _, result = results[0]
    assert result.check_state == "success", result.reason

    # Both attempted vendors must have been sent a prompt declaring
    # THEIR OWN identity, not the other's.
    assert priority[0] in seen_prompts and priority[1] in seen_prompts
    for vendor in (priority[0], priority[1]):
        expected = expected_identity_for_vendor(vendor)
        assert f"IDENTITY: {expected}" in seen_prompts[vendor], (
            f"{vendor!r} was sent a prompt not instructing it to declare {expected!r}"
        )
    # The two vendors have distinct configured names in this priority
    # list, so they must have received genuinely different prompts.
    assert seen_prompts[priority[0]] != seen_prompts[priority[1]]


# ===========================================================================
# Round-3 Finding 2 (advice quorum): REVIEWER_ENV_PROVIDER_ALLOWLIST has
# zero entries for agy/cursor-agent, unlike codex/gemini/claudem. On
# inspection this is more subtle than "just add their credential env
# var": REVIEWER_SECRET_ENV_DENY unconditionally strips HOME (and
# USER/SHELL/etc.) for EVERY reviewer, checked BEFORE the provider
# allowlist — a deliberate CI security boundary, not an oversight. So
# an entry like `"agy": {"HOME"}` would be silently defeated by the
# deny list: it looks like a fix but grants nothing. Neither agy nor
# cursor-agent has a verified, narrowly-scoped (non-HOME) credential
# env var anywhere in this codebase, so fabricating one would violate
# this repo's root-cause-first discipline. This test instead locks in
# a real, generally-useful invariant that would have caught that exact
# mistake: no REVIEWER_ENV_PROVIDER_ALLOWLIST entry may name a key that
# REVIEWER_SECRET_ENV_DENY would shadow.
# ===========================================================================


def test_reviewer_env_allowlist_entries_are_never_shadowed_by_deny_list():
    """No REVIEWER_ENV_PROVIDER_ALLOWLIST value may name an env var that
    REVIEWER_SECRET_ENV_DENY strips first — such an entry is dead code
    that misleadingly looks like it grants a credential."""
    from runner.skeptic_gate_cli import (
        REVIEWER_ENV_PROVIDER_ALLOWLIST,
        REVIEWER_SECRET_ENV_DENY,
    )

    for vendor, keys in REVIEWER_ENV_PROVIDER_ALLOWLIST.items():
        shadowed = keys & REVIEWER_SECRET_ENV_DENY
        assert not shadowed, (
            f"REVIEWER_ENV_PROVIDER_ALLOWLIST[{vendor!r}] grants {shadowed}, "
            f"but REVIEWER_SECRET_ENV_DENY strips it first — dead entry"
        )


@pytest.mark.parametrize("vendor", ["agy", "cursor-agent", "cursor", "agentf"])
def test_reviewer_env_still_denies_home_for_unresolved_vendors(vendor):
    """Documents current, correct (if incomplete) behavior: agy and
    cursor-agent get only the base allowlist today — no HOME, no
    provider-specific credential — since no verified mechanism for
    either is known. This is a known gap (see module comment above),
    not a claim that these vendors can currently authenticate in this
    CI-invoked path; it exists so a future fix attempt has a failing
    test to point at rather than silently colliding with the deny list
    again."""
    from runner.skeptic_gate_cli import _reviewer_env

    parent_env = {
        "PATH": "/usr/bin", "HOME": "/Users/tester", "SECRET_TOKEN": "shh",
        "OPENAI_API_KEY": "sk-x", "GOOGLE_API_KEY": "g-x",
    }
    env = _reviewer_env(parent_env, vendor)
    assert "HOME" not in env
    assert "SECRET_TOKEN" not in env
    assert "OPENAI_API_KEY" not in env
    assert "GOOGLE_API_KEY" not in env
    assert env.get("PATH") == "/usr/bin"


def test_reviewer_env_does_not_broaden_codex_or_gemini_scope():
    """codex/gemini's env stays exactly as narrowly scoped as before —
    this fix round touches only the allowlist's dead-code shape, not
    codex/gemini's actual grants."""
    from runner.skeptic_gate_cli import _reviewer_env

    parent_env = {"PATH": "/usr/bin", "HOME": "/Users/tester", "OPENAI_API_KEY": "sk-x"}
    codex_env = _reviewer_env(parent_env, "codex")
    assert "HOME" not in codex_env
    assert codex_env.get("OPENAI_API_KEY") == "sk-x"
