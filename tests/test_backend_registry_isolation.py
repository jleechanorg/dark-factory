"""Round-4 isolation tests — registry hooks MUST NOT bypass runner isolation.

The round-3 reviewer (Opus, /tmp/advice-brs/results-v3/opus.txt) found
that ``_gate_subprocess_env`` was using the registered ``gate_env`` hook
return as a complete replacement for ``_sanitized_env()`` and passing it
straight through to ``env=`` at handler_dispatch.py:966-970 — so a
downstream hook could ship ``DARK_FACTORY_HOLDOUTS`` / ``*HOLDOUT*``
into a reviewer subprocess and bypass the jleechan-113 holdout
isolation guarantee.

These tests prove:

  1. ``_gate_subprocess_env(backend)`` for a registered backend strips
     any key matching ``*HOLDOUT*`` from the hook return — even if the
     hook tries to leak it.
  2. The result is layered on top of ``_sanitized_env()``, so the
     runner's existing sanitization still applies.
  3. ``probe_bin`` does NOT silently fall back to the claude binary
     when given a registered backend name — it surfaces a clear error
     so the failure mode is visible.
"""
from __future__ import annotations

import pathlib

import pytest

from runner import backend_registry
from runner import handler_dispatch as _dispatch


EXPECTED_BUILTINS = frozenset(
    {"echo", "claude", "codex", "ao", "agy", "mock_llm", "minimax", "claude-sonnet"}
)


@pytest.fixture(autouse=True)
def _reset_registry():
    backend_registry.reset_for_tests()
    yield
    backend_registry.reset_for_tests()


def _register_with_env(env: dict[str, str]) -> None:
    backend_registry.register_backend(
        "leaky",
        gate_args=lambda *a, **kw: ["custom-cli"],
        gate_env=lambda b: env,
    )


def test_gate_env_dark_factory_holdouts_is_stripped(monkeypatch):
    """A hook that returns ``DARK_FACTORY_HOLDOUTS`` MUST NOT leak it
    into the reviewer subprocess env."""
    base_env = {"PATH": "/usr/bin", "HOME": "/root"}
    monkeypatch.setattr(
        "runner.handlers._sanitized_env", lambda: dict(base_env)
    )
    _register_with_env(
        {"DARK_FACTORY_HOLDOUTS": "/leak", "BENIGN_VAR": "ok"}
    )
    env = _dispatch._gate_subprocess_env("leaky")
    assert "DARK_FACTORY_HOLDOUTS" not in env, (
        f"Holdout leak: gate_env returned DARK_FACTORY_HOLDOUTS to "
        f"the reviewer subprocess. Env: {env!r}"
    )
    assert "BENIGN_VAR" in env and env["BENIGN_VAR"] == "ok"
    assert env["PATH"] == "/usr/bin"


def test_gate_env_any_holdout_key_is_stripped(monkeypatch):
    """Any key whose name contains HOLDOUT (case-insensitive) is stripped."""
    base_env = {"PATH": "/usr/bin"}
    monkeypatch.setattr(
        "runner.handlers._sanitized_env", lambda: dict(base_env)
    )
    _register_with_env(
        {
            "DARK_FACTORY_HOLDOUTS": "/leak",
            "SNAP_HOLDOUT_PATH": "/leak2",
            "my_holdout": "/leak3",
            "HOLDOUTS": "/leak4",
            "SAFE": "ok",
        }
    )
    env = _dispatch._gate_subprocess_env("leaky")
    for leaked in ("DARK_FACTORY_HOLDOUTS", "SNAP_HOLDOUT_PATH",
                   "my_holdout", "HOLDOUTS"):
        assert leaked not in env, (
            f"Holdout-leak class bug: {leaked!r} survived in env {env!r}"
        )
    assert env["SAFE"] == "ok"
    assert env["PATH"] == "/usr/bin"


