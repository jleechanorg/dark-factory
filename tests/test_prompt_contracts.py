"""Per-prompt contract tests for the 5 prompts that use dynamic
substitution tokens.

These tests pin the substitution contract for the prompts whose
content directly controls a fix loop, evidence review, or
bug-fix gate. If any of these prompts loses a required ``${state.X}``
or ``${diff}`` / ``${lint_findings}`` token, the corresponding
runner handler will silently write empty strings at dispatch time,
which is the exact failure mode the prompt-substitution audit
(``runner.prompt_substitution_audit``) catches structurally.

The audit's Check A already covers these at the bulk level — the
per-prompt tests here are an *explicit* record of which tokens
each prompt needs, so a future agent can grep
``test_prompt_contracts.py`` to see "this prompt needs
``${state.last_test_output}``" without reading the audit's
allowlist or the prompt file.

Each test pins three things:

1. The prompt still contains every expected ``${...}`` token.
2. The prompt is >= ``MIN_PROMPT_CHARS`` (mirrors the audit's
   Check C threshold).
3. The prompt contains ``${goal}`` (or is in
   ``PROMPTS_WITHOUT_GOAL_OK``).

If a future refactor splits a prompt or moves its body into a
template, the test fails BEFORE the engine does, exactly like
the structural audit.

The 5 prompts covered here are the ones with dynamic substitution
tokens as of HEAD: ``prompts/codergen.md``,
``prompts/catalog/review.md``, ``prompts/slim/fix.md``,
``prompts/slim/review.md``, ``prompts/bug_fix/fix.md``. The other
30 prompts use only ``${goal}`` (the static-attribute goal
injection) and are covered by ``runner.prompt_substitution_audit``'s
Check C bulk rule.
"""

from __future__ import annotations

