"""Evidence bundle materialiser (orch-1ouv).

Per-run artifact directory that mirrors the manually-curated `/es` bundles.
Layout:

    <bundle>/
        manifest.json          # run summary + qhez metric totals
        README.md              # human-readable summary table
        summary.json           # explicit run summary artifact
        command.txt            # command echo used by the invocation
        node_io.jsonl          # per-node I/O and log references
        pipeline.dot           # verbatim copy of the source DOT
        pipeline.dot.sha256    # sidecar checksum
        events.jsonl           # structured JSONL event stream (optional)
        cxdb-<run_id>.sqlite   # single-run extract from the parent CXDB
        steps/
            <seq>-<node>.txt   # per-step output capture

Holdout-evaluator steps are written as redacted verdict JSON only — the raw
evaluator stdout is never copied into the bundle, so scenario content cannot
leak through the bundle into a future implementing-agent context.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import shutil
import sqlite3
import time
from typing import Optional

from ._git import _SHA_RE, _git_rev_parse
from ._slug import safe_slug


_HOLDOUT_NODE_TYPES = {"holdout_eval"}
# Conservative redaction: only the verdict + scenario-pass-count fields are
# allowed through. Anything else is dropped to preserve the seal.
_HOLDOUT_ALLOWED_KEYS = {"verdict", "passed", "total", "outcome"}

# Sentinel substring guarding against accidental leakage of holdout repo
# paths into bundle artifacts. The asserter in tests greps for this.
_HOLDOUT_PATH_TOKEN = "dark-factory-holdouts"


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _dark_factory_head_sha(workdir: pathlib.Path) -> Optional[str]:
    """Best-effort: read the parent dark-factory repo HEAD. Returns None if git
    is unavailable, `workdir` is not a git checkout, or the output is not a
    valid 40-hex SHA."""
    sha = _git_rev_parse(workdir, "HEAD", timeout=10)
    if sha is None:
        return None
    if _SHA_RE.match(sha.lower()):
        return sha.lower()
    return None


def _sha256_file(path: pathlib.Path) -> Optional[str]:
    try:
        return _sha256(pathlib.Path(path).read_bytes())
    except Exception:
        return None


def _wall_clock_ms(started_ts: object, ended_ts: object) -> Optional[int]:
    if started_ts is None or ended_ts is None:
        return None
    try:
        return int((float(ended_ts) - float(started_ts)) * 1000)
    except (TypeError, ValueError):
        return None


def _iter_step_rows(cxdb_path: pathlib.Path, run_id: str) -> list[dict]:
    conn = sqlite3.connect(str(cxdb_path))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            """
            SELECT seq, node, outcome, ts, output_head, metadata_json
            FROM steps WHERE run_id = ? ORDER BY seq ASC
            """,
            (run_id,),
        ).fetchall()
    finally:
        conn.close()

    out: list[dict] = []
    for r in rows:
        try:
            meta = json.loads(r["metadata_json"] or "{}")
        except (TypeError, ValueError):
            meta = {}
        if not isinstance(meta, dict):
            meta = {}
        out.append(
            {
                "seq": r["seq"],
                "node": r["node"],
                "outcome": r["outcome"],
                "ts": r["ts"],
                "output_head": r["output_head"] or "",
                "metadata": meta,
            }
        )
    return out


def _is_holdout_step(node: str, metadata: dict, all_holdout_nodes: set[str]) -> bool:
    if node in all_holdout_nodes:
        return True
    # Fallback heuristic for older rows without explicit type info.
    return node.startswith("holdout") and ("verdict" in metadata)


def _redact_holdout_output(output: str, metadata: dict) -> str:
    """Emit only the allowlisted fields. Never copy raw evaluator stdout."""
    payload = {k: metadata.get(k) for k in _HOLDOUT_ALLOWED_KEYS if k in metadata}
    payload["_note"] = (
        "Holdout evaluator output is redacted to preserve the adversarial seal. "
        "Only verdict-level metadata is bundled; the raw scenarios remain in the "
        "sealed holdouts repo."
    )
    return json.dumps(payload, indent=2, sort_keys=True)


def _extract_run(cxdb_path: pathlib.Path, run_id: str, dest: pathlib.Path) -> None:
    """Copy a single run's rows into a fresh SQLite file.

    We open the source read-only and the destination read/write, then re-run
    the schema and `INSERT INTO ... SELECT` filtered by `run_id`. This keeps
    the extract small and avoids leaking other runs that may share the parent
    CXDB.
    """
    # Lazy import — keep CXDB schema co-located with its module.
    from .cxdb import SCHEMA

    if dest.exists():
        dest.unlink()
    out = sqlite3.connect(str(dest))
    try:
        out.executescript(SCHEMA)
        out.execute(
            "ATTACH DATABASE ? AS src",
            (str(cxdb_path),),
        )
        out.execute(
            "INSERT INTO runs SELECT * FROM src.runs WHERE run_id = ?",
            (run_id,),
        )
        out.execute(
            "INSERT INTO steps SELECT * FROM src.steps WHERE run_id = ?",
            (run_id,),
        )
        out.commit()
        out.execute("DETACH DATABASE src")
    finally:
        out.close()


def _summarise_run(
    cxdb_path: pathlib.Path, run_id: str
) -> tuple[list[dict], dict]:
    """Return (steps, totals) for the given run.

    steps: list of {seq, node, outcome, ts, metadata}
    totals: {total_tokens, total_cost_usd, total_wall_ms, started_ts, ended_ts, final_outcome, pipeline}
    """
    conn = sqlite3.connect(str(cxdb_path))
    conn.row_factory = sqlite3.Row
    try:
        run_row = conn.execute(
            "SELECT pipeline, goal, started_ts, ended_ts, final FROM runs WHERE run_id = ?",
            (run_id,),
        ).fetchone()
        step_rows = conn.execute(
            """
            SELECT seq, node, outcome, ts, metadata_json
            FROM steps WHERE run_id = ? ORDER BY seq ASC
            """,
            (run_id,),
        ).fetchall()
    finally:
        conn.close()

    steps: list[dict] = []
    total_tokens = 0
    total_cost = 0.0
    total_wall = 0
    saw_tokens = False
    saw_cost = False
    saw_wall = False

    for r in step_rows:
        try:
            meta = json.loads(r["metadata_json"] or "{}")
        except (TypeError, ValueError):
            meta = {}
        steps.append(
            {
                "seq": r["seq"],
                "node": r["node"],
                "outcome": r["outcome"],
                "ts": r["ts"],
                "metadata": meta,
            }
        )
        for k in ("tokens_in", "tokens_out", "tokens_total"):
            v = meta.get(k)
            if v not in (None, ""):
                try:
                    total_tokens += int(v)
                    saw_tokens = True
                except (TypeError, ValueError):
                    pass
        v = meta.get("cost_usd")
        if v not in (None, ""):
            try:
                total_cost += float(v)
                saw_cost = True
            except (TypeError, ValueError):
                pass
        v = meta.get("wall_ms")
        if v not in (None, ""):
            try:
                total_wall += int(v)
                saw_wall = True
            except (TypeError, ValueError):
                pass

    totals = {
        "pipeline": run_row["pipeline"] if run_row else "",
        "goal": run_row["goal"] if run_row else "",
        "started_ts": run_row["started_ts"] if run_row else None,
        "ended_ts": run_row["ended_ts"] if run_row else None,
        "final_outcome": run_row["final"] if run_row else None,
        "total_tokens": total_tokens if saw_tokens else None,
        "total_cost_usd": total_cost if saw_cost else None,
        "total_wall_ms": total_wall if saw_wall else None,
    }
    return steps, totals


def _holdout_nodes_in_graph(graph) -> set[str]:
    out: set[str] = set()
    for name, node in graph.nodes.items():
        if node.attrs.get("type") in _HOLDOUT_NODE_TYPES:
            out.add(name)
    return out


def _collect_node_sidecars(
    steps: list[dict],
    event_log_path: Optional[pathlib.Path],
    run_id: str,
) -> list[dict]:
    event_path = str(event_log_path) if event_log_path else None
    run_log_path = pathlib.Path.home() / ".dark-factory" / "logs" / f"{run_id}.log"
    return [
        {
            "seq": step["seq"],
            "node": step["node"],
            "outcome": step["outcome"],
            "ts": step["ts"],
            "io_refs": {
                k: str(v)
                for k, v in step["metadata"].items()
                if isinstance(k, str) and (k.endswith("_path") or k.endswith("_sha256")) and v
            },
            "log_refs": {
                "events": event_path,
                "run_log": str(run_log_path) if run_log_path.exists() else None,
            },
        }
        for step in steps
    ]


def _write_step_files(
    bundle_dir: pathlib.Path,
    cxdb_path: pathlib.Path,
    run_id: str,
    steps: list[dict],
    holdout_nodes: set[str],
) -> None:
    steps_dir = bundle_dir / "steps"
    steps_dir.mkdir(parents=True, exist_ok=True)

    # Pull full output_head per step for the per-file artifact.
    by_seq = {row["seq"]: row for row in _iter_step_rows(cxdb_path, run_id)}

    for step in steps:
        seq = step["seq"]
        node = step["node"]
        meta = step["metadata"]
        row = by_seq.get(seq)
        if row is None:
            continue
        raw_output = row["output_head"] or ""
        if _is_holdout_step(node, meta, holdout_nodes):
            content = _redact_holdout_output(raw_output, meta)
        else:
            content = raw_output
        fname = f"{seq:03d}-{safe_slug(node, fallback='node')}.txt"
        (steps_dir / fname).write_text(content)


def _copy_events(event_log_path: Optional[pathlib.Path], bundle_dir: pathlib.Path) -> Optional[pathlib.Path]:
    if not event_log_path:
        return None
    source = pathlib.Path(event_log_path)
    if not source.exists():
        return None
    target = bundle_dir / "events.jsonl"
    try:
        shutil.copy2(source, target)
    except Exception:
        return None
    return target


def _readme(
    pipeline_name: str,
    run_id: str,
    totals: dict,
    steps: list[dict],
) -> str:
    lines = [
        f"# Dark Factory Evidence Bundle",
        "",
        f"- run_id: `{run_id}`",
        f"- pipeline: `{pipeline_name}`",
        f"- goal: {totals.get('goal', '')!r}",
        f"- final_outcome: `{totals.get('final_outcome')}`",
        f"- total_tokens: {totals.get('total_tokens')}",
        f"- total_cost_usd: {totals.get('total_cost_usd')}",
        f"- total_wall_ms: {totals.get('total_wall_ms')}",
        "",
        "## Steps",
        "",
        "| seq | node | outcome | wall_ms | tokens_in | tokens_out |",
        "|----:|------|---------|--------:|----------:|-----------:|",
    ]
    for s in steps:
        meta = s["metadata"]
        lines.append(
            "| {seq} | `{node}` | `{outcome}` | {wall} | {ti} | {to} |".format(
                seq=s["seq"],
                node=s["node"],
                outcome=s["outcome"],
                wall=meta.get("wall_ms", ""),
                ti=meta.get("tokens_in", ""),
                to=meta.get("tokens_out", ""),
            )
        )
    return "\n".join(lines) + "\n"


def write_bundle(
    bundle_dir: pathlib.Path,
    cxdb_path: pathlib.Path,
    run_id: str,
    pipeline_path: pathlib.Path,
    graph,
    workdir: pathlib.Path,
    command: str | None = None,
    event_log_path: Optional[pathlib.Path] = None,
) -> dict:
    """Materialise the evidence bundle for `run_id` under `bundle_dir`.

    Returns the manifest dict that was written, so callers / tests can assert
    on it without re-reading the JSON file.
    """
    bundle_dir = pathlib.Path(bundle_dir).expanduser()
    bundle_dir.mkdir(parents=True, exist_ok=True)

    # 1. Copy the pipeline DOT verbatim + sha256 sidecar.
    dot_bytes = pathlib.Path(pipeline_path).read_bytes()
    (bundle_dir / "pipeline.dot").write_bytes(dot_bytes)
    (bundle_dir / "pipeline.dot.sha256").write_text(_sha256(dot_bytes) + "\n")

    # 2. Single-run CXDB extract.
    extract_path = bundle_dir / f"cxdb-{run_id}.sqlite"
    _extract_run(cxdb_path, run_id, extract_path)

    # 3. Summarise the run for manifest + README + step files.
    steps, totals = _summarise_run(cxdb_path, run_id)
    command_text = command or ""
    (bundle_dir / "command.txt").write_text(command_text + ("\n" if command_text else ""))

    # A successful run must have reached an exit node. Preserve explicit
    # terminal outcomes like exhausted/error so the bundle matches the runner.
    from .parser import is_exit_node
    has_exit = False
    if graph is not None and hasattr(graph, "nodes"):
        for step in steps:
            node_name = step.get("node")
            if node_name in graph.nodes:
                if is_exit_node(graph.nodes[node_name]):
                    has_exit = True
                    break
    if not has_exit and totals.get("final_outcome") == "success":
        totals["final_outcome"] = "failure"

    holdout_nodes = _holdout_nodes_in_graph(graph)
    _write_step_files(bundle_dir, cxdb_path, run_id, steps, holdout_nodes)
    events_path = _copy_events(event_log_path, bundle_dir)
    node_io = _collect_node_sidecars(steps, events_path, run_id)
    (bundle_dir / "node_io.jsonl").write_text(
        "\n".join(json.dumps(item, sort_keys=True) for item in node_io) + ("\n" if node_io else "")
    )

    cxdb_sha = _sha256_file(cxdb_path)
    extract_sha = _sha256_file(extract_path)
    wall_clock = _wall_clock_ms(totals.get("started_ts"), totals.get("ended_ts"))
    summary = {
        "run_id": run_id,
        "pipeline_path": str(pipeline_path),
        "pipeline_name": getattr(graph, "name", ""),
        "goal": totals.get("goal", ""),
        "command": command_text,
        "started_ts": totals.get("started_ts"),
        "ended_ts": totals.get("ended_ts"),
        "wall_clock_ms": wall_clock,
        "final_outcome": totals.get("final_outcome"),
        "exit_reached": has_exit,
        "steps": len(steps),
        "cxdb_path": str(cxdb_path),
        "cxdb_sha256": cxdb_sha,
        "extract_path": str(extract_path),
        "extract_sha256": extract_sha,
        "node_io_path": "node_io.jsonl",
        "events_path": str(events_path) if events_path is not None else None,
    }
    (bundle_dir / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")

    manifest = {
        "run_id": run_id,
        "pipeline_name": getattr(graph, "name", ""),
        "pipeline_path": str(pipeline_path),
        "pipeline_copy": "pipeline.dot",
        "goal": totals.get("goal", ""),
        "command": command_text,
        "command_path": "command.txt",
        "started_ts": totals.get("started_ts"),
        "ended_ts": totals.get("ended_ts"),
        "wall_clock_ms": wall_clock,
        "final_outcome": totals.get("final_outcome"),
        "exit_reached": has_exit,
        "steps": len(steps),
        "events_path": str(events_path) if events_path is not None else None,
        "summary_path": "summary.json",
        "node_io_path": "node_io.jsonl",
        "cxdb_path": str(cxdb_path),
        "cxdb_sha256": cxdb_sha,
        "cxdb_extract_path": str(extract_path),
        "cxdb_extract_sha256": extract_sha,
        "dark_factory_head_sha": _dark_factory_head_sha(workdir),
        "total_tokens": totals.get("total_tokens"),
        "total_cost_usd": totals.get("total_cost_usd"),
        "total_wall_ms": totals.get("total_wall_ms"),
        "generated_ts": time.time(),
    }
    (bundle_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    )
    (bundle_dir / "README.md").write_text(
        _readme(manifest["pipeline_name"], run_id, totals, steps)
    )
    return manifest
