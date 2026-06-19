"""Structural preflight check for Dark Factory pipelines.

Lane B of the 2026-06-12 fanout. Sits alongside (not inside) the existing
``runner.preflight`` module — that one probes the CLI backend (claude /
codex / agy), this one validates the *structure* of a single pipeline
``.dot`` file before it is handed to the runner.

Checks
------
1. ``prompt_paths``    every ``prompt="@relative/path.md"`` resolves to an
                       existing file. Resolution mirrors the runner's
                       own ``_render_prompt`` behavior (see
                       ``runner/handlers.py:_render_prompt``): the
                       relative path is tried first against the .dot
                       file's directory, then against the dark-factory
                       repo root (``factory_home()``). This matches
                       runtime behavior so the check has bite on real
                       typos and missing files without flagging
                       well-formed pipelines whose prompts live
                       alongside the repo (e.g. ``prompts/hello/plan.md``
                       from a .dot under ``pipelines/factory/``).
2. ``timeout_thresholds`` every node with ``validation="true"`` OR a
                       ``codergen`` shape has ``timeout`` >= 60s.
3. ``edge_resolution`` every ``from -> to`` edge points to a defined node.

The output shape mirrors ``runner.preflight`` (``{"status": ..., "checks":
[...]}``) so downstream tooling (cron, automation, future PR-gate) can
parse both with the same envelope.

Exit codes
----------
- ``0``  pass
- ``2``  fail (one or more checks did not pass)
- ``1``  usage error (e.g. file not found, parser crashed)

The module is runnable as
``python -m runner.structural_preflight <pipeline.dot> --json``
so the bash wrapper in ``bin/df-validate`` can capture structured output.

Bead: jleechan-wou
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shlex
import shutil
import sys
from typing import Any, Optional

from runner import parser as _parser
from runner.paths import factory_home  # type: ignore


# Threshold (seconds) below which a node's `timeout` is considered too low
# for any prompt-driven work. Mirrors ``parser._VALIDATION_TIMEOUT_MIN_SECONDS``.
TIMEOUT_THRESHOLD_S = 60


# Shell builtins that ``shutil.which`` cannot resolve but ``tool`` nodes may
# legitimately invoke as the head of ``command="..."``. Exposed at module
# level so conformance tests can pin the exact skip list. Mirrors the set
# of POSIX builtins dark-factory pipelines actually use; new entries must
# be added with a corresponding test so the failure UX stays contractually
# stable.
_SHELL_BUILTINS: frozenset[str] = frozenset(
    {"cd", "test", "echo", "true", "false", "pwd", "[", "[["}
)


def _first_binary_token(command: str) -> Optional[str]:
    """Return the first non-placeholder token of ``command``.

    The runner's ``_tool`` handler runs ``shlex.split(cmd)`` and invokes
    the resulting argv through ``subprocess``. To match runtime behavior,
    the preflight takes the first whitespace-delimited token — provided
    it is not a ``${state.<key>}`` placeholder that the runner resolves
    at execution time (e.g. ``command="${state.slim.test_command}"``).
    Quoted tokens are stripped of their surrounding quotes by ``shlex``.
    Returns ``None`` for empty input or for an unresolved placeholder
    head (which we deliberately skip — the binary is not knowable until
    the runner binds state).
    """
    stripped = command.strip()
    if not stripped:
        return None
    if stripped.startswith("${"):
        return None
    try:
        tokens = shlex.split(stripped)
    except ValueError:
        # Unbalanced quotes — let the runtime surface the parse error;
        # preflight should not shadow it with a binary-not-found
        # diagnostic.
        return None
    if not tokens:
        return None
    head = tokens[0]
    if head in _SHELL_BUILTINS:
        return None
    if head.startswith("${"):
        return None
    return head


def _check_prompt_paths(graph: _parser.Graph, pipeline_path: pathlib.Path) -> dict[str, Any]:
    """Check that every ``prompt="@..."`` attribute resolves to a real file.

    Resolution mirrors ``runner.handlers._render_prompt`` so the check
    matches runtime behavior: relative paths are tried first against the
    .dot file's directory, then against the dark-factory repo root
    (``factory_home()``). Absolute paths are honored as-is.

    Returns a check dict with ``ok`` (bool) and ``missing`` (list of
    strings — one per broken prompt, in the form
    ``"<node>: <absolute-prompt-path>"``).
    """
    missing: list[str] = []
    for node in graph.nodes.values():
        # The parser exposes a prompt_ref property that strips the leading '@'.
        ref = node.prompt_ref
        if not ref:
            continue
        prompt_path = pathlib.Path(ref)
        if prompt_path.is_absolute():
            if not prompt_path.exists():
                missing.append(f"{node.name}: {prompt_path}")
            continue
        # Try .dot-dir-relative first (per spec), then factory_home()-relative
        # (per the runner's own _render_prompt). The first hit wins; only if
        # BOTH miss do we report the prompt as missing.
        dot_relative = (pipeline_path.parent / prompt_path).resolve()
        home_relative: Optional[pathlib.Path] = None
        home = factory_home()
        if home is not None:
            home_relative = (home / prompt_path).resolve()
        if dot_relative.exists():
            continue
        if home_relative is not None and home_relative.exists():
            continue
        # Report the .dot-relative path as the canonical "where we looked"
        # since that's the spec-mandated resolution base; this is the path
        # a human would copy-paste to debug.
        missing.append(f"{node.name}: {dot_relative}")
    return {"name": "prompt_paths", "ok": not missing, "missing": missing}


def _check_timeout_thresholds(graph: _parser.Graph) -> dict[str, Any]:
    """Check that every validation/codergen node has ``timeout`` >= threshold.

    A node needs a timeout check if it has ``validation="true"`` OR if it
    is a ``codergen`` (the most common backend-driven node type, where
    a missing/short timeout leads to silent truncation).

    The check accepts the integer directly; a non-integer timeout is
    reported as missing (the parser coerces to int, so this is also a
    sanity check on the .dot source).

    Returns a check dict with ``ok`` (bool) and ``under_threshold``
    (list of strings in the form ``"<node>: <timeout>s"``).
    """
    under: list[str] = []
    for node in graph.nodes.values():
        is_validation = bool(node.attrs.get("validation", False))
        is_codergen = node.attrs.get("type") == "codergen"
        if not (is_validation or is_codergen):
            continue
        timeout = node.attrs.get("timeout")
        if not isinstance(timeout, int) or timeout < TIMEOUT_THRESHOLD_S:
            under.append(f"{node.name}: {timeout!r}")
    return {"name": "timeout_thresholds", "ok": not under, "under_threshold": under}


def _check_edge_resolution(graph: _parser.Graph) -> dict[str, Any]:
    """Check that every edge's source and destination reference defined nodes.

    The parser already rejects unknown nodes at parse time, so a parsed
    graph is by construction consistent. This check remains as a
    defense-in-depth layer that emits the same envelope shape even if
    the parser's checks are ever relaxed; the ``Graph`` data model is
    the contract callers see.

    Returns a check dict with ``ok`` (bool) and ``unresolved`` (list of
    strings in the form ``"<src> -> <dst>"``).
    """
    defined = set(graph.nodes)
    unresolved: list[str] = []
    for edge in graph.edges:
        if edge.src not in defined or edge.dst not in defined:
            unresolved.append(f"{edge.src} -> {edge.dst}")
    return {"name": "edge_resolution", "ok": not unresolved, "unresolved": unresolved}


def _check_command_binaries(graph: _parser.Graph) -> dict[str, Any]:
    """Check that every ``tool`` node's ``command`` head resolves on PATH.

    The runner's ``_tool`` handler executes the first token of the
    ``command`` attribute via ``subprocess``. A typo in that token
    (e.g. ``pytes`` instead of ``pytest``) currently only surfaces at
    runtime as a cryptic ``[Errno 2] No such file or directory``; this
    preflight check catches the same class of mistake at validation
    time with a precise, actionable diagnostic.

    The check uses ``shutil.which`` (not ``pathlib.exists``) so PATH
    resolution matches runtime behavior — ``/usr/bin/echo`` may exist
    but the tool handler runs through PATH, and operators may not have
    that exact directory mounted.

    Skipped cases (no false positives):

    - Empty ``command`` attribute (defer to runtime — empty is
      already a runtime failure surfaced by the tool handler).
    - ``${state.<key>}`` placeholder heads (the runner substitutes
      state at execution time; the actual binary is not knowable
      during preflight).
    - Shell builtins listed in ``_SHELL_BUILTINS`` (these have no
      on-disk binary; the kernel invokes them via ``/bin/sh``-style
      dispatch or, in the runner's subprocess.run path, the OS still
      resolves them through the shell PATH differently).
    - Unbalanced-quote commands (defer to runtime).

    Returns a check dict with ``ok`` (bool) and ``missing`` (list of
    strings in the form ``"<node>: binary not found on PATH: <name>"``).
    The message format is pinned by a conformance test in
    ``tests/test_structural_preflight_command_binaries.py``.
    """
    missing: list[str] = []
    for node in graph.nodes.values():
        if node.attrs.get("type") != "tool":
            continue
        cmd = node.attrs.get("command")
        if not cmd or not isinstance(cmd, str):
            continue
        binary = _first_binary_token(cmd)
        if binary is None:
            continue
        if shutil.which(binary) is None:
            missing.append(f"{node.name}: binary not found on PATH: {binary}")
    return {"name": "command_binaries", "ok": not missing, "missing": missing}


def validate_structure(pipeline_path: pathlib.Path) -> dict[str, Any]:
    """Validate a pipeline .dot file and return a structured status dict.

    On success: ``status == "pass"``, all three checks are ``ok: true``,
    ``errors == []``.

    On failure: ``status == "fail"``, at least one check is
    ``ok: false``, and ``errors`` carries one human-readable string per
    failure (so callers can print or log without re-walking checks).

    On parser crash: ``status == "fail"``, ``checks == []``,
    ``errors == [<the exception text>]`` — fail fast with a structured
    envelope, never a Python traceback.
    """
    pipeline_path = pathlib.Path(pipeline_path)
    errors: list[str] = []

    if not pipeline_path.exists():
        return {
            "status": "fail",
            "pipeline_path": str(pipeline_path),
            "checks": [],
            "errors": [f"pipeline file does not exist: {pipeline_path}"],
        }

    try:
        graph = _parser.parse(pipeline_path)
    except Exception as exc:
        return {
            "status": "fail",
            "pipeline_path": str(pipeline_path),
            "checks": [],
            "errors": [f"failed to parse pipeline: {type(exc).__name__}: {exc}"],
        }

    checks = [
        _check_prompt_paths(graph, pipeline_path),
        _check_timeout_thresholds(graph),
        _check_edge_resolution(graph),
        _check_command_binaries(graph),
    ]

    for check in checks:
        if check["ok"]:
            continue
        if check["name"] == "prompt_paths":
            for entry in check["missing"]:
                errors.append(f"missing prompt path: {entry}")
        elif check["name"] == "timeout_thresholds":
            for entry in check["under_threshold"]:
                errors.append(f"timeout below {TIMEOUT_THRESHOLD_S}s: {entry}")
        elif check["name"] == "edge_resolution":
            for entry in check["unresolved"]:
                errors.append(f"unresolved edge: {entry}")
        elif check["name"] == "command_binaries":
            for entry in check["missing"]:
                errors.append(entry)

    return {
        "status": "pass" if not errors else "fail",
        "pipeline_path": str(pipeline_path),
        "checks": checks,
        "errors": errors,
    }


def main(argv: list[str] | None = None) -> int:
    """CLI entry point. Returns process exit code (0, 2, or 1 for usage errors)."""
    p = argparse.ArgumentParser(
        prog="runner.structural_preflight",
        description="Validate a pipeline .dot file before launch (prompt paths, timeouts, edge resolution).",
    )
    p.add_argument(
        "pipeline",
        type=pathlib.Path,
        help="Path to the pipeline .dot file to validate.",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Emit JSON to stdout (default: human-readable summary).",
    )
    args = p.parse_args(argv)

    result = validate_structure(args.pipeline)

    if args.json:
        json.dump(result, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
    else:
        if result["status"] == "pass":
            print(f"OK {result['pipeline_path']}")
            for check in result["checks"]:
                print(f"  {check['name']}: ok")
        else:
            print(f"FAIL {result['pipeline_path']}")
            for error in result["errors"]:
                print(f"  {error}")

    if result["status"] == "fail":
        return 2
    if result["status"] == "pass":
        return 0
    # Defensive default: validate_structure only emits pass/fail, but if a
    # future status string slips in we still want a non-zero exit code.
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