import pathlib
import re
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.prompt_substitution_audit import (  # noqa: E402
    MIN_PROMPT_CHARS,
    PROMPTS_WITHOUT_GOAL_OK,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _read_prompt(relpath: str) -> str:
    return (ROOT / relpath).read_text(encoding="utf-8")


def _has_all_tokens(text: str, tokens: list[str]) -> list[str]:
    """Return the list of tokens in ``tokens`` that are MISSING from ``text``."""
    return [t for t in tokens if t not in text]


# ---------------------------------------------------------------------------
# Per-prompt contracts
# ---------------------------------------------------------------------------


def test_codergen_prompt_has_state_substitutions() -> None:
    """``prompts/codergen.md`` documents the state-substitution contract
    for all codergen nodes. Both the inline ``${state._last_output}``
    example and the ``${state.<upstream_node>.output_head}`` pattern
    must remain in the doc so future authors know how to read prior
    node outputs.
    """
    text = _read_prompt("prompts/codergen.md")
    missing = _has_all_tokens(text, ["${state._last_output}", "${goal}"])
    assert not missing, (
        f"prompts/codergen.md is missing required tokens: {missing}. "
        f"This is the canonical reference for state-substitution; "
        f"removing these examples would break the doc contract."
    )


def test_catalog_review_prompt_has_diff_and_lint_findings() -> None:
    """``prompts/catalog/review.md`` is the Level-5 evidence-review
    prompt. ``${diff}`` is wired by ``runner/handler_codergen.py:165``
    on every successful codergen run; ``${lint_findings}`` is wired
    by ``runner/handler_render.py:44`` on first render. Both are
    critical to the reviewer's ability to see the work.
    """
    text = _read_prompt("prompts/catalog/review.md")
    missing = _has_all_tokens(text, ["${diff}", "${lint_findings}", "${goal}"])
    assert not missing, (
        f"prompts/catalog/review.md is missing required tokens: {missing}. "
        f"Without ${{diff}} the reviewer cannot see the work; without "
        f"${{lint_findings}} the reviewer cannot pre-check style."
    )


def test_slim_fix_prompt_has_failure_handoff_tokens() -> None:
    """``prompts/slim/fix.md`` is the canonical PR #95 regression
    case. The prompt must contain ``${state.last_test_command}``,
    ``${state.last_test_rc}``, and ``${state.last_test_output}`` —
    all three are written by ``runner/handler_control.py:130-135``
    on ``goal_gate`` tool failures. It must also contain
    ``${state._last_output}``, which is written after every node by
    ``runner/engine_run.py`` so reviewer/gate prose reaches the fix
    agent. This is the fix-loop prompt; losing any of these tokens
    makes the fix attempt blind.
    """
    text = _read_prompt("prompts/slim/fix.md")
    missing = _has_all_tokens(
        text,
        [
            "${state._last_output}",
            "${state.last_test_command}",
            "${state.last_test_rc}",
            "${state.last_test_output}",
            "${goal}",
        ],
    )
    assert not missing, (
        f"prompts/slim/fix.md is missing required tokens: {missing}. "
        f"This is the exact PR #95 regression class — the fix prompt "
        f"silently lost ${{state.last_test_output}} before the audit "
        f"existed, and every fix attempt returned success blindly. "
        f"DO NOT remove any of these tokens without also updating the "
        f"handler writer in runner/handler_control.py."
    )


def test_slim_review_prompt_has_diff_and_lint_findings() -> None:
    """``prompts/slim/review.md`` is the slim-lane evidence-review
    prompt. Same contract as ``prompts/catalog/review.md`` but for
    the slim lane — both ``${diff}`` and ``${lint_findings}`` are
    required.
    """
    text = _read_prompt("prompts/slim/review.md")
    missing = _has_all_tokens(text, ["${diff}", "${lint_findings}", "${goal}"])
    assert not missing, (
        f"prompts/slim/review.md is missing required tokens: {missing}. "
        f"Without ${{diff}} the reviewer cannot see the work; without "
        f"${{lint_findings}} the reviewer cannot pre-check style."
    )
    for required in ("## Coder Handoff", "Blocking findings", "Required fix", "Verification to rerun"):
        assert required in text, (
            f"prompts/slim/review.md is missing {required!r}. "
            f"Reviewer output must remain useful as free-form input to the next fix node."
        )


def test_bug_fix_prompt_has_test_path() -> None:
    """``prompts/bug_fix/fix.md`` consumes the user-set
    ``${state.bug_fix.test_path}`` key (allowlisted in
    ``USER_SET_KEYS``). The prompt does NOT contain ``${goal}``
    because the bug-fix lane's "goal" is implicit in the test
    failure state — the test oracle IS the goal. This test pins
    both: the test_path token must be present, AND the prompt
    must remain in the no-goal allowlist.
    """
    text = _read_prompt("prompts/bug_fix/fix.md")
    assert "${state.bug_fix.test_path}" in text, (
        "prompts/bug_fix/fix.md is missing ${state.bug_fix.test_path}. "
        "The bug-fix gate reads this to run the red/green test cycle; "
        "without it the prompt cannot tell the agent which test to fix."
    )
    assert "prompts/bug_fix/fix.md" in PROMPTS_WITHOUT_GOAL_OK, (
        "prompts/bug_fix/fix.md is no longer in PROMPTS_WITHOUT_GOAL_OK. "
        "The bug-fix lane's goal is implicit in the test failure state, "
        "not in the graph's goal attribute; the prompt must remain "
        "allowlisted from the ${goal} presence check."
    )


# ---------------------------------------------------------------------------
# Cross-prompt invariants
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "relpath,expected_tokens",
    [
        (
            "prompts/codergen.md",
            ["${state._last_output}", "${goal}"],
        ),
        (
            "prompts/catalog/review.md",
            ["${diff}", "${lint_findings}", "${goal}"],
        ),
        (
            "prompts/slim/fix.md",
            [
                "${state._last_output}",
                "${state.last_test_command}",
                "${state.last_test_rc}",
                "${state.last_test_output}",
                "${goal}",
            ],
        ),
        (
            "prompts/slim/review.md",
            ["${diff}", "${lint_findings}", "${goal}"],
        ),
        (
            "prompts/bug_fix/fix.md",
            ["${state.bug_fix.test_path}"],
        ),
    ],
)
def test_dynamic_prompts_meet_minimum_size(
    relpath: str, expected_tokens: list[str]
) -> None:
    """Every prompt that uses dynamic substitution tokens must also
    meet the audit's Check C minimum-size threshold. This guards
    against a refactor that splits a prompt into a too-short stub
    while leaving the substitution tokens intact.
    """
    text = _read_prompt(relpath)
    stripped = text.strip()
    assert len(stripped) >= MIN_PROMPT_CHARS, (
        f"{relpath} is only {len(stripped)} chars "
        f"(minimum is {MIN_PROMPT_CHARS}). Templated one-liner or "
        f"placeholder stub; the PR #95 fix.md regression had a "
        f"similarly short body. Restore the full prompt body."
    )
    # Confirm the expected tokens survived the refactor too.
    for token in expected_tokens:
        assert token in text, (
            f"{relpath} is missing required token {token!r}."
        )


def test_dynamic_prompts_contain_goal_or_allowlisted() -> None:
    """Every dynamic-substitution prompt must contain ``${goal}`` OR
    be in ``PROMPTS_WITHOUT_GOAL_OK``. This mirrors the audit's
    Check C goal-presence rule but at the per-prompt level for the
    5 dynamic prompts.
    """
    for relpath in (
        "prompts/codergen.md",
        "prompts/catalog/review.md",
        "prompts/slim/fix.md",
        "prompts/slim/review.md",
        "prompts/bug_fix/fix.md",
    ):
        text = _read_prompt(relpath)
        if relpath in PROMPTS_WITHOUT_GOAL_OK:
            continue
        assert "${goal}" in text, (
            f"{relpath} does not contain ${{goal}} and is not in "
            f"PROMPTS_WITHOUT_GOAL_OK. The engine injects the "
            f"pipeline's top-level goal into every prompt; omitting "
            f"it is a bug."
        )


