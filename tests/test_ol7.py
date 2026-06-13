# -*- coding: utf-8 -*-
import json
import pathlib
import sys
import time
import subprocess
import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse

def test_heartbeat_lifecycle(tmp_path, monkeypatch):
    dot_path = tmp_path / 'simple.dot'
    dot_path.write_text(
        'digraph simple {\n'
        '  start [shape=Mdiamond]\n'
        '  node1 [type="codergen", timeout=120]\n'
        '  exit [shape=Msquare]\n'
        '  start -> node1 -> exit\n'
        '}\n'
    )
    
    verified_node_start = []
    
    def test_handler(node, ctx):
        hb_path = pathlib.Path.home() / '.dark-factory' / 'runs' / ctx.run_id / 'heartbeat.json'
        assert hb_path.exists()
        hb_data = json.loads(hb_path.read_text(encoding="utf-8"))
        
        assert hb_data["pipeline"] == str(dot_path.resolve())
        assert hb_data["goal"] == "test_goal"
        assert hb_data["workdir"] == str(ctx.workdir)
        assert hb_data["current_node"] == "node1"
        assert isinstance(hb_data["start_timestamp"], float)
        assert hb_data["backend"] == "echo"
        assert hb_data["timeout"] == 120.0
        assert isinstance(hb_data["last_completed_seq"], int)
        
        verified_node_start.append(True)
        return Result(outcome="success", output="node1 done")
        
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", test_handler)
    
    graph = parse(dot_path)
    ctx = Context(goal="test_goal", workdir=tmp_path, backend="echo")
    
    history = run(graph, ctx)
    
    assert verified_node_start
    
    hb_path = pathlib.Path.home() / '.dark-factory' / 'runs' / ctx.run_id / 'heartbeat.json'
    assert hb_path.exists()
    hb_data = json.loads(hb_path.read_text(encoding="utf-8"))
    
    assert hb_data["current_node"] is None
    assert isinstance(hb_data["elapsed_time"], float)
    assert isinstance(hb_data["timestamp"], float)
    assert hb_data["last_completed_seq"] == ctx.last_completed_seq

def test_heartbeat_survives_kill(tmp_path):
    dot_path = tmp_path / 'hang.dot'
    dot_path.write_text(
        'digraph hang {\n'
        '  start [shape=Mdiamond]\n'
        '  hang_node [type="codergen"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> hang_node -> exit\n'
        '}\n'
    )
    
    run_id = 'test_run_kill_123'
    
    run_script = tmp_path / 'run_pipeline.py'
    run_script.write_text(f"""
import sys
import time
import pathlib
sys.path.insert(0, {repr(str(ROOT))})

from runner.parser import parse
from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY

def hang_handler(node, ctx):
    pathlib.Path({repr(str(tmp_path / 'started.txt'))}).write_text("ok")
    time.sleep(10)
    return Result(outcome="success")

TYPE_REGISTRY["codergen"] = hang_handler

graph = parse(pathlib.Path({repr(str(dot_path))}))
ctx = Context(goal="test_kill", workdir=pathlib.Path({repr(str(tmp_path))}), backend="echo", run_id={repr(run_id)})
run(graph, ctx)
""")
    
    proc = subprocess.Popen([sys.executable, str(run_script)], cwd=tmp_path)
    
    sentinel = tmp_path / 'started.txt'
    for _ in range(50):
        if sentinel.exists():
            break
        time.sleep(0.1)
    else:
        proc.kill()
        pytest.fail("Subprocess failed to start node handler in time")
        
    time.sleep(0.1)
    
    hb_path = pathlib.Path.home() / '.dark-factory' / 'runs' / run_id / 'heartbeat.json'
    assert hb_path.exists()
    hb_data = json.loads(hb_path.read_text(encoding="utf-8"))
    assert hb_data["current_node"] == "hang_node"
    
    proc.kill()
    proc.wait()
    
    assert hb_path.exists()
    hb_data = json.loads(hb_path.read_text(encoding="utf-8"))
    assert hb_data["current_node"] == "hang_node"
