"""Regression tests for the handler_parallel_reviewer <-> handlers circular import.

Background
----------
``runner/handler_parallel_reviewer.py`` previously imported
``runner.handlers`` as ``_handlers_shim`` so tests/legacy code could keep
``monkeypatch.setattr("runner.handlers._X", ...)`` against the re-export
shim. That created a module-load cycle:

    runner.handlers -> runner.handler_parallel_reviewer -> runner.handlers

The cycle was hidden by pytest's collection order (every test file imports
``runner.handlers`` first via conftest, so the partial module is fully
populated before the verdict-consistency test re-imports
``handler_parallel_reviewer``). Standalone ``pytest <one_file>`` invocations
and direct `python -c "from runner.handler_parallel_reviewer import ..."``
failed with::

    ImportError: cannot import name '_parallel_reviewer' from partially
    initialized module 'runner.handler_parallel_reviewer' (most likely due
    to a circular import)

These tests prove the cycle is broken:

1. Direct import of ``_enforce_outcome_verdict_consistency`` succeeds with no
   pre-import of ``runner.handlers`` and no sys.path hacks.
2. The verdict-consistency test file loads without its
   "Import via handlers first to avoid circular import" workaround.
3. Direct import of ``_parallel_reviewer`` succeeds (the handler was the
   trigger name in the original ImportError).
4. The runtime resolution chain still works (``runner.handlers._parallel_reviewer``
   resolves to the same callable as ``runner.handler_parallel_reviewer._parallel_reviewer``),
   so existing test monkeypatching of the shim is preserved.

Note on test isolation: we use a ``subprocess`` to test the "direct import"
path so we don't have to mutate ``sys.modules`` (which would poison the
other tests in the same session by removing the ``runner.handlers`` shim
that ``monkeypatch.setattr("runner.handlers._X", ...)`` targets).
"""

from __future__ import annotations

import pathlib
import subprocess
import sys


def _run_in_clean_subprocess(snippet: str) -> subprocess.CompletedProcess:
    """Run ``snippet`` in a fresh interpreter and return the result.

    The snippet is appended to a header that puts the repo root on
    ``sys.path`` (same convention the other tests use) but does NOT
    pre-import ``runner.handlers``. If the circular import returns, the
    snippet's import raises ImportError and the subprocess exits non-zero.
    """
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    header = (
        "import sys, pathlib\n"
        f"sys.path.insert(0, {str(repo_root)!r})\n"
    )
    return subprocess.run(
        [sys.executable, "-c", header + snippet],
        capture_output=True,
        text=True,
        check=False,
    )


def test_direct_import_enforce_outcome_verdict_consistency():
    """A direct import must succeed in a fresh interpreter without first importing runner.handlers.

    Before the fix this raised ImportError because
    handler_parallel_reviewer.py imported runner.handlers, which in turn
    imported handler_parallel_reviewer to register _parallel_reviewer in
    TYPE_REGISTRY. Loading handler_parallel_reviewer standalone left the
    cycle half-initialized and the second leg failed.
    """
    proc = _run_in_clean_subprocess(
        "from runner.handler_parallel_reviewer import _enforce_outcome_verdict_consistency; "
        "print('OK')"
    )
    assert proc.returncode == 0, (
        f"direct import failed: stderr={proc.stderr!r}"
    )
    assert "OK" in proc.stdout


def test_direct_import_parallel_reviewer_handler():
    """The handler trigger name from the original ImportError must be importable.

    The cycle surface was specifically ``_parallel_reviewer``: handlers.py
    line 168 does ``from .handler_parallel_reviewer import (_parallel_reviewer,)``
    which is what raised the original ImportError when re-imported standalone.
    """
    proc = _run_in_clean_subprocess(
        "from runner.handler_parallel_reviewer import _parallel_reviewer; "
        "print('OK')"
    )
    assert proc.returncode == 0, (
        f"direct import failed: stderr={proc.stderr!r}"
    )
    assert "OK" in proc.stdout


def test_handlers_shim_still_resolves_parallel_reviewer():
    """Backward compatibility: legacy ``runner.handlers._parallel_reviewer`` must still resolve.

    Tests and external callers historically use
    ``monkeypatch.setattr("runner.handlers._parallel_reviewer", ...)`` to
    stub the handler. The re-export shim must keep working.
    """
    import runner.handlers
    import runner.handler_parallel_reviewer

    assert runner.handlers._parallel_reviewer is runner.handler_parallel_reviewer._parallel_reviewer


def test_handlers_shim_still_resolves_render_prompt():
    """Backward compatibility: legacy ``runner.handlers._render_prompt`` must still resolve."""
    import runner.handlers
    import runner.handler_render

    assert runner.handlers._render_prompt is runner.handler_render._render_prompt


def test_verdict_consistency_test_no_sys_path_hack():
    """The verdict-consistency test must pass standalone WITHOUT sys.path.insert(0, ROOT).

    Before the fix, that test was forced to do::

        ROOT = pathlib.Path(__file__).parent.parent
        sys.path.insert(0, str(ROOT))
        import runner.handlers  # forces full module init first
        from runner.handler_parallel_reviewer import _enforce_outcome_verdict_consistency

    That ``sys.path.insert`` is a sys.path hack (and the import ordering
    comment is the cycle workaround). After the fix, the test should be
    importable as plain ``from runner.handler_parallel_reviewer import ...``
    with no sys.path munging.
    """
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    test_file = repo_root / "tests" / "test_reviewer_outcome_verdict_consistency.py"
    assert test_file.exists(), f"missing test file: {test_file}"

    src = test_file.read_text()
    assert "sys.path.insert" not in src, (
        "test_reviewer_outcome_verdict_consistency.py still has a sys.path "
        "insert — the cycle workaround has not been removed."
    )
    assert "to avoid circular import" not in src, (
        "test_reviewer_outcome_verdict_consistency.py still documents the "
        "circular-import workaround — it should be removed after the cycle "
        "is broken."
    )


def test_handlers_module_imports_without_swallowing_import_errors():
    """Loading runner.handlers must not use try/except ImportError to mask cycles.

    Swallowing ImportError around module-import statements is the second
    common cycle workaround — the task explicitly forbids it. Verify the
    handler_parallel_reviewer source does not paper over ImportError.
    """
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    src = (repo_root / "runner" / "handler_parallel_reviewer.py").read_text()
    assert "except ImportError" not in src, (
        "handler_parallel_reviewer.py still swallows ImportError — the "
        "cycle must be broken structurally, not via exception handling."
    )