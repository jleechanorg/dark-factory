"""Contract checks for the model-owned portion of the web-advice prompt."""

from __future__ import annotations

import json
import pathlib
import re

import pytest

from runner.handler_web_advice import (
    PANEL_SEATS,
    _PANEL_CONTRACT_KEYS,
    _PANEL_VERDICTS,
    _normalise_panel_result,
)


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


def _assert_no_runner_side_effects(text: str) -> None:
    """Reject command-shaped runner mutations, regardless of verb choice."""
    lowered = text.lower()
    forbidden = (
        r"\b(?:gh|bd|br)\s+[a-z][a-z0-9_-]*\b",
        r"\b(?:cxdb|append_step)\b",
        r"\b(?:mkdir|touch|tee)\b",
        r"\b(?:write|save|persist|store)\s+(?:the\s+|a\s+|to\s+)?(?:file|filesystem|database|evidence|cxdb)\b",
        r"\bpost\s+(?:a|the|any)\b",
        r"\bfile\s+(?:a|the)\s+bead\b",
        r"web-advice-(?:share-urls|cxdb-event)\.json",
    )
    for pattern in forbidden:
        assert re.search(pattern, lowered) is None, pattern


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
    assert contract["decision"] == "continue"
    assert contract["panel_seats_attempted"] == SEATS
    assert contract["panel_seats_live"] == ["chatgpt"]
    assert contract["panel_seats_unavailable"] == ["gemini", "grok", "perplexity"]
    assert set(contract["panel_seats_unavailable_reasons"]) == {
        "gemini", "grok", "perplexity",
    }
    assert contract["panel_convergence"] == {}
    assert set(contract["panel_verdict_summary"]) == set(contract["panel_seats_live"])
    assert set(contract["panel_verdict_summary"]["chatgpt"]) == REQUIRED_SEAT_FIELDS


def test_prompt_verdict_enum_and_live_summary_match_runtime() -> None:
    text = _prompt()
    assert "INCOMPLETE" not in text
    assert all(f"`{verdict}`" in text for verdict in sorted(_PANEL_VERDICTS))
    assert "exactly for seats listed in" in text
    assert "Unavailable seats belong only" in text

    contract = _fenced_json_contract(text)
    normalised = _normalise_panel_result(contract)
    assert normalised["panel_contract_valid"] is True
    assert set(normalised["panel_verdict_summary"]) == set(normalised["panel_seats_live"])
    assert set(normalised["panel_seats_unavailable"]) == set(PANEL_SEATS) - set(normalised["panel_seats_live"])


def test_prompt_top_level_envelope_matches_runtime_without_top_level_verdict() -> None:
    """Keep the model's envelope instructions aligned with the strict parser.

    ``_parse_structured_panel_result`` intentionally accepts exactly the seven
    panel fields in ``_PANEL_CONTRACT_KEYS``.  A top-level ``verdict`` is not
    part of that envelope; verdict tokens belong to each live seat summary.
    """
    text = _prompt()
    lowered = re.sub(r"\s+", " ", text.lower())

    assert "top-level json envelope has exactly the seven canonical keys and no others" in lowered
    assert "do not add a top-level `verdict`" in lowered
    assert "top-level `verdict` must" not in lowered

    contract = _fenced_json_contract(text)
    assert set(contract) == _PANEL_CONTRACT_KEYS
    assert "verdict" not in contract
    assert _PANEL_CONTRACT_KEYS == REQUIRED_RESULT_FIELDS


def test_prompt_leaves_commands_and_persistence_to_runner() -> None:
    text = _prompt()
    lowered = text.lower()

    # Concrete command/API and persistence instructions would let the browser
    # model duplicate work owned by the runner.  Reject every command verb,
    # not a gameable allowlist of currently known mutations.
    _assert_no_runner_side_effects(text)

    ownership = lowered.split("### machine-readable return contract", 1)[0]
    assert re.search(r"runner.{0,100}side effect", ownership, re.DOTALL)
    assert "exactly once" in ownership
    assert "return data only" in ownership


@pytest.mark.parametrize(
    "mutation",
    [
        "Run gh issue create --title duplicate.",
        "Execute gh workflow run privileged.yml.",
        "Invoke bd reopen jleechan-123.",
        "Call br dependency add jleechan-a jleechan-b.",
        "Use append_step to mutate CXDB.",
        "Persist the evidence database before returning.",
    ],
)
def test_prompt_side_effect_guard_rejects_arbitrary_mutations(mutation: str) -> None:
    with pytest.raises(AssertionError):
        _assert_no_runner_side_effects(_prompt() + "\n" + mutation)
