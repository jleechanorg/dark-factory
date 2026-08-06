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
import tempfile

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

# Scratch workdir in the OS tempdir — using the repo root here leaks one
# branch_* mkdtemp per fan-out test into the working tree.
SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="test_evidence_bundle_"))

from conftest import _pipeline, register_scratch_dir  # noqa: E402

register_scratch_dir(SCRATCH)

from runner.engine import run  # noqa: E402
from runner.evidence import write_bundle, _HOLDOUT_PATH_TOKEN  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.parser import parse  # noqa: E402
from runner.panic_hook import PANIC_EXIT_CODE  # noqa: E402


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
    ctx = Context(goal="bundle smoke", workdir=SCRATCH, backend="echo", cxdb_path=db_path)
    history = run(graph, ctx, max_steps=20)
    assert history[-1].outcome == "success"
    assert ctx.run_id is not None
    return db_path, ctx.run_id, graph


def test_bundle_layout_and_manifest_fields(tmp_path, monkeypatch):
    db_path, run_id, graph = _drive_run(tmp_path, monkeypatch)
    bundle = tmp_path / "bundle"
    command = "dark-factory --pipeline pipelines/hello.dot --goal bundle smoke --backend echo"
    manifest = write_bundle(
        bundle_dir=bundle,
        cxdb_path=db_path,
        run_id=run_id,
        pipeline_path=_pipeline("hello.dot"),
        graph=graph,
        workdir=SCRATCH,
        command=command,
    )

    # Files
    assert (bundle / "manifest.json").exists()
    assert (bundle / "README.md").exists()
    assert (bundle / "pipeline.dot").exists()
    assert (bundle / "pipeline.dot.sha256").exists()
    assert (bundle / f"cxdb-{run_id}.sqlite").exists()
    assert (bundle / "summary.json").exists()
    assert (bundle / "command.txt").exists()
    assert (bundle / "node_io.jsonl").exists()
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
        "command",
        "command_path",
        "started_ts",
        "ended_ts",
        "wall_clock_ms",
        "final_outcome",
        "summary_path",
        "steps",
        "dark_factory_head_sha",
        "cxdb_sha256",
        "cxdb_path",
        "cxdb_extract_path",
        "cxdb_extract_sha256",
        "total_tokens",
        "total_cost_usd",
        "total_wall_ms",
        "pipeline_copy",
        "node_io_path",
    ):
        assert required in on_disk, f"missing manifest key {required!r}"
    assert on_disk["run_id"] == run_id
    assert on_disk["pipeline_name"] == "hello"
    assert on_disk["goal"] == "bundle smoke"
    assert on_disk["final_outcome"] == "success"
    assert on_disk["steps"] >= 4  # start, plan, implement, holdout, exit
    assert on_disk["command"] == command
    assert on_disk["pipeline_copy"] == "pipeline.dot"
    assert on_disk["node_io_path"] == "node_io.jsonl"
    assert on_disk["summary_path"] == "summary.json"
    assert on_disk["command_path"] == "command.txt"
    assert on_disk["wall_clock_ms"] is not None
    assert on_disk["cxdb_extract_path"] == str(bundle / f"cxdb-{run_id}.sqlite")

    summary = json.loads((bundle / "summary.json").read_text())
    assert summary["command"] == command
    assert summary["wall_clock_ms"] == on_disk["wall_clock_ms"]
    assert summary["cxdb_path"] == str(db_path)
    assert summary["pipeline_path"] == str(_pipeline("hello.dot"))
    assert summary["steps"] == on_disk["steps"]
    assert summary["node_io_path"] == "node_io.jsonl"

    node_io_lines = (bundle / "node_io.jsonl").read_text().strip().splitlines()
    assert node_io_lines, "expected per-node I/O refs"
    sample = json.loads(node_io_lines[0])
    assert {"seq", "node", "outcome", "ts", "io_refs", "log_refs"} <= set(sample)
    assert "events" in sample["log_refs"]


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
        workdir=SCRATCH,
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
        workdir=SCRATCH,
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
    # Mirror the repo's implementation tree into the scratch workdir so the
    # sealed holdout evaluator can locate `implementation` (it resolves
    # relative to ctx.workdir). Without this symlink the holdout returns
    # "implementation missing".
    impl_link = tmp_path / "impl"
    if not impl_link.exists():
        impl_link.symlink_to(ROOT / "impl")
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
            "--workdir",
            str(tmp_path),
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
    summary = json.loads((bundle / "summary.json").read_text())
    assert manifest["pipeline_name"] == "hello"
    assert manifest["command"].startswith("dark-factory")
    assert (bundle / "command.txt").exists()
    assert (bundle / "node_io.jsonl").exists()
    assert (bundle / "summary.json").exists()
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
    assert summary["final_outcome"] in {"success", "failure"}
    assert summary["cxdb_path"] == str(cxdb)
    assert summary["steps"] == len(rows)


