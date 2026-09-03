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
def test_per_vendor_prompt_declares_the_correct_identity_for_every_vendor(vendor):
    """Round-4 fix: ``build_rule_prompt(..., reviewer_identity=...)``
    must produce a prompt whose IDENTITY line matches exactly what
    ``bind_reviewer_identity`` will require for that same vendor —
    closing the gap the hand-written verdict-text tests above could
    not catch. Round-4 replaced round-3's blind ``_retarget_identity``
    string-replace (removed — see the diff-preservation test below for
    why) with per-vendor prompt construction."""
    from runner.dispatcher import VerifierDispatcher
    from runner.rule_loader import Rule
    from runner.skeptic_gate import IDENTITY_TOKEN_PLACEHOLDER, expected_identity_for_vendor

    dispatcher = VerifierDispatcher()
    rule = Rule(
        rule_id="r1", name="R1", target_globs=["*"], model_tier="cheap",
        description="d", prompt="p",
    )
    # Without reviewer_identity, the placeholder is left in place
    # (legacy/back-compat callers that don't know the target vendor).
    base_prompt = dispatcher.build_rule_prompt(
        rule, "jleechanorg/dark-factory", 819,
        "0123456789abcdef0123456789abcdef01234567",
        "0000000000000000000000000000000000000000",
        "+x", "unknown",
    )
    assert IDENTITY_TOKEN_PLACEHOLDER in base_prompt

    expected = expected_identity_for_vendor(vendor)
    vendor_prompt = dispatcher.build_rule_prompt(
        rule, "jleechanorg/dark-factory", 819,
        "0123456789abcdef0123456789abcdef01234567",
        "0000000000000000000000000000000000000000",
        "+x", "unknown",
        reviewer_identity=expected,
    )

    assert f"IDENTITY: {expected}" in vendor_prompt
    assert IDENTITY_TOKEN_PLACEHOLDER not in vendor_prompt

    # The instructed identity must itself satisfy bind_reviewer_identity
    # for this vendor — proves the prompt and the deterministic gate
    # can never drift apart (single source of truth).
    ok, why = bind_reviewer_identity(vendor, expected)
    assert ok, f"prompt tells {vendor!r} to declare {expected!r}, but bind rejects it: {why}"


def test_per_vendor_prompt_construction_never_corrupts_a_diff_containing_the_placeholder(
):
    """Round-4 regression test for the round-3 defect both Codex and
    Opus independently found: ``_retarget_identity`` did a blind
    ``str.replace(IDENTITY_TOKEN_PLACEHOLDER, ...)`` over the ENTIRE
    assembled prompt, including the embedded PR diff. THIS PR's own
    diff contains the literal placeholder text
    (``IDENTITY_TOKEN_PLACEHOLDER = "<<REVIEWER_IDENTITY_TOKEN>>"``),
    so reviewing this PR would have silently corrupted the reviewer's
    view of the diff it was reviewing. Round-4 removed the post-hoc
    replace entirely in favor of ``.format()``-time substitution, which
    only fills the template's own named slots and never re-scans
    already-substituted values (like ``diff``) for further matches."""
    from runner.dispatcher import VerifierDispatcher
    from runner.rule_loader import Rule
    from runner.skeptic_gate import IDENTITY_TOKEN_PLACEHOLDER

    dangerous_diff = (
        "+IDENTITY_TOKEN_PLACEHOLDER = \"<<REVIEWER_IDENTITY_TOKEN>>\"\n"
        "+def expected_identity_for_vendor(vendor):\n"
        "+    return REVIEWER_CLI_TO_IDENTITY.get(vendor)\n"
    )
    dispatcher = VerifierDispatcher()
    rule = Rule(
        rule_id="r1", name="R1", target_globs=["*"], model_tier="cheap",
        description="d", prompt="p",
    )
    vendor_prompt = dispatcher.build_rule_prompt(
        rule, "jleechanorg/dark-factory", 819,
        "0123456789abcdef0123456789abcdef01234567",
        "0000000000000000000000000000000000000000",
        dangerous_diff, "unknown",
        reviewer_identity="claudem",
    )
    # The diff content must survive byte-identical: the placeholder
    # occurrence INSIDE the diff must not have been rewritten.
    assert dangerous_diff in vendor_prompt, (
        "the embedded diff was mutated — a per-vendor prompt build must "
        "never alter diff content, only its own IDENTITY template slot"
    )
    # And the actual IDENTITY line (outside the diff) must still be
    # correctly substituted with the real per-vendor value.
    assert "IDENTITY: claudem" in vendor_prompt
    # No bare, un-substituted placeholder should remain anywhere
    # OUTSIDE the diff (the diff's own occurrence is expected content).
    outside_diff = vendor_prompt.replace(dangerous_diff, "", 1)
    assert IDENTITY_TOKEN_PLACEHOLDER not in outside_diff


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


