#!/usr/bin/env python3
"""
Score Candidate - Score a benchmark run against the evaluation rubric

Usage: python score_candidate.py <workdir> [--output <score.json>]

Scores (100 points total):
  - build (10 pts): make build succeeds
  - self_tests (10 pts): make test passes
  - holdouts (35 pts): % passed scenarios
  - edge_cases (15 pts): >=8 scenarios = 15, else scaled
  - evidence (10 pts): has video = 10, else 5
  - iteration (10 pts): based on fix node count
  - cost (10 pts): placeholder 8
"""

import argparse
import json
import os
import sqlite3
import sys
from pathlib import Path
from typing import Any


def score_build(workdir: Path) -> tuple[int, str]:
    """Score build step: 10 pts if make build succeeds"""
    makefile = workdir / "Makefile"
    if not makefile.exists():
        return 0, "No Makefile found"

    result_file = workdir / "results" / "build_result.json"
    if result_file.exists():
        try:
            data = json.loads(result_file.read_text())
            if data.get("status") == "success":
                return 10, "Build succeeded"
        except (json.JSONDecodeError, KeyError):
            pass

    return 5, "Build status unknown (partial score)"


def score_self_tests(workdir: Path) -> tuple[int, str]:
    """Score self-tests: 10 pts if make test passes"""
    result_file = workdir / "results" / "test_result.json"
    if result_file.exists():
        try:
            data = json.loads(result_file.read_text())
            if data.get("status") == "success":
                return 10, "Tests passed"
        except (json.JSONDecodeError, KeyError):
            pass

    return 0, "Tests not run or failed"


def score_holdouts(workdir: Path) -> tuple[int, str]:
    """Score holdout scenarios: 35 pts for 100% pass rate"""
    results_dir = workdir / "results"

    # Look for holdout results
    holdout_results = results_dir / "holdout_results.json"
    if holdout_results.exists():
        try:
            data = json.loads(holdout_results.read_text())
            passed = data.get("passed", 0)
            total = data.get("total", 0)
            if total > 0:
                pct = (passed / total) * 100
                score = int((passed / total) * 35)
                return score, f"Holdouts: {passed}/{total} ({pct:.1f}%)"
        except (json.JSONDecodeError, KeyError, ZeroDivisionError):
            pass

    return 0, "No holdout results found"


def score_edge_cases(workdir: Path) -> tuple[int, str]:
    """Score edge cases: 15 pts if >=8 scenarios, scaled below"""
    results_dir = workdir / "results"

    edge_results = results_dir / "edge_case_results.json"
    if edge_results.exists():
        try:
            data = json.loads(edge_results.read_text())
            scenarios = data.get("scenarios_tested", 0)
            if scenarios >= 8:
                return 15, f"Edge cases: {scenarios} scenarios (full score)"
            else:
                score = int((scenarios / 8) * 15)
                return score, f"Edge cases: {scenarios} scenarios (scaled)"
        except (json.JSONDecodeError, KeyError):
            pass

    return 0, "No edge case results found"


def score_evidence(workdir: Path) -> tuple[int, str]:
    """Score evidence: 10 pts if video exists, else 5"""
    results_dir = workdir / "results"

    # Look for video evidence
    video_patterns = ["*.mp4", "*.gif", "*.webm"]
    for pattern in video_patterns:
        videos = list(results_dir.glob(pattern))
        if videos:
            return 10, f"Video evidence found: {videos[0].name}"

    # Check for screenshot evidence
    screenshot_patterns = ["*.png", "*.jpg", "*.jpeg"]
    for pattern in screenshot_patterns:
        screenshots = list(results_dir.glob(pattern))
        if screenshots:
            return 7, f"Screenshot evidence found: {screenshots[0].name}"

    return 5, "No video evidence"


def score_iteration(workdir: Path) -> tuple[int, str]:
    """Score iteration efficiency: based on fix node count in CXDB"""
    results_dir = workdir / "results"
    cxdb_path = results_dir / "cxdb.sqlite"

    if not cxdb_path.exists():
        return 5, "No CXDB found (partial score)"

    try:
        conn = sqlite3.connect(str(cxdb_path))
        cursor = conn.cursor()

        # Count fix nodes (iteration attempts)
        cursor.execute("""
            SELECT COUNT(*) FROM events
            WHERE node LIKE '%fix%' OR node LIKE '%retry%'
        """)
        fix_count = cursor.fetchone()[0]
        conn.close()

        # Higher fix count = lower score (less efficient)
        if fix_count == 0:
            return 10, "No fix iterations (optimal)"
        elif fix_count <= 2:
            return 8, f"Low iteration: {fix_count} fixes"
        elif fix_count <= 5:
            return 5, f"Medium iteration: {fix_count} fixes"
        else:
            return 2, f"High iteration: {fix_count} fixes"

    except (sqlite3.Error, KeyError):
        return 5, "CXDB read error (partial score)"