def test_cli_creates_default_evidence_bundle_under_run_id(tmp_path):
    """Every CLI run should leave evidence/<run-id>/ unless explicitly disabled."""
    cxdb = tmp_path / "default-evidence.sqlite"
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            str(_pipeline("hello.dot")),
            "--goal",
            "default evidence smoke",
            "--backend",
            "echo",
            "--feature",
            "hello",
            "--workdir",
            str(tmp_path),
            "--cxdb",
            str(cxdb),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert proc.returncode in {0, 1}, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    run_id = payload["run_id"]
    bundle = tmp_path / "evidence" / run_id
    assert payload["evidence_bundle"] == str(bundle)
    assert (bundle / "manifest.json").exists()
    assert (bundle / "summary.json").exists()
    summary = json.loads((bundle / "summary.json").read_text(encoding="utf-8"))
    assert summary["final_outcome"] == payload["final_outcome"]
    assert summary["events_path"] == str(bundle / "events.jsonl")
    assert (bundle / "command.txt").exists()
    assert (bundle / "node_io.jsonl").exists()
    assert (bundle / "events.jsonl").exists()
    node_io = [
        json.loads(line)
        for line in (bundle / "node_io.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    assert any("transcript_path" in item["io_refs"] for item in node_io)
    assert not list((tmp_path / "evidence").glob("_pending-*"))


def test_cli_panic_writes_default_evidence_bundle(tmp_path, monkeypatch, capsys):
    """A top-level CLI panic after run_id allocation still leaves evidence."""
    import runner.__main__ as cli
    from runner.cxdb import CXDB

    def exploding_run(graph, ctx, **_kwargs):
        db = CXDB(ctx.cxdb_path)
        try:
            ctx.run_id = db.start_run(pipeline=graph.name, goal=ctx.goal)
            db.record_step(
                run_id=ctx.run_id,
                seq=0,
                node="before_panic",
                outcome="success",
                ts=0.0,
                output="allocated run id before panic",
                metadata={},
            )
        finally:
            db.close()
        raise RuntimeError("cli panic after run id")

    monkeypatch.setattr(cli, "run", exploding_run)
    rc = cli.main(
        [
            "--pipeline",
            str(ROOT / "tests" / "fixtures" / "graph_audit" / "clean.dot"),
            "--goal",
            "panic evidence smoke",
            "--backend",
            "echo",
            "--workdir",
            str(tmp_path),
        ]
    )

    assert rc == PANIC_EXIT_CODE
    payload = json.loads(capsys.readouterr().out)
    run_id = payload["run_id"]
    bundle = tmp_path / "evidence" / run_id
    assert payload["evidence_bundle"] == str(bundle)
    assert payload["evidence_bundle_written"] == "true"
    assert pathlib.Path(payload["panic_artifact"]).exists()
    assert str(bundle) in payload["panic_artifact"]
    summary = json.loads((bundle / "summary.json").read_text(encoding="utf-8"))
    assert summary["final_outcome"] == "error"
    assert summary["events_path"] == str(bundle / "events.jsonl")
    assert (bundle / "command.txt").exists()
    assert (bundle / f"cxdb-{run_id}.sqlite").exists()
