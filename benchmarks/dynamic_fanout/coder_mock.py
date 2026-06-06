"""The deterministic coder — a single capability shared by BOTH modes.

This is the controlled variable. ``add_validation`` (G3 scenario) and the
schema/migration writers (G1 scenario) perform real file edits identically no
matter which mode calls them. The ONLY thing that differs between modes is the
*dispatch*: which endpoints get an ``add_validation`` call (fixed set vs
discovered set), and which column list the migration writer is handed (a static
default vs node 1's threaded output). Keeping the coder itself identical is what
makes any measured separation attributable to dispatch, not to coder skill.
"""

from __future__ import annotations

import json
import pathlib

from .scenario import API_FILENAME, GUARD_MARKER

SCHEMA_FILENAME = "schema.json"
MIGRATION_FILENAME = "migration.sql"


def add_validation(workdir: str | pathlib.Path, endpoint_index: int) -> bool:
    """Insert the validation guard into ``endpoint_<i>`` if it exists.

    Real on-disk edit. Returns True if a guard was added (endpoint existed and
    was previously unguarded), False if there is no such endpoint (a wasted
    dispatch — what Mode A does when its fixed node count exceeds K). Idempotent:
    a second call on an already-guarded endpoint returns False.
    """
    path = pathlib.Path(workdir) / API_FILENAME
    lines = path.read_text().splitlines(keepends=True)
    def_marker = f"def endpoint_{endpoint_index}(request):"
    out = []
    inserted = False
    i = 0
    while i < len(lines):
        line = lines[i]
        out.append(line)
        if def_marker in line:
            indent = " " * (len(line) - len(line.lstrip()) + 4)
            nxt = lines[i + 1] if i + 1 < len(lines) else ""
            if GUARD_MARKER not in nxt:
                out.append(f"{indent}{GUARD_MARKER}\n")
                inserted = True
        i += 1
    if inserted:
        path.write_text("".join(out))
    return inserted


def write_schema(workdir: str | pathlib.Path, columns: list[str]) -> list[str]:
    """Node-1 output: write ``schema.json`` declaring ``columns``. Returns them
    so the harness can thread the value into node 2 (Mode A+B only)."""
    path = pathlib.Path(workdir) / SCHEMA_FILENAME
    path.write_text(json.dumps({"table": "widgets", "columns": list(columns)}, indent=2) + "\n")
    return list(columns)


def write_migration(workdir: str | pathlib.Path, columns: list[str]) -> None:
    """Node-2 output: write ``migration.sql`` using whatever column list it is
    handed. The migration is only *consistent* if these match the schema's
    columns — which depends entirely on whether the dispatch threaded node 1's
    output to this call."""
    cols = ", ".join(f"{c} TEXT" for c in columns)
    path = pathlib.Path(workdir) / MIGRATION_FILENAME
    path.write_text(f"CREATE TABLE widgets (\n    {cols}\n);\n")
