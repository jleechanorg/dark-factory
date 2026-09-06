"""Shared skeptic reviewer priority — loaded from config/skeptic_reviewer_priority.json.

Both the Rust daemon (`daemon/src/reviewer_priority.rs`) and the GHA
skeptic gate (`runner/skeptic_gate_cli.py`) read this file so the factory
never drifts WITHIN a given invocation context. There are two contexts:

- `reviewer_priority` — used when `DARK_FACTORY_VIA_AF` is truthy, i.e. this
  process tree was spawned by the /af daemon (set on every AO worker by
  `daemon/src/adapters.rs::ao_spawn_command_with_mode`). Claudem-first;
  codex is deliberately excluded to conserve codex quota for interactive use.
- `reviewer_priority_manual` — used otherwise (a human running
  `dark-factory`/`/factory` directly, or any other non-/af caller).
  Codex-first, per operator intent.

The Rust daemon's own `reviewer_priority.rs` always reads `reviewer_priority`
unconditionally — it IS the /af context by definition, so it never needs to
branch on the env var.
"""

from __future__ import annotations

import json
import os
from functools import lru_cache
from pathlib import Path
from typing import List, Tuple

_CONFIG_PATH = (
    Path(__file__).resolve().parent.parent / "config" / "skeptic_reviewer_priority.json"
)

_VIA_AF_FALSY = {"", "0", "false", "no", "off"}


def _via_af() -> bool:
    return os.environ.get("DARK_FACTORY_VIA_AF", "").strip().lower() not in _VIA_AF_FALSY


@lru_cache(maxsize=1)
def _load() -> dict:
    return json.loads(_CONFIG_PATH.read_text(encoding="utf-8"))


def skeptic_reviewer_priority() -> List[str]:
    """Ordered reviewer vendor list for the current invocation context."""
    key = "reviewer_priority" if _via_af() else "reviewer_priority_manual"
    raw = _load().get(key)
    if not isinstance(raw, list) or not raw:
        raise ValueError(f"{_CONFIG_PATH}: {key} must be a non-empty list")
    out: List[str] = []
    for item in raw:
        if not isinstance(item, str) or not item.strip():
            raise ValueError(f"{_CONFIG_PATH}: {key} entries must be non-empty strings")
        out.append(item.strip())
    return out


def mandatory_reviewers() -> Tuple[str, ...]:
    """Exact reviewer set the GHA gate requires (same as priority list)."""
    return tuple(skeptic_reviewer_priority())


def default_reviewers_json() -> str:
    """Default `--reviewers-json` / `SKEPTIC_REVIEWERS_JSON` value."""
    pairs = [[vendor, ""] for vendor in skeptic_reviewer_priority()]
    return json.dumps(pairs)


def default_coder() -> str:
    return str(_load().get("default_coder", "agy"))


def coder_fallback_chain() -> List[str]:
    raw = _load().get("coder_fallback_chain", ["claudem"])
    if not isinstance(raw, list):
        raise ValueError(f"{_CONFIG_PATH}: coder_fallback_chain must be a list")
    return [str(x).strip() for x in raw if str(x).strip()]
