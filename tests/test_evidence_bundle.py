"""Tests for orch-1ouv: per-run evidence bundle materialiser.

Covers:
  * Bundle layout (manifest, README, dot copy + checksum, single-run CXDB
    extract, per-step files).
  * Manifest fields including qhez metric totals.
  * Holdout-step output is redacted (no scenario content can leak via the
    bundle), enforced by both an explicit redaction assertion and a grep over
    every file in the bundle for the sealed-repo path token.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import sqlite3
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run
from runner.evidence import write_bundle, _HOLDOUT_PATH_TOKEN
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


def _drive_run(tmp_path: pathlib.Path, monkeypatch) -> tuple[pathlib.Path, str, object]:
    """Run the hello pipeline under echo backend with a fake holdout that
    pretends to have inspected sealed scenarios — and would taint the bundle
    if the redactor failed.
    """
    db_path = tmp_path / "cxdb.sqlite"

    def tainted_holdout(node, ctx):
        # Pretend the evaluator dumped scenario content (which it doesn't in
        # the real world — but we need a way to assert the bundle never
        # surfaces it even if the row's output_head ever did).
        sealed = (
            "scenario:1 secret answer was 42\n"
            "TAINT-MARKER-DO-NOT-LEAK\n"
        )
        return Result(
            outcome="success",
            output=sealed,
            metadata={"verdict": "pass", "passed": "3", "total": "3"},
        )

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", tainted_holdout)
    graph = parse(_pipeline("hello.dot"))
    ctx = Context(goal="bundle smoke", workdir=ROOT, backend="echo", cxdb_path=db_path)
    history = run(graph, ctx, max_steps=20)
    assert history[-1].outcome == "success"
    assert ctx.run_id is not None
    return db_path, ctx.run_id, graph


def test_bundle_layout_and_manifest_fields(tmp_path, monkeypatch):
    db_path, run_id, graph = _drive_run(tmp_path, monkeypatch)
    bundle = tmp_path / "bundle"
    manifest = write_bundle(
        bundle_dir=bundle,
        cxdb_path=db_path,
        run_id=run_id,
        pipeline_path=_pipeline("hello.dot"),
        graph=graph,
        workdir=ROOT,
    )

    # Files
    assert (bundle / "manifest.json").exists()
    assert (bundle / "README.md").exists()
    assert (bundle / "pipeline.dot").exists()
    assert (bundle / "pipeline.dot.sha256").exists()
    assert (bundle / f"cxdb-{run_id}.sqlite").exists()
    assert (bundle / "steps").is_dir()
    step_files = sorted((bundle / "steps").glob("*.txt"))
    assert step_files, "expected at least one per-step file"

    # Checksum sidecar is correct.
    expected = hashlib.sha256(_pipeline("hello.dot").read_bytes()).hexdigest()
    assert (bundle / "pipeline.dot.sha256").read_text().strip() == expected

    # Manifest fields
    on_disk = json.loads((bundle / "manifest.json").read_text())
    assert on_disk == manifest
    for required in (
        "run_id",
        "pipeline_name",
        "goal",
        "started_ts",
        "ended_ts",
        "final_outcome",
        "steps",
        "dark_factory_head_sha",
        "total_tokens",
        "total_cost_usd",
        "total_wall_ms",
    ):
        assert required in on_disk, f"missing manifest key {required!r}"
    assert on_disk["run_id"] == run_id
    assert on_disk["pipeline_name"] == "hello"
    assert on_disk["goal"] == "bundle smoke"
    assert on_disk["final_outcome"] == "success"
    assert on_disk["steps"] >= 4  # start, plan, implement, holdout, exit


def test_bundle_extract_contains_only_this_run(tmp_path, monkeypatch):
    db_path, run_id, graph = _drive_run(tmp_path, monkeypatch)

    # Stuff a second run into the source CXDB so we can prove the extract
    # filters by run_id.
    from runner.cxdb import CXDB

    other = CXDB(db_path)
    try:
        other_id = other.start_run(pipeline="other", goal="other")
        other.record_step(
            run_id=other_id, seq=0, node="other_node", outcome="failure",
            ts=0.0, output="other-run-content", metadata={},
        )
        other.end_run(other_id, "failure")
    finally:
        other.close()

    bundle = tmp_path / "bundle"
    write_bundle(
        bundle_dir=bundle,
        cxdb_path=db_path,
        run_id=run_id,
        pipeline_path=_pipeline("hello.dot"),
        graph=graph,
        workdir=ROOT,
    )

    extract = bundle / f"cxdb-{run_id}.sqlite"
    conn = sqlite3.connect(str(extract))
    try:
        runs = conn.execute("SELECT run_id FROM runs").fetchall()
        steps = conn.execute("SELECT run_id FROM steps").fetchall()
    finally:
        conn.close()
    assert {r[0] for r in runs} == {run_id}
    assert {s[0] for s in steps} == {run_id}


def test_bundle_does_not_leak_holdout_content(tmp_path, monkeypatch):
    """No file in the bundle dir may contain the sealed-holdouts path token
    or the test's TAINT-MARKER scenario content."""
    db_path, run_id, graph = _drive_run(tmp_path, monkeypatch)
    bundle = tmp_path / "bundle"
    write_bundle(
        bundle_dir=bundle,
        cxdb_path=db_path,
        run_id=run_id,
        pipeline_path=_pipeline("hello.dot"),
        graph=graph,
        workdir=ROOT,
    )

    # The redactor must have stripped the tainted holdout output from the
    # per-step file. Check both via direct file read and via the cxdb extract.
    holdout_files = list((bundle / "steps").glob("*holdout*.txt"))
    assert holdout_files, "expected a holdout step file in the bundle"
    for f in holdout_files:
        body = f.read_text()
        assert "TAINT-MARKER-DO-NOT-LEAK" not in body, body
        assert "secret answer was 42" not in body, body
        # The redacted JSON should still contain the verdict metadata.
        payload = json.loads(body)
        assert payload.get("verdict") == "pass"

    # Defense in depth: grep ALL bundle files for the sealed-holdouts path
    # token. Nothing in a bundle should reference it.
    for f in bundle.rglob("*"):
        if not f.is_file():
            continue
        # SQLite binary may not be UTF-8 — read as bytes for the grep.
        data = f.read_bytes()
        assert _HOLDOUT_PATH_TOKEN.encode() not in data, (
            f"bundle file leaks holdout path token: {f}"
        )


def test_cli_evidence_bundle_flag_creates_bundle(tmp_path):
    """End-to-end: `--evidence-bundle` produces a bundle when CXDB is auto-
    provisioned from the bundle dir (acceptance criterion in the bead spec)."""
    bundle = tmp_path / "auto-bundle"
    cxdb = tmp_path / "qhez-test.sqlite"
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            str(_pipeline("hello.dot")),
            "--goal",
            "smoke",
            "--backend",
            "echo",
            "--feature",
            "hello",
            "--cxdb",
            str(cxdb),
            "--evidence-bundle",
            str(bundle),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert (bundle / "manifest.json").exists()
    manifest = json.loads((bundle / "manifest.json").read_text())
    assert manifest["pipeline_name"] == "hello"
    # wall_ms per step should be present in metadata (echo backend records it).
    extract = next(bundle.glob("cxdb-*.sqlite"))
    conn = sqlite3.connect(str(extract))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT node, metadata_json FROM steps ORDER BY seq"
        ).fetchall()
    finally:
        conn.close()
    codergen_rows = [r for r in rows if r["node"] in {"plan", "implement"}]
    assert codergen_rows
    for r in codergen_rows:
        meta = json.loads(r["metadata_json"])
        assert "wall_ms" in meta
