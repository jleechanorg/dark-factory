"""Contract tests for the QW5 pilot coder prompt."""

from __future__ import annotations

import pathlib


ROOT = pathlib.Path(__file__).resolve().parents[1]


def test_qw5_prompt_scopes_minimax_only_to_coder_and_pilot() -> None:
    """The reviewer queue must not be confused with the coder backend."""
    text = (ROOT / "daemon" / "qw5-coder-prompt.md").read_text(encoding="utf-8")

    assert (
        "backend_priority" in text
        and "reviewer queue" in text
        and "It does not change the\nMiniMax-only coder or pilot contract" in text
    ), (
        "The prompt must explain that backend_priority is a reviewer queue, "
        "not permission to run the MiniMax-only QW5 coder or pilot elsewhere."
    )
