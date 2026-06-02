"""Resolve Dark Factory install paths (DARK_FACTORY_HOME)."""

from __future__ import annotations

import os
import pathlib


def factory_home() -> pathlib.Path | None:
    raw = os.environ.get("DARK_FACTORY_HOME", "").strip()
    if not raw:
        return None
    return pathlib.Path(raw).expanduser().resolve()


def resolve_factory_path(path: pathlib.Path) -> pathlib.Path:
    """Resolve `path` against cwd, then DARK_FACTORY_HOME when relative."""
    candidate = path.expanduser()
    if candidate.is_absolute():
        return candidate
    if candidate.exists():
        return candidate.resolve()
    home = factory_home()
    if home is not None:
        under_home = (home / candidate).resolve()
        if under_home.exists():
            return under_home
    return candidate.resolve()
