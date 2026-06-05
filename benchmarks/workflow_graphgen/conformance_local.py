"""Self-contained conformance evaluator for the smoke fixtures (hello, roman).

The ``conformance`` axis grades the coder's committed artifact against the
feature's acceptance criteria. For the smoke fixtures those criteria are stated
verbatim in the **public goal text** (e.g. "empty name -> 'Hello, World!'",
"4 -> 'IV'", "ValueError outside 1..3999"), so checking them does NOT use the
sealed holdouts repo and does NOT break agent isolation: the coder already knows
the spec; this only verifies the spec was met. It mirrors the
``holdout_evaluator`` contract the harness already exposes —
``(workdir, feature, baseline_ref, head_ref) -> {"pass": int, "total": int}`` —
so a real sealed evaluator can be swapped in for real features unchanged.

Each case is structured data (module, function, args, expected | raises); the
child interpreter calls ``getattr(module, func)(*args)`` — no ``eval`` of the
goal or any model output (ZFC-clean: no text routing / heuristic scoring).
The child prints a single JSON line ``{"pass": p, "total": t}`` (last-JSON-line
contract, identical to the sealed evaluator) which the parent parses.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import textwrap

# Acceptance tables. A case is {func, args, expect} for a value assertion or
# {func, args, raises} for an exception assertion (matched by exception class
# __name__). These encode the PUBLIC behavior in each SMOKE_GOALS string.
_CASES: dict[str, dict] = {
    "hello": {
        "module": "hello",
        "cases": [
            {"func": "hello", "args": ["Ada"], "expect": "Hello, Ada!"},
            {"func": "hello", "args": ["Bob"], "expect": "Hello, Bob!"},
            {"func": "hello", "args": [""], "expect": "Hello, World!"},
            {"func": "hello", "args": ["   "], "expect": "Hello, World!"},
            {"func": "hello", "args": ["World"], "expect": "Hello, World!"},
        ],
    },
    "roman": {
        "module": "roman",
        "cases": [
            {"func": "to_roman", "args": [1], "expect": "I"},
            {"func": "to_roman", "args": [4], "expect": "IV"},
            {"func": "to_roman", "args": [40], "expect": "XL"},
            {"func": "to_roman", "args": [58], "expect": "LVIII"},
            {"func": "to_roman", "args": [1994], "expect": "MCMXCIV"},
            {"func": "to_roman", "args": [3999], "expect": "MMMCMXCIX"},
            {"func": "to_roman", "args": [0], "raises": "ValueError"},
            {"func": "to_roman", "args": [4000], "raises": "ValueError"},
            {"func": "to_roman", "args": [-1], "raises": "ValueError"},
            {"func": "to_roman", "args": [1.5], "raises": "ValueError"},
            {"func": "to_roman", "args": ["X"], "raises": "ValueError"},
        ],
    },
}


def feature_total(feature: str) -> int | None:
    """Number of acceptance cases for ``feature``, or None when unsupported."""
    spec = _CASES.get(feature)
    return len(spec["cases"]) if spec else None


# Child harness: imports the module from the workdir (cwd) and runs each case in
# isolation, counting passes. Kept as a string so it runs in a clean subprocess
# (a crashing/​hanging coder artifact cannot take down the benchmark process).
_CHILD = textwrap.dedent(
    """
    import importlib, json, sys
    spec = json.loads(sys.argv[1])
    sys.path.insert(0, ".")
    passed = 0
    total = len(spec["cases"])
    try:
        mod = importlib.import_module(spec["module"])
    except Exception as exc:  # module missing / import error -> 0/total
        print(json.dumps({"pass": 0, "total": total, "error": f"import: {exc!r}"}))
        raise SystemExit(0)
    for case in spec["cases"]:
        fn = getattr(mod, case["func"], None)
        if fn is None:
            continue
        try:
            result = fn(*case["args"])
        except Exception as exc:
            if case.get("raises") and type(exc).__name__ == case["raises"]:
                passed += 1
            continue
        if "raises" in case:
            continue  # expected an exception, got a value -> fail
        if result == case["expect"]:
            passed += 1
    print(json.dumps({"pass": passed, "total": total}))
    """
)


def evaluate(workdir, feature: str, baseline_ref: str = "", head_ref: str = "") -> dict:
    """Grade the artifact in ``workdir`` for ``feature``. Returns {pass,total}.

    ``baseline_ref``/``head_ref`` are part of the sealed-evaluator contract but
    unused here (the current worktree IS the committed middle diff). Raises
    ValueError for an unsupported feature so the harness records conformance
    unavailable rather than a misleading 0/0.
    """
    spec = _CASES.get(feature)
    if spec is None:
        raise ValueError(f"no local conformance table for feature {feature!r}")
    proc = subprocess.run(
        [sys.executable, "-c", _CHILD, json.dumps(spec)],
        cwd=str(workdir),
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    last = ""
    for line in proc.stdout.splitlines():
        line = line.strip()
        if line.startswith("{") and line.endswith("}"):
            last = line
    if not last:
        return {"pass": 0, "total": spec and len(spec["cases"]) or 0,
                "error": f"no json from child (rc={proc.returncode}): {proc.stderr[-200:]!r}"}
    data = json.loads(last)
    return {"pass": int(data.get("pass", 0)), "total": int(data.get("total", len(spec["cases"])))}
