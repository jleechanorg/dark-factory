import json
import pathlib
import sys
import pytest

from runner.parser import parse
from runner.engine import run
from runner.handlers import Context, Result
from runner.__main__ import main

ROOT = pathlib.Path(__file__).parent.parent

def test_manifest_creation_and_auto_checkpoint(tmp_path, monkeypatch):
    monkeypatch.setattr(pathlib.Path, "home", lambda: tmp_path)
    
    # Create a simple DOT file
    dot = tmp_path / "simple.dot"
    dot.write_text(
        'digraph simple {\n'
        '  graph [goal="Test manifest creation" goal_gate=false]\n'
        '  start [shape=Mdiamond]\n'
        '  one [type="codergen"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> one -> exit\n'
        '}\n',
        encoding="utf-8"
    )
    
    graph = parse(dot)
    ctx = Context(goal="Test manifest goal", workdir=tmp_path, backend="echo")
    
    # Run the graph
    history = run(graph, ctx)
    
    # Verify that manifest.json was written
    run_id = ctx.run_id
    assert run_id is not None
    
    manifest_path = tmp_path / ".dark-factory" / "runs" / run_id / "manifest.json"
    assert manifest_path.exists()
    
    # Load manifest and check content
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    assert manifest["pipeline"] == str(dot)
    assert manifest["goal"] == "Test manifest goal"
    assert manifest["backend"] == "echo"
    assert manifest["workdir"] == str(tmp_path)
    assert isinstance(manifest["state"], dict)
    
    # Verify that checkpoint.json was also written (since checkpoint defaults to None, it should have been auto-set)
    checkpoint_path = tmp_path / ".dark-factory" / "runs" / run_id / "checkpoint.json"
    assert checkpoint_path.exists()
    
    checkpoint_data = json.loads(checkpoint_path.read_text(encoding="utf-8"))
    assert len(checkpoint_data) > 0
    assert checkpoint_data[0]["node"] == "start"


def test_cli_resume_rewriting_and_manifest_load(tmp_path, monkeypatch):
    monkeypatch.setattr(pathlib.Path, "home", lambda: tmp_path)
    
    # Create the run dir
    run_id = "abcde1234567"
    run_dir = tmp_path / ".dark-factory" / "runs" / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    
    # Create a mock pipeline DOT file
    dot = tmp_path / "resume_test.dot"
    dot.write_text(
        'digraph resume_test {\n'
        '  graph [goal="Resume target" goal_gate=false]\n'
        '  start [shape=Mdiamond]\n'
        '  step1 [type="codergen"]\n'
        '  step2 [type="codergen"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> step1 -> step2 -> exit\n'
        '}\n',
        encoding="utf-8"
    )
    
    # Write mock manifest.json
    manifest_path = run_dir / "manifest.json"
    manifest_data = {
        "pipeline": str(dot),
        "goal": "Resumed Goal Value",
        "backend": "echo",
        "workdir": str(tmp_path),
        "state": {"custom_key": "custom_val"},
    }
    manifest_path.write_text(json.dumps(manifest_data, indent=2), encoding="utf-8")
    
    # Write checkpoint.json representing completed start and step1
    checkpoint_path = run_dir / "checkpoint.json"
    checkpoint_data = [
        {
            "node": "start",
            "outcome": "success",
            "ts": 0.0,
            "output_preview": "start",
            "metadata": {},
        },
        {
            "node": "step1",
            "outcome": "success",
            "ts": 0.0,
            "output_preview": "step1",
            "metadata": {},
        }
    ]
    checkpoint_path.write_text(json.dumps(checkpoint_data, indent=2), encoding="utf-8")
    
    # Capture sys.stdout/sys.stderr or check return code
    import io
    stdout_buf = io.StringIO()
    monkeypatch.setattr(sys, "stdout", stdout_buf)
    
    rc = main(["resume", run_id])
    assert rc == 0
    
    output = stdout_buf.getvalue()
    summary = json.loads(output)
    
    # Verify final outcome is success
    assert summary["final_outcome"] == "success"
    
    # Verify the trace contains step2 and exit (which ran after resumption)
    trace_nodes = [step["node"] for step in summary["trace"]]
    assert trace_nodes == ["start", "step1", "step2", "exit"]
    
    # Let's verify that the checkpoint file was updated
    updated_checkpoint = json.loads(checkpoint_path.read_text(encoding="utf-8"))
    assert [step["node"] for step in updated_checkpoint] == ["start", "step1", "step2", "exit"]
