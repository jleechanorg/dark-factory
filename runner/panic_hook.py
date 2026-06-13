"""Top-level panic hook — write a crash artifact on uncaught failures.

Belt-and-suspenders companion to the in-Python panic in
``runner.__main__`` (which fires on exceptions inside the runner's
own try/except boundary). This module is invoked from the bash
wrappers in ``bin/`` and catches failures the in-Python panic
cannot see:

  * bash-level failures (e.g. venv missing, set -e exits before
    Python even starts)
  * Python processes that die with a signal (SIGSEGV / SIGABRT)
  * Python processes that exit with a non-zero code we want to
    preserve verbatim rather than overwriting

Public API
----------
- :data:`PANIC_DIR`
- :data:`PANIC_EXIT_CODE`
- :func:`write_crash_artifact`
- :func:`filter_env`  (also re-exported for unit tests)
- :func:`extract_run_id_from_argv`
- :func:`main` (CLI entry point — ``python -m runner.panic_hook ...``)

JSON artifact shape (stable; CI / Healer parses this):

.. code-block:: json

    {
      "ts": "2026-06-12T19:45:00Z",
      "run_id": "abc123" or null,
      "argv": ["dark-factory", "--backend", "claude", "..."],
      "cwd": "/Users/jleechan/projects/dark-factory",
      "traceback": "Traceback (most recent call last):\\n  ...",
      "env_filtered": {"DARK_FACTORY_HOME": "...", ...},
      "exit_code": 1
    }

Tenets
------
- **Non-invasive** — this module is invoked from the bash wrappers,
  not from the Python runner. ``runner/__main__.py`` and
  ``runner/handlers.py`` are intentionally not modified.
- **Fail-safe** — every code path swallows its own exceptions and
  writes ``panic_artifact_write_error`` to the artifact so the hook
  itself never crashes the process it is trying to diagnose.
- **Machine-readable** — the artifact is strict JSON (sort_keys=True,
  indent=2). No free text, no printf debugging, no banner.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import pathlib
import re
import sys
import traceback
from typing import Iterable

PANIC_DIR: pathlib.Path = pathlib.Path.home() / ".dark-factory" / "panics"

# Distinct exit code reserved for panics. 124 is the canonical
# `timeout(1)` "killed" sentinel; using it here means CI / the
# Healer can group panics together with timeout-class failures
# (both are "process aborted externally from normal flow").
# Override via env var ``DARK_FACTORY_PANIC_EXIT_CODE`` if a caller
# needs to disambiguate timeout vs. panic in the same pipeline.
PANIC_EXIT_CODE: int = int(os.environ.get("DARK_FACTORY_PANIC_EXIT_CODE", "124"))

# Substrings that mark a variable as a secret. Match is
# case-insensitive on the variable name.
_SECRET_SUBSTRINGS: tuple[str, ...] = (
    "TOKEN",
    "KEY",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH",
)

# Variable names that are stripped even if they don't match the
# substring pattern (defense in depth).
_BLACKLISTED_VARS: frozenset[str] = frozenset(
    {
        "DARK_FACTORY_HOLDOUTS",  # sealed-holdout path is sensitive
    }
)

# How many characters of the argv-hash to embed in the filename.
_HASH_PREFIX_LEN: int = 8


def _is_secret(var_name: str) -> bool:
    """True when ``var_name`` looks like it holds a secret.

    Both the explicit allow-list and the substring pattern are matched
    case-insensitively, so a caller passing ``dark_factory_holdouts``
    is treated the same as ``DARK_FACTORY_HOLDOUTS``.
    """
    upper = var_name.upper()
    if upper in {b.upper() for b in _BLACKLISTED_VARS}:
        return True
    return any(token in upper for token in _SECRET_SUBSTRINGS)


def filter_env(env: os._Environ[str] | dict[str, str] | Iterable[tuple[str, str]]) -> dict[str, str]:
    """Return ``env`` with secret-bearing variables removed.

    The contract is "secrets NEVER reach the artifact" — not "only
    the documented list". We strip any variable whose name contains
    a known secret substring (case-insensitive) AND a small
    allow-list of specific names (``DARK_FACTORY_HOLDOUTS``).

    Accepts ``os.environ``, a plain dict, or an iterable of pairs.
    """
    if isinstance(env, dict):
        items: Iterable[tuple[str, str]] = env.items()
    elif isinstance(env, os._Environ):  # type: ignore[attr-defined]
        items = list(env.items())
    else:
        items = list(env)

    return {k: v for k, v in items if not _is_secret(k)}


def extract_run_id_from_argv(argv: list[str]) -> str | None:
    """Best-effort run_id extraction from the dark-factory CLI argv.

    Two patterns are supported:

    * ``--state run_id=foo``  (the documented state-seed pattern)
    * ``--state-slim-test-id foo``  (slim / test plumbing, if added later)

    Returns ``None`` when no recognizable pattern is present. The
    hook is intentionally permissive — a ``None`` run_id is
    perfectly valid and the artifact records it as JSON null.
    """
    for idx, arg in enumerate(argv):
        if arg == "--state" and idx + 1 < len(argv):
            value = argv[idx + 1]
            if "=" in value:
                key, _, val = value.partition("=")
                if key.strip() == "run_id" and val.strip():
                    return val.strip()
    return None


def _utc_timestamp() -> str:
    """ISO-8601 UTC, second precision, ``Z`` suffix."""
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _argv_hash(argv: list[str]) -> str:
    """Stable short hash of ``argv`` for filename disambiguation."""
    blob = "\x1f".join(argv).encode("utf-8", errors="replace")
    return hashlib.sha1(blob).hexdigest()[:_HASH_PREFIX_LEN]


def _argv_basename(argv0: str) -> str:
    """Strip path from ``argv[0]``; fall back to ``dark-factory``."""
    if not argv0:
        return "dark-factory"
    return pathlib.Path(argv0).name or "dark-factory"


def _safe_traceback(exc: BaseException | None) -> str:
    """Format the current exception's traceback; never raises."""
    if exc is None:
        return ""
    try:
        return "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
    except Exception:  # noqa: BLE001 — fail-safe
        return f"{type(exc).__name__}: {exc}"


