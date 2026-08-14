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

Configuration (Bead jleechan-ev6m)
---------------------------------
The probed backend list, transitive deps, and fallback priority are
sourced from ``config/backends.json`` (or ``~/.dark-factory/backends.json``)
when available. The legacy constants below are the hardcoded fallback
used when no JSON config exists. See ``runner/backend_config.py`` for the
schema.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import pathlib
import shutil
import sys
from typing import Optional

from . import backend_config


_LOG = logging.getLogger("runner.preflight")

# Legacy defaults — used when no JSON config is present. The JSON-driven
# config (``config/backends.json``) is the canonical source.
LEGACY_PROBED_BACKENDS = ("claude", "codex", "agy", "ao", "echo")

LEGACY_TRANSITIVE_DEPS = {
    "ao": ("sandbox-exec",),
}

LEGACY_FALLBACK_PRIORITY = ("codex", "claude", "agy", "ao", "echo")

LEGACY_HINTS = {
    "claude": "Install: npm install -g @anthropics/claude-code",
    "codex": "Install: npm install -g @openai/codex",
    "agy": "Install: see https://github.com/jleechanorg/agent-orchestrator",
    "ao": "Install: see Agent Orchestrator setup docs",
    "sandbox-exec": "macOS-only; built-in",
}


def _load_config_or_default() -> dict | None:
    """Try to load JSON config; return ``None`` if no config found."""
    try:
        return backend_config.load_with_precedence()
    except FileNotFoundError:
        return None
    except Exception as exc:  # pragma: no cover - defensive
        _LOG.warning("backend_config load failed: %s; using legacy defaults", exc)
        return None


def _resolve_probed_backends() -> tuple[str, ...]:
    """Return the list of backend names to probe, from JSON config or legacy."""
    cfg = _load_config_or_default()
    if cfg:
        names = tuple(cfg["backends"].keys())
        if names:
            return names
    return LEGACY_PROBED_BACKENDS


def _resolve_fallback_priority(cfg: dict | None) -> tuple[str, ...]:
    """Return fallback priority from JSON config or legacy defaults."""
    if cfg:
        chain = cfg.get("fallback_chain", [])
        reviewer = cfg.get("reviewer_default")
        ordered: list[str] = []
        if reviewer:
            ordered.append(backend_config.resolve_alias(cfg, reviewer))
        for entry in chain:
            canonical = backend_config.resolve_alias(cfg, entry)
            if canonical and canonical not in ordered:
                ordered.append(canonical)
        if ordered:
            return tuple(ordered)
    return LEGACY_FALLBACK_PRIORITY


def _resolve_transitive_deps(backend: str, cfg: dict | None) -> tuple[str, ...]:
    """Return transitive deps for ``backend`` from JSON or legacy."""
    if cfg and backend in cfg["backends"]:
        deps = cfg["backends"][backend].get("transitive_deps", [])
        return tuple(deps)
    return LEGACY_TRANSITIVE_DEPS.get(backend, ())


def _resolve_hints(cfg: dict | None) -> dict[str, str]:
    """Return install hints from JSON (if available) merged with legacy."""
    hints: dict[str, str] = dict(LEGACY_HINTS)
    return hints


def _probe(name: str) -> Optional[str]:
    """Return resolved PATH for ``name`` or None if missing.

    Imports ``shutil`` inside the function so tests can monkeypatch
    ``runner.preflight.shutil.which`` to control availability.
    """
    import shutil as _shutil

    return _shutil.which(name)


def preflight_check(backend: str, workdir: pathlib.Path | None = None) -> dict:
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

    Returns
    -------
    dict with keys:
        ``status``           ("pass" | "warn" | "fail")
        ``configured``       the backend the caller asked about
        ``configured_ok``    bool — whether the configured backend resolved
        ``backends``         dict[cli] -> {"ok": bool, "path": str|None, "hint": str|None}
        ``transitive``       dict[cli] -> {"ok": bool, ...} for transitive deps
        ``fallback_recommendation`` first available CLI in FALLBACK_PRIORITY
        ``message``          human-readable summary line
    """
    workdir = workdir or pathlib.Path.cwd()
    del workdir  # currently unused; keep the parameter for future expansion

    cfg = _load_config_or_default()
    probed = _resolve_probed_backends()
    fallback_priority = _resolve_fallback_priority(cfg)
    hints = _resolve_hints(cfg)

    # Normalize: a backend we don't know about is treated as missing
    # but still appears in the report so the caller can see what was
    # asked for.
    known = backend in probed
    configured_present = (backend == "echo") or _probe(backend) is not None

    backends: dict[str, dict] = {}
    for name in probed:
        if name == "echo":
            backends[name] = {"ok": True, "path": None, "hint": None}
            continue
        path = _probe(name)
        backends[name] = {
            "ok": path is not None,
            "path": path,
            "hint": None if path else hints.get(name),
        }

    # Include the configured backend in the report even if it's an
    # unknown name (e.g. user typo) so the caller sees what was asked.
    if not known and backend != "echo":
        path = _probe(backend)
        backends[backend] = {
            "ok": path is not None,
            "path": path,
            "hint": hints.get(backend, f"Unknown backend {backend!r}"),
        }

    transitive: dict[str, dict] = {}
    for dep in _resolve_transitive_deps(backend, cfg):
        path = _probe(dep)
        transitive[dep] = {
            "ok": path is not None,
            "path": path,
            "hint": None if path else hints.get(dep),
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
    # backend is present, prefer it; otherwise pick the first fallback_priority
    # entry that resolves.
    if backend == "echo" or configured_present:
        fallback = backend if backend == "echo" else backend
    else:
        fallback = "echo"
        for cand in fallback_priority:
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

    return {
        "status": status,
        "configured": backend,
        "configured_ok": configured_present,
        "backends": backends,
        "transitive": transitive,
        "fallback_recommendation": fallback,
        "message": message,
    }


def main(argv: list[str] | None = None) -> int:
    """CLI entry point. Returns process exit code (0, 2)."""
    p = argparse.ArgumentParser(
        prog="runner.preflight",
        description="Probe configured backend CLI availability.",
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
        "--json",
        action="store_true",
        help="Emit JSON to stdout (default: human-readable summary)",
    )
    args = p.parse_args(argv)

    result = preflight_check(args.backend, args.workdir)

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
