"""Unit + smoke tests for runner.preflight."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

import pytest

from runner import preflight
from runner import backend_config


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Default: every probed CLI is missing. Tests override per-case.
def _which_map(values: dict[str, str | None]):
    """Build a shutil.which replacement that returns the given path or None."""

    def _which(name: str):
        return values.get(name)

    return _which


@pytest.fixture
def which_all_present(monkeypatch):
    """All probed CLIs resolve to a fake path."""
    paths = {
        "claude": "/usr/local/bin/claude",
        "codex": "/usr/local/bin/codex",
        "agy": "/usr/local/bin/agy",
        "ao": "/usr/local/bin/ao",
        "sandbox-exec": "/usr/bin/sandbox-exec",
    }
    monkeypatch.setattr(preflight, "_probe", lambda name: paths.get(name))
    return paths


@pytest.fixture
def which_none(monkeypatch):
    """No probed CLI is present (echo is always considered present)."""
    monkeypatch.setattr(preflight, "_probe", lambda name: None)


@pytest.fixture
def which_claude_only(monkeypatch):
    """Only claude is present."""
    monkeypatch.setattr(
        preflight,
        "_probe",
        lambda name: "/usr/local/bin/claude" if name == "claude" else None,
    )


@pytest.fixture
def which_ao_no_sandbox(monkeypatch):
    """ao is present but sandbox-exec is missing."""
    paths = {"ao": "/usr/local/bin/ao"}
    monkeypatch.setattr(preflight, "_probe", lambda name: paths.get(name))
    return paths


# ---------------------------------------------------------------------------
# Unit tests
# ---------------------------------------------------------------------------


def test_configured_backend_present_returns_pass(monkeypatch, which_all_present):
    """Configured backend present => status=pass, configured_ok=True."""
    result = preflight.preflight_check("claude")

    assert result["status"] == "pass"
    assert result["configured"] == "claude"
    assert result["configured_ok"] is True
    assert result["backends"]["claude"]["ok"] is True
    assert result["backends"]["claude"]["path"] == "/usr/local/bin/claude"
    # Fallback should be the configured backend when it's present.
    assert result["fallback_recommendation"] == "claude"


def test_configured_backend_missing_others_available_returns_warn(
    monkeypatch, which_claude_only
):
    """Configured (codex) missing but claude present => status=warn, exit 0."""
    # Force legacy probe list / fallback priority so we exercise the
    # pre-JSON-config path explicitly.
    monkeypatch.setattr(preflight, "_load_config_or_default", lambda: None)
    monkeypatch.setattr(
        preflight,
        "_resolve_probed_backends",
        lambda: preflight.LEGACY_PROBED_BACKENDS,
    )
    monkeypatch.setattr(
        preflight,
        "_resolve_fallback_priority",
        lambda _cfg: preflight.LEGACY_FALLBACK_PRIORITY,
    )
    monkeypatch.setattr(
        preflight,
        "_resolve_transitive_deps",
        lambda backend, _cfg: preflight.LEGACY_TRANSITIVE_DEPS.get(backend, ()),
    )

    result = preflight.preflight_check("codex")

    assert result["status"] == "warn"
    assert result["configured"] == "codex"
    assert result["configured_ok"] is False
    assert result["backends"]["claude"]["ok"] is True
    assert result["backends"]["codex"]["ok"] is False
    assert result["backends"]["codex"]["hint"] is not None
    # LEGACY_FALLBACK_PRIORITY = ("codex", "claude", "agy", "ao", "echo").
    # codex is missing and excluded; first present is claude.
    assert result["fallback_recommendation"] == "claude"
    # Exit code 0 for warn
    assert preflight.main(["--backend", "codex", "--json"]) == 0


def test_zero_backends_available_returns_fail(monkeypatch, which_none):
    """All probed CLIs missing => status=fail, exit code 2."""
    result = preflight.preflight_check("claude")

    assert result["status"] == "fail"
    assert result["configured"] == "claude"
    assert result["configured_ok"] is False
    # echo is always ok
    assert result["backends"]["echo"]["ok"] is True
    # fallback falls through to echo
    assert result["fallback_recommendation"] == "echo"
    # Exit code mapping
    assert preflight.main(["--backend", "claude", "--json"]) == 2


def test_ao_configured_checks_sandbox_exec_transitive(
    monkeypatch, which_ao_no_sandbox
):
    """ao present, sandbox-exec missing => status=warn (ao usable, deps not)."""
    result = preflight.preflight_check("ao")

    # ao is present, so configured_ok is True and status=pass
    # (transitive deps don't downgrade a *present* configured backend)
    assert result["configured_ok"] is True
    assert result["backends"]["ao"]["ok"] is True
    assert result["transitive"]["sandbox-exec"]["ok"] is False

    # Now flip the scenario: ao MISSING, sandbox-exec MISSING => fail
    # (no usable backend at all)
    monkeypatch.setattr(preflight, "_probe", lambda name: None)
    result2 = preflight.preflight_check("ao")
    assert result2["status"] == "fail"
    assert result2["transitive"]["sandbox-exec"]["ok"] is False


def test_echo_backend_always_ok():
    """echo backend is always pass regardless of filesystem."""
    # No monkeypatch — _probe is called for non-echo entries too, but
    # echo is short-circuited before any probe.
    result = preflight.preflight_check("echo")
    assert result["status"] == "pass"
    assert result["backends"]["echo"]["ok"] is True
    assert preflight.main(["--backend", "echo", "--json"]) == 0


def test_fallback_priority_order(monkeypatch):
    """First present in legacy fallback priority wins."""
    # Force legacy priority list so this test stays focused on the
    # hardcoded fallback chain (the JSON-driven chain is exercised by
    # the test_preflight_config_driven_fallback_priority test below).
    monkeypatch.setattr(preflight, "_load_config_or_default", lambda: None)
    monkeypatch.setattr(
        preflight,
        "_resolve_probed_backends",
        lambda: preflight.LEGACY_PROBED_BACKENDS,
    )
    monkeypatch.setattr(
        preflight,
        "_resolve_fallback_priority",
        lambda _cfg: preflight.LEGACY_FALLBACK_PRIORITY,
    )
    monkeypatch.setattr(
        preflight,
        "_resolve_transitive_deps",
        lambda backend, _cfg: preflight.LEGACY_TRANSITIVE_DEPS.get(backend, ()),
    )
    # Only agy present
    monkeypatch.setattr(
        preflight,
        "_probe",
        lambda name: "/usr/local/bin/agy" if name == "agy" else None,
    )
    result = preflight.preflight_check("claude")
    assert result["status"] == "warn"
    # LEGACY_FALLBACK_PRIORITY = ("codex", "claude", "agy", "ao", "echo")
    # configured=claude missing; first present alt in priority order is agy.
    assert result["fallback_recommendation"] == "agy"


# ---------------------------------------------------------------------------
# Smoke test — invoke as a real subprocess
# ---------------------------------------------------------------------------


def test_subprocess_emits_valid_json(tmp_path):
    """Running `python -m runner.preflight --json` emits valid JSON."""
    proc = subprocess.run(
        [sys.executable, "-m", "runner.preflight", "--backend", "echo", "--json"],
        cwd=str(pathlib.Path(__file__).resolve().parent.parent),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "pass"
    assert payload["configured"] == "echo"


def test_subprocess_fail_exit_code(tmp_path):
    """Subprocess exit code 2 on hard-stop, regardless of JSON flag.

    Force-fail by setting PATH to an empty directory so no probed CLI resolves.
    """
    import os

    def _is_clean_dir(d: str) -> bool:
        p = pathlib.Path(d)
        if not p.is_dir():
            return False
        for name in ("claude", "codex", "agy", "ao"):
            if (p / name).exists():
                return False
        return True

    clean_paths = [
        d for d in os.environ.get("PATH", "").split(os.pathsep)
        if _is_clean_dir(d)
    ]
    clean_path_str = os.pathsep.join([str(tmp_path)] + clean_paths)

    sanitized_env = {
        "PATH": clean_path_str,
        "HOME": os.environ.get("HOME", "/tmp"),
        "PYTHONPATH": str(
            pathlib.Path(__file__).resolve().parent.parent
        ),
    }
    # Self-hosted Linux CI runners promote LD_LIBRARY_PATH via $GITHUB_ENV
    # (see .github/workflows/ci.yml "Fix Python 3.13 shared-library path")
    # so subprocess python3 can find libpython3.13.so.1.0. Without it, THIS
    # subprocess (sys.executable, a from-scratch env) fails with rc=127
    # "error while loading shared libraries" before runner.preflight even
    # gets a chance to run — a false negative unrelated to what this test
    # is actually probing (the hard-stop no-backend-available path).
    if "LD_LIBRARY_PATH" in os.environ:
        sanitized_env["LD_LIBRARY_PATH"] = os.environ["LD_LIBRARY_PATH"]
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner.preflight",
            "--backend",
            "claude",
            "--json",
        ],
        cwd=str(pathlib.Path(__file__).resolve().parent.parent),
        capture_output=True,
        text=True,
        timeout=30,
        env=sanitized_env,
    )
    assert proc.returncode == 2, f"returncode={proc.returncode}\nstdout={proc.stdout}\nstderr={proc.stderr}"
    payload = json.loads(proc.stdout)
    assert payload["status"] == "fail"


# ---------------------------------------------------------------------------
# JSON-config-driven preflight (Bead jleechan-ev6m)
# ---------------------------------------------------------------------------


def test_preflight_uses_json_config_for_backends(monkeypatch, tmp_path):
    """When a backends.json is present, preflight probes its backends list."""
    cfg = {
        "version": 1,
        "default_backend": "ao",
        "backends": {
            "ao": {"cli": "ao", "args": ["spawn"], "transitive_deps": ["sandbox-exec"]},
            "claude": {"cli": "claude", "args": ["--print"]},
            "codex": {"cli": "codex", "args": ["exec", "--yolo"]},
            "echo": {"cli": "echo", "args": []},
        },
    }
    cfg_path = tmp_path / "backends.json"
    cfg_path.write_text(json.dumps(cfg))
    monkeypatch.setenv("DARK_FACTORY_BACKENDS_CONFIG", str(cfg_path))

    # Pretend only claude is on PATH.
    monkeypatch.setattr(
        preflight,
        "_probe",
        lambda name: "/usr/local/bin/claude" if name == "claude" else None,
    )

    result = preflight.preflight_check("ao")
    # ao configured, but missing -> warn, fallback to claude
    assert result["status"] == "warn"
    assert "ao" in result["backends"]
    assert "claude" in result["backends"]
    assert "sandbox-exec" in result["transitive"]
    assert result["fallback_recommendation"] == "claude"


def test_preflight_config_driven_fallback_priority(
    monkeypatch, tmp_path
):
    """Fallback priority is derived from JSON config's fallback_chain order."""
    cfg = {
        "version": 1,
        "default_backend": "ao",
        "reviewer_default": "claude",
        "fallback_chain": ["claude", "codex", "echo"],
        "backends": {
            "ao": {"cli": "ao", "args": ["spawn"]},
            "claude": {"cli": "claude", "args": ["--print"]},
            "codex": {"cli": "codex", "args": ["exec", "--yolo"]},
            "echo": {"cli": "echo", "args": []},
        },
    }
    cfg_path = tmp_path / "backends.json"
    cfg_path.write_text(json.dumps(cfg))
    monkeypatch.setenv("DARK_FACTORY_BACKENDS_CONFIG", str(cfg_path))

    # ao missing, claude + codex present.
    monkeypatch.setattr(
        preflight,
        "_probe",
        lambda name: {
            "claude": "/usr/local/bin/claude",
            "codex": "/usr/local/bin/codex",
        }.get(name),
    )
    result = preflight.preflight_check("ao")
    assert result["fallback_recommendation"] == "claude"


def test_preflight_falls_back_to_legacy_when_no_config(monkeypatch, tmp_path):
    """If no JSON config is found, legacy PROBED_BACKENDS list still works."""
    monkeypatch.setenv(
        "DARK_FACTORY_BACKENDS_CONFIG", str(tmp_path / "missing.json")
    )
    monkeypatch.setattr(
        preflight,
        "_probe",
        lambda name: "/usr/local/bin/claude" if name == "claude" else None,
    )
    # Should not raise; falls back to legacy probe list.
    result = preflight.preflight_check("claude")
    assert result["status"] == "pass"
    assert result["backends"]["claude"]["ok"] is True