# ===========================================================================
# Round-4 Finding B (advice quorum, both Codex and Opus independently):
# COMMIT_PREFIX_TO_IDENTITY collapsed `claudem/` into implementation
# identity "claude", while round-3 made `claudem` independently reachable
# as a REVIEWER identity too (expected_identity_for_vendor("claudem") ==
# "claudem", via REVIEWER_CLI_TO_IDENTITY's setdefault extension). That
# meant a claudem-authored commit reviewed by a claudem reviewer compared
# implementation_identity="claude" against reviewer_identity="claudem" —
# a mismatch — so verify_provenance's self-review check silently accepted
# what is actually a self-review. Round-3's own expansion is what made
# this specific bypass newly reachable (claudem couldn't appear as a
# declared reviewer identity before it).
# ===========================================================================


def test_claudem_authored_commit_reviewed_by_claudem_is_rejected_as_self_review():
    """The exact bypass both reviewers found: a claudem-authored commit
    (subject starting with `claudem/`) reviewed by a claudem reviewer
    (declared IDENTITY: claudem) must be refused as self-review, not
    silently accepted because the implementer side used to collapse to
    a different string than the reviewer side."""
    from runner.skeptic_gate import (
        extract_implementation_identity_from_commit,
        expected_identity_for_vendor,
        verify_provenance,
    )

    impl_identity = extract_implementation_identity_from_commit(
        "claudem/minimax-M3: feat(x): add thing"
    )
    reviewer_identity = expected_identity_for_vendor("claudem")

    assert impl_identity == reviewer_identity == "claudem", (
        "implementation and reviewer identity namespaces must agree for "
        "claudem, or verify_provenance cannot detect self-review at all"
    )

    ok, reason = verify_provenance(impl_identity, reviewer_identity)
    assert ok is False, (
        f"a claudem-authored commit reviewed by a claudem reviewer must be "
        f"rejected as self-review, but verify_provenance returned ok={ok!r} "
        f"({reason!r})"
    )
    assert "self-review" in reason.lower()


# ===========================================================================
# Codex /advice finding, PR #819 round-7: `cursor` and `agentf` are aliases
# that dispatch the identical `cursor-agent` executable (see
# skeptic_gate_cli.py's `reviewer in ("cursor-agent", "cursor", "agentf")`
# branch), but REVIEWER_CLI_TO_IDENTITY never canonicalized them — they
# fell back to their own distinct, unmapped names via
# expected_identity_for_vendor(). A cursor-agent-authored commit reviewed
# via the "cursor"/"agentf" alias evaded both the round-6 self-review
# pre-filter and the post-hoc verify_provenance check, since the two
# identity tokens never matched. Reproduced by Codex during round-7
# /advice.
# ===========================================================================


def test_cursor_and_agentf_vendor_identity_canonicalize_to_cursor_agent():
    """`cursor` and `agentf` are aliases for the identical `cursor-agent`
    backend; expected_identity_for_vendor must map all three to the same
    reviewer-identity token, or verify_provenance cannot detect self-review
    for whichever alias ever gets dispatched directly."""
    from runner.skeptic_gate import expected_identity_for_vendor

    assert expected_identity_for_vendor("cursor-agent") == "cursor-agent"
    assert expected_identity_for_vendor("cursor") == "cursor-agent"
    assert expected_identity_for_vendor("agentf") == "cursor-agent"


