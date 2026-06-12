"""AttractorBench Tier-3 contract regression tests.

Locks in the JSON shape expected by AttractorBench
(https://github.com/strongdm/attractorbench) tier-3 harness so future refactors
of ``bin/conformance`` cannot silently regress the contract. The harness lives
out-of-tree at ``~/projects/attractorbench/tasks/main/tests/conformance/`` and
checks each subcommand's stdout against these invariants.

Original failure mode (2026-05-21 audit, score 5/27):
    - ``parse`` nested payload under ``"graph"`` key
    - ``list-handlers`` returned ``{"handlers":[...]}`` not a bare list
    - ``run`` parser refused ``done [shape=Msquare]`` because of literal
      ``name == "exit"`` check
    - ``run`` had no mock-URL detection so ``OPENAI_BASE_URL`` was ignored

This module exercises the post-fix contract end-to-end against synthetic DOTs
matching the harness fixtures.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import run_conformance  # noqa: E402

CONFORMANCE = ROOT / "bin" / "conformance"


def _run(*args: str, env: dict | None = None) -> "subprocess.CompletedProcess[str]":
    return run_conformance(*args, env=env)


SIMPLE_DOT = """digraph simple {
    start [shape=Mdiamond]
    step_a [shape=box, prompt="Do step A"]
    done [shape=Msquare]
    start -> step_a -> done
}
"""

CONDITIONAL_DOT = """digraph conditional {
    graph [goal="Test conditional routing"]
    start [shape=Mdiamond]
    check [shape=box, prompt="Check something"]
    path_a [shape=box, prompt="Path A"]
    path_b [shape=box, prompt="Path B"]
    done [shape=Msquare]
    start -> check
    check -> path_a [condition="outcome=success"]
    check -> path_b [condition="outcome=fail"]
    path_a -> done
    path_b -> done
}
"""

MISSING_START_DOT = """digraph bad {
    step_a [shape=box, prompt="Step A"]
    done [shape=Msquare]
    step_a -> done
}
"""

ORPHAN_DOT = """digraph orphan {
    start [shape=Mdiamond]
    step_a [shape=box, prompt="Step A"]
    orphan [shape=box, prompt="Orphan node"]
    done [shape=Msquare]
    start -> step_a -> done
}
"""

MISSING_PROMPT_DOT = """digraph no_prompt {
    start [shape=Mdiamond]
    step_a [shape=box]
    done [shape=Msquare]
    start -> step_a -> done
}
"""


def test_parse_returns_top_level_nodes_and_edges(tmp_path):
    """Gap 1 (parse): nodes/edges must live at top level, not under "graph"."""
    dot = tmp_path / "simple.dot"
    dot.write_text(SIMPLE_DOT)
    proc = _run("parse", str(dot))
    assert proc.returncode == 0, proc.stderr
    ast = json.loads(proc.stdout)
    assert isinstance(ast, dict)
    assert "nodes" in ast and "edges" in ast
    assert "graph" not in ast or not isinstance(ast["graph"], dict) or "nodes" not in ast["graph"]
    assert len(ast["nodes"]) >= 3 and len(ast["edges"]) >= 2
    # Harness invariants
    assert all("id" in n for n in ast["nodes"])
    assert all(("from" in e and "to" in e) or ("source" in e and "target" in e) for e in ast["edges"])
    assert any(n.get("id") == "start" or n.get("shape") == "Mdiamond" for n in ast["nodes"])


def test_parse_msquare_exit_node(tmp_path):
    """Gap 2: parser must accept ``done [shape=Msquare]`` (no literal name=='exit')."""
    dot = tmp_path / "simple.dot"
    dot.write_text(SIMPLE_DOT)
    proc = _run("parse", str(dot))
    assert proc.returncode == 0, proc.stderr
    ast = json.loads(proc.stdout)
    assert any(n.get("shape") == "Msquare" or n.get("id") == "done" for n in ast["nodes"])


def test_parse_conditional_edges_have_condition_attr(tmp_path):
    """Conditional edges from `check` must surface the `condition` attribute."""
    dot = tmp_path / "conditional.dot"
    dot.write_text(CONDITIONAL_DOT)
    proc = _run("parse", str(dot))
    assert proc.returncode == 0, proc.stderr
    ast = json.loads(proc.stdout)
    cond_edges = [
        e
        for e in ast["edges"]
        if "condition" in e or "condition" in e.get("attrs", {})
    ]
    check_edges = [e for e in ast["edges"] if e.get("from") == "check" or e.get("source") == "check"]
    assert len(cond_edges) >= 2
    assert len(check_edges) >= 2


def test_list_handlers_returns_bare_list():
    """Gap 1 (list-handlers): must be a bare JSON list, not ``{"handlers":[...]}``."""
    proc = _run("list-handlers")
    assert proc.returncode == 0, proc.stderr
    handlers = json.loads(proc.stdout)
    assert isinstance(handlers, list), f"Expected list, got {type(handlers).__name__}"
    text = json.dumps(handlers).lower()
    assert ("start" in text or "mdiamond" in text)
    assert ("box" in text or "codergen" in text)
    assert ("exit" in text or "msquare" in text or "done" in text)


def test_validate_diagnostics_have_severity(tmp_path):
    """Gap 1 (validate): diagnostics list with per-item severity field."""
    dot = tmp_path / "missing_start.dot"
    dot.write_text(MISSING_START_DOT)
    proc = _run("validate", str(dot))
    payload = json.loads(proc.stdout)
    assert isinstance(payload, dict) and "diagnostics" in payload
    assert isinstance(payload["diagnostics"], list)
    # Either non-zero exit or at least one error-severity diagnostic
    has_error = any(d.get("severity") in ("error", "Error") for d in payload["diagnostics"])
    assert has_error or proc.returncode != 0


def test_validate_orphan_produces_warning(tmp_path):
    """Unreachable node must surface as ``severity:"warning"``, not crash."""
    dot = tmp_path / "orphan.dot"
    dot.write_text(ORPHAN_DOT)
    proc = _run("validate", str(dot))
    payload = json.loads(proc.stdout)
    assert isinstance(payload, dict) and "diagnostics" in payload
    diags = payload["diagnostics"]
    has_warning = any(
        d.get("severity") in ("warning", "Warning")
        or "orphan" in str(d).lower()
        or "unreachable" in str(d).lower()
        for d in diags
    )
    assert has_warning, f"Expected warning diagnostic, got {diags!r}"


def test_validate_missing_prompt_emits_warning(tmp_path):
    """Box node without prompt must yield ``severity:"warning"`` (not raise)."""
    dot = tmp_path / "missing_prompt.dot"
    dot.write_text(MISSING_PROMPT_DOT)
    proc = _run("validate", str(dot))
    payload = json.loads(proc.stdout)
    diags = payload["diagnostics"]
    has_prompt_warning = any(
        d.get("severity") in ("warning", "Warning") and "prompt" in str(d).lower()
        for d in diags
    )
    assert has_prompt_warning, f"Expected prompt warning, got {diags!r}"


def test_run_emits_top_level_status(tmp_path):
    """Gap 1 (run): ``status`` must live at top level (harness reads it directly)."""
    dot = tmp_path / "simple.dot"
    dot.write_text(SIMPLE_DOT)
    proc = _run("run", str(dot))
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    assert isinstance(result, dict)
    assert "status" in result or "outcome" in result
    status = result.get("status", result.get("outcome"))
    assert status in ("success", "completed", "done")


def test_run_executes_msquare_terminal(tmp_path):
    """Gap 2 (engine): pipeline with ``done [shape=Msquare]`` must run to completion."""
    dot = tmp_path / "simple.dot"
    dot.write_text(SIMPLE_DOT)
    proc = _run("run", str(dot))
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout)
    steps = result.get("steps", [])
    node_ids = {s.get("node") for s in steps}
    assert "done" in node_ids