def test_gate_env_builtin_backend_unaffected(monkeypatch):
    """The hook-strip only applies when a hook exists; built-ins use
    ``_sanitized_env()`` unchanged."""
    base_env = {"PATH": "/usr/bin"}
    monkeypatch.setattr(
        "runner.handlers._sanitized_env", lambda: dict(base_env)
    )
    env = _dispatch._gate_subprocess_env("claude")
    assert env == base_env
    # minimax adds the gateway URL on top
    env = _dispatch._gate_subprocess_env("minimax")
    assert env["ANTHROPIC_BASE_URL"] == "https://api.minimax.io/anthropic"
    assert env["PATH"] == "/usr/bin"


def test_probe_bin_rejects_registered_backend(monkeypatch):
    """``probe_bin`` MUST NOT silently fall back to the claude binary
    for a registered name — the failure mode must be visible. This
    test calls the production guard
    (``_probe_registered_backend_guard``) rather than reimplementing
    the condition, so any future change to the production guard is
    caught here."""
    class _ShadowStub:
        launch_error = None
        proc = None

    shadow = _ShadowStub()
    _register_with_env({})
    # Patch shutil.which to FAIL so we can prove the guard short-circuits
    # BEFORE the probe_bin path is reached.
    import shutil as _shutil
    def _which_should_not_be_called(_name):
        raise AssertionError(
            "probe_bin must not call shutil.which for a registered backend"
        )
    monkeypatch.setattr(_shutil, "which", _which_should_not_be_called)
    # Invoke the production guard directly.
    short_circuited = _dispatch._probe_registered_backend_guard("leaky", shadow)
    assert short_circuited is True
    assert shadow.launch_error == "leaky registered backend; probe_bin not supported"


def test_gate_subprocess_args_gate_args_exception_returns_none(monkeypatch):
    """A buggy ``gate_args`` hook MUST NOT crash the runner — the
    caller sees ``None`` (sandbox-unavailable), which the gate
    handler maps to a structured ``outcome='error'`` Result that
    ``_is_gate_infra_failure`` classifies for the fallback path."""
    base_env = {"PATH": "/usr/bin"}
    monkeypatch.setattr(
        "runner.handlers._sanitized_env", lambda: dict(base_env)
    )
    monkeypatch.setattr(
        "runner.handlers._sandboxed_args", lambda argv: list(argv)
    )
    monkeypatch.setattr(
        "runner.handlers._sandboxed_args_for_workdir",
        lambda argv, workdir: list(argv),
    )

    def _exploding_hook(*a, **kw):
        raise RuntimeError("buggy downstream hook")

    backend_registry.register_backend(
        "boom",
        gate_args=_exploding_hook,
        gate_env=lambda b: {},
    )
    # Sealed-workdir path: returns None on exception.
    sentinel_workdir = pathlib.Path("/tmp/sentinel-workdir")
    sealed_result = _dispatch._gate_subprocess_args(
        "boom", "prompt", ctx=None, timeout=300, workdir=sentinel_workdir
    )
    assert sealed_result is None
    # Legacy path (workdir=None): also returns None on exception.
    legacy_result = _dispatch._gate_subprocess_args(
        "boom", "prompt", ctx=None, timeout=300
    )
    assert legacy_result is None


def test_gate_subprocess_env_gate_env_exception_returns_base(monkeypatch):
    """A buggy ``gate_env`` hook returns the sanitized base env
    unchanged (no hook override applied)."""
    base_env = {"PATH": "/usr/bin"}
    monkeypatch.setattr(
        "runner.handlers._sanitized_env", lambda: dict(base_env)
    )

    def _exploding_hook(_backend):
        raise RuntimeError("buggy downstream hook")

    backend_registry.register_backend(
        "boom-env",
        gate_args=lambda *a, **kw: ["custom-cli"],
        gate_env=_exploding_hook,
    )
    env = _dispatch._gate_subprocess_env("boom-env")
    assert env == base_env, (
        f"Hook exception should fall back to base env, got {env!r}"
    )
