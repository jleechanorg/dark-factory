"""JSON-driven backend + fallback chain configuration (Bead jleechan-ev6m).

Single source of truth for runner backends, vendor fallback chains, and
per-agent flags. Replaces the legacy ``DARK_FACTORY_BACKEND`` and
``DARK_FACTORY_REVIEWER_FALLBACK_CHAIN`` environment variables.

Resolution precedence (highest first):
    1. CLI ``--backend`` argument (Python ``runner/__main__.py``)
    2. Legacy ``DARK_FACTORY_BACKEND`` env var (deprecated — emits warning)
    3. ``default_backend`` from ``config/backends.json`` (or user override)

Config file lookup order:
    1. ``$DARK_FACTORY_BACKENDS_CONFIG`` env var (explicit path)
    2. ``~/.dark-factory/backends.json`` (user override)
    3. ``<repo_root>/config/backends.json`` (committed default)

Schema (version 1):

    {
      "version": 1,
      "default_backend": "ao",
      "reviewer_default": "minimax",
      "fallback_chain": ["agy", "minimax", "claude-code"],
      "alias_map": {"aow": "minimax", "agy": "antigravity"},
      "backends": {
        "ao":   {"cli": "ao",    "args": ["spawn"],
                 "agent": "antigravity",
                 "default_project": "worldarchitect.ai",
                 "transitive_deps": ["sandbox-exec"]},
        "agy":  {"cli": "agy",   "args": ["--print", "--dangerously-skip-permissions"],
                 "agent": "gemini-3.6-flash-high"},
        "claude": {"cli": "claude", "args": ["--print", "--dangerously-skip-permissions"]},
        "codex":  {"cli": "codex",  "args": ["exec", "--yolo"]},
        "echo":   {"cli": "echo",   "args": []},
        "mock_llm": {"cli": "mock_llm", "args": []}
      }
    }

Why a hand-rolled validator (vs jsonschema)?  The schema is small enough
that pulling a dependency would be more cost than benefit, and we want
clear error messages that name the offending field. See tests in
``tests/test_backend_config.py``.
"""

from __future__ import annotations

import json
import logging
import os
import pathlib
from typing import Any

_LOG = logging.getLogger("runner.backend_config")

# Environment variable names — all deprecated.
_LEGACY_ENV_BACKEND = "DARK_FACTORY_BACKEND"
_LEGACY_ENV_FALLBACK_CHAIN = "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN"
_LEGACY_ENV_REVIEWER_DEFAULT = "DARK_FACTORY_REVIEWER_DEFAULT"
_EXPLICIT_CONFIG_ENV = "DARK_FACTORY_BACKENDS_CONFIG"

# Default locations (relative to repo root / user home).
_REPO_CONFIG = pathlib.Path("config") / "backends.json"
_USER_CONFIG = pathlib.Path.home() / ".dark-factory" / "backends.json"

# Supported schema versions. Bump ``SCHEMA_VERSION`` when adding a field.
SCHEMA_VERSION = 1


class SchemaError(ValueError):
    """Raised when a backend config file fails schema validation."""


# ---------------------------------------------------------------------------
# Loader
# ---------------------------------------------------------------------------


def load(path: pathlib.Path | str) -> dict[str, Any]:
    """Load + validate a backend config from ``path``.

    Raises
    ------
    FileNotFoundError
        ``path`` does not exist.
    ValueError
        File is not valid JSON.
    SchemaError
        File fails schema validation.
    """
    p = pathlib.Path(path)
    if not p.exists():
        raise FileNotFoundError(f"backend config not found: {p}")
    try:
        raw = json.loads(p.read_text())
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid JSON in backend config {p}: {exc}") from exc
    if not isinstance(raw, dict):
        raise SchemaError(
            f"backend config root must be an object, got {type(raw).__name__}"
        )
    validate(raw)
    return raw


