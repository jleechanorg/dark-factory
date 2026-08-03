"""CLI backend preflight check for Dark Factory.

Runs *before* the runner Python code via the bash wrappers in ``bin/``.
Probes the configured backend and a small set of alternates via
``shutil.which`` (no version probes) and reports a structured JSON
status consumable by callers and humans.

States
------
- ``pass``  configured backend and every enabled Codex lane are present
- ``warn``  configured backend missing but at least one other CLI present
- ``fail``  a required Codex lane is invalid, or zero non-echo backends are reachable

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
import pathlib
import shutil
import sys
from typing import Optional

from . import codex_runtime


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


def preflight_check(
    backend: str,
    workdir: pathlib.Path | None = None,
    *,
    shadow_codex: bool = True,
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
    shadow_codex:
        Whether the runner configuration enables the default Codex shadow
        reviewer. When true, Codex runtime skew is fatal before launch even
        when another backend is primary.

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

    try:
        codex = codex_runtime.resolve_codex_runtime()
        codex_path: Optional[str] = str(codex.executable)
        codex_error = ""
    except codex_runtime.CodexRuntimeError as exc:
        codex_path = None
        codex_error = str(exc)

    def _backend_path(name: str) -> Optional[str]:
        return codex_path if name == "codex" else _probe(name)

    # Normalize: a backend we don't know about is treated as missing
    # but still appears in the report so the caller can see what was
    # asked for.
    known = backend in PROBED_BACKENDS
    configured_present = (backend == "echo") or _backend_path(backend) is not None

    backends: dict[str, dict] = {}
    for name in PROBED_BACKENDS:
        if name == "echo":
            backends[name] = {"ok": True, "path": None, "hint": None}
            continue
        path = _backend_path(name)
        backends[name] = {
            "ok": path is not None,
            "path": path,
            "hint": None if path else (codex_error if name == "codex" else HINTS.get(name)),
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
    codex_required = backend == "codex" or shadow_codex
    if codex_required and codex_error:
        status = "fail"
    elif backend == "echo" or configured_present:
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

    if backend == "codex" and codex_error:
        message = f"codex runtime rejected: {codex_error}"
    elif shadow_codex and codex_error:
        message = f"shadow Codex runtime rejected: {codex_error}"
    elif status == "pass":
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
        "shadow_codex": shadow_codex,
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
        "--shadow-codex",
        choices=("true", "false"),
        default="true",
        help="Whether the default Codex shadow reviewer is enabled (default: true)",
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
        shadow_codex=args.shadow_codex == "true",
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
