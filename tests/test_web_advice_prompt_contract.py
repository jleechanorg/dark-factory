"""Contract checks for the model-owned portion of the web-advice prompt."""

from __future__ import annotations

import pathlib
import re


PROMPT = pathlib.Path(__file__).parents[1] / "prompts" / "web_advice.txt"


def test_prompt_assigns_browser_review_and_structured_result() -> None:
    text = PROMPT.read_text(encoding="utf-8")
    lowered = text.lower()

    for seat in ("chatgpt", "gemini", "grok", "perplexity"):
        assert seat in lowered
    for responsibility in (
        "attach the diff",
        "share button",
        "public share url",
        "probe that url",
        "exactly one final json object",
    ):
        assert responsibility in lowered

    for field in (
        '"decision"',
        '"panel_seats_attempted"',
        '"panel_seats_live"',
        '"panel_seats_unavailable_reasons"',
        '"panel_verdict_summary"',
        '"panel_convergence"',
    ):
        assert field in text


def test_prompt_leaves_downstream_side_effects_to_runner() -> None:
    text = PROMPT.read_text(encoding="utf-8")
    lowered = text.lower()

    # These are concrete command/API or artifact instructions that would let
    # the browser model duplicate work owned by the runner.  The decision
    # value ``continue_with_bead`` is intentionally still part of the schema.
    forbidden = (
        r"\bgh\s+api\b",
        r"\bbr\s+(?:create|comment|close|show)\b",
        "append_step",
        "cxdb",
        "web-advice-share-urls.json",
        "web-advice-cxdb-event.json",
        "curl ",
        "file a bead",
        "post the",
        "persist evidence",
    )
    for pattern in forbidden:
        assert re.search(pattern, lowered) is None, pattern

    assert re.search(r"runner.{0,100}side effect", lowered, re.DOTALL)
    assert "exactly once" in lowered
    assert "return data only" in lowered