def load_with_precedence(
    repo_path: pathlib.Path | str | None = None,
    user_path: pathlib.Path | str | None = None,
) -> dict[str, Any]:
    """Load config, preferring ``user_path`` over ``repo_path``.

    Resolution order (highest first):
        1. ``$DARK_FACTORY_BACKENDS_CONFIG`` env var (if set and exists)
        2. ``user_path`` (default ``~/.dark-factory/backends.json``)
        3. ``repo_path`` (default ``<cwd>/config/backends.json``)
    """
    explicit = os.environ.get(_EXPLICIT_CONFIG_ENV)
    if explicit:
        explicit_path = pathlib.Path(explicit)
        if explicit_path.exists():
            return load(explicit_path)

    candidates: list[pathlib.Path] = []
    if user_path is not None:
        candidates.append(pathlib.Path(user_path))
    else:
        candidates.append(_USER_CONFIG)
    if repo_path is not None:
        candidates.append(pathlib.Path(repo_path))
    else:
        candidates.append(_REPO_CONFIG)

    for c in candidates:
        if c.exists():
            return load(c)

    raise FileNotFoundError(
        "no backend config found; tried: "
        + ", ".join(str(c) for c in candidates)
    )


# ---------------------------------------------------------------------------
# Schema validation
# ---------------------------------------------------------------------------


def validate(cfg: dict[str, Any]) -> None:
    """Validate ``cfg`` in place; raise :class:`SchemaError` on failure."""
    required_top = ("version", "default_backend", "backends")
    for key in required_top:
        if key not in cfg:
            raise SchemaError(f"missing required field: {key}")

    version = cfg["version"]
    if not isinstance(version, int) or version < 1:
        raise SchemaError(f"version must be int >= 1, got {version!r}")
    if version > SCHEMA_VERSION:
        raise SchemaError(
            f"config version {version} is newer than supported "
            f"({SCHEMA_VERSION}); upgrade the runner"
        )

    default_backend = cfg["default_backend"]
    if not isinstance(default_backend, str) or not default_backend:
        raise SchemaError(
            f"default_backend must be a non-empty string, got {default_backend!r}"
        )

    backends = cfg["backends"]
    if not isinstance(backends, dict) or not backends:
        raise SchemaError("backends must be a non-empty object")

    known_names: set[str] = set()
    for name, spec in backends.items():
        if not isinstance(spec, dict):
            raise SchemaError(f"backends[{name!r}] must be an object")
        cli = spec.get("cli")
        if not isinstance(cli, str) or not cli:
            raise SchemaError(f"backends[{name!r}].cli must be a non-empty string")
        args = spec.get("args", [])
        if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
            raise SchemaError(
                f"backends[{name!r}].args must be a list of strings"
            )
        if "agent" in spec and not isinstance(spec["agent"], str):
            raise SchemaError(f"backends[{name!r}].agent must be a string")
        if "default_project" in spec and not isinstance(spec["default_project"], str):
            raise SchemaError(f"backends[{name!r}].default_project must be a string")
        deps = spec.get("transitive_deps", [])
        if not isinstance(deps, list) or not all(isinstance(d, str) for d in deps):
            raise SchemaError(
                f"backends[{name!r}].transitive_deps must be a list of strings"
            )
        known_names.add(name)

    if default_backend not in known_names:
        raise SchemaError(
            f"default_backend {default_backend!r} is not defined in backends"
        )

    if "reviewer_default" in cfg:
        if not isinstance(cfg["reviewer_default"], str):
            raise SchemaError("reviewer_default must be a string")

    if "fallback_chain" in cfg:
        chain = cfg["fallback_chain"]
        if not isinstance(chain, list):
            raise SchemaError("fallback_chain must be a list of strings")
        for entry in chain:
            if not isinstance(entry, str):
                raise SchemaError(
                    f"fallback_chain entries must be strings, got {entry!r}"
                )
            canonical = _canonicalize(cfg, entry)
            if canonical not in known_names:
                raise SchemaError(
                    f"fallback_chain references unknown backend {entry!r} "
                    f"(canonical: {canonical!r})"
                )

    if "alias_map" in cfg:
        amap = cfg["alias_map"]
        if not isinstance(amap, dict):
            raise SchemaError("alias_map must be an object")
        for alias, target in amap.items():
            if not isinstance(target, str):
                raise SchemaError(
                    f"alias_map[{alias!r}] must be a string, got {target!r}"
                )
            canonical = _canonicalize(cfg, target)
            if canonical not in known_names:
                raise SchemaError(
                    f"alias_map[{alias!r}] -> {target!r} (canonical "
                    f"{canonical!r}) is not defined in backends"
                )


# ---------------------------------------------------------------------------
# Lookup helpers
# ---------------------------------------------------------------------------