def write_crash_artifact(
    traceback_str: str,
    argv: list[str],
    cwd: str,
    run_id: str | None,
    env_filtered: dict[str, str],
    *,
    exit_code: int = 1,
    panic_dir: pathlib.Path | None = None,
) -> pathlib.Path:
    """Write a JSON crash artifact and return the path.

    The function is best-effort: any I/O failure is swallowed and
    re-attempted once. If both attempts fail, an empty placeholder
    file is written so the path still exists for forensic recovery.

    Parameters
    ----------
    traceback_str
        Pre-formatted Python traceback. Use :func:`_safe_traceback`
        to derive this from a live exception.
    argv
        The full argv that was being executed. ``argv[0]`` is the
        wrapper name (``dark-factory`` / ``df-healer``).
    cwd
        Process working directory at the time of the crash.
    run_id
        Optional run identifier. ``None`` is allowed.
    env_filtered
        Pre-filtered environment variables (call :func:`filter_env`
        upstream; this function does NOT filter again to keep the
        contract explicit).
    exit_code
        The process exit code that triggered the panic.
    panic_dir
        Override the panic output directory (used by tests).

    Returns
    -------
    pathlib.Path
        The path of the file that was actually written, even if the
        write itself failed (in which case the file may be empty).
    """
    target_dir = pathlib.Path(panic_dir) if panic_dir is not None else PANIC_DIR

    payload: dict[str, object] = {
        "ts": _utc_timestamp(),
        "run_id": run_id,
        "argv": list(argv),
        "cwd": cwd,
        "traceback": traceback_str,
        "env_filtered": dict(env_filtered),
        "exit_code": int(exit_code),
    }

    argv0 = argv[0] if argv else ""
    basename = _argv_basename(argv0)
    digest = _argv_hash(argv)
    filename = f"{payload['ts']}-{basename}-{digest}.json"
    target = target_dir / filename
    # NOTE: every step below is wrapped in a try/except — the hook
    # itself must NEVER crash the process it is trying to diagnose.
    # mkdir may fail if the parent is a file (NotADirectoryError) or
    # the FS is read-only (PermissionError). All three are caught
    # and degrade to "we tried, the path is recorded for forensics".

    try:
        target_dir.mkdir(parents=True, exist_ok=True)
    except Exception as mkdir_exc:  # noqa: BLE001 — fail-safe
        payload["panic_artifact_mkdir_error"] = f"{type(mkdir_exc).__name__}: {mkdir_exc}"

    try:
        target.write_text(json.dumps(payload, sort_keys=True, indent=2))
    except Exception as write_exc:  # noqa: BLE001 — fail-safe
        try:
            target.write_text(
                json.dumps(
                    {**payload, "panic_artifact_write_error": f"{type(write_exc).__name__}: {write_exc}"},
                    sort_keys=True,
                    indent=2,
                )
            )
        except Exception:  # noqa: BLE001 — fall through
            try:
                target.touch(exist_ok=True)
            except Exception:  # noqa: BLE001 — give up
                pass
    return target


