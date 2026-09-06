"""Regression tests for the controller's strict transport boundary."""

from __future__ import annotations

import json

import pytest

from runner.review_controller import ReviewContractError, parse_codex_jsonl


@pytest.mark.parametrize("event", ([], None, 7, "event"))
def test_parse_codex_jsonl_rejects_non_object_events(event):
    """Every decoded JSONL event must be an object before field access."""
    with pytest.raises(ReviewContractError, match="JSONL event at line 1"):
        parse_codex_jsonl(json.dumps(event))
