#!/usr/bin/env python3
"""Score an Amazon-clone benchmark run from concrete artifacts."""

from __future__ import annotations

import argparse
import json
import sqlite3
import subprocess
import sys
from pathlib import Path
from typing import Any


SCORE_MAX = {
    "build": 10,
    "self_tests": 10,
    "holdouts": 35,
    "edge_cases": 15,
    "evidence": 10,
    "iteration": 10,
    "cost": 10,
}


def _write_result(path: Path, proc: subprocess.CompletedProcess[str]) -> None:
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps({
        "status": "success" if proc.returncode == 0 else "failure",
        "returncode": proc.returncode,
        "stdout_tail": proc.stdout[-4000:],
        "stderr_tail": proc.stderr[-4000:],
    }, indent=2))


def score_build(workdir: Path) -> tuple[int, str]:
    if not (workdir / "Makefile").exists():
        return 0, "No Makefile found"
    proc = subprocess.run(
        ["make", "build"],
        cwd=workdir,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    _write_result(workdir / "results" / "build_result.json", proc)
    if proc.returncode == 0:
        return SCORE_MAX["build"], "Build succeeded"
    return 0, f"Build failed with rc={proc.returncode}"


def score_self_tests(workdir: Path) -> tuple[int, str]:
    if not (workdir / "Makefile").exists():
        return 0, "No Makefile found"
    proc = subprocess.run(
        ["make", "test"],
        cwd=workdir,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    _write_result(workdir / "results" / "test_result.json", proc)
    if proc.returncode == 0:
        return SCORE_MAX["self_tests"], "Tests passed"
    return 0, f"Tests failed with rc={proc.returncode}"


def _load_holdouts(workdir: Path) -> dict[str, Any] | None:
    path = workdir / "results" / "holdout_results.json"
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text())
    except json.JSONDecodeError:
        return None
    if "scenarios" in data:
        return {"leak": True}
    return data


def score_holdouts(workdir: Path) -> tuple[int, str]:
    data = _load_holdouts(workdir)
    if data is None:
        return 0, "No redacted holdout results found"
    if data.get("leak"):
        return 0, "Holdout results leaked per-scenario data"
    passed = int(data.get("passed", 0))
    total = int(data.get("total", 0))
    if total <= 0:
        return 0, "Holdout total missing"
    score = int((passed / total) * SCORE_MAX["holdouts"])
    return score, f"Holdouts: {passed}/{total}"


def score_edge_cases(workdir: Path) -> tuple[int, str]:
    data = _load_holdouts(workdir)
    if not data or data.get("leak"):
        return 0, "No redacted holdout coverage found"
    passed = int(data.get("passed", 0))
    total = int(data.get("total", 0))
    if total <= 0:
        return 0, "Holdout total missing"
    score = int((passed / total) * SCORE_MAX["edge_cases"])
    return score, f"Redacted holdout coverage: {passed}/{total}"


def score_evidence(workdir: Path) -> tuple[int, str]:
    results_dir = workdir / "results"
    for pattern in ("*.mp4", "*.gif", "*.webm"):
        videos = list(results_dir.glob(pattern))
        if videos:
            return SCORE_MAX["evidence"], f"Video evidence found: {videos[0].name}"
    for pattern in ("*.png", "*.jpg", "*.jpeg"):
        screenshots = list(results_dir.glob(pattern))
        if screenshots:
            return 7, f"Screenshot evidence found: {screenshots[0].name}"
    if (results_dir / "evidence.md").exists():
        return 5, "Text evidence found"
    return 0, "No evidence artifact found"


def score_iteration(workdir: Path) -> tuple[int, str]:
    cxdb_path = workdir / "results" / "cxdb.sqlite"
    if not cxdb_path.exists():
        return 5, "No CXDB found"
    try:
        conn = sqlite3.connect(str(cxdb_path))
        try:
            fix_count = conn.execute(
                "SELECT COUNT(*) FROM steps WHERE node LIKE '%fix%' OR node LIKE '%retry%'"
            ).fetchone()[0]
        finally:
            conn.close()
    except sqlite3.Error:
        return 5, "CXDB read error"
    if fix_count == 0:
        return SCORE_MAX["iteration"], "No fix iterations"
    if fix_count <= 2:
        return 8, f"Low iteration: {fix_count} fixes"
    if fix_count <= 5:
        return 5, f"Medium iteration: {fix_count} fixes"
    return 2, f"High iteration: {fix_count} fixes"


def score_cost(workdir: Path) -> tuple[int, str]:
    for path in sorted((workdir / "results").glob("*.json")):
        try:
            data = json.loads(path.read_text())
        except json.JSONDecodeError:
            continue
        cost = data.get("cost")
        if not isinstance(cost, dict):
            continue
        api_calls = int(cost.get("api_calls", 0))
        tokens = int(cost.get("tokens", 0))
        if api_calls == 0 and tokens == 0:
            return SCORE_MAX["cost"], "No API cost reported"
        if tokens <= 500_000:
            return 8, f"Cost reported: {api_calls} calls, {tokens} tokens"
        if tokens <= 1_500_000:
            return 5, f"High token cost: {tokens}"
        return 2, f"Very high token cost: {tokens}"
    return 0, "No cost artifact found"


def load_cxdb_summary(cxdb_path: Path) -> dict[str, Any]:
    if not cxdb_path.exists():
        return {}
    try:
        conn = sqlite3.connect(str(cxdb_path))
        try:
            node_counts = dict(conn.execute("SELECT node, COUNT(*) FROM steps GROUP BY node").fetchall())
            total_events = conn.execute("SELECT COUNT(*) FROM steps").fetchone()[0]
        finally:
            conn.close()
        return {"node_counts": node_counts, "total_events": total_events}
    except sqlite3.Error:
        return {}


def main() -> int:
    parser = argparse.ArgumentParser(description="Score benchmark run")
    parser.add_argument("workdir", type=Path, help="Work directory path")
    parser.add_argument("--output", type=Path, help="Output JSON file")
    args = parser.parse_args()

    workdir = args.workdir.resolve()
    if not workdir.exists():
        print(f"ERROR: Workdir not found: {workdir}", file=sys.stderr)
        return 1
    (workdir / "results").mkdir(exist_ok=True)

    scorers = {
        "build": score_build,
        "self_tests": score_self_tests,
        "holdouts": score_holdouts,
        "edge_cases": score_edge_cases,
        "evidence": score_evidence,
        "iteration": score_iteration,
        "cost": score_cost,
    }
    scores: dict[str, dict[str, Any]] = {}
    for name, scorer in scorers.items():
        score, detail = scorer(workdir)
        scores[name] = {"score": score, "max": SCORE_MAX[name], "detail": detail}

    total_score = sum(item["score"] for item in scores.values())
    max_score = sum(item["max"] for item in scores.values())
    result: dict[str, Any] = {
        "workdir": str(workdir),
        "scope": "candidate_quality_score",
        "fairness_note": (
            "This scorer grades one artifact. Fair method comparison requires "
            "the outer harness to normalize model access, runtime, token budget, "
            "retry budget, starter state, and sealed evaluator version."
        ),
        "scores": scores,
        "total_score": total_score,
        "max_score": max_score,
        "percentage": round((total_score / max_score) * 100, 1) if max_score else 0,
    }
    cxdb_summary = load_cxdb_summary(workdir / "results" / "cxdb.sqlite")
    if cxdb_summary:
        result["cxdb_summary"] = cxdb_summary

    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
