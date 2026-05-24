#!/usr/bin/env python3
"""Line-aware NLSpec validator for Attractor-style public specs."""

from __future__ import annotations

import argparse
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


COMMENT_PREFIXES = (
    "<!--",
    "[//]",
    "---",
    "***",
    "___",
)

PLACEHOLDER_TOKENS = (
    "todo",
    "tbd",
    "placeholder",
    "stub",
    "fixme",
    "xxx",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Validate NLSpec line coverage.")
    parser.add_argument(
        "--spec",
        default="spec/feature.md",
        help="Path to the spec file",
    )
    parser.add_argument(
        "--report",
        default="spec_review/validation_report.json",
        help="Destination JSON report path",
    )
    return parser.parse_args()


def is_reviewable(line: str) -> bool:
    text = line.strip()
    if not text:
        return False
    lowered = text.lower()
    if any(lowered.startswith(prefix) for prefix in COMMENT_PREFIXES):
        return False
    return True


def classify_line(line_no: int, line: str) -> dict[str, Any]:
    text = line.rstrip("\n")
    stripped = text.strip()
    lowered = stripped.lower()

    status = "pass"
    reason = "line is reviewable"

    if any(token in lowered for token in PLACEHOLDER_TOKENS):
        status = "fail"
        reason = "contains placeholder/unfinished language"
    elif len(stripped) < 6:
        status = "warn"
        reason = "line is short and may be underspecified"
    elif stripped.startswith("|") and stripped.count("|") < 3:
        status = "warn"
        reason = "table-like fragment with low structure"
    return {
        "line": line_no,
        "status": status,
        "text": stripped,
        "text_sha256": hashlib.sha256(stripped.encode("utf-8")).hexdigest(),
        "reason": reason,
    }


def build_report(spec_path: Path, report_path: Path) -> tuple[dict[str, Any], bool]:
    lines = spec_path.read_text(encoding="utf-8").splitlines()
    checks = []
    missing_lines: list[int] = []
    issues: list[str] = []
    total_lines = 0

    for idx, line in enumerate(lines, start=1):
        if not line.strip():
            continue
        total_lines += 1
        if is_reviewable(line):
            check = classify_line(idx, line)
            checks.append(check)
            if check["status"] == "fail":
                issues.append(f"line {idx}: {check['reason']}")
            elif check["status"] == "warn":
                issues.append(f"line {idx}: {check['reason']}")
        else:
            missing_lines.append(idx)

    reviewable_lines = len(checks)
    coverage_reviewable_ratio = reviewable_lines / total_lines if total_lines else 0.0
    has_fail = any(check["status"] == "fail" for check in checks)
    verdict = "pass" if (not has_fail and coverage_reviewable_ratio >= 0.90) else "fail"

    report = {
        "verdict": verdict,
        "coverage": {
            "total_lines": total_lines,
            "reviewable_lines": reviewable_lines,
            "covered_lines": len(checks),
            "missing_lines": missing_lines,
            "reviewable_ratio": round(coverage_reviewable_ratio, 4),
        },
        "line_checks": checks,
        "issues": issues,
        "generated_at": datetime.now(timezone.utc).isoformat(),
    }

    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2), encoding="utf-8")
    return report, verdict == "pass"


def main() -> int:
    args = parse_args()
    spec_path = Path(args.spec)
    report_path = Path(args.report)

    if not spec_path.exists():
        report = {
            "verdict": "fail",
            "coverage": {
                "total_lines": 0,
                "reviewable_lines": 0,
                "covered_lines": 0,
                "missing_lines": [],
                "reviewable_ratio": 0.0,
            },
            "line_checks": [],
            "issues": [f"missing spec file: {spec_path}"],
            "generated_at": datetime.now(timezone.utc).isoformat(),
        }
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps(report, indent=2))
        return 1

    report, ok = build_report(spec_path, report_path)
    print(json.dumps(report))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
