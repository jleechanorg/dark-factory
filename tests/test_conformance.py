from __future__ import annotations

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import run_conformance  # noqa: E402


def test_conformance_parse_outputs_stable_ast():
    proc = run_conformance("parse", "pipelines/factory/hello.dot", timeout=120)

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["name"] == "hello"
    assert payload["nodes"]
    assert payload["edges"]
    assert all("id" in node for node in payload["nodes"])
    assert all("from" in edge and "to" in edge for edge in payload["edges"])


def test_conformance_validate_outputs_machine_readable_json():
    proc = run_conformance("validate", "pipelines/factory/hello.dot")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert "diagnostics" in payload
    assert isinstance(payload["diagnostics"], list)


def test_conformance_validate_rejects_bad_dot(tmp_path):
    bad_dot = tmp_path / "bad.dot"
    bad_dot.write_text("digraph bad { start; start -> missing; }")

    proc = run_conformance("validate", str(bad_dot))

    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "diagnostics" in payload
    assert isinstance(payload["diagnostics"], list)
    assert len(payload["diagnostics"]) > 0


def test_conformance_run_uses_echo_backend_and_zero_cost():
    proc = run_conformance("run", "pipelines/factory/hello.dot", "--feature", "hello")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "success"
    assert payload["outcome"] == "success"
    assert payload["cost"] == {"api_calls": 0, "tokens": 0}


def test_conformance_list_handlers():
    proc = run_conformance("list-handlers")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert isinstance(payload, list)
    text = json.dumps(payload)
    assert "codergen" in text
    assert "holdout_eval" in text


def test_conformance_score_is_deterministic_mock_surface():
    proc = run_conformance("score")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["subcommand"] == "score"
    assert payload["status"] == "pass"
    assert payload["scope"] == "local_mock"
    assert payload["tiers"]["sealed_benchmark"] == "not_run"
    assert payload["cost"] == {"api_calls": 0, "tokens": 0}


def test_conformance_run_supports_mock_url():
    proc = run_conformance("run", "pipelines/factory/hello.dot", "--mock-url", "http://127.0.0.1:54321")
    assert proc.returncode == 0, proc.stdout + proc.stderr


def test_conformance_validate_walker_skips_underscore_dot_libraries():
    """`conformance validate` (no args) walks the pipelines + benchmarks trees
    and rejects any file without `start`/`exit`. Library fragments
    (`_*.dot`, e.g. `pipelines/_base.dot`) are deliberately free of those
    nodes — they are included by lanes via `include="@..."` and parsed with
    `require_start_exit=False`. The walker must filter them out so the
    strict parser invariant is preserved while `conformance score` (CI Gate
    2) stays green. Regression test for bead jleechan-u8e."""
    # 1. The walker must succeed (no diagnostics) on the full tree.
    proc = run_conformance("validate")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["diagnostics"] == [], payload

    # 2. The parser invariant is still strict: explicit validate of a
    # library fragment must fail (so authors cannot accidentally run a
    # `_base.dot`-style file as a top-level pipeline).
    proc_explicit = run_conformance("validate", "pipelines/_base.dot")
    assert proc_explicit.returncode == 1, proc_explicit.stdout + proc_explicit.stderr
    payload_explicit = json.loads(proc_explicit.stdout)
    assert any(
        diag.get("severity") == "error" and "start" in diag.get("message", "").lower()
        for diag in payload_explicit["diagnostics"]
    ), payload_explicit
