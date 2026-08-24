"""Contract checks for the model-owned portion of the web-advice prompt."""

from __future__ import annotations

import json
import pathlib
import re


PROMPT = pathlib.Path(__file__).parents[1] / "prompts" / "web_advice.txt"
SEATS = ["chatgpt", "gemini", "grok", "perplexity"]
REQUIRED_RESULT_FIELDS = {
    "decision",
    "panel_seats_attempted",
    "panel_seats_live",
    "panel_seats_unavailable",
    "panel_seats_unavailable_reasons",
    "panel_verdict_summary",
    "panel_convergence",
}
REQUIRED_SEAT_FIELDS = {
    "verdict",
    "confidence",
    "reasoning",
    "share_url",
    "share_url_probe",
}


def _prompt() -> str:
    return PROMPT.read_text(encoding="utf-8")


def _fenced_json_contract(text: str) -> dict:
    blocks = re.findall(r"```json\s*\n(.*?)\n```", text, flags=re.IGNORECASE | re.DOTALL)
    assert len(blocks) == 1, "the prompt must contain exactly one fenced JSON contract"
    contract = json.loads(blocks[0])
    assert isinstance(contract, dict)
    return contract


def test_prompt_scopes_browser_work_and_requires_one_complete_result() -> None:
    text = _prompt()
    scope = text.split("The runner, not you", 1)[0]
    share_section = text.split("2. **LLM-driven Share mandate", 1)[1].split(
        "3. **Honest seat accounting", 1
    )[0]
    result_section = text.split("### Machine-readable return contract", 1)[1]

    # Scope assertions are anchored to the responsibility sections, so a
    # stray mention elsewhere cannot satisfy the browser-review contract.
    assert re.search(
        r"Drive all four real, authenticated\s+browser seats\s+"
        r"\(ChatGPT, Gemini, Grok, and Perplexity\)",
        scope,
        flags=re.IGNORECASE,
    )
    assert "attach the diff or PR context in each seat" in scope
    assert all(re.search(rf"\b{seat}\b", scope, flags=re.IGNORECASE) for seat in SEATS)

    assert re.search(r"in each live vendor UI.*Share button", share_section, re.DOTALL | re.IGNORECASE)
    assert re.search(
        r"public share URL.*probe that URL.*unauthenticated browser context",
        share_section,
        flags=re.DOTALL | re.IGNORECASE,
    )
    assert "share_url_probe" in share_section

    assert "emit exactly one final JSON object" in result_section
    result_compact = re.sub(r"\s+", " ", result_section.lower())
    assert "do not emit a second result envelope" in result_compact

    contract = _fenced_json_contract(text)
    assert set(contract) == REQUIRED_RESULT_FIELDS
    assert contract["decision"] == "continue | continue_with_pr_warning | continue_with_bead"
    assert contract["panel_seats_attempted"] == SEATS
    assert contract["panel_seats_live"] == []
    assert contract["panel_seats_unavailable"] == []
    assert contract["panel_seats_unavailable_reasons"] == {}
    assert contract["panel_convergence"] == {}
    assert set(contract["panel_verdict_summary"]) == {"seat"}
    assert set(contract["panel_verdict_summary"]["seat"]) == REQUIRED_SEAT_FIELDS


def test_prompt_leaves_commands_and_persistence_to_runner() -> None:
    text = _prompt()
    lowered = text.lower()

    # Concrete command/API and persistence instructions would let the browser
    # model duplicate work owned by the runner.  The decision value
    # ``continue_with_bead`` is intentionally still part of the schema.
    forbidden = (
        r"\bgh\s+(?:pr\s+comment|api)\b",
        r"\b(?:bd|br)\s+(?:create|comment|close|show|edit|update|add|delete)\b",
        r"\b(?:cxdb|append_step)\b",
        r"\b(?:mkdir|touch|tee)\b",
        r"\b(?:write|save|persist|store)\s+(?:the\s+|a\s+|to\s+)?(?:file|filesystem|database|evidence|cxdb)\b",
        r"\bpost\s+(?:a|the|any)\b",
        r"\bfile\s+(?:a|the)\s+bead\b",
        r"web-advice-(?:share-urls|cxdb-event)\.json",
    )
    for pattern in forbidden:
        assert re.search(pattern, lowered) is None, pattern

    ownership = lowered.split("### machine-readable return contract", 1)[0]
    assert re.search(r"runner.{0,100}side effect", ownership, re.DOTALL)
    assert "exactly once" in ownership
    assert "return data only" in ownership
