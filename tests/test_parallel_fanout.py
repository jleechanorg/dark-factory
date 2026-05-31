"""Tests for parallel fan-out / fan-in execution (Attractor parity, bead orch-kt8m).

TDD — all tests are written BEFORE implementation so they start red.

Covers:
  - wait_all policy (both succeed / one fails)
  - first_success policy (one succeeds / all fail)
  - k_of_n policy (meets / below quorum)
  - shape aliases: shape=component (fan-out) + shape=tripleoctagon (join)
  - true concurrency: branches execute in separate threads
  - multi-hop branches: each branch has multiple nodes before join
  - CXDB: branch steps recorded with monotonic seq
"""
from __future__ import annotations

import threading
import time
from pathlib import Path

import pytest

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse

ROOT = Path(__file__).parent.parent


# ---------------------------------------------------------------------------
# DOT helpers — branch nodes have no custom type; echo backend resolves from state
# ---------------------------------------------------------------------------

def _dot_2branch(tmp_path: Path, policy: str, k: str = "") -> Path:
    k_attr = f', k="{k}"' if k else ""
    dot = tmp_path / "parallel.dot"
    dot.write_text(
        f'digraph parallel_test {{\n'
        f'  graph [goal="Parallel test"]\n'
        f'  start [shape=Mdiamond]\n'
        f'  fanout [type="parallel"]\n'
        f'  branch_a\n'
        f'  branch_b\n'
        f'  join [type="join", policy="{policy}"{k_attr}]\n'
        f'  exit [shape=Msquare]\n'
        f'  start -> fanout\n'
        f'  fanout -> branch_a\n'
        f'  fanout -> branch_b\n'
        f'  branch_a -> join\n'
        f'  branch_b -> join\n'
        f'  join -> exit\n'
        f'}}\n'
    )
    return dot


def _dot_3branch(tmp_path: Path, policy: str, k: str) -> Path:
    dot = tmp_path / "parallel3.dot"
    dot.write_text(
        f'digraph parallel3 {{\n'
        f'  graph [goal="k_of_n test"]\n'
        f'  start [shape=Mdiamond]\n'
        f'  fanout [type="parallel"]\n'
        f'  branch_a\n'
        f'  branch_b\n'
        f'  branch_c\n'
        f'  join [type="join", policy="{policy}", k="{k}"]\n'
        f'  exit [shape=Msquare]\n'
        f'  start -> fanout\n'
        f'  fanout -> branch_a\n'
        f'  fanout -> branch_b\n'
        f'  fanout -> branch_c\n'
        f'  branch_a -> join\n'
        f'  branch_b -> join\n'
        f'  branch_c -> join\n'
        f'  join -> exit\n'
        f'}}\n'
    )
    return dot


def _ctx(workdir: Path = ROOT, backend: str = "echo", cxdb_path: str | None = None) -> Context:
    return Context(goal="test parallel", workdir=workdir, backend=backend,
                   cxdb_path=cxdb_path)


def _join_step(history):
    return next(s for s in history if s.node == "join")


# ---------------------------------------------------------------------------
# wait_all policy
# ---------------------------------------------------------------------------

def test_parallel_wait_all_both_succeed(tmp_path):
    """wait_all: both branches succeed -> join outcome is success."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    graph = parse(_dot_2branch(tmp_path, "wait_all"))
    history = run(graph, ctx, max_steps=20)

    nodes = [s.node for s in history]
    assert "fanout" in nodes
    assert "branch_a" in nodes
    assert "branch_b" in nodes
    assert "join" in nodes
    assert "exit" in nodes

    assert _join_step(history).outcome == "success"


def test_parallel_wait_all_one_fails(tmp_path):
    """wait_all: one branch fails -> join outcome is failure."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "failure"

    graph = parse(_dot_2branch(tmp_path, "wait_all"))
    history = run(graph, ctx, max_steps=20)

    assert _join_step(history).outcome == "failure"


# ---------------------------------------------------------------------------
# first_success policy
# ---------------------------------------------------------------------------

