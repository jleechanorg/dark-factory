"""Prompt catalog loader + pre-run path validation for workflow_graphgen.

Spec: specs/workflow_graphgen.md §4a / feature spec "Prompt catalog requirement".

The generator Workflow may only reference node prompt paths that appear in
``prompts/catalog.json``. Before any agent runs in either mode the harness calls
:func:`validate_node_prompts`, which fails the run when a generated node points
at a path that is absent from the catalog or absent on disk. This keeps a graph
that uses ``refactor``/``research``/``stack_smoke`` from dispatching against a
missing file.
"""

from __future__ import annotations

import json
import os
import pathlib
from typing import Iterable

# The eight allowed dynamic node vocabulary types (fixed set).
VOCABULARY = (
    "plan",
    "implement",
    "test",
    "fix",
    "review",
    "refactor",
    "research",
    "stack_smoke",
)


class CatalogError(ValueError):
    """Raised when the catalog is malformed or a node prompt path is invalid."""


def _home() -> pathlib.Path:
    """DARK_FACTORY_HOME, falling back to this repo root (two levels up)."""
    env = os.environ.get("DARK_FACTORY_HOME")
    if env:
        return pathlib.Path(env)
    return pathlib.Path(__file__).resolve().parent.parent.parent


def catalog_path() -> pathlib.Path:
    return _home() / "prompts" / "catalog.json"


def load_catalog(path: pathlib.Path | None = None) -> dict:
    """Load and structurally validate ``prompts/catalog.json``.

    Asserts every vocabulary type is mapped and that each mapped template exists
    on disk, so a healthy catalog is a precondition the harness can trust.
    """
    p = path or catalog_path()
    if not p.exists():
        raise CatalogError(f"prompt catalog missing: {p}")
    data = json.loads(p.read_text())
    prompts = data.get("prompts")
    if not isinstance(prompts, dict):
        raise CatalogError("catalog has no 'prompts' object")

    missing_types = [t for t in VOCABULARY if t not in prompts]
    if missing_types:
        raise CatalogError(f"catalog missing vocabulary types: {missing_types}")

    home = _home()
    absent = []
    for vocab, rel in prompts.items():
        if not (home / rel).exists():
            absent.append(f"{vocab} -> {rel}")
    if absent:
        raise CatalogError(f"catalog points at absent prompt files: {absent}")
    return data


def catalog_prompt_paths(path: pathlib.Path | None = None) -> set[str]:
    """The set of catalog-approved prompt paths (relative to DARK_FACTORY_HOME)."""
    return set(load_catalog(path)["prompts"].values())


def _normalize(prompt_ref: str) -> str:
    """Strip the optional leading '@' the DOT parser also strips."""
    return prompt_ref[1:] if prompt_ref.startswith("@") else prompt_ref


def validate_node_prompts(
    node_prompt_refs: Iterable[str],
    *,
    path: pathlib.Path | None = None,
) -> None:
    """Fail the run if any generated node prompt is not catalog-approved + on disk.

    ``node_prompt_refs`` are the ``prompt=`` values of the generated dynamic-middle
    nodes (with or without the leading ``@``). Raises :class:`CatalogError` listing
    every offending reference; returns ``None`` when all references are valid.
    """
    approved = catalog_prompt_paths(path)
    home = _home()
    bad = []
    for ref in node_prompt_refs:
        rel = _normalize(ref)
        if rel not in approved:
            bad.append(f"{ref} (not in catalog)")
        elif not (home / rel).exists():
            bad.append(f"{ref} (catalog path absent on disk)")
    if bad:
        raise CatalogError(f"invalid node prompt path(s): {bad}")