def get_backend_spec(
    cfg: dict[str, Any] | pathlib.Path | str, name: str
) -> dict[str, Any]:
    """Return the backend spec for ``name`` from ``cfg``.

    If ``cfg`` is a path, it is loaded first.
    Raises ``KeyError`` if ``name`` is not defined.
    """
    if not isinstance(cfg, dict):
        cfg = load(cfg)
    canonical = resolve_alias(cfg, name)
    if canonical not in cfg["backends"]:
        raise KeyError(
            f"unknown backend {name!r} (canonical {canonical!r}); "
            f"defined: {sorted(cfg['backends'].keys())}"
        )
    return cfg["backends"][canonical]


def resolve_alias(cfg: dict[str, Any], name: str) -> str:
    """Canonicalize a vendor alias via ``alias_map``; pass-through if unknown."""
    if not isinstance(cfg, dict):
        cfg = load(cfg)
    amap = cfg.get("alias_map", {})
    return amap.get(name, name)


def resolve_fallback_chain(cfg: dict[str, Any]) -> list[str]:
    """Resolve the configured fallback chain with alias canonicalization.

    Prepends ``reviewer_default`` if defined, then iterates
    ``fallback_chain`` entries. Dedup by canonical form. Empty entries
    are skipped.
    """
    if not isinstance(cfg, dict):
        cfg = load(cfg)
    chain: list[str] = []
    seen: set[str] = set()
    reviewer_default = cfg.get("reviewer_default")
    for entry in [reviewer_default] + list(cfg.get("fallback_chain", [])):
        if not entry or not isinstance(entry, str):
            continue
        canonical = resolve_alias(cfg, entry)
        if canonical and canonical not in seen:
            chain.append(canonical)
            seen.add(canonical)
    return chain


# ---------------------------------------------------------------------------
# Backend / chain resolution with env-var precedence
# ---------------------------------------------------------------------------


def resolve_backend(
    config_path: pathlib.Path | str | None = None,
    cli_backend: str | None = None,
) -> str:
    """Resolve the active backend name with precedence:
    CLI arg > legacy env var > JSON default.

    Emits a deprecation warning if the legacy env var is set.
    """
    if cli_backend:
        return cli_backend

    legacy = os.environ.get(_LEGACY_ENV_BACKEND)
    if legacy:
        _LOG.warning(
            "environment variable %s=%r is deprecated; "
            "configure default_backend in config/backends.json instead",
            _LEGACY_ENV_BACKEND,
            legacy,
        )
        return legacy

    cfg = _load_for_resolve(config_path)
    return cfg["default_backend"]


def resolve_fallback_chain_with_precedence(
    config_path: pathlib.Path | str | None = None,
) -> list[str]:
    """Resolve the fallback chain. Legacy env var, when set, overrides JSON."""
    cfg = _load_for_resolve(config_path)

    legacy_chain = os.environ.get(_LEGACY_ENV_FALLBACK_CHAIN)
    if legacy_chain:
        _LOG.warning(
            "environment variable %s=%r is deprecated; "
            "configure fallback_chain in config/backends.json instead",
            _LEGACY_ENV_FALLBACK_CHAIN,
            legacy_chain,
        )
        chain_cfg = dict(cfg)
        chain_cfg["fallback_chain"] = [
            part.strip() for part in legacy_chain.split("->")
        ]
        return resolve_fallback_chain(chain_cfg)

    legacy_reviewer = os.environ.get(_LEGACY_ENV_REVIEWER_DEFAULT)
    if legacy_reviewer:
        _LOG.warning(
            "environment variable %s=%r is deprecated; "
            "configure reviewer_default in config/backends.json instead",
            _LEGACY_ENV_REVIEWER_DEFAULT,
            legacy_reviewer,
        )
        chain_cfg = dict(cfg)
        chain_cfg["reviewer_default"] = legacy_reviewer
        return resolve_fallback_chain(chain_cfg)

    return resolve_fallback_chain(cfg)


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------


def _canonicalize(cfg: dict[str, Any], name: str) -> str:
    """Apply alias_map, then leave as-is."""
    return resolve_alias(cfg, name)


def _load_for_resolve(
    config_path: pathlib.Path | str | None,
) -> dict[str, Any]:
    """Load the config with full precedence (explicit > user > repo)."""
    if config_path is not None:
        return load(pathlib.Path(config_path))
    return load_with_precedence()