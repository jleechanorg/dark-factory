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


def test_base_dot_has_explore_stitch_between_join_and_out():
    """explore_stitch closes the 4-way fanout: it reads the partials and
    writes the consolidated .dark-factory/explore-findings.md that the
    lane's `plan` node hard-requires (`prompts/slim/plan.md:6`).

    Without this node, the first run of any feature lane that includes
    _base.dot aborts at `plan` with "explore artifact missing".
    """
    g = parse(BASE, require_start_exit=False)

    # The node exists, is a codergen, and points at the (repurposed) stitch prompt.
    assert "explore_stitch" in g.nodes, "explore_stitch node must exist in _base.dot"
    stitch = g.nodes["explore_stitch"]
    assert stitch.attrs.get("type") == "codergen", "explore_stitch must be type=codergen"
    assert stitch.attrs.get("prompt") == "@prompts/slim/explore.md", (
        "explore_stitch must prompt the (repurposed) explore.md stitch prompt"
    )

    # Topology: explore_join -> explore_stitch -> explore_out, in that order.
    join_to_stitch = [
        e for e in g.edges if e.src == "explore_join" and e.dst == "explore_stitch"
    ]
    stitch_to_out = [
        e for e in g.edges if e.src == "explore_stitch" and e.dst == "explore_out"
    ]
    assert len(join_to_stitch) == 1, (
        f"explore_join must feed explore_stitch exactly once, found {len(join_to_stitch)}"
    )
    assert len(stitch_to_out) == 1, (
        f"explore_stitch must feed explore_out exactly once, found {len(stitch_to_out)}"
    )

    # No other edges leave explore_join (the join must be a single successor
    # now that the stitch owns the post-join step).
    join_succs = {e.dst for e in g.edges if e.src == "explore_join"}
    assert join_succs == {"explore_stitch"}, (
        f"explore_join must have explore_stitch as its only successor, got {join_succs!r}"
    )