def test_cursor_agent_authored_commit_reviewed_by_cursor_alias_is_rejected_as_self_review():
    """The exact latent bypass Codex reproduced: a cursor-agent-authored
    commit reviewed by a reviewer declaring IDENTITY: cursor or
    IDENTITY: agentf (same backend, different alias) must be refused as
    self-review, not silently accepted as independent."""
    from runner.skeptic_gate import expected_identity_for_vendor, verify_provenance

    impl_identity = "cursor-agent"

    for alias in ("cursor", "agentf", "cursor-agent"):
        reviewer_identity = expected_identity_for_vendor(alias)
        assert reviewer_identity == "cursor-agent", (
            f"expected_identity_for_vendor({alias!r}) must canonicalize to "
            f"'cursor-agent', got {reviewer_identity!r}"
        )
        ok, reason = verify_provenance(impl_identity, reviewer_identity)
        assert ok is False, (
            f"a cursor-agent-authored commit reviewed via the {alias!r} "
            f"alias (same backend) must be rejected as self-review, but "
            f"verify_provenance returned ok={ok!r} ({reason!r})"
        )
        assert "self-review" in reason.lower()


def test_claude_authored_commit_is_still_distinct_from_claudem_reviewer():
    """Sanity check the fix didn't over-collapse: a genuinely `claude/`
    (real Anthropic Claude CLI) authored commit reviewed by a `claudem`
    (Claude CLI routed through a different backend) reviewer are still
    treated as distinct identities — they are, in fact, different model
    backends, so this is correct independence, not a new bypass."""
    from runner.skeptic_gate import (
        extract_implementation_identity_from_commit,
        expected_identity_for_vendor,
        verify_provenance,
    )

    impl_identity = extract_implementation_identity_from_commit(
        "claude/claude-sonnet-5: feat(x): add thing"
    )
    reviewer_identity = expected_identity_for_vendor("claudem")

    assert impl_identity == "claude"
    assert reviewer_identity == "claudem"
    ok, _reason = verify_provenance(impl_identity, reviewer_identity)
    assert ok is True


# ===========================================================================
# /wa finding (ChatGPT + Perplexity, both independently converged, PR819
# rebased head 41816b19): `invoke_reviewer` treats "claudem" and "minimax"
# as the identical backend (same Claude-CLI-via-MiniMax-API code path —
# see skeptic_gate_cli.py's `reviewer in ("claudem", "minimax")` branches),
# but REVIEWER_CLI_TO_IDENTITY never canonicalized "minimax" to "claudem",
# so expected_identity_for_vendor("minimax") fell back to its own unmapped
# name "minimax" instead. If "minimax" is ever dispatched directly as a
# reviewer vendor (the dispatch code already accepts it as a valid value),
# verify_provenance("claudem", "minimax") would treat a claudem-authored
# commit and a minimax-identity reviewer as independent when they're the
# same backend — a latent self-review bypass, not currently reachable
# under the shipped default skeptic_reviewer_priority() (only "claudem"
# is listed), but a real gap in the identity table.
# ===========================================================================


def test_minimax_vendor_identity_canonicalizes_to_claudem():
    """`minimax` and `claudem` are the identical backend (same Claude CLI
    routed through MiniMax's API); expected_identity_for_vendor must map
    both to the same reviewer-identity token, or verify_provenance cannot
    detect self-review for whichever alias ever gets dispatched directly."""
    from runner.skeptic_gate import expected_identity_for_vendor

    assert expected_identity_for_vendor("minimax") == "claudem"
    assert expected_identity_for_vendor("minimax") == expected_identity_for_vendor("claudem")


