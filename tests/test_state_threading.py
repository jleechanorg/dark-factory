"""Durable regression proof that the runner threads one node's output into the
next via the ``${state._last_output}`` placeholder.

This is the empirical spine of the dynamic_fanout G1 fairness correction: it
proves that *adjacent* inter-node data flow works in a **static** ``.dot`` today
with **no engine change** — it is a prompt-authoring choice, not a paradigm gap.
``benchmarks/dynamic_fanout/modes.py::run_mode_a_schema_threaded`` cites this
test by name as the justification for giving Mode A its best honest config.

Mechanism under test (engine.py ``_run_single_node``):
    after every node the engine writes ``ctx.state["_last_output"]`` = the
    node's full output; ``_render_prompt`` then substitutes
    ``${state._last_output}`` in the next node's prompt template.

The echo backend returns the *rendered prompt* as its output, so if node 2's
rendered prompt contains node 1's marker, threading happened.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run  # noqa: E402
from runner.engine_run import _run_single_node  # noqa: E402
from runner.handlers import Context  # noqa: E402
from runner.parser import Graph, Node, parse  # noqa: E402

NODE1_MARKER = "SCHEMA-COLUMNS=id,name,created_at"


def _build_pipeline(tmp_path: pathlib.Path) -> pathlib.Path:
    """Two codergen echo nodes; node 2's prompt threads node 1's output."""
    p1 = tmp_path / "p1.md"
    p2 = tmp_path / "p2.md"
    p1.write_text(f"node1 emits: {NODE1_MARKER}\n")
    # The whole point: node 2 reads the previous node's output, not ${goal}.
    p2.write_text("migration must match upstream schema: ${state._last_output}\n")
    dot = tmp_path / "thread.dot"
    dot.write_text(
        "digraph thread {\n"
        '  graph [goal="state threading proof"]\n'
        "  start [shape=Mdiamond]\n"
        f'  schema [type="codergen", backend="echo", prompt="@{p1}"]\n'
        f'  migration [type="codergen", backend="echo", prompt="@{p2}"]\n'
        "  exit [shape=Msquare]\n"
        "  start -> schema -> migration -> exit\n"
        "}\n"
    )
    return dot


def test_last_output_threads_into_next_node(tmp_path):
    graph = parse(_build_pipeline(tmp_path))
    ctx = Context(goal="state threading proof", workdir=tmp_path, backend="echo")
    history = run(graph, ctx)

    # The migration node's recorded output is its rendered prompt; it must contain
    # node 1's marker => ${state._last_output} threading occurred. (We read from
    # history, not ctx.state["_last_output"], because the exit node overwrites
    # _last_output with its own output after migration runs.)
    migration_steps = [s for s in history if s.node == "migration"]
    assert migration_steps, "migration node never ran"
    threaded = migration_steps[-1].output_preview
    assert NODE1_MARKER in threaded, (
        "node 2 did not see node 1's output via ${state._last_output}; "
        f"got: {threaded!r}"
    )
    # And the placeholder was actually substituted, not left literal.
    assert "${state._last_output}" not in threaded

    nodes_visited = [s.node for s in history]
    assert nodes_visited == ["start", "schema", "migration", "exit"]


def test_goal_only_prompt_does_not_see_upstream_output(tmp_path):
    """Control: a node whose prompt references only ${goal} stays blind to the
    previous node's output — proving the threading in the test above is caused by
    the ${state._last_output} placeholder, not by some ambient leak."""
    p1 = tmp_path / "p1.md"
    p2 = tmp_path / "p2.md"
    p1.write_text(f"node1 emits: {NODE1_MARKER}\n")
    p2.write_text("migration for goal: ${goal}\n")  # no state reference
    dot = tmp_path / "blind.dot"
    dot.write_text(
        "digraph blind {\n"
        '  graph [goal="control"]\n'
        "  start [shape=Mdiamond]\n"
        f'  schema [type="codergen", backend="echo", prompt="@{p1}"]\n'
        f'  migration [type="codergen", backend="echo", prompt="@{p2}"]\n'
        "  exit [shape=Msquare]\n"
        "  start -> schema -> migration -> exit\n"
        "}\n"
    )
    graph = parse(dot)
    ctx = Context(goal="control", workdir=tmp_path, backend="echo")
    history = run(graph, ctx)
    migration = [s for s in history if s.node == "migration"][-1]
    assert NODE1_MARKER not in migration.output_preview


def test_last_output_handoff_is_not_truncated(tmp_path):
    """A reviewer can return a long free-form finding; the next coder must
    receive the full text through ``${state._last_output}``, not only the
    preview stored in CXDB/event summaries.
    """
    tail_marker = "TAIL-FINDING-coder-must-see-this"
    long_review = "review finding\n" + ("x" * 4500) + tail_marker
    p1 = tmp_path / "review.md"
    p2 = tmp_path / "fix.md"
    p1.write_text(long_review, encoding="utf-8")
    p2.write_text("fix handoff:\n${state._last_output}\n", encoding="utf-8")

    review = Node(name="review", attrs={"type": "codergen", "backend": "echo", "prompt": f"@{p1}"})
    fix = Node(name="fix", attrs={"type": "codergen", "backend": "echo", "prompt": f"@{p2}"})
    graph = Graph(name="thread", goal="no truncation", nodes={"review": review, "fix": fix}, edges=[])
    ctx = Context(goal="no truncation", workdir=tmp_path, backend="echo")

    _run_single_node(review, ctx, graph, seq_base=1)
    assert tail_marker in ctx.state["_last_output"]
    assert len(ctx.state["_last_output"]) > 4000

    results, _records = _run_single_node(fix, ctx, graph, seq_base=2)
    assert tail_marker in results[-1].output
