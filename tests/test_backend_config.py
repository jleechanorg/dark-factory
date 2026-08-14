"""TDD tests for runner.backend_config — JSON-driven backend + fallback chain config.

Covers:
- Schema validation (missing/invalid fields)
- Default config loading from config/backends.json
- User-override precedence (user file wins over repo default)
- Fallback chain resolution with alias canonicalization
- Per-backend spec lookup (cli/args/agent/default_project/transitive_deps)
- Deprecation of DARK_FACTORY_BACKEND / DARK_FACTORY_REVIEWER_FALLBACK_CHAIN
  env vars: still honored if set (backward-compat) with a deprecation warning.
"""

from __future__ import annotations

import json
import os
import pathlib
from typing import Any

import pytest

from runner import backend_config


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def tmp_config_dir(tmp_path: pathlib.Path) -> pathlib.Path:
    """Return a fresh directory for writing a config JSON file."""
    return tmp_path


@pytest.fixture
def valid_minimal_config() -> dict[str, Any]:
    """A minimal-but-valid backend config."""
    return {
        "version": 1,
        "default_backend": "ao",
        "reviewer_default": "minimax",
        "fallback_chain": ["agy", "minimax", "claude-code"],
        "alias_map": {"aow": "minimax", "agy": "antigravity"},
        "backends": {
            "antigravity": {
                "cli": "agy",
                "args": ["--print", "--dangerously-skip-permissions"],
                "agent": "gemini-3.6-flash-high",
            },
            "minimax": {
                "cli": "minimax",
                "args": ["--print"],
            },
            "claude-code": {
                "cli": "claude",
                "args": ["--print", "--dangerously-skip-permissions"],
            },
            "ao": {
                "cli": "ao",
                "args": ["spawn"],
                "agent": "antigravity",
                "default_project": "worldarchitect.ai",
                "transitive_deps": ["sandbox-exec"],
            },
            "agy": {
                "cli": "agy",
                "args": ["--print", "--dangerously-skip-permissions"],
                "agent": "gemini-3.6-flash-high",
            },
            "claude": {
                "cli": "claude",
                "args": ["--print", "--dangerously-skip-permissions"],
            },
            "codex": {
                "cli": "codex",
                "args": ["exec", "--yolo"],
            },
            "echo": {"cli": "echo", "args": []},
            "mock_llm": {"cli": "mock_llm", "args": []},
        },
    }


@pytest.fixture
def write_valid_config(tmp_config_dir, valid_minimal_config):
    """Write a valid config file to the temp dir; return its path."""
    path = tmp_config_dir / "backends.json"
    path.write_text(json.dumps(valid_minimal_config))
    return path


# ---------------------------------------------------------------------------
# Schema validation
# ---------------------------------------------------------------------------


def test_load_valid_config_returns_dict(write_valid_config):
    """Loading a well-formed config returns a structured object."""
    cfg = backend_config.load(write_valid_config)
    assert cfg["version"] == 1
    assert cfg["default_backend"] == "ao"
    assert "ao" in cfg["backends"]
    assert cfg["alias_map"]["aow"] == "minimax"


def test_load_missing_file_raises_filenotfound(tmp_path):
    """Missing config file → FileNotFoundError."""
    with pytest.raises(FileNotFoundError):
        backend_config.load(tmp_path / "does-not-exist.json")


def test_load_malformed_json_raises_valueerror(tmp_path):
    """Malformed JSON → ValueError with helpful message."""
    bad = tmp_path / "bad.json"
    bad.write_text("{this is not json")
    with pytest.raises(ValueError, match="[Ii]nvalid JSON"):
        backend_config.load(bad)


def test_validate_missing_required_field_raises(tmp_path, valid_minimal_config):
    """Config missing a required top-level field → SchemaError."""
    del valid_minimal_config["default_backend"]
    path = tmp_path / "incomplete.json"
    path.write_text(json.dumps(valid_minimal_config))
    with pytest.raises(backend_config.SchemaError, match="default_backend"):
        backend_config.load(path)


def test_validate_unknown_backend_name_raises(tmp_path, valid_minimal_config):
    """Backend referenced in fallback_chain but not defined → SchemaError."""
    valid_minimal_config["fallback_chain"] = ["ao", "ghost-cli"]
    path = tmp_path / "broken.json"
    path.write_text(json.dumps(valid_minimal_config))
    with pytest.raises(backend_config.SchemaError, match="ghost-cli"):
        backend_config.load(path)