def test_claudem_authored_commit_reviewed_by_minimax_is_rejected_as_self_review():
    """The exact latent bypass /wa found: a claudem-authored commit
    reviewed by a reviewer declaring IDENTITY: minimax (same backend,
    different alias) must be refused as self-review."""
    from runner.skeptic_gate import (
        extract_implementation_identity_from_commit,
        expected_identity_for_vendor,
        verify_provenance,
    )

    impl_identity = extract_implementation_identity_from_commit(
        "claudem/minimax-M3: feat(x): add thing"
    )
    reviewer_identity = expected_identity_for_vendor("minimax")

    assert impl_identity == reviewer_identity == "claudem", (
        "implementation and reviewer identity namespaces must agree for "
        "the claudem/minimax alias pair, or verify_provenance cannot "
        "detect self-review at all"
    )

    ok, reason = verify_provenance(impl_identity, reviewer_identity)
    assert ok is False, (
        f"a claudem-authored commit reviewed by a minimax-identity reviewer "
        f"(same backend) must be rejected as self-review, but "
        f"verify_provenance returned ok={ok!r} ({reason!r})"
    )
    assert "self-review" in reason.lower()


# ===========================================================================
# round-8 /advice findings (Codex + Opus, both independently converged,
# head 8fd6f92569): REVIEWER_CLI_TO_IDENTITY and COMMIT_PREFIX_TO_IDENTITY
# were hand-maintained separately and kept drifting out of sync (round-5
# fixed a REVIEWER_CLI_TO_IDENTITY gap for minimax; round-7 fixed one for
# cursor/agentf; both left COMMIT_PREFIX_TO_IDENTITY un-mirrored). Codex
# additionally found a live example in this repo's own history --
# `cursor/composer-2.5-fast: ...` -- that resolves to "unknown" today.
# Both tables are now derived from one VENDOR_REGISTRY so this class of
# gap cannot recur: adding an alias/prefix to a VendorIdentity entry
# automatically updates both consumers.
# ===========================================================================


def test_cursor_prefix_commit_resolves_to_cursor_agent_identity():
    """Codex /advice round-7/8: a real commit in this repo's history
    (`cursor/composer-2.5-fast: ...`) authored via the `cursor` alias for
    `cursor-agent` must resolve to the SAME implementation identity that
    `expected_identity_for_vendor("cursor-agent")` declares, or the
    round-6 self-review pre-filter can never fire for a cursor-agent-
    authored commit at all."""
    from runner.skeptic_gate import (
        extract_implementation_identity_from_commit,
        expected_identity_for_vendor,
    )

    impl_identity = extract_implementation_identity_from_commit(
        "cursor/composer-2.5-fast: feat(daemon): claudem→agy→cursor-agent reviewer"
    )
    assert impl_identity == "cursor-agent", (
        f"a real cursor/-prefixed commit must resolve to 'cursor-agent', "
        f"got {impl_identity!r} (was previously 'unknown', per Codex's "
        f"round-8 finding)"
    )
    assert impl_identity == expected_identity_for_vendor("cursor-agent")


@pytest.mark.parametrize("prefix", ["cursor-agent/", "agentf/"])
def test_cursor_agent_and_agentf_commit_prefixes_also_resolve(prefix):
    """Future-proofing: even though only `cursor/` has been observed in
    this repo's actual git history, the other two dispatch aliases
    (`cursor-agent`, `agentf`) get symmetric commit-prefix coverage too,
    matching their reviewer-side canonicalization."""
    from runner.skeptic_gate import extract_implementation_identity_from_commit

    impl_identity = extract_implementation_identity_from_commit(
        f"{prefix}some-model: feat(x): add thing"
    )
    assert impl_identity == "cursor-agent"


def test_minimax_prefix_commit_resolves_to_claudem_identity():
    """Symmetric with test_cursor_prefix_commit_resolves_to_cursor_agent_
    identity: `minimax/` is not yet observed in this repo's git history,
    but gets the same commit-prefix coverage as its `claudem` alias for
    forward-compatibility with the VENDOR_REGISTRY consolidation."""
    from runner.skeptic_gate import extract_implementation_identity_from_commit

    impl_identity = extract_implementation_identity_from_commit(
        "minimax/MiniMax-M3: feat(x): add thing"
    )
    assert impl_identity == "claudem"