def test_parallel_first_success_one_succeeds(tmp_path):
    """first_success: at least one branch succeeds -> join success."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "failure"

    graph = parse(_dot_2branch(tmp_path, "first_success"))
    history = run(graph, ctx, max_steps=20)

    assert _join_step(history).outcome == "success"


def test_parallel_first_success_all_fail(tmp_path):
    """first_success: all branches fail -> join failure."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "failure"
    ctx.state["branch_b.outcome"] = "failure"

    graph = parse(_dot_2branch(tmp_path, "first_success"))
    history = run(graph, ctx, max_steps=20)

    assert _join_step(history).outcome == "failure"


# ---------------------------------------------------------------------------
# k_of_n policy
# ---------------------------------------------------------------------------

def test_parallel_k_of_n_meets_quorum(tmp_path):
    """k_of_n k=2: 2 of 3 branches succeed -> success."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"
    ctx.state["branch_c.outcome"] = "failure"

    graph = parse(_dot_3branch(tmp_path, "k_of_n", "2"))
    history = run(graph, ctx, max_steps=20)

    js = _join_step(history)
    assert js.outcome == "success"
    assert js.metadata.get("policy") == "k_of_n"
    assert js.metadata.get("successes") == "2"


def test_parallel_k_of_n_below_quorum(tmp_path):
    """k_of_n k=2: only 1 of 3 succeeds -> failure."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "failure"
    ctx.state["branch_c.outcome"] = "failure"

    graph = parse(_dot_3branch(tmp_path, "k_of_n", "2"))
    history = run(graph, ctx, max_steps=20)

    assert _join_step(history).outcome == "failure"


def test_parallel_k_of_n_invalid_k_zero(tmp_path):
    """k_of_n k=0: out-of-range -> failure (not always-success)."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    graph = parse(_dot_2branch(tmp_path, "k_of_n", k="0"))
    history = run(graph, ctx, max_steps=20)

    assert _join_step(history).outcome == "failure"


def test_parallel_k_of_n_invalid_k_exceeds_n(tmp_path):
    """k_of_n k=5 with only 2 branches: out-of-range -> failure."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    graph = parse(_dot_2branch(tmp_path, "k_of_n", k="5"))
    history = run(graph, ctx, max_steps=20)

    assert _join_step(history).outcome == "failure"


# ---------------------------------------------------------------------------
# Shape aliases: component / tripleoctagon
# ---------------------------------------------------------------------------

