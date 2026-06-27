"""Regression tests for sealed-benchmark-doc deny rules (jleechan-113).

The implementing agent's filesystem view MUST NOT include operator-only
docs that live under ``benchmarks/<name>/``. The runner enforces this
via ``_sandboxed_args_for_workdir`` in ``runner/handler_sandbox.py``,
which extends the sandbox-exec deny rules to cover ``README.md``,
``DESIGN.md``, ``SCORING.md``, and ``SCENARIOS.md`` under every
``benchmarks/*/`` directory inside the implementing agent's workdir.

These tests pin three contracts:

  1. ``_sealed_benchmark_doc_paths`` enumerates the operator-only docs
     present under a worktree's ``benchmarks/`` subtree.
  2. ``_sandboxed_args_for_workdir`` adds a ``deny file-read*`` rule
     for every such doc — the deny rule's subpath matches the resolved
     absolute path of the operator-only doc.
  3. A stray README.md containing holdout-path strings (an
     adversarial probe pattern) is denied by the sandbox profile.

Lock-in for the 2026-06-27 prompt-domain-agnostic audit. File-disjoint:
new test file. Only imports from ``runner.handlers`` (the public shim).
"""

from __future__ import annotations

import os
import pathlib
import shutil
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.handlers import (  # noqa: E402
    _sealed_benchmark_doc_paths,
    _sandboxed_args_for_workdir,
)


SEALED_DOC_NAMES = ("README.md", "DESIGN.md", "SCORING.md", "SCENARIOS.md")


# ---------------------------------------------------------------------------
# (1) _sealed_benchmark_doc_paths: enumerates operator-only docs
# ---------------------------------------------------------------------------


def _make_benchmark_worktree(tmp_path: pathlib.Path) -> pathlib.Path:
    """Build a worktree-shaped fixture: benchmarks/<name>/ with operator docs.

    Mirrors the actual repo shape (two benchmarks, one with all four sealed
    docs, one with only README.md) plus a visible_acceptance.md that is
    NOT in the deny list.
    """
    bench_a = tmp_path / "benchmarks" / "fibonacci"
    bench_b = tmp_path / "benchmarks" / "amazon-clone"
    bench_a.mkdir(parents=True)
    bench_b.mkdir(parents=True)
    # fibonacci has README + DESIGN + visible_acceptance (no SCORING/SCENARIOS).
    (bench_a / "README.md").write_text(
        "# Fibonacci README\n\nOperator-only design notes.\n"
    )
    (bench_a / "DESIGN.md").write_text(
        "# Fibonacci DESIGN\n\nSealed design notes for the operator.\n"
    )
    (bench_a / "visible_acceptance.md").write_text(
        "# Visible Acceptance\n\nThe agent IS allowed to read this.\n"
    )
    (bench_a / "spec.md").write_text(
        "# Spec\n\nThe agent IS allowed to read this.\n"
    )
    # amazon-clone has all four sealed docs.
    for name in SEALED_DOC_NAMES:
        (bench_b / name).write_text(f"# amazon-clone {name}\n")
    return tmp_path


def test_sealed_doc_paths_finds_all_operator_only_files(tmp_path):
    """Every operator-only doc under benchmarks/*/ is enumerated."""
    worktree = _make_benchmark_worktree(tmp_path)

    paths = _sealed_benchmark_doc_paths(worktree)

    found = {p.name for p in paths}
    # All four sealed doc names are covered across both benchmark dirs.
    assert "README.md" in found
    assert "DESIGN.md" in found
    assert "SCORING.md" in found
    assert "SCENARIOS.md" in found
    # The visible docs are NOT enumerated.
    assert "visible_acceptance.md" not in found
    assert "spec.md" not in found