@pytest.mark.parametrize("prefix", ["agy/", "antig/", "antigravity/"])
def test_agy_and_antigravity_prefixes_deliberately_resolve_to_unknown(prefix):
    """round-8 /advice (Codex + Opus) flagged that `config/skeptic_
    reviewer_priority.json`'s `default_coder` is `agy`, and an agy-
    authored commit's implementation identity resolves to 'unknown' --
    so round-6's self-review pre-filter can never fire for the actual
    configured default coder.

    Investigated via `git blame`: `antig/` -> "unknown" is PRE-EXISTING
    repository behavior (commit 7c67efbb, months before PR #819) --
    Antigravity/agy route through a variable underlying model per
    invocation (this repo's own history shows "agy/gemini-3.7-flash",
    "agy/gpt-5.6-sol" -- the wrapper tool name is fixed, the model
    varies), and ZFC forbids parsing the model name out of the commit
    subject as keyword/heuristic classification. `verify_provenance`
    already fails closed on an 'unknown' implementer (conservative,
    correct, and predates PR #819). This test asserts that documented,
    deliberate fail-closed behavior for ALL THREE agy/antigravity
    aliases (not a bug PR #819 introduces or needs to "fix") -- same
    category as the pre-existing gemini/agy HOME-exemption code an
    earlier /wa round of this PR already correctly ruled out of scope.
    """
    from runner.skeptic_gate import extract_implementation_identity_from_commit

    impl_identity = extract_implementation_identity_from_commit(
        f"{prefix}gemini-3.7-flash: feat(x): add thing"
    )
    assert impl_identity == "unknown", (
        f"{prefix!r}-prefixed commits deliberately resolve to 'unknown' "
        f"(cannot statically know which underlying model ran) -- this "
        f"is pre-existing, intentional, fail-closed repository behavior, "
        f"not a PR819 defect; got {impl_identity!r}"
    )


def test_vendor_registry_is_the_single_source_for_both_identity_tables():
    """Consolidation regression guard: for every non-'unknown' vendor in
    VENDOR_REGISTRY, every one of its cli_names must resolve (via
    expected_identity_for_vendor) to the SAME identity that every one of
    its commit_prefixes resolves to (via extract_implementation_identity_
    from_commit). This is the property that makes rounds 5/6/7's class of
    bug (one table updated, the other forgotten) structurally impossible
    going forward -- both tables are generated from this one list, so
    they cannot independently drift."""
    from runner.skeptic_gate import (
        VENDOR_REGISTRY,
        expected_identity_for_vendor,
        extract_implementation_identity_from_commit,
    )

    checked_any_prefix = False
    for vendor in VENDOR_REGISTRY:
        if vendor.identity == "unknown":
            continue  # `unknown` is the deliberate fail-closed catch-all, not a real vendor identity
        for cli_name in vendor.cli_names:
            resolved = expected_identity_for_vendor(cli_name)
            assert resolved == vendor.identity, (
                f"cli_name {cli_name!r} in the {vendor.identity!r} vendor "
                f"entry resolved to {resolved!r} via expected_identity_"
                f"for_vendor, expected {vendor.identity!r}"
            )
        for prefix in vendor.commit_prefixes:
            checked_any_prefix = True
            resolved = extract_implementation_identity_from_commit(
                f"{prefix}some-model: feat(x): add thing"
            )
            assert resolved == vendor.identity, (
                f"commit prefix {prefix!r} in the {vendor.identity!r} "
                f"vendor entry resolved to {resolved!r} via extract_"
                f"implementation_identity_from_commit, expected "
                f"{vendor.identity!r}"
            )
    assert checked_any_prefix, "sanity: the registry must have at least one commit-prefixed vendor"
