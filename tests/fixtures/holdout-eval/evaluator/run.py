#!/usr/bin/env python3
"""Stub holdout evaluator for CI/testing when the sealed holdouts repo is absent.

For the 'hello' feature: imports impl/greet.py from --impl dir and verifies
hello() returns "Hello, world!". All other features fail with feature_not_found.

Usage: python3 run.py --feature <name> --impl <path>
Output: one JSON line {"verdict": "pass"|"fail", "scenarios": [...]}
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import pathlib
import sys


def _check_hello(impl_root: pathlib.Path) -> tuple[str, list[dict]]:
    greet_py = impl_root / "impl" / "greet.py"
    if not greet_py.exists():
        greet_py = impl_root / "greet.py"
    if not greet_py.exists():
        return "fail", [{"name": "hello_world", "status": "fail",
                         "error": f"greet.py not found under {impl_root}"}]
    try:
        spec = importlib.util.spec_from_file_location("_greet_stub", greet_py)
        assert spec and spec.loader
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)  # type: ignore[union-attr]
        result = mod.hello()  # type: ignore[attr-defined]
        if result == "Hello, world!":
            return "pass", [{"name": "hello_world", "status": "pass"}]
        return "fail", [{"name": "hello_world", "status": "fail",
                         "error": f"expected 'Hello, world!', got {result!r}"}]
    except Exception as exc:
        return "fail", [{"name": "hello_world", "status": "fail", "error": str(exc)}]


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--feature", default="hello")
    p.add_argument("--impl", default=".")
    args = p.parse_args()

    impl = pathlib.Path(args.impl).resolve()

    if args.feature == "hello":
        verdict, scenarios = _check_hello(impl)
    else:
        verdict = "fail"
        scenarios = [{"name": "feature_not_found", "status": "fail",
                      "error": f"unknown feature: {args.feature!r}"}]

    print(json.dumps({
        "verdict": verdict,
        "scenarios": scenarios,
        "cost": {"api_calls": 0, "tokens": 0},
    }))
    sys.exit(0)


main()
