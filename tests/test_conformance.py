from __future__ import annotations

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent


def _run_conformance(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(ROOT / "bin" / "conformance"), *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )


def test_conformance_parse_outputs_stable_ast():
    proc = _run_conformance("parse", "pipelines/factory/hello.dot")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["name"] == "hello"
    assert payload["nodes"]
    assert payload["edges"]
    assert all("id" in node for node in payload["nodes"])
    assert all("from" in edge and "to" in edge for edge in payload["edges"])


def test_conformance_validate_outputs_machine_readable_json():
    proc = _run_conformance("validate", "pipelines/factory/hello.dot")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert "diagnostics" in payload
    assert isinstance(payload["diagnostics"], list)


def test_conformance_validate_rejects_bad_dot(tmp_path):
    bad_dot = tmp_path / "bad.dot"
    bad_dot.write_text("digraph bad { start; start -> missing; }")

    proc = _run_conformance("validate", str(bad_dot))

    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "diagnostics" in payload
    assert isinstance(payload["diagnostics"], list)
    assert len(payload["diagnostics"]) > 0


def test_conformance_run_uses_echo_backend_and_zero_cost():
    proc = _run_conformance("run", "pipelines/factory/hello.dot", "--feature", "hello")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "success"
    assert payload["outcome"] == "success"
    assert payload["cost"] == {"api_calls": 0, "tokens": 0}


def test_conformance_list_handlers():
    proc = _run_conformance("list-handlers")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert isinstance(payload, list)
    text = json.dumps(payload)
    assert "codergen" in text
    assert "holdout_eval" in text


def test_conformance_score_is_deterministic_mock_surface():
    proc = _run_conformance("score")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["subcommand"] == "score"
    assert payload["status"] == "pass"
    assert payload["scope"] == "local_mock"
    assert payload["tiers"]["sealed_benchmark"] == "not_run"
    assert payload["cost"] == {"api_calls": 0, "tokens": 0}
