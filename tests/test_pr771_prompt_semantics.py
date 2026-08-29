"""Focused contract checks for the slim cold-review authority prompt."""

from __future__ import annotations

from pathlib import Path


def test_slim_prompt_states_the_complete_semantic_comparison_duty() -> None:
    prompt = (
        Path(__file__).resolve().parents[1]
        / "prompts"
        / "catalog"
        / "controller_cold_review_v1.md"
    ).read_text(encoding="utf-8")
    normalized = " ".join(prompt.lower().split())

    required_concepts = (
        ("goal", ("goal",)),
        ("description and claims", ("description", "claim")),
        ("code and evidence", ("code", "evidence")),
        ("callers and consumers", ("callers", "consumers")),
    )
    for name, terms in required_concepts:
        assert all(term in normalized for term in terms), (
            f"prompt must require comparison of {name}"
        )

    assert "untrusted" in normalized
    assert (
        "evidence with source head equal evidence_origin.source_head_sha is "
        "lineage-bound through validated snapshot_parent_sha and snapshot_delta "
        "and should not be rejected solely for predating the evidence snapshot"
    ) in normalized
    assert "future trace" not in normalized
    assert "future receipt" not in normalized
    assert len(prompt.splitlines()) <= 40
    assert "c0" not in normalized
    assert "e14" not in normalized
    assert "diff_sha256" not in normalized
