"""Tests for pipelines/_base.dot library."""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.parser import parse

BASE = ROOT / "pipelines" / "_base.dot"


def test_base_dot_has_four_explore_subagents_and_join():
    g = parse(BASE, require_start_exit=False)
    # 4 explore sub-agents
    for name in ("explore_concept", "explore_auth", "explore_reuse", "explore_risks"):
        assert name in g.nodes, f"missing {name} from _base.dot"
        assert g.nodes[name].attrs.get("class") == "explore"
    # fan-out and join
    assert "explore_fanout" in g.nodes
    assert "explore_join" in g.nodes
    # in / out points
    assert "explore_in" in g.nodes
    assert "explore_out" in g.nodes


def test_base_dot_has_no_start_or_exit():
    g = parse(BASE, require_start_exit=False)
    assert "start" not in g.nodes
    assert "exit" not in g.nodes


def test_base_dot_explore_join_uses_wait_all():
    g = parse(BASE, require_start_exit=False)
    join = g.nodes["explore_join"]
    assert join.attrs.get("policy") == "wait_all"


def test_base_dot_fanout_connects_to_all_four_subagents():
    g = parse(BASE, require_start_exit=False)
    fanout_edges = {e.dst for e in g.edges if e.src == "explore_fanout"}
    assert fanout_edges == {
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
    }


def test_base_dot_subagents_feed_into_join():
    g = parse(BASE, require_start_exit=False)
    for sub in ("explore_concept", "explore_auth", "explore_reuse", "explore_risks"):
        assert any(e.src == sub and e.dst == "explore_join" for e in g.edges), (
            f"{sub} does not feed explore_join"
        )


def test_base_dot_subagents_prompt_under_prompts_slim():
    g = parse(BASE, require_start_exit=False)
    for sub, prompt in (
        ("explore_concept", "prompts/slim/explore_concept.md"),
        ("explore_auth", "prompts/slim/explore_authorities.md"),
        ("explore_reuse", "prompts/slim/explore_reuse.md"),
        ("explore_risks", "prompts/slim/explore_risks.md"),
    ):
        assert g.nodes[sub].attrs.get("prompt") == f"@{prompt}", (
            f"{sub} should prompt {prompt!r}"
        )