def test_prompts_with_diff_inject_are_codergen_evidence_reviewers() -> None:
    """Only reviewer-class prompts use ``${diff}`` / ``${lint_findings}``.
    A future refactor that adds ``${diff}`` to a non-reviewer prompt
    is almost certainly a bug (the diff variable is wired only after
    a successful codergen run; a non-reviewer prompt would render
    empty ``${diff}``).
    """
    for relpath in ("prompts/catalog/review.md", "prompts/slim/review.md"):
        text = _read_prompt(relpath)
        assert "${diff}" in text, (
            f"{relpath} should contain ${{diff}} (it's a reviewer prompt)"
        )
    # The other 3 dynamic prompts do NOT use ${diff}.
    for relpath in (
        "prompts/codergen.md",
        "prompts/slim/fix.md",
        "prompts/bug_fix/fix.md",
    ):
        text = _read_prompt(relpath)
        assert "${diff}" not in text, (
            f"{relpath} unexpectedly contains ${{diff}}; this is a "
            f"non-reviewer prompt and the diff variable would render "
            f"empty at runtime."
        )


def test_prompts_with_state_dot_test_path_are_bug_fix_lane_only() -> None:
    """The ``${state.bug_fix.test_path}`` key is the user-set
    contract for the bug_fix lane. No other prompt should reference
    it. If a future prompt does, it almost certainly is borrowing
    from the bug_fix lane's contract — flag for review.
    """
    for relpath in (
        "prompts/codergen.md",
        "prompts/catalog/review.md",
        "prompts/slim/fix.md",
        "prompts/slim/review.md",
    ):
        text = _read_prompt(relpath)
        assert "${state.bug_fix.test_path}" not in text, (
            f"{relpath} unexpectedly references "
            f"${{state.bug_fix.test_path}}; this is the bug_fix lane's "
            f"contract and should not leak into other prompts."
        )


# ---------------------------------------------------------------------------
# Domain-agnosticism invariants (jleechan-9bi / Lane A audit-2026-06-27)
# ---------------------------------------------------------------------------


# Banned terms: any reappearance in the slim/review.md reviewer prompt is
# a regression of the domain-agnosticism audit. The list is exhaustive as of
# 2026-06-27 and mirrors the per-bead acceptance criteria.
DOMAIN_BIAS_TERMS: tuple[str, ...] = (
    # world-architect / D&D-shaped phrasing
    "level-up",
    "level_up",
    "world_logic",
    "wizard",
    "Fighter",
    "campaign class",
    # streaming-app evidence filename baked in as universal
    "streaming_evidence",
)


@pytest.mark.parametrize("banned_term", DOMAIN_BIAS_TERMS)
def test_slim_review_prompt_contains_no_domain_bias_terms(banned_term: str) -> None:
    """``prompts/slim/review.md`` is the slim-lane reviewer prompt and
    must remain domain-agnostic. Any reappearance of a D&D /
    world-architect term (``level-up``, ``wizard``, ``Fighter``,
    ``campaign class``, ...) or the streaming-app-specific evidence
    filename (``streaming_evidence``) means a refactor accidentally
    re-leaked the legacy benchmark-specific phrasing into the generic
    reviewer. This pins the 2026-06-27 audit Lane A acceptance criteria
    for bead ``jleechan-9bi``.
    """
    text = _read_prompt("prompts/slim/review.md")
    assert banned_term not in text, (
        f"prompts/slim/review.md contains banned domain-bias term "
        f"{banned_term!r}. The slim reviewer prompt must remain "
        f"domain-agnostic; replace with the generic phrasing documented "
        f"in the 2026-06-27 audit goal file "
        f"(.dark-factory/audit-2026-06-27-goal.md Lane A)."
    )


DOMAIN_BIAS_FILENAMES: tuple[str, ...] = (
    "streaming_evidence.json",
    "llm_request_responses.jsonl",
)


@pytest.mark.parametrize("banned_filename", DOMAIN_BIAS_FILENAMES)
def test_slim_review_prompt_contains_no_vendor_specific_filenames(
    banned_filename: str,
) -> None:
    """``prompts/slim/review.md`` references canonical evidence
    filenames in step 4 (Evidence quality check). Those filenames must
    be vendor-neutral placeholders (``<primary-evidence.json>``,
    ``<run-trace.jsonl>``) rather than streaming-app-specific defaults
    (``streaming_evidence.json``) or LLM-vendor-specific JSONL
    (``llm_request_responses.jsonl``).

    Pinned by bead ``jleechan-9bi`` (Lane A, audit-2026-06-27).
    """
    text = _read_prompt("prompts/slim/review.md")
    assert banned_filename not in text, (
        f"prompts/slim/review.md contains vendor/streaming-specific "
        f"filename {banned_filename!r}. Replace with a generic "
        f"placeholder (``<primary-evidence.json>`` or "
        f"``<run-trace.jsonl>``)."
    )