def test_validate_backend_missing_cli_raises(tmp_path, valid_minimal_config):
    """Backend spec missing required 'cli' field → SchemaError."""
    del valid_minimal_config["backends"]["ao"]["cli"]
    path = tmp_path / "broken.json"
    path.write_text(json.dumps(valid_minimal_config))
    with pytest.raises(backend_config.SchemaError, match="cli"):
        backend_config.load(path)


def test_validate_alias_must_map_to_known_backend(
    tmp_path, valid_minimal_config
):
    """alias_map target must resolve to a known backend → SchemaError."""
    valid_minimal_config["alias_map"]["aow"] = "non-existent"
    path = tmp_path / "broken.json"
    path.write_text(json.dumps(valid_minimal_config))
    with pytest.raises(backend_config.SchemaError, match="non-existent"):
        backend_config.load(path)


# ---------------------------------------------------------------------------
# Spec lookup
# ---------------------------------------------------------------------------


def test_get_backend_spec_returns_dict(write_valid_config):
    """get_backend_spec('ao') returns the structured spec."""
    spec = backend_config.get_backend_spec(write_valid_config, "ao")
    assert spec["cli"] == "ao"
    assert "spawn" in spec["args"]
    assert spec["agent"] == "antigravity"
    assert spec["default_project"] == "worldarchitect.ai"
    assert "sandbox-exec" in spec["transitive_deps"]


def test_get_backend_spec_unknown_raises(write_valid_config):
    """Looking up an unknown backend → KeyError."""
    with pytest.raises(KeyError, match="ghost"):
        backend_config.get_backend_spec(write_valid_config, "ghost")


def test_resolve_alias_canonicalizes_name(write_valid_config):
    """alias_map canonicalizes vendor aliases (aow -> minimax)."""
    cfg = backend_config.load(write_valid_config)
    assert backend_config.resolve_alias(cfg, "aow") == "minimax"
    assert backend_config.resolve_alias(cfg, "agy") == "antigravity"
    # Unknown alias passes through unchanged.
    assert backend_config.resolve_alias(cfg, "claude") == "claude"


# ---------------------------------------------------------------------------
# Fallback chain resolution
# ---------------------------------------------------------------------------


def test_resolve_fallback_chain_dedupes_and_canonicalizes(write_valid_config):
    """fallback_chain dedupes by canonical form, includes reviewer_default."""
    cfg = backend_config.load(write_valid_config)
    # reviewer_default=minimax; fallback_chain=[agy, minimax, claude-code]
    # agy -> antigravity, minimax unchanged, claude-code unchanged.
    # antigravity already in chain? No. minimax already in chain? No.
    chain = backend_config.resolve_fallback_chain(cfg)
    assert chain == ["minimax", "antigravity", "claude-code"]


def test_resolve_fallback_chain_dedupes_agy_alias(write_valid_config):
    """If alias and canonical both appear, only one entry survives."""
    cfg = backend_config.load(write_valid_config)
    cfg["fallback_chain"] = ["agy", "antigravity"]
    chain = backend_config.resolve_fallback_chain(cfg)
    assert chain.count("antigravity") == 1


def test_resolve_fallback_chain_omits_empty_entries(write_valid_config):
    """Empty entries in fallback_chain are skipped."""
    cfg = backend_config.load(write_valid_config)
    cfg["fallback_chain"] = ["agy", "", "minimax", "  "]
    chain = backend_config.resolve_fallback_chain(cfg)
    assert "antigravity" in chain
    assert "minimax" in chain
    assert "" not in chain


# ---------------------------------------------------------------------------
# Precedence: user config wins over repo default
# ---------------------------------------------------------------------------


def test_load_with_user_override(
    tmp_path, valid_minimal_config, monkeypatch
):
    """User config path overrides the repo default when present."""
    repo_cfg = tmp_path / "repo_backends.json"
    repo_cfg.write_text(json.dumps(valid_minimal_config))
    user_cfg = tmp_path / "user_backends.json"
    override = json.loads(json.dumps(valid_minimal_config))
    override["default_backend"] = "claude"
    user_cfg.write_text(json.dumps(override))
    cfg = backend_config.load_with_precedence(
        repo_path=repo_cfg, user_path=user_cfg
    )
    assert cfg["default_backend"] == "claude"