def _build_argparser() -> argparse.ArgumentParser:
    """CLI parser used by the bash wrappers (``python -m runner.panic_hook ...``).

    The wrapper signature is intentionally forgiving — every flag is
    optional except ``--exit-code``, and the parser accepts unknown
    argv transparently so a wrapper can pass through the original
    command line without re-encoding it.

    The original bash argv is passed as a single JSON-encoded string
    via ``--bash-argv`` (so there is no possibility of argparse
    greedily consuming the first positional as a flag value).
    """
    p = argparse.ArgumentParser(
        prog="python -m runner.panic_hook",
        description="Write a crash artifact for a failed dark-factory invocation.",
        allow_abbrev=False,
    )
    p.add_argument("--exit-code", type=int, required=True, help="Process exit code that triggered the panic.")
    p.add_argument("--line", type=int, default=0, help="Bash $LINENO where the panic was raised (0 if unknown).")
    p.add_argument(
        "--bash-argv",
        default=None,
        help=(
            "JSON-encoded list of the original bash argv. "
            "Pass the wrapper's $0 and $@ as a single JSON string."
        ),
    )
    p.add_argument(
        "--traceback",
        default="",
        help="Optional pre-formatted traceback string (default: empty).",
    )
    p.add_argument(
        "--panic-dir",
        default=None,
        help="Override the panic output directory (used by tests).",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    """CLI entry point. Always returns :data:`PANIC_EXIT_CODE`."""
    if argv is None:
        argv = sys.argv[1:]

    parser = _build_argparser()
    args, remaining = parser.parse_known_args(argv)

    # When --bash-argv is set, its value is a JSON-encoded list of the
    # original wrapper argv. This avoids argparse ambiguity where the
    # first positional could be greedily consumed as a flag value.
    if args.bash_argv:
        try:
            original_argv = list(json.loads(args.bash_argv))
        except (ValueError, TypeError):
            # Malformed JSON — fall back to treating it as a single token.
            original_argv = [args.bash_argv]
    elif remaining:
        original_argv = [sys.argv[0], *remaining]
    else:
        original_argv = [sys.argv[0]]

    # We only have a path hint from --line; argv[0] is the python
    # interpreter when invoked via ``-m runner.panic_hook``. The
    # bash wrapper supplies the wrapper name as the first remaining
    # arg after --bash-argv, so prefer that when present.
    argv0 = original_argv[0] if original_argv else "dark-factory"

    cwd = os.getcwd()
    run_id = extract_run_id_from_argv(original_argv[1:])
    env_filtered = filter_env(os.environ)
    tb = args.traceback or ""
    if not tb:
        # Bash-level crash → no Python traceback. Synthesize one so
        # the artifact is uniform across Python and bash failures.
        tb = (
            f"Bash panic at line {args.line or '?'}: wrapper exited with code {args.exit_code}\n"
            f"argv: {original_argv!r}\n"
        )

    write_crash_artifact(
        traceback_str=tb,
        argv=[argv0, *original_argv[1:]],
        cwd=cwd,
        run_id=run_id,
        env_filtered=env_filtered,
        exit_code=args.exit_code,
        panic_dir=args.panic_dir,
    )
    return PANIC_EXIT_CODE


if __name__ == "__main__":
    raise SystemExit(main())