def test_parallel_component_tripleoctagon_shapes(tmp_path):
    """shape=component triggers fan-out; shape=tripleoctagon triggers join."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    dot = tmp_path / "shapes.dot"
    dot.write_text(
        'digraph shape_test {\n'
        '  graph [goal="shape alias test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [shape="component"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  join [shape="tripleoctagon", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    graph = parse(dot)
    history = run(graph, ctx, max_steps=20)

    assert _join_step(history).outcome == "success"


# ---------------------------------------------------------------------------
# True concurrency — monkeypatch is necessary here to capture thread IDs
# ---------------------------------------------------------------------------

def test_parallel_branches_run_in_separate_threads(tmp_path, monkeypatch):
    """Branches must execute in different threads (real concurrency, not sequential)."""
    thread_ids: list[int] = []
    lock = threading.Lock()

    def branch_handler(n, c):
        with lock:
            thread_ids.append(threading.get_ident())
        time.sleep(0.01)
        return Result(outcome="success")

    monkeypatch.setitem(TYPE_REGISTRY, "branch_a", branch_handler)
    monkeypatch.setitem(TYPE_REGISTRY, "branch_b", branch_handler)

    dot = tmp_path / "thread_test.dot"
    dot.write_text(
        'digraph thread_test {\n'
        '  graph [goal="concurrency"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a [type="branch_a"]\n'
        '  branch_b [type="branch_b"]\n'
        '  join [type="join", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    graph = parse(dot)
    run(graph, _ctx(), max_steps=20)

    assert len(thread_ids) == 2, f"Expected 2 branch executions, got {len(thread_ids)}"
    assert thread_ids[0] != thread_ids[1], "Both branches ran in the same thread — not concurrent"


# ---------------------------------------------------------------------------
# Multi-hop branches
# ---------------------------------------------------------------------------

def test_parallel_multi_hop_branches(tmp_path):
    """Each parallel branch can have multiple nodes before reaching the join."""
    ctx = _ctx()
    ctx.state["work_a1.outcome"] = "success"
    ctx.state["work_a2.outcome"] = "success"
    ctx.state["work_b1.outcome"] = "failure"

    dot = tmp_path / "multihop.dot"
    dot.write_text(
        'digraph multihop {\n'
        '  graph [goal="multi-hop parallel"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  work_a1\n'
        '  work_a2\n'
        '  work_b1\n'
        '  join [type="join", policy="first_success"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> work_a1\n'
        '  fanout -> work_b1\n'
        '  work_a1 -> work_a2\n'
        '  work_a2 -> join\n'
        '  work_b1 -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    graph = parse(dot)
    history = run(graph, ctx, max_steps=20)

    nodes = [s.node for s in history]
    assert "work_a1" in nodes
    assert "work_a2" in nodes
    assert "work_b1" in nodes

    # first_success: branch_a (work_a1 -> work_a2) succeeds -> join success
    assert _join_step(history).outcome == "success"


# ---------------------------------------------------------------------------
# CXDB recording: monotonic seq across parallel branches
# ---------------------------------------------------------------------------

def test_parallel_branch_steps_in_cxdb_monotonic_seq(tmp_path):
    """Branch steps are written to CXDB and seq is strictly monotonically increasing."""
    import sqlite3

    ctx = _ctx(cxdb_path=str(tmp_path / "cxdb.sqlite"))
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    graph = parse(_dot_2branch(tmp_path, "wait_all"))
    run(graph, ctx, max_steps=20)

    conn = sqlite3.connect(str(tmp_path / "cxdb.sqlite"))
    rows = conn.execute("SELECT node, seq FROM steps ORDER BY seq").fetchall()
    conn.close()

    seqs = [r[1] for r in rows]
    nodes_seen = [r[0] for r in rows]

    # All seq values must be unique and strictly increasing
    assert len(seqs) == len(set(seqs)), f"Duplicate seq values: {seqs}"
    assert seqs == sorted(seqs), f"seq not monotonically increasing: {seqs}"

    assert "branch_a" in nodes_seen
    assert "branch_b" in nodes_seen
    assert "join" in nodes_seen


# ---------------------------------------------------------------------------
# Fix P2: edge conditions on branch edges are respected
# ---------------------------------------------------------------------------

def test_parallel_respects_edge_conditions(tmp_path):
    """Branches guarded by a failing edge condition must NOT be launched."""
    ctx = _ctx()
    ctx.state["branch_yes.outcome"] = "success"
    ctx.state["branch_no.outcome"] = "success"

    dot = tmp_path / "cond_branches.dot"
    dot.write_text(
        'digraph cond_branches {\n'
        '  graph [goal="edge cond test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_yes\n'
        '  branch_no\n'
        '  join [type="join", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_yes [condition="outcome=success"]\n'
        '  fanout -> branch_no  [condition="outcome=failure"]\n'
        '  branch_yes -> join\n'
        '  branch_no  -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    graph = parse(dot)
    history = run(graph, ctx, max_steps=20)

    # fanout succeeds -> only branch_yes condition matches
    nodes = [s.node for s in history]
    assert "branch_yes" in nodes
    assert "branch_no" not in nodes


# ---------------------------------------------------------------------------
# Fix P1: branch max_visits prevents runaway loops
# ---------------------------------------------------------------------------

def test_parallel_branch_max_visits_terminates(tmp_path):
    """A branch node with max_visits=1 must not cycle; branch ends with exhausted."""
    ctx = _ctx()
    ctx.state["cycle_node.outcome"] = "success"

    dot = tmp_path / "cycle.dot"
    # cycle_node -> cycle_node (self-loop) with max_visits=1
    dot.write_text(
        'digraph cycle_test {\n'
        '  graph [goal="cycle guard"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  cycle_node [max_visits="1"]\n'
        '  join [type="join", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> cycle_node\n'
        '  cycle_node -> cycle_node\n'
        '  cycle_node -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    graph = parse(dot)
    # Must complete (not hang)
    history = run(graph, ctx, max_steps=20)

    # cycle_node ran at most 2 times (visit 1 ok, visit 2 → exhausted)
    cycle_steps = [s for s in history if s.node == "cycle_node"]
    assert len(cycle_steps) <= 2
    assert "join" in [s.node for s in history]


# ---------------------------------------------------------------------------
# Fix Medium: failure state node is join, not fanout
# ---------------------------------------------------------------------------

def test_parallel_failure_state_not_cleared_by_fanout_retry(tmp_path):
    """When join fails, failure must be attributed to join (not fanout).

    Validates the cursor bugbot concern: before the fix, failure was attributed
    to the fanout node. When fanout ran again (succeeds), it cleared the failure
    state from join — even though join was never retried.
    After the fix, failure is attributed to join, so fanout retry leaves it untouched.
    """
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "failure"
    ctx.state["branch_b.outcome"] = "failure"
    ctx.state["checker.outcome"] = "success"

    dot = tmp_path / "retry_loop.dot"
    dot.write_text(
        'digraph retry_loop {\n'
        '  graph [goal="failure attribution"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  join [type="join", policy="wait_all"]\n'
        '  checker\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  join -> checker\n'
        '  checker -> exit\n'
        '}\n'
    )
    graph = parse(dot)
    run(graph, ctx, max_steps=20)

    # After join fails, _unresolved_failure_node must be "join" (not "fanout").
    # checker and exit don't clear it because they're not the join node.
    assert ctx.state.get("_unresolved_failure_node") == "join", (
        f"Failure attributed to {ctx.state.get('_unresolved_failure_node')!r}, expected 'join'. "
        "Fanout retry would have incorrectly cleared this failure."
    )


# ---------------------------------------------------------------------------
# Bug fix: stuck step after parallel names the join node, not the fanout node
# ---------------------------------------------------------------------------

def test_stuck_after_parallel_names_join_not_fanout(tmp_path):
    """When parallel completes but no outgoing edge matches from join, stuck record names join."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    dot = tmp_path / "stuck_join.dot"
    # join has no outgoing edge matching the outcome, so next_node = None -> stuck
    dot.write_text(
        'digraph stuck_join {\n'
        '  graph [goal="stuck after join"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  join [type="join", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  join -> exit [condition="outcome=failure"]\n'
        '}\n'
    )
    # branches succeed -> join succeeds -> condition "outcome=failure" doesn't match -> stuck
    graph = parse(dot)
    history = run(graph, ctx, max_steps=20)

    stuck_steps = [s for s in history if s.outcome == "stuck"]
    assert stuck_steps, "Expected a stuck step"
    assert stuck_steps[-1].node == "join", (
        f"Stuck step attributed to {stuck_steps[-1].node!r}; expected 'join' (not 'fanout')"
    )


