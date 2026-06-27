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

import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run  # noqa: E402
from runner.engine_run import _run_single_node  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
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


def test_long_output_handoff_and_sidecars_are_full(monkeypatch, tmp_path):
    """End-to-end handoff must keep long outputs intact in prompt and transcript
    sidecars; only previews can be capped.
    """
    home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(home))

    tail_marker = "TAIL-FINDING-coder-must-see-this"
    long_review = "review finding\n" + ("x" * 4500) + tail_marker
    p1 = tmp_path / "review.md"
    p2 = tmp_path / "fix.md"
    p3 = tmp_path / "implement.md"
    p1.write_text(long_review, encoding="utf-8")
    p2.write_text("fix handoff:\n${state._last_output}\n", encoding="utf-8")
    p3.write_text("implementer must receive:\n${state._last_output}\n", encoding="utf-8")
    dot = tmp_path / "thread_long.dot"
    dot.write_text(
        "digraph thread_long {\n"
        '  graph [goal="long handoff"]\n'
        "  start [shape=Mdiamond]\n"
        f'  review [type="codergen", backend="echo", prompt="@{p1}"]\n'
        f'  fix [type="codergen", backend="echo", prompt="@{p2}"]\n'
        f'  implement [type="codergen", backend="echo", prompt="@{p3}"]\n'
        "  exit [shape=Msquare]\n"
        "  start -> review -> fix -> implement -> exit\n"
        "}\n"
    )

    graph = parse(dot)
    ctx = Context(goal="long handoff", workdir=tmp_path, backend="echo")
    history = run(graph, ctx)
    assert history[-1].node == "exit"

    assert ctx.event_log_path is not None
    events = [
        json.loads(line)
        for line in ctx.event_log_path.read_text().splitlines()
        if line.strip()
    ]

    fix_input_event = next(
        e for e in events
        if e["event"] == "node_input" and e.get("node") == "fix" and e.get("attempt") == "1"
    )
    fix_trans_event = next(
        e for e in events
        if e["event"] == "node_result" and e.get("node") == "fix" and e.get("attempt") == "1"
    )
    implement_input_event = next(
        e for e in events
        if e["event"] == "node_input" and e.get("node") == "implement" and e.get("attempt") == "1"
    )
    implement_trans_event = next(
        e for e in events
        if e["event"] == "node_result" and e.get("node") == "implement" and e.get("attempt") == "1"
    )

    for path in [fix_input_event["input_path"], implement_input_event["input_path"]]:
        content = pathlib.Path(path).read_text(encoding="utf-8")
        assert tail_marker in content
        assert len(content) > 4000

    for path in [fix_trans_event["transcript_path"], implement_trans_event["transcript_path"]]:
        content = pathlib.Path(path).read_text(encoding="utf-8")
        assert tail_marker in content
        assert len(content) > 4000

    implement = next(step for step in history if step.node == "implement")
    assert tail_marker not in implement.output_preview
    assert len(implement.output_preview) <= 280


def test_node_result_emits_generic_handoff_refs(monkeypatch, tmp_path):
    """Any metadata key ending in `_path`/`_sha256` is carried as a node_result handoff ref."""
    home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(home))

    p_prompt = tmp_path / "shadow_prompt.txt"
    p_output = tmp_path / "shadow_output.txt"
    p_prompt.write_text("shadow prompt")
    p_output.write_text("shadow output")

    def fake_codergen(node, ctx):
        return Result(
            outcome="success",
            output="handoff test",
            metadata={
                "shadow_codex_prompt_path": str(p_prompt),
                "shadow_codex_prompt_sha256": "prompt-sha",
                "shadow_codex_output_path": str(p_output),
                "shadow_codex_output_sha256": "output-sha",
                "command_path": str(tmp_path / "command.log"),
                "command_sha256": "command-sha",
            },
        )

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    dot = tmp_path / "refs.dot"
    dot.write_text(
        'digraph refs {\n'
        '  graph [goal="refhand"]\n'
        "  start [shape=Mdiamond]\n"
        "  cod [type=\"codergen\"]\n"
        "  exit [shape=Msquare]\n"
        "  start -> cod -> exit\n"
        "}\n"
    )

    ctx = Context(goal="refhand", workdir=tmp_path, backend="echo")
    run(parse(dot), ctx)

    events = [
        json.loads(line)
        for line in ctx.event_log_path.read_text().splitlines()
        if line.strip()
    ]
    node_result = next(
        e for e in events
        if e["event"] == "node_result" and e.get("node") == "cod" and e.get("attempt") == "1"
    )
    assert node_result["shadow_codex_prompt_path"] == str(p_prompt)
    assert node_result["shadow_codex_output_path"] == str(p_output)
    assert node_result["command_path"] == str(tmp_path / "command.log")
    assert node_result["shadow_codex_prompt_sha256"] == "prompt-sha"
    assert node_result["shadow_codex_output_sha256"] == "output-sha"
    assert node_result["command_sha256"] == "command-sha"
