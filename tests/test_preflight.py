"""Unit + smoke tests for runner.preflight."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

import pytest

from runner import preflight


@pytest.fixture(autouse=True)
def unavailable_codex_runtime(monkeypatch):
    """Keep backend availability tests independent of the operator install."""
    def _unavailable():
        raise preflight.codex_runtime.CodexRuntimeError("fixture Codex unavailable")

    monkeypatch.setattr(preflight.codex_runtime, "resolve_codex_runtime", _unavailable)


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
    result = preflight.preflight_check("claude", shadow_codex=False)

    assert result["status"] == "pass"
    assert result["configured"] == "claude"
    assert result["configured_ok"] is True
    assert result["backends"]["claude"]["ok"] is True
    assert result["backends"]["claude"]["path"] == "/usr/local/bin/claude"
    # Fallback should be the configured backend when it's present.
    assert result["fallback_recommendation"] == "claude"


def test_configured_codex_runtime_skew_fails_closed_even_with_fallback(
    monkeypatch, which_claude_only
):
    """Configured Codex skew is fatal even when another CLI is available."""
    def _skew():
        raise preflight.codex_runtime.CodexRuntimeError("cache schema mismatch")

    monkeypatch.setattr(preflight.codex_runtime, "resolve_codex_runtime", _skew)
    result = preflight.preflight_check("codex")

    assert result["status"] == "fail"
    assert result["configured"] == "codex"
    assert result["configured_ok"] is False
    assert result["backends"]["claude"]["ok"] is True
    assert result["backends"]["codex"]["ok"] is False
    assert result["backends"]["codex"]["hint"] is not None
    # FALLBACK_PRIORITY = ("codex", "claude", "agy", "ao", "echo").
    # codex is missing and excluded; first present is claude.
    assert result["fallback_recommendation"] == "claude"
    # Codex skew must stop the wrapper before a subprocess launch.
    assert preflight.main(["--backend", "codex", "--json"]) == 2


def test_default_shadow_codex_skew_is_fatal_for_other_primary(
    monkeypatch, which_claude_only
):
    """The default shadow lane makes Codex part of a Claude configuration."""
    def _skew():
        raise preflight.codex_runtime.CodexRuntimeError("cache schema mismatch")

    monkeypatch.setattr(preflight.codex_runtime, "resolve_codex_runtime", _skew)

    result = preflight.preflight_check("claude")

    assert result["configured_ok"] is True
    assert result["status"] == "fail"
    assert "shadow Codex runtime rejected" in result["message"]


def test_explicit_no_codex_configuration_preserves_other_primary(
    monkeypatch, which_claude_only
):
    """Disabling the shadow lane keeps a non-Codex-only run usable."""
    def _skew():
        raise preflight.codex_runtime.CodexRuntimeError("cache schema mismatch")

    monkeypatch.setattr(preflight.codex_runtime, "resolve_codex_runtime", _skew)

    result = preflight.preflight_check("claude", shadow_codex=False)

    assert result["configured_ok"] is True
    assert result["status"] == "pass"
    assert result["message"] == "claude: ok"


@pytest.mark.parametrize("backend", ["echo", "mock_llm"])
def test_builtin_backend_suppresses_default_shadow_codex_requirement(
    backend, which_none
):
    """Built-in non-LLM backends never launch the default Codex shadow."""
    result = preflight.preflight_check(backend)

    assert result["configured_ok"] is True
    assert result["status"] == "pass"
    assert result["shadow_codex"] is False


def test_zero_backends_available_returns_fail(monkeypatch, which_none):
    """All probed CLIs missing => status=fail, exit code 2."""
    result = preflight.preflight_check("claude", shadow_codex=False)

    assert result["status"] == "fail"
    assert result["configured"] == "claude"
    assert result["configured_ok"] is False
    # echo is always ok
    assert result["backends"]["echo"]["ok"] is True
    # fallback falls through to echo
    assert result["fallback_recommendation"] == "echo"
    # Exit code mapping
    assert preflight.main(
        ["--backend", "claude", "--shadow-codex", "false", "--json"]
    ) == 2


def test_ao_configured_checks_sandbox_exec_transitive(
    monkeypatch, which_ao_no_sandbox
):
    """ao present, sandbox-exec missing => status=warn (ao usable, deps not)."""
    result = preflight.preflight_check("ao", shadow_codex=False)

    # ao is present, so configured_ok is True and status=pass
    # (transitive deps don't downgrade a *present* configured backend)
    assert result["configured_ok"] is True
    assert result["backends"]["ao"]["ok"] is True
    assert result["transitive"]["sandbox-exec"]["ok"] is False

    # Now flip the scenario: ao MISSING, sandbox-exec MISSING => fail
    # (no usable backend at all)
    monkeypatch.setattr(preflight, "_probe", lambda name: None)
    result2 = preflight.preflight_check("ao", shadow_codex=False)
    assert result2["status"] == "fail"
    assert result2["transitive"]["sandbox-exec"]["ok"] is False


def test_echo_backend_always_ok():
    """echo backend is always pass regardless of filesystem."""
    # No monkeypatch — _probe is called for non-echo entries too, but
    # echo is short-circuited before any probe.
    result = preflight.preflight_check("echo", shadow_codex=False)
    assert result["status"] == "pass"
    assert result["backends"]["echo"]["ok"] is True
    assert preflight.main(
        ["--backend", "echo", "--shadow-codex", "false", "--json"]
    ) == 0


def test_fallback_priority_order(monkeypatch):
    """First present in FALLBACK_PRIORITY wins."""
    # Only agy present
    monkeypatch.setattr(
        preflight,
        "_probe",
        lambda name: "/usr/local/bin/agy" if name == "agy" else None,
    )
    result = preflight.preflight_check("claude", shadow_codex=False)
    assert result["status"] == "warn"
    # FALLBACK_PRIORITY = ("codex", "claude", "agy", "ao", "echo")
    # configured=claude missing; first present alt in priority order is agy.
    assert result["fallback_recommendation"] == "agy"


# ---------------------------------------------------------------------------
# Smoke test — invoke as a real subprocess
# ---------------------------------------------------------------------------


def test_subprocess_emits_valid_json(tmp_path):
    """Running `python -m runner.preflight --json` emits valid JSON."""
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner.preflight",
            "--backend",
            "echo",
            "--shadow-codex",
            "false",
            "--json",
        ],
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

    sanitized_env = {
        "PATH": str(tmp_path),
        "HOME": os.environ.get("HOME", "/tmp"),
        "PYTHONPATH": str(
            pathlib.Path(__file__).resolve().parent.parent
        ),
    }
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner.preflight",
            "--backend",
            "claude",
            "--shadow-codex",
            "false",
            "--json",
        ],
        cwd=str(pathlib.Path(__file__).resolve().parent.parent),
        capture_output=True,
        text=True,
        timeout=30,
        env=sanitized_env,
    )
    assert proc.returncode == 2, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "fail"
