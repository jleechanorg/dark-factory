"""Shared skeptic reviewer priority — loaded from config/skeptic_reviewer_priority.json.

Both the Rust daemon (`daemon/src/reviewer_priority.rs`) and the GHA
skeptic gate (`runner/skeptic_gate_cli.py`) read this file so the factory
never drifts between Linux daemon gate-7 and GitHub Actions skeptic.
"""

from __future__ import annotations

import json
from functools import lru_cache
from pathlib import Path
from typing import List, Tuple

_CONFIG_PATH = (
    Path(__file__).resolve().parent.parent / "config" / "skeptic_reviewer_priority.json"
)

_GHA_SUPPORTED_REVIEWERS = frozenset({"codex", "gemini"})


@lru_cache(maxsize=1)
def _load() -> dict:
    return json.loads(_CONFIG_PATH.read_text(encoding="utf-8"))


def skeptic_reviewer_priority() -> List[str]:
    """Ordered reviewer vendor list (daemon + GHA must all run these)."""
    raw = _load()["reviewer_priority"]
    if not isinstance(raw, list) or not raw:
        raise ValueError(f"{_CONFIG_PATH}: reviewer_priority must be a non-empty list")
    out: List[str] = []
    for item in raw:
        if not isinstance(item, str) or not item.strip():
            raise ValueError(f"{_CONFIG_PATH}: reviewer_priority entries must be non-empty strings")
        out.append(item.strip())
    return out


def mandatory_reviewers() -> Tuple[str, ...]:
    """Exact reviewer set the GHA gate requires (same as priority list)."""
    return tuple(skeptic_reviewer_priority())


def gha_reviewer_priority() -> List[str]:
    """Ordered Python/GHA reviewer vendors with implemented transports."""
    raw = _load().get("gha_reviewer_priority")
    if raw is None:
        # Pre-capability-split configs used the tested GHA pair directly.
        raw = ["codex", "gemini"]
    if not isinstance(raw, list) or not raw:
        raise ValueError(f"{_CONFIG_PATH}: gha_reviewer_priority must be a non-empty list")
    out: List[str] = []
    for item in raw:
        if not isinstance(item, str) or not item.strip():
            raise ValueError(
                f"{_CONFIG_PATH}: gha_reviewer_priority entries must be non-empty strings"
            )
        vendor = item.strip()
        if vendor not in _GHA_SUPPORTED_REVIEWERS:
            raise ValueError(
                f"{_CONFIG_PATH}: gha_reviewer_priority vendor {vendor!r} "
                f"has no implemented skeptic_gate_cli transport"
            )
        out.append(vendor)
    return out


def gha_mandatory_reviewers() -> Tuple[str, ...]:
    """Exact GHA reviewer set consumed by the Python skeptic gate."""
    return tuple(gha_reviewer_priority())


def gha_default_reviewers_json() -> str:
    """Default Python/GHA reviewer JSON from implemented capabilities."""
    return json.dumps([[vendor, ""] for vendor in gha_reviewer_priority()])


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
