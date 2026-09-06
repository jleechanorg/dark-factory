"""Drift guard for the backend-name closed set (round 3 — regex).

Scans ``runner/handler_dispatch.py`` and ``runner/handler_codergen.py``
for every literal that names a specific backend (``backend == "X"``,
``backend in {"X", "Y"}``) and asserts every such name appears in
``runner.backend_registry._BUILTIN_BACKEND_NAMES``. Round 4 swaps the
regex for an AST parser that also catches tuple form and hyphenated
names.
"""
from __future__ import annotations

import pathlib
import re

from runner import backend_registry


_DISPATCH_FILES = (
    pathlib.Path(__file__).resolve().parent.parent / "runner" / "handler_dispatch.py",
    pathlib.Path(__file__).resolve().parent.parent / "runner" / "handler_codergen.py",
)


def _extract_literal_backends(source: str) -> set[str]:
    found: set[str] = set()
    for match in re.finditer(r'backend\s*[!=]=\s*"([\w-]+)"', source):
        found.add(match.group(1))
    for match in re.finditer(r'backend\s+in\s+[\{\(]([^})\n]*)[\}\)]', source):
        body = match.group(1)
        for name_match in re.finditer(r'"([\w-]+)"', body):
            found.add(name_match.group(1))
    return found


def test_dispatch_files_only_reference_known_builtins():
    missing: set[str] = set()
    for path in _DISPATCH_FILES:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        for name in _extract_literal_backends(text):
            if name not in backend_registry._BUILTIN_BACKEND_NAMES:
                missing.add(name)
    assert not missing, (
        f"Dispatch ladders reference backends not in "
        f"_BUILTIN_BACKEND_NAMES: {sorted(missing)}. Extend "
        f"runner/backend_registry.py:_BUILTIN_BACKEND_NAMES before "
        f"using a new name."
    )


def test_extracted_literals_are_nonempty_for_smoke():
    dispatch = _DISPATCH_FILES[0].read_text(encoding="utf-8")
    found = _extract_literal_backends(dispatch)
    assert "claude" in found or "codex" in found