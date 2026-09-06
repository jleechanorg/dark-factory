"""Backend registry extension point for downstream snap_factory.

Downstream forks (e.g. snap_factory) register additional LLM backends by
name here; the runner looks them up AFTER built-in branches resolve so a
registered name cannot shadow a built-in.

Contract:
  * ``_BUILTIN_BACKEND_NAMES`` is the closed set of names the runner
    handles natively. The drift guard in
    ``tests/test_backend_registry_drift_guard.py`` fails CI if a
    ``backend == "X"`` / ``backend in {"X", ...}`` literal appears in
    the dispatch ladders without extending this set first.
  * ``register_backend(name, gate_args, gate_env)`` raises
    ``ValueError`` if ``name`` is a built-in — built-in dispatch ladders
    are sealed; downstream forks must use a fresh name.
  * ``get_backend(name)`` returns ``None`` for any built-in name (the
    dispatch ladder short-circuits to its legacy branch) and the
    registered hook pair for any other name.
  * Registered hooks must respect the runner's isolation guarantees —
    see ``tests/test_backend_registry_isolation.py``.
"""
from __future__ import annotations

from typing import Callable, Optional

_BUILTIN_BACKEND_NAMES: frozenset[str] = frozenset(
    {"echo", "claude", "codex", "ao", "agy", "mock_llm", "minimax", "claude-sonnet"}
)


GateArgsFn = Callable[..., Optional[list[str]]]
GateEnvFn = Callable[[str], dict[str, str]]


class _BackendHook:
    __slots__ = ("gate_args", "gate_env")

    def __init__(self, *, gate_args: GateArgsFn, gate_env: GateEnvFn) -> None:
        self.gate_args = gate_args
        self.gate_env = gate_env


_REGISTRY: dict[str, _BackendHook] = {}


def register_backend(name: str, *, gate_args: GateArgsFn, gate_env: GateEnvFn) -> None:
    if name in _BUILTIN_BACKEND_NAMES:
        raise ValueError(
            f"{name!r} is a built-in backend name; choose a different name. "
            f"Built-ins: {sorted(_BUILTIN_BACKEND_NAMES)}"
        )
    _REGISTRY[name] = _BackendHook(gate_args=gate_args, gate_env=gate_env)


def get_backend(name: str) -> Optional[_BackendHook]:
    return _REGISTRY.get(name)


def registered_names() -> frozenset[str]:
    return frozenset(_REGISTRY.keys())


def reset_for_tests() -> None:
    _REGISTRY.clear()


__all__ = [
    "_BUILTIN_BACKEND_NAMES",
    "GateArgsFn",
    "GateEnvFn",
    "register_backend",
    "get_backend",
    "registered_names",
    "reset_for_tests",
]