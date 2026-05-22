#!/usr/bin/env python3
"""
Summarize Results - Generate leaderboard from all benchmark scores

Usage: python summarize.py <results_dir> [--output <summary.md>]

Loads all score.json files from results directory and generates a markdown table.
"""

import argparse
import json
import sys
from pathlib import Path
from typing import Any


def load_score_files(results_dir: Path) -> list[dict[str, Any]]:
    """Load all score.json files from results subdirectories"""
    scores = []

    for method_dir in results_dir.iterdir():
        if not method_dir.is_dir():
            continue

        for score_file in method_dir.glob("*_score.json"):
            try:
                data = json.loads(score_file.read_text())
                data["_source_file"] = str(score_file)
                data["_method_dir"] = method_dir.name
                scores.append(data)
            except (json.JSONDecodeError, OSError) as e:
                print(f"Warning: Could not load {score_file}: {e}", file=sys.stderr)

    return scores


def parse_score(data: dict[str, Any]) -> dict[str, float]:
    """Extract individual category scores from result data"""
    if "scores" not in data:
        return {}

    return {
        "build": data["scores"].get("build", {}).get("score", 0),
        "self_tests": data["scores"].get("self_tests", {}).get("score", 0),
        "holdouts": data["scores"].get("holdouts", {}).get("score", 0),
        "edge_cases": data["scores"].get("edge_cases", {}).get("score", 0),
        "evidence": data["scores"].get("evidence", {}).get("score", 0),
        "iteration": data["scores"].get("iteration", {}).get("score", 0),
        "cost": data["scores"].get("cost", {}).get("score", 0),
        "total": data.get("total_score", 0),
        "max": data.get("max_score", 100),
    }


def aggregate_by_method(scores: list[dict[str, Any]]) -> dict[str, list[dict[str, float]]]:
    """Group scores by method"""
    by_method: dict[str, list[dict[str, float]]] = {}

    for score in scores:
        method = score.get("_method_dir", "unknown")
        parsed = parse_score(score)

        if method not in by_method:
            by_method[method] = []
        by_method[method].append(parsed)

    return by_method


def calculate_averages(scores: list[dict[str, float]]) -> dict[str, float]:
    """Calculate average scores across runs"""
    if not scores:
        return {}

    categories = ["build", "self_tests", "holdouts", "edge_cases", "evidence", "iteration", "cost", "total"]

    averages = {}
    for cat in categories:
        values = [s.get(cat, 0) for s in scores if cat in s]
        averages[cat] = sum(values) / len(values) if values else 0

    return averages


def generate_table(by_method: dict[str, list[dict[str, float]]]) -> str:
    """Generate markdown leaderboard table"""
    lines = []

    # Header
    lines.append("| Method | Runs | Avg Total | Build | Tests | Holdouts | Edge | Evidence | Iter | Cost |")
    lines.append("|--------|------|----------|-------|-------|----------|------|----------|------|------|")

    # Sort by average total descending
    method_avgs = {}
    for method, scores in by_method.items():
        avgs = calculate_averages(scores)
        method_avgs[method] = avgs.get("total", 0)

    sorted_methods = sorted(method_avgs.items(), key=lambda x: x[1], reverse=True)

    for method, avg_total in sorted_methods:
        scores = by_method[method]
        avgs = calculate_averages(scores)
        run_count = len(scores)

        row = [
            method,
            str(run_count),
            f"{avgs.get('total', 0):.1f}",
            f"{avgs.get('build', 0):.1f}",
            f"{avgs.get('self_tests', 0):.1f}",
            f"{avgs.get('holdouts', 0):.1f}",
            f"{avgs.get('edge_cases', 0):.1f}",
            f"{avgs.get('evidence', 0):.1f}",
            f"{avgs.get('iteration', 0):.1f}",
            f"{avgs.get('cost', 0):.1f}",
        ]
        lines.append("| " + " | ".join(row) + " |")

    return "\n".join(lines)


def generate_failure_modes(scores: list[dict[str, Any]]) -> str:
    """Generate failure modes section (placeholder)"""
    lines = []
    lines.append("\n## Failure Modes\n")
    lines.append("> This section will be populated after benchmark runs complete.\n")
    lines.append("> Failure modes will be auto-generated from CXDB error clustering.\n")
    lines.append("\nPlaceholder: TBD after first benchmark runs.\n")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Summarize benchmark results")
    parser.add_argument("results_dir", type=Path, help="Results directory containing method subdirs")
    parser.add_argument("--output", type=Path, help="Output markdown file")
    args = parser.parse_args()

    results_dir = args.results_dir
    if not results_dir.exists():
        print(f"ERROR: Results directory not found: {results_dir}", file=sys.stderr)
        sys.exit(1)

    # Load all score files
    print(f"Loading scores from: {results_dir}")
    scores = load_score_files(results_dir)
    print(f"Found {len(scores)} score files")

    if not scores:
        print("Warning: No score files found. Generating empty summary.", file=sys.stderr)
        output = "# Benchmark Results\n\nNo results yet.\n"
        if args.output:
            args.output.write_text(output)
            print(f"Summary saved to: {args.output}")
        return 0

    # Aggregate by method
    by_method = aggregate_by_method(scores)

    # Generate report
    lines = []
    lines.append("# Amazon Clone MVP Benchmark Results\n")

    # Timestamp
    from datetime import datetime
    lines.append(f"_Generated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}_\n")

    # Summary stats
    lines.append(f"**Total runs:** {len(scores)}")
    lines.append(f"**Methods tested:** {len(by_method)}\n")

    # Leaderboard table
    lines.append("## Leaderboard\n")
    lines.append(generate_table(by_method))

    # Failure modes section (placeholder)
    lines.append(generate_failure_modes(scores))

    # Detailed breakdown per method
    lines.append("\n## Detailed Results\n")
    for method in sorted(by_method.keys()):
        lines.append(f"### {method}\n")
        scores_list = by_method[method]
        avgs = calculate_averages(scores_list)

        for i, score in enumerate(scores_list, 1):
            lines.append(f"- Run {i}: total={score.get('total', 0):.0f}/100, "
                        f"holdouts={score.get('holdouts', 0):.0f}, "
                        f"edge={score.get('edge_cases', 0):.0f}")

        lines.append(f"- **Average:** {avgs.get('total', 0):.1f}/100\n")

    output = "\n".join(lines)

    # Write output
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(output)
        print(f"\nSummary saved to: {args.output}")
    else:
        print(output)

    return 0


if __name__ == "__main__":
    sys.exit(main())