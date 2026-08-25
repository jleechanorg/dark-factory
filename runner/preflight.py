"""CLI backend preflight check for Dark Factory.

Runs *before* the runner Python code via the bash wrappers in ``bin/``.
Probes the configured backend and a small set of alternates via
``shutil.which`` (no version probes) and reports a structured JSON
status consumable by callers and humans.

States
------
- ``pass``  configured backend is present (or backend is ``echo``)
- ``warn``  configured backend missing but at least one other CLI present
- ``fail``  zero non-echo backends reachable — hard-stop with exit 2

Exit codes
----------
- ``0``  pass or warn (warn prints a one-line warning, then continues)
- ``2``  fail (zero reachable backends)

The module is runnable as ``python -m runner.preflight --backend claude --json``
so the bash wrappers can capture structured output.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import sys
from typing import Any, Optional


# Backends we probe. ``echo`` is always considered available — it is
# the no-LLM fallback built into the runner.
PROBED_BACKENDS = ("claude", "codex", "agy", "ao", "echo")

# Transitive deps: if a backend is configured, also check these.
# Currently only ``ao`` requires ``sandbox-exec`` (macOS seatbelt).
TRANSITIVE_DEPS = {
    "ao": ("sandbox-exec",),
}

# Priority order for fallback_recommendation when the configured backend
# is missing. The first CLI in this tuple that resolves to a real path
# wins. ``echo`` is the final always-present fallback.
FALLBACK_PRIORITY = ("codex", "claude", "agy", "ao", "echo")

# Install hint shown for each backend when missing.
HINTS = {
    "claude": "Install: npm install -g @anthropics/claude-code",
    "codex": "Install: npm install -g @openai/codex",
    "agy": "Install: see https://github.com/jleechanorg/agent-orchestrator",
    "ao": "Install: see Agent Orchestrator setup docs",
    "sandbox-exec": "macOS-only; built-in",
}


def _probe(name: str) -> Optional[str]:
    """Return resolved PATH for ``name`` or None if missing.

    Imports ``shutil`` inside the function so tests can monkeypatch
    ``runner.preflight.shutil.which`` to control availability.
    """
    import shutil as _shutil

    return _shutil.which(name)


def _check_holdout_scenarios(
    feature: str | None,
    holdouts_path: pathlib.Path | None = None,
) -> tuple[bool, str | None]:
    """Check if holdout scenarios.yaml exists for a given feature."""
    if not feature or not str(feature).strip():
        return False, "feature name is required when require_holdouts is True"
    feature_name = str(feature).strip()
    if holdouts_path is None:
        import os
        repo = os.environ.get(
            "DARK_FACTORY_HOLDOUTS",
            str(pathlib.Path.home() / "projects" / "dark-factory-holdouts"),
        )
        holdouts_path = pathlib.Path(repo).expanduser().resolve()
    else:
        holdouts_path = pathlib.Path(holdouts_path).expanduser().resolve()
    if not holdouts_path.is_dir():
        return False, f"Sealed holdouts repo not found at {holdouts_path}"
    scenarios_yaml = holdouts_path / "holdouts" / feature_name / "scenarios.yaml"
    scenarios_yml = holdouts_path / "holdouts" / feature_name / "scenarios.yml"
    if scenarios_yaml.is_file() or scenarios_yml.is_file():
        return True, None
    return False, f"no holdout scenarios found for feature '{feature_name}' at {scenarios_yaml}"


def preflight_check(
    backend: str,
    workdir: pathlib.Path | None = None,
    feature: str | None = None,
    require_holdouts: bool = False,
    holdouts_path: pathlib.Path | None = None,
) -> dict:
    """Probe the configured backend and alternates; return structured status.

    Parameters
    ----------
    backend:
        Name of the configured backend (one of ``PROBED_BACKENDS`` or any
        user-supplied string). ``echo`` is always ``ok``.
    workdir:
        Reserved for future per-workdir probing. Currently unused; included
        in the API so the signature matches the spec and the bash wrapper
        can pass ``--workdir`` if it ever wants to.
    feature:
        Optional feature name to check holdout scenarios for.
    require_holdouts:
        If True, validates that scenarios.yaml exists for the specified feature
        and fails preflight if absent.
    holdouts_path:
        Optional override for holdouts directory.

    Returns
    -------
    dict with keys:
        ``status``           ("pass" | "warn" | "fail")
        ``configured``       the backend the caller asked about
        ``configured_ok``    bool — whether the configured backend resolved
        ``backends``         dict[cli] -> {"ok": bool, "path": str|None, "hint": str|None}
        ``transitive``       dict[cli] -> {"ok": bool, ...} for transitive deps
        ``holdouts``         dict -> {"required": bool, "feature": str|None, "ok": bool, "error": str|None}
        ``fallback_recommendation`` first available CLI in FALLBACK_PRIORITY
        ``message``          human-readable summary line
    """
    workdir = workdir or pathlib.Path.cwd()
    del workdir  # currently unused; keep the parameter for future expansion

    # Normalize: a backend we don't know about is treated as missing
    # but still appears in the report so the caller can see what was
    # asked for.
    known = backend in PROBED_BACKENDS
    configured_present = (backend == "echo") or _probe(backend) is not None

    backends: dict[str, dict] = {}
    for name in PROBED_BACKENDS:
        if name == "echo":
            backends[name] = {"ok": True, "path": None, "hint": None}
            continue
        path = _probe(name)
        backends[name] = {
            "ok": path is not None,
            "path": path,
            "hint": None if path else HINTS.get(name),
        }

    # Include the configured backend in the report even if it's an
    # unknown name (e.g. user typo) so the caller sees what was asked.
    if not known and backend != "echo":
        path = _probe(backend)
        backends[backend] = {
            "ok": path is not None,
            "path": path,
            "hint": HINTS.get(backend, f"Unknown backend {backend!r}"),
        }

    transitive: dict[str, dict] = {}
    for dep in TRANSITIVE_DEPS.get(backend, ()):
        path = _probe(dep)
        transitive[dep] = {
            "ok": path is not None,
            "path": path,
            "hint": None if path else HINTS.get(dep),
        }

    # Determine status.
    if backend == "echo" or configured_present:
        status = "pass"
    else:
        # At least one non-echo backend present AND all transitive deps OK?
        any_alt = any(
            info["ok"] for name, info in backends.items() if name != "echo"
        )
        deps_ok = all(info["ok"] for info in transitive.values())
        if any_alt and deps_ok:
            status = "warn"
        else:
            # No usable backend — could be missing only the configured one
            # (warn) or zero reachables (fail). Distinguish by looking at
            # non-echo availability.
            any_non_echo = any(
                info["ok"] for name, info in backends.items() if name != "echo"
            )
            if any_non_echo:
                # We have a non-echo CLI but its transitive dep is missing —
                # the only path that would use the configured backend is dead,
                # but the user can fall back. Still warn, not fail.
                status = "warn"
            else:
                status = "fail"

    # Pick fallback: first available in priority order. If the configured
    # backend is present, prefer it; otherwise pick the first FALLBACK_PRIORITY
    # entry that resolves.
    if backend == "echo" or configured_present:
        fallback = backend if backend == "echo" else backend
    else:
        fallback = "echo"
        for cand in FALLBACK_PRIORITY:
            if cand == backend:
                continue
            info = backends.get(cand)
            if info and info["ok"]:
                fallback = cand
                break

    if status == "pass":
        if backend == "echo":
            message = "echo backend: always available"
        else:
            message = f"{backend}: ok"
    elif status == "warn":
        message = (
            f"{backend} missing; fallback to {fallback}"
        )
    else:
        message = "no backends reachable"

    holdouts_info: dict[str, Any] = {
        "required": bool(require_holdouts),
        "feature": feature,
        "ok": True,
        "error": None,
    }
    if require_holdouts:
        h_ok, h_err = _check_holdout_scenarios(feature, holdouts_path=holdouts_path)
        holdouts_info["ok"] = h_ok
        holdouts_info["error"] = h_err
        if not h_ok:
            status = "fail"
            message = h_err or "holdout scenarios missing"

    return {
        "status": status,
        "configured": backend,
        "configured_ok": configured_present,
        "backends": backends,
        "transitive": transitive,
        "holdouts": holdouts_info,
        "fallback_recommendation": fallback,
        "message": message,
    }


def main(argv: list[str] | None = None) -> int:
    """CLI entry point. Returns process exit code (0, 2)."""
    p = argparse.ArgumentParser(
        prog="runner.preflight",
        description="Probe configured backend CLI availability and holdouts.",
    )
    p.add_argument(
        "--backend",
        default="echo",
        help="Configured backend name (default: echo)",
    )
    p.add_argument(
        "--workdir",
        type=pathlib.Path,
        default=None,
        help="Reserved for future per-workdir probing",
    )
    p.add_argument(
        "--feature",
        default=None,
        help="Feature name to check holdout scenarios for",
    )
    p.add_argument(
        "--require-holdouts",
        action="store_true",
        help="Fail fast if holdout scenarios.yaml does not exist for the feature",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Emit JSON to stdout (default: human-readable summary)",
    )
    args = p.parse_args(argv)

    result = preflight_check(
        args.backend,
        args.workdir,
        feature=args.feature,
        require_holdouts=args.require_holdouts,
    )

    if args.json:
        json.dump(result, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
    else:
        print(result["message"])

    status = result["status"]
    if status == "fail":
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
