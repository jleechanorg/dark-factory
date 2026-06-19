"""Tests for _gate_audit end-to-end contract.

Six contract checks: missing artifact → stale evidence → unresolved review →
replacement/warn → non-replacement/warn → pass.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def test_gate_audit_contract(monkeypatch, tmp_path):
    from runner.handlers import TYPE_REGISTRY, _gate_audit, Context, Result, Node

    # 1. Missing artifact test
    node = Node(name="audit_node", attrs={"type": "gate_audit", "evidence_paths": "missing_ev.jsonl", "shape": "hexagon"})
    ctx = Context(goal="audit test", workdir=tmp_path, backend="echo")
    res = _gate_audit(node, ctx)
    assert res.outcome == "error"
    assert "missing evidence artifacts" in res.output
    assert (tmp_path / "gate_audit_verdict.json").exists()

    # Let's seed a fake target_head_sha
    fake_sha = "a" * 40
    ctx.state["target_head_sha"] = fake_sha

    # Create the evidence file
    ev_file = tmp_path / "evidence.jsonl"
    ev_file.write_text("dummy content without head sha")

    node = Node(name="audit_node", attrs={"type": "gate_audit", "evidence_paths": "evidence.jsonl", "shape": "hexagon"})

    # 2. Stale evidence test (since fake_sha is not in evidence file)
    res = _gate_audit(node, ctx)
    assert res.outcome == "failure"
    assert "stale evidence" in res.output

    # Update evidence file to contain HEAD SHA to make it fresh
    ev_file.write_text(f"dummy content with {fake_sha}")

    # Mock gh pr view to return changes requested
    def fake_subprocess_run(args, **kwargs):
        # If gh pr view is called:
        if "gh" in args and "pr" in args and "view" in args:
            class FakeProc:
                returncode = 0
                stdout = json.dumps({"reviewDecision": "CHANGES_REQUESTED", "reviews": []})
                stderr = ""
            return FakeProc()
        # Fallback to a success process
        class FakeProcSuccess:
            returncode = 0
            stdout = ""
            stderr = ""
        return FakeProcSuccess()

    monkeypatch.setattr("subprocess.run", fake_subprocess_run)
    ctx.state["target_pr"] = "123"

    # 3. Unresolved reviews test
    res = _gate_audit(node, ctx)
    assert res.outcome == "failure"
    assert "unresolved required review state" in res.output

    # Update mock to return APPROVED review state
    def fake_subprocess_run_approved(args, **kwargs):
        if "gh" in args and "pr" in args and "view" in args:
            class FakeProc:
                returncode = 0
                stdout = json.dumps({"reviewDecision": "APPROVED", "reviews": []})
                stderr = ""
            return FakeProc()
        class FakeProcSuccess:
            returncode = 0
            stdout = ""
            stderr = ""
        return FakeProcSuccess()
    monkeypatch.setattr("subprocess.run", fake_subprocess_run_approved)

    # 4. Replacement work with warn verdict test
    ctx.state["diff_summary"] = "5 files changed, 10 insertions(+), 50 deletions(-)" # net negative delta -> replacement
    ctx.state["audit_node.outcome"] = "warn"
    res = _gate_audit(node, ctx)
    assert res.outcome == "failure" # Warn verdict is overridden to failure for replacement work!

    # 5. Non-replacement work with warn verdict test
    ctx.state["diff_summary"] = "5 files changed, 50 insertions(+), 10 deletions(-)" # positive delta -> not replacement
    res = _gate_audit(node, ctx)
    assert res.outcome == "warn" # Warn verdict is allowed for non-replacement!

    # 6. Valid pass test
    ctx.state["audit_node.outcome"] = "success"
    res = _gate_audit(node, ctx)
    assert res.outcome == "success"

    # Check the final verdict artifact file
    verdict_json = json.loads((tmp_path / "gate_audit_verdict.json").read_text())
    assert verdict_json["target_head_sha"] == fake_sha
    assert verdict_json["outcome"] == "success"
    assert verdict_json["verdict"] == "pass"
