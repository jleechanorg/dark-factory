#!/usr/bin/env python3
"""Deterministic redacted scorer for the Fibonacci benchmark."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
from dataclasses import dataclass


@dataclass(frozen=True)
class Case:
    name: str
    args: list[str]
    expected_stdout: str | None
    should_pass: bool


CASES = [
    Case("base-zero", ["0"], "0\n", True),
    Case("base-one", ["1"], "1\n", True),
    Case("small", ["7"], "13\n", True),
    Case("medium", ["20"], "6765\n", True),
    Case("large", ["50"], "12586269025\n", True),
    Case("negative", ["-5"], None, False),
    Case("text", ["not-an-int"], None, False),
    Case("missing", [], None, False),
]


def run_case(candidate: pathlib.Path, case: Case) -> bool:
    proc = subprocess.run(
        [sys.executable, "fib.py", *case.args],
        cwd=candidate,
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    if case.should_pass:
        return proc.returncode == 0 and proc.stdout == case.expected_stdout
    return proc.returncode != 0 and bool(proc.stderr.strip())


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: score_candidate.py <candidate-dir>", file=sys.stderr)
        return 2
    candidate = pathlib.Path(argv[1]).resolve()
    if not (candidate / "fib.py").exists():
        print(json.dumps({"status": "fail", "sealed": True, "passed": 0, "total": len(CASES)}))
        return 1

    passed = sum(1 for case in CASES if run_case(candidate, case))
    total = len(CASES)
    status = "pass" if passed == total else "fail"
    payload = {
        "benchmark": "fibonacci",
        "status": status,
        "sealed": True,
        "passed": passed,
        "total": total,
    }
    print(json.dumps(payload, indent=2))
    return 0 if status == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

