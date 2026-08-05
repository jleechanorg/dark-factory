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

import tempfile
import threading
import time
from pathlib import Path

import pytest

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse

ROOT = Path(__file__).parent.parent

# Scratch workdir in the OS tempdir — using the repo root here leaked one
# branch_* mkdtemp per fan-out test into the working tree (378 observed).
SCRATCH = Path(tempfile.mkdtemp(prefix="test_parallel_fanout_"))


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


def _ctx(workdir: Path = SCRATCH, backend: str = "echo", cxdb_path: str | None = None) -> Context:
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


def test_join_max_visits_no_spurious_success_before_exhausted(tmp_path):
    """Join max_visits pre-check: branches must NOT run on the over-limit cycle.

    With max_visits=1 on the join, the second fan-out cycle should emit ONLY
    an exhausted step — no spurious join-success record and no branch workers.

    Bug: engine checked max_visits AFTER running branches and appending the
    join success record, producing contradictory history [success, exhausted].
    Fix: pre-check max_visits before launching branch workers.
    """
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"
    ctx.state["retry.outcome"] = "success"

    dot = tmp_path / "join_max_visits_pre.dot"
    dot.write_text(
        'digraph join_max_visits_pre {\n'
        '  graph [goal="join max_visits pre-check"]\n'
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

    join_records = [s for s in history if s.node == "join"]
    # Exactly 2: one success (cycle 1) + one exhausted (cycle 2 pre-check)
    assert len(join_records) == 2, (
        f"Expected 2 join records (success + exhausted), got {len(join_records)}: "
        f"{[(s.outcome, s.node) for s in join_records]}"
    )
    assert join_records[0].outcome == "success", (
        f"First join record must be success, got {join_records[0].outcome!r}"
    )
    assert join_records[1].outcome == "exhausted", (
        f"Second join record must be exhausted, got {join_records[1].outcome!r}"
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


# ---------------------------------------------------------------------------
# Bug fix: concurrent branches must use distinct workdirs to prevent subprocess
# collisions when file-writing backends (claude/codex/agy) are in use
# ---------------------------------------------------------------------------

def test_parallel_branches_get_distinct_workdirs(tmp_path):
    """Each parallel branch must receive a unique workdir subdirectory.

    RED: _clone_context passes ctx.workdir as-is; all branches share the same
         directory, so any backend that writes to cwd causes races/conflicts.
    GREEN: the fan-out block creates a per-branch tmp subdir under ctx.workdir.
    """
    recorded_workdirs: list[str] = []
    lock = threading.Lock()

    def _record_workdir_handler(node, ctx):
        with lock:
            recorded_workdirs.append(str(ctx.workdir))
        return Result(outcome="success", output=f"workdir={ctx.workdir}")

    dot = tmp_path / "workdir_isolation.dot"
    dot.write_text(
        'digraph workdir_isolation {\n'
        '  graph [goal="workdir isolation test"]\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a [type="record_wd"]\n'
        '  branch_b [type="record_wd"]\n'
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
    ctx = _ctx()
    ctx.workdir = tmp_path
    try:
        TYPE_REGISTRY["record_wd"] = _record_workdir_handler
        graph = parse(dot)
        run(graph, ctx, max_steps=20)
    finally:
        TYPE_REGISTRY.pop("record_wd", None)

    assert len(recorded_workdirs) == 2, f"Expected 2 branch workdir records, got {recorded_workdirs}"
    assert recorded_workdirs[0] != recorded_workdirs[1], (
        f"Both branches used the same workdir {recorded_workdirs[0]!r}; "
        "concurrent file-writing backends would race. Each branch must get a unique subdir."
    )
    # Each branch workdir must be inside the parent workdir
    for wd in recorded_workdirs:
        assert wd.startswith(str(tmp_path)), (
            f"Branch workdir {wd!r} is not under parent workdir {tmp_path!r}"
        )


# ---------------------------------------------------------------------------
# Bug fix: exit node must run even when max_steps is tight and fan-out adds
# many branch steps (the join step itself must not consume the last slot)
# ---------------------------------------------------------------------------

def test_parallel_exit_runs_when_max_steps_tight(tmp_path):
    """exit node must execute even if join step pushes history len == max_steps.

    Graph: start(1) -> fanout(2) -> [branch_a(3), branch_b(4)] -> join(5) -> exit(6)
    max_steps=5 means 5 main-pipeline slots; branch records are overhead and
    must not count. Without the _parallel_overhead fix, start+join = 2 main
    steps but history has start + branch_a + branch_b + join = 4 entries,
    and the next iteration's guard 4 >= 5 fails to block exit prematurely.

    This tests the tight edge case: branch steps must be excluded from the
    main-pipeline step budget so the exit node always gets its slot.
    """
    dot = tmp_path / "tight_max_steps.dot"
    dot.write_text(
        'digraph tight_max_steps {\n'
        '  graph [goal="tight max_steps test"]\n'
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
        '  join -> exit\n'
        '}\n'
    )
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"
    graph = parse(dot)
    # max_steps=4: start(1) + fanout(2) + join(3) + exit(4)
    # branch_a and branch_b are overhead and must NOT count
    history = run(graph, ctx, max_steps=4)

    nodes = [s.node for s in history]
    assert "exit" in nodes, (
        f"exit node must run even with tight max_steps=4; history nodes: {nodes}"
    )


# ---------------------------------------------------------------------------
# Bug fix: branch routing must use current step result, not frozen first failure
# RED: _pick_next(current, last_result) → last_result frozen at first failure;
#      a subsequent successful step with a condition="outcome=success" edge would
#      never match, causing the branch to report stuck.
# GREEN: _pick_next(current, step_result) → routing uses current step's result.
# ---------------------------------------------------------------------------

def test_branch_routing_uses_current_step_result_not_frozen_failure(tmp_path):
    """Branch routing must use current step result, not the preserved first failure.

    Branch: fanout → fail_step → ok_step -[outcome=success]→ after_ok → join
    - fail_step returns failure
    - ok_step returns success
    - ok_step has a conditional edge condition="outcome=success" to after_ok
    - after_ok is an unconditional step before join

    With stale-failure bug: _pick_next(ok_step, last=failure) skips the success
    edge → ok_step stuck → after_ok is NEVER called.
    With fix: _pick_next(ok_step, step_result=success) → success edge matches →
    after_ok is called → branch walks through to join.
    """
    calls: list[str] = []
    lock = threading.Lock()

    def _fail_step_handler(node, ctx):
        with lock:
            calls.append("fail_step")
        return Result(outcome="failure", output="deliberate failure")

    def _ok_step_handler(node, ctx):
        with lock:
            calls.append("ok_step")
        return Result(outcome="success", output="recovered ok")

    def _after_ok_handler(node, ctx):
        with lock:
            calls.append("after_ok")
        return Result(outcome="success", output="after ok ran")

    # Branch: fail_step -[unconditional]→ ok_step -[outcome=success]→ after_ok -[unconditional]→ join
    # after_ok is the canary: it only runs if ok_step's routing uses step_result=success.
    dot_path = tmp_path / "stale_failure.dot"
    dot_path.write_text(
        'digraph stale_failure {\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  fail_step [type="fail_step_type"]\n'
        '  ok_step [type="ok_step_type"]\n'
        '  after_ok [type="after_ok_type"]\n'
        '  join [type="join", policy="wait_all"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> fail_step\n'
        '  fail_step -> ok_step\n'
        '  ok_step -> after_ok [condition="outcome=success"]\n'
        '  after_ok -> join\n'
        '  join -> exit\n'
        '}\n'
    )
    try:
        TYPE_REGISTRY["fail_step_type"] = _fail_step_handler
        TYPE_REGISTRY["ok_step_type"] = _ok_step_handler
        TYPE_REGISTRY["after_ok_type"] = _after_ok_handler
        ctx = _ctx()
        graph = parse(dot_path)
        run(graph, ctx, max_steps=20)
    finally:
        TYPE_REGISTRY.pop("fail_step_type", None)
        TYPE_REGISTRY.pop("ok_step_type", None)
        TYPE_REGISTRY.pop("after_ok_type", None)

    # Canary: after_ok must be called only if routing used step_result=success,
    # not the frozen last_result=failure from fail_step.
    assert "ok_step" in calls, f"ok_step was never executed; calls={calls}"
    assert "after_ok" in calls, (
        f"after_ok was never called — ok_step routing used stale failure result "
        f"and skipped the condition='outcome=success' edge. calls={calls}"
    )


def test_join_as_exit_node_reports_success(tmp_path):
    """When the join node IS the exit node, a successful pipeline must report success.

    Bug: _para_jump_to is found as join; is_exit_node(_para_jump_to)==True triggers a
    break at engine.py without setting ended_at_exit=True. The finally block then
    downgrades any 'success' outcome to 'failure' because it thinks we left early.
    """
    dot_path = tmp_path / "join_is_exit.dot"
    dot_path.write_text(
        'digraph join_is_exit {\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  exit [type="join", policy="wait_all"]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  branch_a -> exit\n'
        '}\n'
    )
    ctx = _ctx()
    graph = parse(dot_path)
    results = run(graph, ctx, max_steps=20)
    final = results[-1].outcome if results else "empty"
    assert final == "success", (
        f"Pipeline with join==exit reported '{final}' instead of 'success'. "
        "ended_at_exit was not set before break in parallel jump-to-exit path."
    )


def test_last_output_updated_after_parallel_join(tmp_path):
    """After parallel fan-out, ctx.state['_last_output'] must reflect the join node.

    Bug: the parallel block updates _last_node and _last_outcome to the join node's
    values but never sets _last_output, leaving it at the fanout handler's output.
    Downstream decision nodes or handlers reading _last_output would see stale data.

    Uses join-as-exit (node named 'exit' with type='join') so the exit handler does
    not subsequently overwrite _last_output, letting us assert the join's output.
    """
    dot_path = tmp_path / "join_exit_last_output.dot"
    dot_path.write_text(
        'digraph join_exit_last_output {\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  exit [type="join", policy="wait_all"]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> exit\n'
        '  branch_b -> exit\n'
        '}\n'
    )
    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    graph = parse(dot_path)
    run(graph, ctx, max_steps=20)

    # The join-as-exit node's output_preview is "join wait_all 2 branches"
    last_output = ctx.state.get("_last_output", "")
    assert "join" in last_output or "wait_all" in last_output or "branch" in last_output, (
        f"_last_output should reflect the join step output, got: '{last_output!r}'. "
        "The parallel block must set ctx.state['_last_output'] when it sets _last_node/_last_outcome."
    )


def test_explicit_type_overrides_shape_for_parallel_detection():
    """_is_parallel_node and _is_join_node must respect explicit type over shape.

    Bug: both functions checked the shape attribute unconditionally, so a node with
    type='codergen' and shape='component' returned True for _is_parallel_node, while
    resolve() dispatches the node to the codergen handler (type wins over shape).
    This inconsistency triggers the parallel fan-out block for non-parallel nodes.

    Fix: when an explicit 'type' attribute is present, only the type determines
    parallel/join identity — same priority rule as resolve().
    """
    from runner.engine import _is_parallel_node, _is_join_node
    from runner.parser import Node

    # type=codergen + shape=component → NOT parallel (type overrides shape)
    codergen_component = Node(name="impl", attrs={"type": "codergen", "shape": "component"})
    assert not _is_parallel_node(codergen_component), (
        "_is_parallel_node must return False when type='codergen' (explicit non-parallel type). "
        "resolve() gives this node the codergen handler; the parallel block must not fire."
    )

    # type=tool + shape=tripleoctagon → NOT join (type overrides shape)
    tool_tripleoctagon = Node(name="t", attrs={"type": "tool", "shape": "tripleoctagon"})
    assert not _is_join_node(tool_tripleoctagon), (
        "_is_join_node must return False when type='tool' (explicit non-join type)."
    )

    # no explicit type + shape=component → parallel (shape-based detection still works)
    shape_only_component = Node(name="fanout", attrs={"shape": "component"})
    assert _is_parallel_node(shape_only_component), (
        "_is_parallel_node must return True for shape=component when no explicit type is set."
    )

    # no explicit type + shape=tripleoctagon → join (shape-based detection still works)
    shape_only_triple = Node(name="join", attrs={"shape": "tripleoctagon"})
    assert _is_join_node(shape_only_triple), (
        "_is_join_node must return True for shape=tripleoctagon when no explicit type is set."
    )

    # explicit type=parallel → parallel regardless of shape
    explicit_parallel = Node(name="fanout2", attrs={"type": "parallel"})
    assert _is_parallel_node(explicit_parallel), (
        "_is_parallel_node must return True when type='parallel'."
    )

    # explicit type=join → join regardless of shape
    explicit_join = Node(name="join2", attrs={"type": "join"})
    assert _is_join_node(explicit_join), (
        "_is_join_node must return True when type='join'."
    )


def test_stuck_branch_returns_failure_not_stale_success(tmp_path):
    """A branch that loses routing (no outgoing edge to the join) must contribute failure.

    Bug: _run_branch_until_join exited the loop with last_result="success" when
    _pick_next returned None mid-branch. The join would then see 2/2 successes and
    produce a spurious "success" outcome even though one branch never reached the join.

    Fix: after the loop, if current is None (no successor), override last_result to
    "failure" with a "branch stuck" message before returning.
    """
    dot_path = tmp_path / "stuck_branch.dot"
    dot_path.write_text(
        'digraph stuck_branch {\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  good_branch\n'
        '  dead_end\n'
        '  join [type="join", policy="wait_all"]\n'
        '  finish [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> good_branch\n'
        '  fanout -> dead_end\n'
        '  good_branch -> join\n'
        # dead_end has no edge to join; it will get stuck
        '  join -> finish\n'
        '}\n'
    )
    ctx = _ctx()
    ctx.state["good_branch.outcome"] = "success"
    ctx.state["dead_end.outcome"] = "success"

    graph = parse(dot_path)
    results = run(graph, ctx, max_steps=20)
    final = results[-1].outcome if results else "empty"
    assert final == "failure", (
        f"Pipeline with stuck branch reported '{final}' instead of 'failure'. "
        "_run_branch_until_join must set outcome='failure' when a branch has no successor "
        "before reaching the join node."
    )


def test_parallel_branch_exit_before_join_returns_failure(tmp_path):
    """Branch that routes to exit before the join barrier must return failure, not success.

    Topology: fanout -> {good_branch -> join, early_exit -> exit}.
    The early_exit branch hits an exit-shaped node before the join.
    With wait_all policy, one branch failing means the join should fail.
    """
    dot_path = tmp_path / "exit_before_join.dot"
    dot_path.write_text(
        'digraph exit_before_join {\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  good_branch\n'
        '  early_exit [shape=Msquare]\n'
        '  join [type="join", policy="wait_all"]\n'
        '  finish [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> good_branch\n'
        '  fanout -> early_exit\n'
        '  good_branch -> join\n'
        '  join -> finish\n'
        '}\n'
    )
    ctx = _ctx()
    ctx.state["good_branch.outcome"] = "success"
    ctx.state["early_exit.outcome"] = "success"

    graph = parse(dot_path)
    results = run(graph, ctx, max_steps=20)
    final = results[-1].outcome if results else "empty"
    assert final == "failure", (
        f"Branch-exits-before-join reported '{final}' instead of 'failure'. "
        "_run_branch_until_join must set outcome='failure' when a branch reaches "
        "an exit node before the join barrier."
    )


def test_parallel_no_join_node_returns_failure(tmp_path):
    """Parallel node with no reachable join must return failure, not silently skip fan-out.

    Topology: start -> fanout(type=parallel) -> {branch_a, branch_b} -> exit
    No join node exists in the graph, so _find_join_node returns None.
    Without this fix, the engine silently treats fanout as a normal node and
    follows one unconditional edge, ending with success. With the fix it must
    detect the miswired graph and report failure.
    """
    dot_path = tmp_path / "no_join.dot"
    dot_path.write_text(
        'digraph no_join {\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  branch_b\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  fanout -> branch_b\n'
        '  branch_a -> exit\n'
        '  branch_b -> exit\n'
        '}\n'
    )
    ctx = _ctx()
    ctx.state["fanout.outcome"] = "success"
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    graph = parse(dot_path)
    results = run(graph, ctx, max_steps=20)
    final = results[-1].outcome if results else "empty"
    assert final == "failure", (
        f"Parallel node with no join reported '{final}' instead of 'failure'. "
        "When _find_join_node returns None, the engine must return failure "
        "to alert about the miswired graph instead of silently skipping fan-out."
    )


def test_branch_context_warns_when_mkdtemp_fails(tmp_path):
    """_branch_context must emit a RuntimeWarning when mkdtemp falls back to parent workdir.

    Isolation is silently disabled when mkdtemp raises OSError. A RuntimeWarning makes
    the degraded isolation observable so operators know file-writing backends may race.
    """
    import warnings
    from unittest.mock import patch
    from runner.engine import _branch_context

    ctx = _ctx(workdir=tmp_path)

    with patch("tempfile.mkdtemp", side_effect=OSError("disk full")):
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            result = _branch_context(ctx, "my_branch")

    assert any("isolation" in str(w.message).lower() for w in caught), (
        "Expected a RuntimeWarning mentioning 'isolation' when mkdtemp fails. "
        "_branch_context must warn callers that branch isolation is degraded."
    )
    assert result.workdir == ctx.workdir, (
        "Fallback workdir should equal parent workdir when mkdtemp fails."
    )


def test_parallel_no_join_emits_node_complete_event(tmp_path):
    """Parallel early-break (no join) must still emit a node_complete event.

    When _find_join_node returns None the engine breaks early, skipping the
    normal _emit_event("node_complete") at the bottom of the main loop.  The
    perf/event log then has a dangling node_enter with no matching node_exit.

    Fix: call _emit_event("node_complete") before the break so every node_enter
    has a corresponding event even on the error path.
    """
    import json

    event_log = tmp_path / "events.jsonl"
    dot_path = tmp_path / "no_join_evt.dot"
    dot_path.write_text(
        'digraph no_join_evt {\n'
        '  start [shape=Mdiamond]\n'
        '  fanout [type="parallel"]\n'
        '  branch_a\n'
        '  exit [shape=Msquare]\n'
        '  start -> fanout\n'
        '  fanout -> branch_a\n'
        '  branch_a -> exit\n'
        '}\n'
    )

    ctx = _ctx()
    ctx.event_log_path = event_log
    ctx.run_id = "test-perf-break"

    graph = parse(dot_path)
    run(graph, ctx, max_steps=20)

    events = [
        json.loads(line)
        for line in event_log.read_text().splitlines()
        if line.strip()
    ]
    node_complete_nodes = {
        e["node"] for e in events if e.get("event") == "node_complete"
    }
    assert "fanout" in node_complete_nodes, (
        "Expected a node_complete event for 'fanout' even when no join node "
        "exists. The early break at line ~1152 skips _emit_event('node_complete'), "
        "leaving an orphaned node_enter in the perf log. Fix: add _emit_event "
        "before the break."
    )


# ---------------------------------------------------------------------------
# Bug fix: resume from incomplete parallel fan-out must re-run branches
# ---------------------------------------------------------------------------

def test_resume_from_incomplete_parallel_fanout_reruns_branches(tmp_path):
    """When a checkpoint ends at the fan-out step (branches never ran),
    resume must re-execute all branches instead of routing to a single successor.

    RED: current resume path calls _pick_next(fanout, ...) which returns one
    branch start node; the other branch and the parallel block are skipped.

    GREEN: resume detects the incomplete fan-out (last step has role=fanout),
    re-runs from the parallel node so both branches execute concurrently.
    """
    import json

    dot = tmp_path / "resume_fanout2.dot"
    dot.write_text(
        'digraph resume_fanout2 {\n'
        '  graph [goal="resume incomplete fanout"]\n'
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
        '  join -> exit\n'
        '}\n'
    )

    # Incomplete checkpoint: fan-out step was recorded, but branches never ran.
    ckpt = tmp_path / "ckpt.json"
    ckpt.write_text(json.dumps([
        {"node": "start", "outcome": "success", "ts": 0.0, "output_preview": "", "metadata": {}},
        {"node": "fanout", "outcome": "success", "ts": 0.0,
         "output_preview": "fanout: fanout", "metadata": {"role": "fanout"}},
    ]))

    ctx = _ctx()
    ctx.state["branch_a.outcome"] = "success"
    ctx.state["branch_b.outcome"] = "success"

    graph = parse(dot)
    history = run(graph, ctx, max_steps=20, resume=ckpt)

    nodes = [s.node for s in history]
    assert "branch_a" in nodes, (
        f"branch_a must run after resuming from incomplete fan-out, got: {nodes}"
    )
    assert "branch_b" in nodes, (
        f"branch_b must run after resuming from incomplete fan-out, got: {nodes}"
    )
    assert "join" in nodes, (
        f"join must run after resuming from incomplete fan-out, got: {nodes}"
    )
    assert "exit" in nodes, (
        f"exit must run after resuming from incomplete fan-out, got: {nodes}"
    )