def test_load_with_precedence_repo_only(
    tmp_path, valid_minimal_config
):
    """When user config missing, falls back to repo config."""
    repo_cfg = tmp_path / "repo_backends.json"
    repo_cfg.write_text(json.dumps(valid_minimal_config))
    cfg = backend_config.load_with_precedence(
        repo_path=repo_cfg, user_path=tmp_path / "missing.json"
    )
    assert cfg["default_backend"] == "ao"


# ---------------------------------------------------------------------------
# Backward compatibility: deprecated env vars still honored
# ---------------------------------------------------------------------------


def test_env_var_overrides_json_default(
    tmp_path, valid_minimal_config, monkeypatch
):
    """DARK_FACTORY_BACKEND env var overrides JSON default (deprecated)."""
    path = tmp_path / "backends.json"
    path.write_text(json.dumps(valid_minimal_config))
    monkeypatch.setenv("DARK_FACTORY_BACKEND", "claude")
    resolved = backend_config.resolve_backend(
        config_path=path, cli_backend=None
    )
    assert resolved == "claude"


def test_cli_arg_overrides_env_var(
    tmp_path, valid_minimal_config, monkeypatch
):
    """CLI --backend arg wins over DARK_FACTORY_BACKEND."""
    path = tmp_path / "backends.json"
    path.write_text(json.dumps(valid_minimal_config))
    monkeypatch.setenv("DARK_FACTORY_BACKEND", "claude")
    resolved = backend_config.resolve_backend(
        config_path=path, cli_backend="codex"
    )
    assert resolved == "codex"


def test_no_env_no_cli_uses_json_default(
    tmp_path, valid_minimal_config, monkeypatch
):
    """When no override, JSON config's default_backend wins."""
    path = tmp_path / "backends.json"
    path.write_text(json.dumps(valid_minimal_config))
    monkeypatch.delenv("DARK_FACTORY_BACKEND", raising=False)
    resolved = backend_config.resolve_backend(
        config_path=path, cli_backend=None
    )
    assert resolved == "ao"


def test_deprecated_env_var_emits_warning(
    tmp_path, valid_minimal_config, monkeypatch, caplog
):
    """Setting DARK_FACTORY_BACKEND logs a deprecation warning."""
    path = tmp_path / "backends.json"
    path.write_text(json.dumps(valid_minimal_config))
    monkeypatch.setenv("DARK_FACTORY_BACKEND", "claude")
    import logging

    with caplog.at_level(logging.WARNING, logger="runner.backend_config"):
        backend_config.resolve_backend(
            config_path=path, cli_backend=None
        )
    assert any(
        "DARK_FACTORY_BACKEND" in record.message
        and "deprecated" in record.message.lower()
        for record in caplog.records
    )


def test_deprecated_fallback_chain_env_emits_warning(
    tmp_path, valid_minimal_config, monkeypatch, caplog
):
    """Setting DARK_FACTORY_REVIEWER_FALLBACK_CHAIN logs a deprecation warning."""
    path = tmp_path / "backends.json"
    path.write_text(json.dumps(valid_minimal_config))
    monkeypatch.setenv(
        "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "minimax->claude-code"
    )
    import logging

    with caplog.at_level(logging.WARNING, logger="runner.backend_config"):
        backend_config.resolve_fallback_chain_with_precedence(
            config_path=path
        )
    assert any(
        "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN" in record.message
        and "deprecated" in record.message.lower()
        for record in caplog.records
    )


def test_fallback_chain_env_overrides_json_when_set(
    tmp_path, valid_minimal_config, monkeypatch
):
    """Legacy env var, when set, overrides JSON fallback_chain."""
    path = tmp_path / "backends.json"
    path.write_text(json.dumps(valid_minimal_config))
    monkeypatch.setenv(
        "DARK_FACTORY_REVIEWER_FALLBACK_CHAIN", "agy->claude-code"
    )
    chain = backend_config.resolve_fallback_chain_with_precedence(
        config_path=path
    )
    # reviewer_default is still from JSON (no DARK_FACTORY_REVIEWER_DEFAULT set)
    assert "antigravity" in chain  # agy -> antigravity
    assert "claude-code" in chain


# ---------------------------------------------------------------------------
# Schema self-test
# ---------------------------------------------------------------------------


def test_default_config_in_repo_is_valid():
    """The shipped config/backends.json must validate against the schema."""
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    default = repo_root / "config" / "backends.json"
    assert default.exists(), f"expected {default} to exist"
    cfg = backend_config.load(default)
    assert cfg["version"] >= 1
    assert "backends" in cfg and cfg["backends"]