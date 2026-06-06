"""Real on-disk evaluators — re-read the artifacts the coder produced and grade.

These never trust the modes' self-reports; they re-open the files in ``workdir``
and count, exactly as a sealed holdout evaluator would. Returns dicts shaped for
``benchmarks.workflow_graphgen.scoring`` (``{available, pass, total}``) so the
same aggregator grades them.
"""

from __future__ import annotations

import json
import pathlib

from .scenario import API_FILENAME, GUARD_MARKER
from .coder_mock import SCHEMA_FILENAME, MIGRATION_FILENAME

_DEF_PREFIX = "def endpoint_"


def coverage(workdir: str | pathlib.Path) -> dict:
    """Grade G3: how many endpoints ended up guarded.

    ``pass`` = endpoints whose def line is immediately followed by the guard
    marker; ``total`` = endpoints present. Pure file read.
    """
    path = pathlib.Path(workdir) / API_FILENAME
    lines = path.read_text().splitlines()
    total = 0
    guarded = 0
    for idx, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith(_DEF_PREFIX) and stripped.endswith("(request):"):
            total += 1
            nxt = lines[idx + 1].strip() if idx + 1 < len(lines) else ""
            if nxt == GUARD_MARKER:
                guarded += 1
    return {"available": True, "pass": guarded, "total": total}


def consistency(workdir: str | pathlib.Path) -> dict:
    """Grade G1: does the migration's column set match the schema's?

    ``pass`` = 1 and ``total`` = 1 when the column lists are equal (as ordered
    lists), else 0/1. Both files must exist.
    """
    wd = pathlib.Path(workdir)
    schema = json.loads((wd / SCHEMA_FILENAME).read_text())
    schema_cols = list(schema.get("columns", []))
    migration = (wd / MIGRATION_FILENAME).read_text()
    # Recover the column order from the CREATE TABLE body.
    mig_cols = []
    for tok in migration.replace("\n", " ").split(","):
        tok = tok.strip()
        # each column reads "<name> TEXT" (the first one trails "( <name> TEXT")
        if " TEXT" in tok:
            name = tok.split(" TEXT")[0].split("(")[-1].strip()
            if name:
                mig_cols.append(name)
    match = schema_cols == mig_cols
    return {"available": True, "pass": 1 if match else 0, "total": 1,
            "schema_columns": schema_cols, "migration_columns": mig_cols}