def test_sealed_doc_paths_returns_absolute_paths(tmp_path):
    """Every enumerated path is absolute (so the sandbox subpath deny works)."""
    worktree = _make_benchmark_worktree(tmp_path)

    paths = _sealed_benchmark_doc_paths(worktree)

    assert paths, "expected at least one sealed doc path"
    for p in paths:
        assert p.is_absolute(), f"path is not absolute: {p}"


def test_sealed_doc_paths_empty_for_worktree_without_benchmarks(tmp_path):
    """A worktree without a benchmarks/ subtree returns an empty list."""
    # tmp_path is empty — no benchmarks/ subdir.
    paths = _sealed_benchmark_doc_paths(tmp_path)
    assert paths == []


def test_sealed_doc_paths_handles_empty_and_none(tmp_path):
    """None / empty workdir returns empty list (defense in depth)."""
    assert _sealed_benchmark_doc_paths(None) == []
    assert _sealed_benchmark_doc_paths("") == []


def test_sealed_doc_paths_skips_benchmarks_files(tmp_path):
    """A regular file directly under benchmarks/ (not a subdir) is skipped."""
    bench_dir = tmp_path / "benchmarks"
    bench_dir.mkdir()
    # A loose file at benchmarks/foo.md is NOT a benchmark subdir, skip.
    (bench_dir / "foo.md").write_text("not a benchmark")
    paths = _sealed_benchmark_doc_paths(tmp_path)
    assert paths == []


# ---------------------------------------------------------------------------
# (2) _sandboxed_args_for_workdir: deny rules cover the operator-only docs
# ---------------------------------------------------------------------------


def test_sandboxed_args_for_workdir_denies_sealed_readme(tmp_path):
    """The sandbox profile includes a deny rule on the stray README.md.

    The implementing agent cannot read an operator-only README.md that
    lives under benchmarks/<name>/ in its workdir.
    """
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec unavailable (non-macOS host)")
    worktree = _make_benchmark_worktree(tmp_path)

    args = _sandboxed_args_for_workdir(["claude", "--print"], worktree)
    assert args is not None, "sandboxed_args_for_workdir returned None"
    # Argv layout: [sandbox-exec, -p, profile, ...real args]
    assert args[0].endswith("sandbox-exec")
    assert args[1] == "-p"
    profile = args[2]
    real_args = args[3:]

    # The legacy holdout deny rule is still present (regression guard for
    # the AO worker contract — the sibling repo deny must not be dropped).
    assert "dark-factory-holdouts" in profile
    assert "(deny file-read*" in profile

    # Every sealed doc we planted shows up as a deny rule in the profile.
    # The path on disk is what `subpath "<path>"` matches in the sandbox.
    for p in _sealed_benchmark_doc_paths(worktree):
        assert f'"{p}"' in profile, (
            f"missing deny rule for sealed doc {p!s} in sandbox profile:\n{profile}"
        )

    # The original argv is preserved after the sandbox wrapper.
    assert real_args == ["claude", "--print"]


def test_sandboxed_args_for_workdir_disabled_when_disable_sandbox_set(tmp_path, monkeypatch):
    """`DISABLE_SANDBOX=1` short-circuits to the raw argv (testing escape hatch)."""
    monkeypatch.setenv("DISABLE_SANDBOX", "1")
    worktree = _make_benchmark_worktree(tmp_path)
    args = _sandboxed_args_for_workdir(["claude", "--print"], worktree)
    # When disabled, the wrapper returns the raw argv (no sandbox-exec prefix).
    assert args == ["claude", "--print"]


def test_sandboxed_args_for_workdir_handles_missing_workdir():
    """An empty / None workdir yields a profile with no extra deny rules.

    Same legacy behavior as ``_sandboxed_args`` — guards against a forged
    workdir causing deny rules to be emitted to attacker-controlled paths.
    """
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec unavailable (non-macOS host)")
    args = _sandboxed_args_for_workdir(["claude", "--print"], None)
    assert args is not None
    profile = args[2]
    # Legacy holdout deny is still present.
    assert "dark-factory-holdouts" in profile
    # No benchmark-doc deny rules were added (the empty workdir short-circuits).
    for sealed in SEALED_DOC_NAMES:
        # If a benchmark-doc deny leaked, the profile would contain a
        # subpath with one of the sealed names. Match the pattern at the
        # subpath opener, which is the only place we'd see the name.
        assert f'(subpath "{sealed}")' not in profile, (
            f"unexpected benchmark deny rule for {sealed!r} when workdir was None"
        )


