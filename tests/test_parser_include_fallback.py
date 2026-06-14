"""Regression test for parser `_resolve_includes` $DARK_FACTORY_HOME fallback.

Pre-fix: `_resolve_includes` (runner/parser.py:_resolve_includes) only
tried `parent_dir` and `cwd` to resolve `@pipelines/_base.dot`. This
meant `dark-factory --pipeline slim/spec_gen.dot` (a bare filename) from
a worktree cwd raised `ValueError: include not found: 'pipelines/_base.dot'`
because the install root was neither the file's parent nor the cwd.

Fix: added a third fallback to `$DARK_FACTORY_HOME` (matching the
`bin/dark-factory` wrapper's default and `paths.resolve_factory_path`'s
cwd→$DARK_FACTORY_HOME contract). Now `--pipeline` from any cwd works
as long as `DARK_FACTORY_HOME` is set (the wrapper sets it; pytest's
conftest sets it via the install root).

These tests exercise:
  1. The happy path: parse a slim/*.dot file with `@pipelines/_base.dot`
     include from a NON-install-root cwd, with `DARK_FACTORY_HOME` set.
  2. The error path: with `DARK_FACTORY_HOME` UNSET and a non-install
     cwd, the parser should still raise (preserves the existing error
     contract — operator can still see "include not found" when env is
     wrong).
"""

from __future__ import annotations

import os
import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.parser import parse  # noqa: E402

# A real slim pipeline that references `@pipelines/_base.dot` via
# `include="@pipelines/_base.dot"` at the graph level.
SLIM_SPEC_GEN = ROOT / "pipelines" / "slim" / "spec_gen.dot"

# A directory that is NOT the install root and not a parent of
# /Users/jleechan/projects/dark-factory/pipelines/slim/spec_gen.dot.
# Using /tmp is the cleanest non-install-root cwd for the regression.
NON_INSTALL_CWD = pathlib.Path("/tmp")


def test_resolve_includes_finds_base_dot_via_dark_factory_home(tmp_path, monkeypatch):
    """`parse` from a non-install-root cwd with DARK_FACTORY_HOME set
    must find `@pipelines/_base.dot` via the install-root fallback."""
    # Run the parse from a tmp cwd so parent_dir is the tmp dir, NOT
    # the install root. Without the fix, this would raise "include not
    # found: 'pipelines/_base.dot'".
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("DARK_FACTORY_HOME", str(ROOT))

    # Should NOT raise. The spec_gen.dot library must contain all
    # 4 explore sub-agents, the join/stitch nodes, and the new
    # plan_main / review_main / plan_attractor / review_attractor
    # nodes that prove `_base.dot` was actually merged in.
    g = parse(SLIM_SPEC_GEN)
    for name in (
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_fanout",
        "explore_join",
        "explore_stitch",
        "plan_main",
        "review_main",
        "plan_attractor",
        "review_attractor",
    ):
        assert name in g.nodes, (
            f"missing {name!r} from parsed graph — "
            f"`@pipelines/_base.dot` was not merged in"
        )


def test_resolve_includes_still_raises_when_dark_factory_home_unset(monkeypatch, tmp_path):
    """The pre-fix contract is preserved: if neither parent_dir, cwd,
    NOR DARK_FACTORY_HOME has the include, parser raises ValueError.
    This guards against an over-eager fix that swallows missing-include
    errors."""
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv("DARK_FACTORY_HOME", raising=False)

    with pytest.raises(ValueError) as excinfo:
        parse(SLIM_SPEC_GEN)
    # Error message must mention the include path so an operator can
    # diagnose which include failed.
    assert "pipelines/_base.dot" in str(excinfo.value), (
        f"error must name the include path; got: {excinfo.value!r}"
    )


def test_resolve_includes_prefers_parent_dir_over_dark_factory_home(monkeypatch, tmp_path):
    """If a sibling file in parent_dir shadows `$DARK_FACTORY_HOME`'s
    copy, the parent_dir version wins (preserves the pre-fix order
    when it was working). This is a tie-breaker test — if the order
    changes, this test catches it."""
    # Create a shadow _base.dot in tmp_path / pipelines/_base.dot.
    # The shadow MUST NOT declare start/exit (libraries are not
    # standalone lanes); the parser enforces that separately.
    shadow = tmp_path / "pipelines" / "_base.dot"
    shadow.parent.mkdir(parents=True, exist_ok=True)
    shadow.write_text(
        'digraph SHADOW {\n'
        '  shadow_node [label="from tmp"]\n'
        '}\n'
    )
    # A separate .dot that uses include="@pipelines/_base.dot" relative
    # to tmp_path.
    consumer = tmp_path / "consumer.dot"
    consumer.write_text(
        'digraph CONSUMER {\n'
        '  start [shape=Mdiamond]\n'
        '  exit  [shape=Msquare]\n'
        '  include="@pipelines/_base.dot"\n'
        '  my_node [label="c"]\n'
        '  start -> my_node\n'
        '  start -> shadow_node\n'
        '  my_node -> exit\n'
        '  shadow_node -> exit\n'
        '}\n'
    )
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("DARK_FACTORY_HOME", str(ROOT))  # also set, but should be ignored

    g = parse(consumer)
    # The shadow's `shadow_node` should be in the parsed graph.
    # If the fallback won, we'd get the real `_base.dot` (no `shadow_node`).
    assert "shadow_node" in g.nodes, (
        "parent_dir should win over $DARK_FACTORY_HOME — "
        "shadow file at <cwd>/pipelines/_base.dot must take precedence"
    )
    # And the real `_base.dot`'s explore nodes should NOT be present.
    assert "explore_concept" not in g.nodes, (
        "explore_concept leaked from $DARK_FACTORY_HOME — "
        "resolution order is wrong (parent_dir should beat install root)"
    )