# ---------------------------------------------------------------------------
# Bug fix: join node max_visits enforced for parallel fan-in
# ---------------------------------------------------------------------------

def test_join_max_visits_enforced(tmp_path):
    """A join node with max_visits=1 must exhaust on the second traversal."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"
    ctx.state["retry.outcome"] = "success"

    dot = tmp_path / "join_max_visits.dot"
    # Loop: fanout -> branches -> join -> retry -> fanout (repeat)
    # join has max_visits=1, so second pass through join must emit exhausted
    dot.write_text(
        'digraph join_max_visits {\n'
        '  graph [goal="join max_visits"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  join [type="join", policy="wait_all", max_visits="1"]\n'
        '  retry\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  join -> retry\n'
        '  retry -> fanout\n'
        '  retry -> exit [condition="outcome=done"]\n'
        '}\n'
    )
    graph = parse(dot)
    history = run(graph, ctx, max_steps=50)

    exhausted = [s for s in history if s.outcome == "exhausted"]
    assert exhausted, "Expected an exhausted step when join max_visits=1 is exceeded"
    assert exhausted[-1].node == "join", (
        f"Exhausted step attributed to {exhausted[-1].node!r}; expected 'join'"
    )


# ---------------------------------------------------------------------------
# Bug fix: resume correctly excludes parallel branch overhead from max_steps
# ---------------------------------------------------------------------------

def test_resume_parallel_overhead_preserved(tmp_path):
    """Resuming from a checkpoint that contains parallel branch records must not
    count those branch records against the main-pipeline max_steps budget.

    Setup:
      - Graph: start -> fanout -> {branch_a, branch_b} -> join -> work1 -> exit
      - Checkpoint file has 4 records: start, branch_a(overhead), branch_b(overhead), join
      - max_steps=4 means 4 main-pipeline slots (start, join, work1, exit)
      - Branch records are marked with metadata["_branch_overhead"]="true"

    RED (before fix):
      _parallel_overhead=0 and pre-resume guard len(history) >= max_steps fires:
      4 >= 4 -> returns immediately; work1 never runs.

    GREEN (after fix):
      _resumed_overhead=2; guard: 4-2=2 >= 4 -> False; work1 and exit run.
    """
    import json

    dot = tmp_path / "resume_fanout.dot"
    dot.write_text(
        'digraph resume_fanout {\n'
        '  graph [goal="resume overhead test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  join [type="join", policy="wait_all"]\n'
        '  work1\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  join -> work1\n'
        '  work1 -> exit\n'
        '}\n'
    )

    # Checkpoint representing a run that stopped after join.
    # Branch records are pre-marked with _branch_overhead="true" (as the fix writes them).
    ckpt_path = tmp_path / "ckpt.json"
    ckpt_path.write_text(json.dumps([
        {"node": "start", "outcome": "success", "ts": 0.0, "output_preview": "", "metadata": {}},
        {"node": "branch_a", "outcome": "success", "ts": 0.0, "output_preview": "",
         "metadata": {"_branch_overhead": "true"}},
        {"node": "branch_b", "outcome": "success", "ts": 0.0, "output_preview": "",
         "metadata": {"_branch_overhead": "true"}},
        {"node": "join", "outcome": "success", "ts": 0.0, "output_preview": "join wait_all 2 branches",
         "metadata": {}},
    ]))

    ctx = _ctx()
    ctx.state["work1.outcome"] = "success"

    graph = parse(dot)
    # max_steps=4: exactly enough for start(1) + join(2) + work1(3) + exit(4)
    history = run(graph, ctx, max_steps=4, resume=ckpt_path)

    nodes = [s.node for s in history]
    assert "work1" in nodes, (
        f"work1 should have run on resume when branch overhead is excluded from "
        f"max_steps budget, but history only has: {nodes}"
    )


# ---------------------------------------------------------------------------
# Bug fix: branch thread exception must produce failure, not abort the run
# ---------------------------------------------------------------------------

def test_branch_exception_produces_failure(tmp_path):
    """An uncaught exception in a branch worker must produce failure, not propagate."""
    from runner.handlers import TYPE_REGISTRY

    def _crash_handler(node, ctx):
        raise RuntimeError("deliberate branch crash")

    dot = tmp_path / "branch_exc.dot"
    dot.write_text(
        'digraph branch_exc {\n'
        '  graph [goal="branch exception test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  exploding_branch [type="crash_test"]\n'
        '  branch_b\n'
        '  join [type="join", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> exploding_branch\n'
        '  fanout -> branch_b\n'
        '  exploding_branch -> join\n'
        '  branch_b -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    ctx = _ctx()
    ctx.state["branch_b.outcome"] = "success"
    try:
        TYPE_REGISTRY["crash_test"] = _crash_handler
        graph = parse(dot)
        history = run(graph, ctx, max_steps=20)
    finally:
        TYPE_REGISTRY.pop("crash_test", None)

    join_step = next((s for s in history if s.node == "join"), None)
    assert join_step is not None, "join must appear in history even if a branch crashed"
    assert join_step.outcome == "failure", (
        f"join with a crashed branch should produce 'failure', got {join_step.outcome!r}"
    )


# ---------------------------------------------------------------------------
# Bug fix: empty branches with first_success/k_of_n must return failure
# ---------------------------------------------------------------------------

def test_empty_branches_first_success_fails(tmp_path):
    """When all fan-out edges are filtered, first_success join must return failure."""
    dot = tmp_path / "empty_branches.dot"
    dot.write_text(
        'digraph empty_branches {\n'
        '  graph [goal="empty branches test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  join [type="join", policy="first_success"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a [condition="skip=yes"]\n'
        '  branch_a -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    ctx = _ctx()
    graph = parse(dot)
    history = run(graph, ctx, max_steps=10)
    join_step = next((s for s in history if s.node == "join"), None)
    assert join_step is not None, "join should appear in history"
    assert join_step.outcome == "failure", (
        f"first_success with 0 branches should produce failure, got {join_step.outcome!r}"
    )


# ---------------------------------------------------------------------------
# Bug fix: join max_visits ctx.state must reflect exhausted outcome
# ---------------------------------------------------------------------------

def test_join_max_visits_state_consistency(tmp_path):
    """After join max_visits exhaustion, ctx.state must reflect exhausted."""
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"
    ctx.state["retry.outcome"] = "success"

    dot = tmp_path / "jmv_state.dot"
    dot.write_text(
        'digraph jmv_state {\n'
        '  graph [goal="state consistency"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  join [type="join", policy="wait_all", max_visits="1"]\n'
        '  retry\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  join -> retry\n'
        '  retry -> fanout\n'
        '  retry -> exit [condition="outcome=done"]\n'
        '}\n'
    )
    graph = parse(dot)
    run(graph, ctx, max_steps=50)
    assert ctx.state.get("join.outcome") == "exhausted", (
        f"ctx.state['join.outcome'] must be 'exhausted' after max_visits exceeded, "
        f"got {ctx.state.get('join.outcome')!r}"
    )


# ---------------------------------------------------------------------------
# Bug fix: join_quorum on fanout node must be respected by type=parallel path
# ---------------------------------------------------------------------------

def test_fanout_join_quorum_respected(tmp_path):
    """join_quorum on fanout must be used as k_of_n when join has no explicit policy."""
    dot = tmp_path / "join_quorum.dot"
    dot.write_text(
        'digraph join_quorum {\n'
        '  graph [goal="join_quorum test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel", join_quorum="2"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  branch_c\n'
        '  join [type="join"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  fanout -> branch_c\n'
        '  branch_a -> join\n'
        '  branch_b -> join\n'
        '  branch_c -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"
    ctx.state["branch_c.outcome"] = "failure"
    graph = parse(dot)
    history = run(graph, ctx, max_steps=20)
    join_step = next((s for s in history if s.node == "join"), None)
    assert join_step is not None
    assert join_step.outcome == "success", (
        f"join_quorum=2 with 2/3 successes should succeed (k_of_n), got {join_step.outcome!r}"
    )


# ---------------------------------------------------------------------------
# Bug fix: _find_join_node must work when some branches have edge conditions
# (a disabled/conditional edge must not prevent join discovery for active branches)
# ---------------------------------------------------------------------------

def test_find_join_node_conditional_branches_handled(tmp_path):
    """_find_join_node must return the correct join even when some branches have conditions.

    Regression guard: an earlier strict-convergence fix broke graphs where a
    conditional (filtered-at-runtime) branch leads to a different join — causing
    _find_join_node to return None and skipping the parallel block entirely.
    The BFS must find the join from graph structure alone (ignoring conditions),
    matching the behavior of the runtime parallel executor.
    """
    from runner.engine import _find_join_node

    dot = tmp_path / "conditional_branch.dot"
    dot.write_text(
        'digraph conditional_branch {\n'
        '  graph [goal="conditional branch test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_active\n'
        '  branch_inactive\n'
        '  join [type="join", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_active [condition="active=yes"]\n'
        '  fanout -> branch_inactive [condition="active=no"]\n'
        '  branch_active -> join\n'
        '  branch_inactive -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    graph = parse(dot)
    fanout = graph.nodes["fanout"]
    jn = _find_join_node(graph, fanout)
    assert jn is not None, "_find_join_node must find join even with conditional branches"
    assert jn.name == "join", f"Expected join, got {jn.name!r}"