# ---------------------------------------------------------------------------
# (3) End-to-end: an adversarial stray README.md is denied
# ---------------------------------------------------------------------------


def _write_fake_claude_shim(shim_dir: pathlib.Path, target_readme: pathlib.Path) -> pathlib.Path:
    """Write a fake `claude` binary that tries to read the target README.

    On read success: emits "SEALED_LEAK <contents>" so the test can detect it.
    On read failure (sandbox deny): exits non-zero with no leakage.
    """
    shim = shim_dir / "claude"
    shim.write_text(
        "#!/bin/sh\n"
        # Attempt to read the operator-only doc. Under sandbox-exec deny,
        # `cat` returns EPERM (sandbox prints to stderr, cat exits non-zero).
        f"contents=$(cat '{target_readme!s}' 2>/dev/null) || "
        "{ echo 'SEALED_BLOCKED'; exit 7; }\n"
        "echo \"SEALED_LEAK $contents\"\n"
    )
    shim.chmod(shim.stat().st_mode | 0o755)
    return shim


def test_fake_claude_shim_cannot_read_sealed_readme_under_sandbox(tmp_path, monkeypatch):
    """A fake claude on PATH cannot read an operator-only benchmarks/<n>/README.md.

    This is the golden regression test for jleechan-113: an adversarial
    stray README.md containing holdout-path strings (e.g. "$DARK_FACTORY_HOLDOUTS"
    or scoring rubric content) MUST be unreachable to the implementing agent.
    """
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec unavailable (non-macOS host)")
    worktree = _make_benchmark_worktree(tmp_path)
    # The README under benchmarks/fibonacci contains operator-only content
    # (including a reference to a sealed path string, simulating an
    # adversarial probe that would leak holdout info into the impl).
    fib_readme = worktree / "benchmarks" / "fibonacci" / "README.md"
    fib_readme.write_text(
        "# Fibonacci README\n\n"
        "SEALED_PROBE: $DARK_FACTORY_HOLDOUTS/internal/fib/scoring-rubric.md\n"
        "SEALED_PROBE: ~/projects/dark-factory-holdouts/internal/fib/answer.txt\n"
    )

    shim_dir = tmp_path / "fake-bin"
    shim_dir.mkdir()
    _write_fake_claude_shim(shim_dir, fib_readme)
    monkeypatch.setenv("PATH", f"{shim_dir}:{os.environ.get('PATH', '')}")

    # Build the sandboxed argv via the public entry point (mirrors what
    # handler_codergen._codergen does for the claude backend).
    args = _sandboxed_args_for_workdir(["claude", "--print"], worktree)
    assert args is not None, "sandboxed_args_for_workdir returned None"

    import subprocess
    proc = subprocess.run(
        args,
        cwd=worktree,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    # The shim's `cat` must have failed under the sandbox deny → exit 7.
    # (We don't strictly require exit 7 — sandbox-exec may print its own
    # EPERM error to stderr before the shim runs. The hard assertion is
    # that the SEALED_LEAK marker did NOT appear.)
    combined = proc.stdout + "\n" + proc.stderr
    assert "SEALED_LEAK" not in combined, (
        f"sandbox failed to block sealed doc read — content leaked:\n{combined}"
    )
    assert "SEALED_PROBE" not in combined, (
        f"sandbox failed to block sealed doc read — content leaked:\n{combined}"
    )
    assert "scoring-rubric.md" not in combined