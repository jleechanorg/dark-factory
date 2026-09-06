"""Round-3 carry — backend registry routing tests.

Pins: ``mock_llm`` in built-ins; built-in names reject registration;
built-ins resolve to None; the sealed-workdir and legacy paths in
``_gate_subprocess_args`` route a registered name through the runner's
sandbox builders rather than the catch-all Claude line.

Isolation guarantees live in ``test_backend_registry_isolation.py``.
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


def _register(name: str) -> None:
    backend_registry.register_backend(
        name,
        gate_args=lambda *a, **kw: ["custom-cli"],
        gate_env=lambda b: {},
    )


def test_mock_llm_is_in_builtin_set():
    assert "mock_llm" in backend_registry._BUILTIN_BACKEND_NAMES


def test_builtin_set_matches_expected_known_backends():
    assert backend_registry._BUILTIN_BACKEND_NAMES == EXPECTED_BUILTINS


@pytest.mark.parametrize("name", sorted(EXPECTED_BUILTINS))
def test_register_backend_rejects_builtin_name(name):
    with pytest.raises(ValueError, match="built-in backend name"):
        _register(name)


def test_get_backend_returns_none_for_builtin():
    for name in EXPECTED_BUILTINS:
        assert backend_registry.get_backend(name) is None


def test_register_and_resolve_custom_backend():
    captured = {}

    def args(backend, prompt, ctx, timeout, *, workdir=None):
        captured["argv"] = (backend, prompt, workdir)
        return ["custom-cli", "--flag", prompt]

    def env(backend):
        captured["env_backend"] = backend
        return {"CUSTOM_VAR": "value"}

    backend_registry.register_backend("snap_factory", gate_args=args, gate_env=env)
    hook = backend_registry.get_backend("snap_factory")
    assert hook is not None
    assert hook.gate_args("snap_factory", "hello", None, 30, workdir=None) == [
        "custom-cli", "--flag", "hello",
    ]
    assert hook.gate_env("snap_factory") == {"CUSTOM_VAR": "value"}
    assert captured == {"argv": ("snap_factory", "hello", None), "env_backend": "snap_factory"}


def test_registered_names_lists_only_custom_backends():
    _register("alpha")
    _register("beta")
    assert backend_registry.registered_names() == frozenset({"alpha", "beta"})


def test_reset_for_tests_clears_registry():
    _register("transient")
    assert backend_registry.get_backend("transient") is not None
    backend_registry.reset_for_tests()
    assert backend_registry.get_backend("transient") is None


def test_gate_subprocess_args_routes_registered_backend_through_sealed_builder():
    import runner.handlers as handlers

    captured = []

    def fake_sealed_args_for_workdir(argv, workdir):
        captured.append((list(argv), workdir))
        return ["sandbox-exec", "wrapped", *argv]

    _register("snap_factory")
    backend_registry.register_backend(
        "snap_factory",
        gate_args=lambda *a, **kw: ["custom-cli", "--prompt", "the prompt"],
        gate_env=lambda b: {},
    )

    original = getattr(handlers, "_sandboxed_args_for_workdir", None)
    handlers._sandboxed_args_for_workdir = fake_sealed_args_for_workdir
    try:
        result = _dispatch._gate_subprocess_args(
            "snap_factory", "the prompt", ctx=_FakeContext(), timeout=30,
            workdir=pathlib.Path("/tmp/fake-workdir"),
        )
    finally:
        if original is not None:
            handlers._sandboxed_args_for_workdir = original

    assert result == ["sandbox-exec", "wrapped", "custom-cli", "--prompt", "the prompt"]
    assert captured == [(["custom-cli", "--prompt", "the prompt"], pathlib.Path("/tmp/fake-workdir"))]


def test_gate_subprocess_args_routes_registered_backend_through_legacy_sandbox():
    import runner.handlers as handlers

    captured = []

    def fake_sandboxed_args(argv):
        captured.append(list(argv))
        return ["sandbox-exec", *argv]

    backend_registry.register_backend(
        "snap_factory",
        gate_args=lambda *a, **kw: ["custom-cli", "--prompt", "the prompt"],
        gate_env=lambda b: {},
    )

    original = getattr(handlers, "_sandboxed_args", None)
    handlers._sandboxed_args = fake_sandboxed_args
    try:
        result = _dispatch._gate_subprocess_args(
            "snap_factory", "the prompt", ctx=_FakeContext(), timeout=30,
        )
    finally:
        if original is not None:
            handlers._sandboxed_args = original

    assert result == ["sandbox-exec", "custom-cli", "--prompt", "the prompt"]
    assert captured == [["custom-cli", "--prompt", "the prompt"]]


def test_gate_subprocess_args_builtin_falls_through_to_legacy_branch():
    import runner.handlers as handlers

    call_log = []

    def fake_sandboxed_args(argv):
        call_log.append("sandboxed_args")
        return ["sandbox-exec", *argv]

    backend_registry.register_backend(
        "snap_factory",
        gate_args=lambda *a, **kw: (call_log.append("registered_hook"), ["custom-cli"])[1],
        gate_env=lambda b: {},
    )

    original = getattr(handlers, "_sandboxed_args", None)
    handlers._sandboxed_args = fake_sandboxed_args
    try:
        result = _dispatch._gate_subprocess_args(
            "claude", "the prompt", ctx=_FakeContext(), timeout=30,
        )
    finally:
        if original is not None:
            handlers._sandboxed_args = original

    assert result is not None
    assert call_log == ["sandboxed_args"]


class _FakeContext:
    workdir: pathlib.Path = pathlib.Path("/tmp/fake-workdir")
    state: dict = {}