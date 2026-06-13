import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent


def test_summary_includes_paths(tmp_path):
    dot = tmp_path / "minimal.dot"
    dot.write_text(
        'digraph minimal {\n'
        '  graph [goal="minimal"]\n'
        '  start [shape=Mdiamond]\n'
        '  exit [shape=Msquare]\n'
        '  start -> exit\n'
        '}\n'
    )
    checkpoint_file = tmp_path / "chk.json"
    cxdb_file = tmp_path / "cxdb.sqlite"
    evidence_dir = tmp_path / "evidence"

    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            str(dot),
            "--goal",
            "test summary",
            "--backend",
            "echo",
            "--checkpoint",
            str(checkpoint_file),
            "--cxdb",
            str(cxdb_file),
            "--evidence-bundle",
            str(evidence_dir),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    summary = json.loads(proc.stdout)

    assert "run_id" in summary
    assert summary["run_id"] is not None
    assert "log_path" in summary
    assert summary["log_path"] == str(pathlib.Path.home() / ".dark-factory" / "logs" / f"{summary['run_id']}.log")
    assert "cxdb_path" in summary
    assert summary["cxdb_path"] == str(cxdb_file.resolve())
    assert "checkpoint_path" in summary
    assert summary["checkpoint_path"] == str(checkpoint_file.resolve())
    assert "evidence_bundle" in summary
    assert summary["evidence_bundle"] == str(evidence_dir.resolve())
    assert "final_outcome" in summary
    assert summary["final_outcome"] == "success"


def test_panic_includes_paths(tmp_path):
    # Pass a non-existent pipeline to trigger a panic (FileNotFoundError)
    checkpoint_file = tmp_path / "chk.json"
    cxdb_file = tmp_path / "cxdb.sqlite"
    evidence_dir = tmp_path / "evidence"

    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            str(tmp_path / "does_not_exist.dot"),
            "--goal",
            "test summary",
            "--backend",
            "echo",
            "--checkpoint",
            str(checkpoint_file),
            "--cxdb",
            str(cxdb_file),
            "--evidence-bundle",
            str(evidence_dir),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 128
    summary = json.loads(proc.stdout)

    assert "run_id" in summary
    assert "log_path" in summary
    assert "cxdb_path" in summary
    assert summary["cxdb_path"] == str(cxdb_file.resolve())
    assert "checkpoint_path" in summary
    assert summary["checkpoint_path"] == str(checkpoint_file.resolve())
    assert "evidence_bundle" in summary
    assert summary["evidence_bundle"] == str(evidence_dir.resolve())
    assert "status" in summary
    assert summary["status"] == "panic"