def score_cost() -> tuple[int, str]:
    """Score cost efficiency: placeholder at 8 pts"""
    return 8, "Placeholder score"


def load_cxdb_summary(cxdb_path: Path) -> dict[str, Any]:
    """Load summary stats from CXDB"""
    if not cxdb_path.exists():
        return {}

    try:
        conn = sqlite3.connect(str(cxdb_path))
        cursor = conn.cursor()

        # Get event counts by node
        cursor.execute("""
            SELECT node, COUNT(*) as count
            FROM events
            GROUP BY node
        """)
        node_counts = {row[0]: row[1] for row in cursor.fetchall()}

        # Get total events
        cursor.execute("SELECT COUNT(*) FROM events")
        total_events = cursor.fetchone()[0]

        conn.close()

        return {
            "node_counts": node_counts,
            "total_events": total_events
        }
    except sqlite3.Error:
        return {}


def main():
    parser = argparse.ArgumentParser(description="Score benchmark run")
    parser.add_argument("workdir", type=Path, help="Work directory path")
    parser.add_argument("--output", type=Path, help="Output JSON file")
    args = parser.parse_args()

    workdir = args.workdir
    if not workdir.exists():
        print(f"ERROR: Workdir not found: {workdir}", file=sys.stderr)
        sys.exit(1)

    results_dir = workdir / "results"
    results_dir.mkdir(exist_ok=True)

    # Run all scoring functions
    scores = {}

    scores["build"] = {"score": 0, "max": 10, "detail": ""}
    scores["self_tests"] = {"score": 0, "max": 10, "detail": ""}
    scores["holdouts"] = {"score": 0, "max": 35, "detail": ""}
    scores["edge_cases"] = {"score": 0, "max": 15, "detail": ""}
    scores["evidence"] = {"score": 0, "max": 10, "detail": ""}
    scores["iteration"] = {"score": 0, "max": 10, "detail": ""}
    scores["cost"] = {"score": 0, "max": 10, "detail": ""}

    # Build score
    s, d = score_build(workdir)
    scores["build"]["score"] = s
    scores["build"]["detail"] = d

    # Self-tests score
    s, d = score_self_tests(workdir)
    scores["self_tests"]["score"] = s
    scores["self_tests"]["detail"] = d

    # Holdouts score
    s, d = score_holdouts(workdir)
    scores["holdouts"]["score"] = s
    scores["holdouts"]["detail"] = d

    # Edge cases score
    s, d = score_edge_cases(workdir)
    scores["edge_cases"]["score"] = s
    scores["edge_cases"]["detail"] = d

    # Evidence score
    s, d = score_evidence(workdir)
    scores["evidence"]["score"] = s
    scores["evidence"]["detail"] = d

    # Iteration score
    s, d = score_iteration(workdir)
    scores["iteration"]["score"] = s
    scores["iteration"]["detail"] = d

    # Cost score
    s, d = score_cost()
    scores["cost"]["score"] = s
    scores["cost"]["detail"] = d

    # Calculate totals
    total_score = sum(s["score"] for s in scores.values())
    max_score = sum(s["max"] for s in scores.values())

    # Build result
    result = {
        "workdir": str(workdir),
        "scores": scores,
        "total_score": total_score,
        "max_score": max_score,
        "percentage": round((total_score / max_score) * 100, 1) if max_score > 0 else 0
    }

    # Add CXDB summary if available
    cxdb_path = results_dir / "cxdb.sqlite"
    if cxdb_path.exists():
        result["cxdb_summary"] = load_cxdb_summary(cxdb_path)

    # Output
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, indent=2))
        print(f"Score saved to: {args.output}")
    else:
        print(json.dumps(result, indent=2))

    # Print summary
    print("\n" + "=" * 50)
    print("SCORE SUMMARY")
    print("=" * 50)
    for category, data in scores.items():
        pct = (data["score"] / data["max"]) * 100 if data["max"] > 0 else 0
        print(f"  {category:15} {data['score']:2}/{data['max']:2} ({pct:5.1f}%) - {data['detail']}")
    print("=" * 50)
    print(f"  {'TOTAL':15} {total_score:2}/{max_score:2} ({result['percentage']:.1f}%)")
    print("=" * 50)

    return 0


if __name__ == "__main__":
    sys.exit(main())