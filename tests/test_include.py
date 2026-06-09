"""Tests for the `include="@path"` parser-level graph inclusion."""

from __future__ import annotations

import pathlib
import sys
import textwrap

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.parser import parse

BASE = ROOT / "pipelines" / "_base.dot"
HELLO = ROOT / "pipelines" / "factory" / "hello.dot"
BUG_FIX = ROOT / "pipelines" / "bug_fix.dot"


def test_lane_include_resolves_base_nodes():
    """A lane that includes _base.dot has all 4 explore sub-agents + in/out + join."""
    g = parse(HELLO)
    for name in (
        "explore_in",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_out",
    ):
        assert name in g.nodes, f"hello.dot missing included node {name}"


def test_lane_include_keeps_lane_specific_nodes():
    """Lane-only nodes (start, exit, plan, implement, holdout, fix) survive the include."""
    g = parse(HELLO)
    for name in ("start", "exit", "plan", "implement", "holdout", "fix"):
        assert name in g.nodes, f"hello.dot dropped lane node {name}"


def test_include_rejects_start_node_in_library(tmp_path):
    """A library file declaring `start` is rejected (libraries have no entry point)."""
    bad_lib = tmp_path / "_bad_base.dot"
    bad_lib.write_text(
        textwrap.dedent(
            """\
            digraph _bad {
              start [shape=Mdiamond]
              x [type="codergen"]
              start -> x
            }
            """
        )
    )
    lane = tmp_path / "lane.dot"
    lane.write_text(
        f'digraph L {{ graph [include="@{bad_lib.name}"] '
        f'exit [shape=Msquare] start -> x -> exit }}\n'
    )
    with pytest.raises(ValueError, match="start"):
        parse(lane)


def test_include_rejects_exit_node_in_library(tmp_path):
    """A library file declaring `exit` is rejected (libraries have no terminal)."""
    bad_lib = tmp_path / "_bad_base2.dot"
    bad_lib.write_text(
        textwrap.dedent(
            """\
            digraph _bad2 {
              x [type="codergen"]
              exit [shape=Msquare]
              x -> exit
            }
            """
        )
    )
    lane = tmp_path / "lane2.dot"
    lane.write_text(
        f'digraph L {{ graph [include="@{bad_lib.name}"] '
        f'start [shape=Mdiamond] start -> x -> exit }}\n'
    )
    with pytest.raises(ValueError, match="exit"):
        parse(lane)


def test_include_rejects_node_name_collision(tmp_path):
    """A lane that re-declares an included node name raises."""
    lib = tmp_path / "_base2.dot"
    lib.write_text('digraph _b2 { shared [type="codergen"] }\n')
    lane = tmp_path / "lane3.dot"
    lane.write_text(
        f'digraph L {{ graph [include="@{lib.name}"] '
        f'start [shape=Mdiamond] exit [shape=Msquare] '
        f'shared [type="codergen"] start -> shared -> exit }}\n'
    )
    with pytest.raises(ValueError, match="collides|collision"):
        parse(lane)


def test_include_resolves_from_parent_dir_first(tmp_path):
    """Include paths starting with `@` are resolved relative to the parent .dot, then cwd."""
    # Place a custom library next to a lane
    (tmp_path / "mylib.dot").write_text('digraph mylib { custom_node [type="codergen"] }\n')
    (tmp_path / "lane.dot").write_text(
        textwrap.dedent(
            """\
            digraph L {
              graph [include="@mylib.dot"]
              start [shape=Mdiamond]
              exit  [shape=Msquare]
              start -> custom_node -> exit
            }
            """
        )
    )
    g = parse(tmp_path / "lane.dot")
    assert "custom_node" in g.nodes


def test_bug_fix_lane_includes_base_and_has_red_green(tmp_path):
    """The bug_fix.dot pipeline wires the red/green discipline on top of base."""
    g = parse(BUG_FIX)
    # include resolved
    for n in ("explore_in", "explore_join", "explore_out", "explore_concept", "explore_risks"):
        assert n in g.nodes
    # red/green discipline
    assert "gate_red" in g.nodes
    assert "gate_green" in g.nodes
    assert "reproduce" in g.nodes
    assert "fix" in g.nodes
    # AttrValue is str | int | bool; the parser stores unquoted "3" as int 3
    assert str(g.nodes["fix"].attrs.get("max_visits")) == "3"
