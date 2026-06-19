"""Regression test for jleechan-z84.

Locks down the contract that ``runner.__main__`` uses
``PANIC_EXIT_CODE`` (from :mod:`runner.panic_hook`) as the single
source of truth for the panic exit code. Pre-fix, ``runner/__main__.py``
hard-coded the string literal ``"128"`` in the crash-event JSON
metadata and the CXDB ``returncode`` field, AND returned the bare
integer ``128`` from :func:`main` — all three drifted from
``PANIC_EXIT_CODE = 124``.

This test fails if any of the following regresses:

1. ``runner.__main__`` does not import ``PANIC_EXIT_CODE`` from
   ``runner.panic_hook``.
2. The crash-event JSON written via :func:`_append_event` uses a
   hard-coded ``"128"`` string instead of ``str(PANIC_EXIT_CODE)``.
3. The CXDB ``returncode`` metadata field uses a hard-coded ``"128"``
   string instead of ``str(PANIC_EXIT_CODE)``.
4. :func:`runner.__main__.main` returns a hard-coded ``128`` instead
   of :data:`runner.panic_hook.PANIC_EXIT_CODE`.

The pre-existing ``tests/test_panic_hook.py`` already pins
``PANIC_EXIT_CODE == 124`` as the default; this test pins the
"single source of truth" contract across the panic-hook / main
boundary specifically.
"""

from __future__ import annotations

import importlib
import re
from pathlib import Path

import pytest

from runner import panic_hook


REPO_ROOT = Path(__file__).resolve().parent.parent
MAIN_PY = REPO_ROOT / "runner" / "__main__.py"


def _read_main_source() -> str:
    return MAIN_PY.read_text(encoding="utf-8")


def test_main_imports_panic_exit_code() -> None:
    """runner.__main__ must import PANIC_EXIT_CODE from runner.panic_hook."""
    main_mod = importlib.import_module("runner.__main__")
    assert hasattr(main_mod, "PANIC_EXIT_CODE"), (
        "runner.__main__ must expose PANIC_EXIT_CODE imported from "
        "runner.panic_hook (jleechan-z84 single-source-of-truth contract)."
    )
    assert main_mod.PANIC_EXIT_CODE is panic_hook.PANIC_EXIT_CODE, (
        "runner.__main__.PANIC_EXIT_CODE must be the SAME object as "
        "runner.panic_hook.PANIC_EXIT_CODE — never re-defined or "
        "re-imported from elsewhere."
    )


def test_main_source_has_no_hardcoded_128_in_metadata_path() -> None:
    """The panic-metadata code paths in runner/__main__.py must not
    embed the string ``"128"`` as a literal — they must derive the
    value from ``str(PANIC_EXIT_CODE)``.

    Scope: this is a *targeted* assertion. The full file may legally
    mention ``128`` in unrelated contexts (docstring examples, other
    comments). We narrow to:
      * the CXDB ``returncode`` field
      * the crash-event ``exit_code`` field
    and we check the *non-imported* form does not appear in those
    neighbouring lines.
    """
    src = _read_main_source()

    # Extract an *indented* call (not a `def`) of the named function. We
    # anchor on a leading newline + whitespace so we skip the function
    # definition line. Matches both `name(` and `.name(` call forms.
    def _extract_call(src: str, name: str) -> str | None:
        pattern = re.compile(
            rf"^[ \t]+(?:\w+\.)?{re.escape(name)}\(",
            flags=re.MULTILINE,
        )
        m = pattern.search(src)
        if not m:
            return None
        idx = m.start()
        # Walk forward, counting parens, to find the matching close.
        depth = 0
        for i in range(idx, len(src)):
            ch = src[i]
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    return src[idx : i + 1]
        return None

    crash_block_src = _extract_call(src, "_append_event")
    assert crash_block_src is not None, (
        "Could not locate the crash-event _append_event call in "
        "runner/__main__.py — the test fixture may be stale."
    )
    assert '"event": "crash"' in crash_block_src, (
        "_append_event call is no longer the crash-event emitter; "
        "update the regression test fixture."
    )
    assert "str(PANIC_EXIT_CODE)" in crash_block_src, (
        "crash-event JSON in runner/__main__.py must use "
        "str(PANIC_EXIT_CODE) — not the string literal '128'."
    )
    assert '"128"' not in crash_block_src, (
        "crash-event JSON in runner/__main__.py must not embed the "
        "string literal '128' (jleechan-z84 drift)."
    )

    cxdb_block_src = _extract_call(src, "record_step")
    assert cxdb_block_src is not None, (
        "Could not locate the CXDB record_step call in "
        "runner/__main__.py — fixture may be stale."
    )
    assert '"returncode"' in cxdb_block_src, (
        "record_step call is no longer the panic emitter; "
        "update the regression test fixture."
    )
    assert "str(PANIC_EXIT_CODE)" in cxdb_block_src, (
        "CXDB returncode metadata must use str(PANIC_EXIT_CODE)."
    )
    assert '"128"' not in cxdb_block_src, (
        "CXDB returncode metadata must not embed the string literal '128'."
    )


def test_main_top_level_returns_panic_exit_code() -> None:
    """The bare `return 128` at the bottom of main() must be
    `return PANIC_EXIT_CODE` (jleechan-z84: also drift)."""
    src = _read_main_source()
    # The literal `return 128\n` would only exist on the panic branch.
    # We assert the panic branch ends with `return PANIC_EXIT_CODE`.
    assert re.search(r"return\s+PANIC_EXIT_CODE\s*$", src, flags=re.MULTILINE), (
        "runner/__main__.py must end its panic branch with "
        "`return PANIC_EXIT_CODE` (no bare `return 128`)."
    )
    assert not re.search(r"return\s+128\s*$", src, flags=re.MULTILINE), (
        "runner/__main__.py must not contain a bare `return 128` — "
        "use `return PANIC_EXIT_CODE` so the metadata and the actual "
        "exit code never drift."
    )


def test_panic_exit_code_default_value_is_124() -> None:
    """Bead z84 explicitly notes: the 124 default is preserved by
    option (a) — the test suite that pins 124 stays green."""
    # Allow override only via the documented env var.
    import os

    if "DARK_FACTORY_PANIC_EXIT_CODE" in os.environ:
        pytest.skip("DARK_FACTORY_PANIC_EXIT_CODE override is set")

    assert panic_hook.PANIC_EXIT_CODE == 124, (
        "Default PANIC_EXIT_CODE drift detected — bead z84 chose "
        "option (a) to preserve the 124 timeout-killed grouping "
        "rationale documented in runner/panic_hook.py:60-71."
    )
