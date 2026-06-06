"""Deterministic scenario generators — real files on disk, no LLM.

``make_api_source`` writes a Python module with K route handlers, each missing
an input-validation guard. The "coder task" is to add the guard to every
endpoint; the evaluator grades how many endpoints ended up guarded. K is the
runtime-discovered quantity that a static Mode A graph cannot adapt its node
count to.
"""

from __future__ import annotations

import pathlib

API_FILENAME = "api.py"

#: A guarded endpoint contains this exact marker on the line after the def.
GUARD_MARKER = "validate(request)  # VALIDATION-GUARD"

_HEADER = '''"""Generated API surface — {k} endpoints, initially unguarded.

The coder task: add an input-validation guard (``{marker}``) to EVERY endpoint.
Coverage = guarded_endpoints / total_endpoints, graded by evaluator.coverage.
"""


def route(path):
    """Trivial decorator stand-in so the module imports without a web framework."""

    def _wrap(fn):
        fn._route = path
        return fn

    return _wrap


def validate(request):
    """Input-validation guard (no-op stand-in; presence is what is graded)."""
    return True


def handle(request):
    return {{"ok": True}}
'''

_ENDPOINT = '''

@route("/endpoint_{i}")
def endpoint_{i}(request):
    return handle(request)
'''


def make_api_source(workdir: str | pathlib.Path, k: int) -> pathlib.Path:
    """Write ``api.py`` with ``k`` unguarded endpoints into ``workdir``.

    Deterministic: same ``k`` always yields byte-identical output. Returns the
    path written.
    """
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    workdir = pathlib.Path(workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    parts = [_HEADER.format(k=k, marker=GUARD_MARKER)]
    for i in range(k):
        parts.append(_ENDPOINT.format(i=i))
    path = workdir / API_FILENAME
    path.write_text("".join(parts))
    return path


def count_endpoints(workdir: str | pathlib.Path) -> int:
    """Count endpoints by reading the source — the *runtime discovery* Mode A+B
    performs and a static Mode A graph cannot. Pure file read, no import."""
    path = pathlib.Path(workdir) / API_FILENAME
    text = path.read_text()
    return sum(1 for line in text.splitlines() if line.startswith("@route("))
