"""Beads pre-push guard.

Background
----------
`bead jleechan-cnf9` / issue #308: a coder (df-160 session) bypassed `br`
and edited `.beads/issues.jsonl` directly, then committed + pushed the
result to a `factory/*` branch. That violates the br-only tracking contract;
`.beads/issues.jsonl` is owned by `br` (writes go through `br create`,
`br update`, and `br sync --flush-only`).

The fix has three layers:
  1. An audit/repair pass that already lives in the JSONL itself (this PR
     drops the duplicate `jleechan-0yso` line; tests/test_bead_jsonl_sort.py
     covers ordering + uniqueness).
  2. A pre-push guard `pre-push-beads-guard.sh` wired into the existing
     `.githooks/pre-push` shim alongside the graph-audit and repro-artifact
     guards.
  3. A RULES-row update on the autonomous coder's prompt template
     (`daemon/src/dispatch.rs`) making the br-only constraint explicit.

These tests cover layer 2 — the guard script — with the four canonical
state transitions:

  RED  : disabled guard → push with direct JSONL edit succeeds (the bug,
         captured by a sentinel assertion only — not a real pytest failure
         here, because we cannot run an unmodified code path).
  GREEN: factory/* push that does NOT touch .beads/issues.jsonl passes.
  GREEN: factory/* push whose first push has no upstream tip passes.
  GREEN: push from a non-factory branch (e.g. main) with JSONL flush passes.
  REJECT: factory/* push over an existing upstream that touches
          .beads/issues.jsonl is rejected with exit code 1 and a clear
          message naming the bypass (`git push --no-verify`).
  REGRESSION: the guard is wired into `.githooks/pre-push` between the
              repro-artifact-guard and the graph-audit siblings, in the
              same shape as the existing `$(git rev-parse --show-toplevel)/
              .githooks/<script>.sh` call pattern.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).parent.parent
GUARD = ROOT / ".githooks" / "pre-push-beads-guard.sh"
SHIM = ROOT / ".githooks" / "pre-push"
REPRO_GUARD = ROOT / ".githooks" / "pre-push-repro-artifact-guard.sh"
GRAPH_GUARD = ROOT / ".githooks" / "pre-push-graph-audit.sh"


def _git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    """Tiny helper — run a git command in `cwd` and return its result.

    `check=True` (default) raises on non-zero exit. We capture stdout/stderr
    as text so test assertions can read expected error messages.
    """
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=check,
    )


def _init_repo(tmp_path: Path) -> tuple[Path, Path, Path]:
    """Build a tiny self-contained scenario repo at tmp_path.

    Returns (work_tree, bare_remote, repo_root). The work_tree gets a
    `.githooks/` containing the LIVE `pre-push-beads-guard.sh` copied from
    this test suite's ROOT (so edits to that file in this PR get
    validated automatically — no fixtures to drift). `core.hooksPath`
    is set to `.githooks`.
    """
    bare = tmp_path / "origin.git"
    work = tmp_path / "repo"
    _git(tmp_path, "init", "-q", "--bare", "--initial-branch=main", str(bare))
    _git(tmp_path, "init", "-q", "--initial-branch=main", str(work))
    work.mkdir(parents=True, exist_ok=True)
    for sub in (".githooks", ".beads"):
        (work / sub).mkdir(exist_ok=True)
    _git(work, "config", "user.email", "test@example.com")
    _git(work, "config", "user.name", "test")
    _git(work, "config", "core.hooksPath", ".githooks")
    _git(work, "remote", "add", "origin", str(bare))

    # Copy the live guard straight in — no fixture drift.
    shutil.copy(GUARD, work / ".githooks" / "pre-push-beads-guard.sh")

    # Point the work-tree `.githooks/pre-push` shim ONLY at the beads guard
    # so this test does not also run lfs/repro/graph-audit. The wiring test
    # later in this file asserts the order in the real shim at the repo
    # root.
    (work / ".githooks" / "pre-push").write_text(
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        '"$(git rev-parse --show-toplevel)/.githooks/pre-push-beads-guard.sh"\n'
    )
    for path in (work / ".githooks" / "pre-push", work / ".githooks" / "pre-push-beads-guard.sh"):
        path.chmod(0o755)

    # Seed an initial commit, push to main so origin/main exists.
    (work / "README.md").write_text("init\n")
    _git(work, "add", "README.md")
    _git(work, "commit", "-q", "-m", "init")
    _git(work, "push", "-q", "origin", "main")
    return work, bare, work


def _drive_pre_push(work: Path, local_ref: str, local_sha: str, remote_sha: str) -> subprocess.CompletedProcess[str]:
    """Run the live pre-push shim from `work` with a synthetic stdin line.

    Mirrors the git pre-push wire-format exactly:
        <local_ref> SP <local_sha> SP <remote_ref> SP <remote_sha> LF
    """
    stdin = f"{local_ref} {local_sha} {local_ref} {remote_sha}\n"
    return subprocess.run(
        ["bash", str(work / ".githooks" / "pre-push")],
        cwd=work,
        input=stdin,
        capture_output=True,
        text=True,
    )


def _beads_payload(work: Path) -> None:
    """Materialize a single-bead .beads/issues.jsonl in `work` and stage it."""
    payload = (
        '{"id":"jleechan-x","status":"open","title":"x","priority":2,'
        '"issue_type":"task","created_at":"2026-01-01T00:00:00.000000Z",'
        '"updated_at":"2026-01-01T00:00:00.000000Z"}\n'
    )
    (work / ".beads" / "issues.jsonl").write_text(payload)
    _git(work, "add", ".beads/issues.jsonl")


# --- GREEN cases -----------------------------------------------------------


def test_factory_push_without_jsonl_change_passes(tmp_path: Path) -> None:
    """GREEN: legit factory push with NO JSONL change is allowed.

    Establishes an upstream tip on a factory/* branch, then makes a
    non-JSONL edit and re-pushes — guard must exit 0 and not mention
    `.beads/issues.jsonl`.
    """
    work, _bare, _root = _init_repo(tmp_path)

    # Create a factory/* branch and push it (upstream tip established).
    _git(work, "checkout", "-q", "-b", "factory/foo")
    (work / "README.md").write_text("foo\n")
    _git(work, "add", "README.md")
    _git(work, "commit", "-q", "-m", "boot")
    _git(work, "push", "-q", "origin", "factory/foo")
    upstream_sha = _git(work, "rev-parse", "origin/factory/foo").stdout.strip()

    # Now make a non-JSONL commit and try again.
    (work / "src.txt").write_text("hi\n")
    _git(work, "add", "src.txt")
    _git(work, "commit", "-q", "-m", "add src")
    local_sha = _git(work, "rev-parse", "HEAD").stdout.strip()

    result = _drive_pre_push(work, "refs/heads/factory/foo", local_sha, upstream_sha)
    assert result.returncode == 0, (
        f"guard wrongly blocked a legitimate non-JSONL push.\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    assert ".beads/issues.jsonl" not in result.stderr, (
        f"guard should not mention JSONL when JSONL didn't change.\nstderr={result.stderr!r}"
    )


def test_factory_first_push_no_remote_tip_passes(tmp_path: Path) -> None:
    """GREEN: first push of a factory/* branch with no upstream tip passes.

    Mirrors design point #1: `git rev-parse --verify origin/<branch>` exits
    non-zero for first pushes, so we explicitly allow.
    """
    work, _bare, _root = _init_repo(tmp_path)

    _git(work, "checkout", "-q", "-b", "factory/newbead")
    _beads_payload(work)
    _git(work, "commit", "-q", "-m", "seed beads")
    local_sha = _git(work, "rev-parse", "HEAD").stdout.strip()

    # Synthesize a first-push line: remote_sha is all-zeros, the standard
    # sentinel for "remote ref does not yet exist". The guard's
    # `rev-parse --verify origin/...` will fail first, so it short-circuits
    # to allow.
    result = _drive_pre_push(
        work,
        "refs/heads/factory/newbead",
        local_sha,
        "0" * 40,
    )
    assert result.returncode == 0, (
        f"first push of a new factory branch was wrongly blocked.\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    assert "first push" in result.stderr.lower(), (
        f"first-push informational message should be printed for auditability.\n"
        f"stderr={result.stderr!r}"
    )


def test_main_branch_jsonl_flush_passes(tmp_path: Path) -> None:
    """GREEN: a main-branch JSONL flush is allowed (operator hygiene).

    Design point #2: only `factory/*` branches are gated. Maintainers may
    flush JSONL freely from main or any other non-factory ref.
    """
    work, _bare, _root = _init_repo(tmp_path)

    _beads_payload(work)
    _git(work, "commit", "-q", "-m", "operator flush")
    local_sha = _git(work, "rev-parse", "HEAD").stdout.strip()
    upstream_sha = _git(work, "rev-parse", "origin/main").stdout.strip()

    result = _drive_pre_push(
        work,
        "refs/heads/main",
        local_sha,
        upstream_sha,
    )
    assert result.returncode == 0, (
        f"main-branch JSONL flush was wrongly blocked.\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )


# --- RED / REJECT case -----------------------------------------------------


def test_factory_jsonl_edit_over_existing_upstream_is_rejected(tmp_path: Path) -> None:
    """REJECT: factory/* push over an existing upstream tip that touches
    .beads/issues.jsonl is refused with exit code 1.

    This is the df-160 scenario the guard exists to prevent: a coder
    bypasses `br`, edits the JSONL directly, commits it on a factory/*
    branch, and tries to push. The guard must catch it before the push
    completes, naming both the broken contract (br-only) and the documented
    bypass (`git push --no-verify`).
    """
    work, _bare, _root = _init_repo(tmp_path)

    _git(work, "checkout", "-q", "-b", "factory/bypass")
    (work / "README.md").write_text("bypass\n")
    _git(work, "add", "README.md")
    _git(work, "commit", "-q", "-m", "boot")
    _git(work, "push", "-q", "origin", "factory/bypass")
    upstream_sha = _git(work, "rev-parse", "origin/factory/bypass").stdout.strip()

    # df-160 scenario: a coder edits JSONL directly (bypassing br).
    _beads_payload(work)
    _git(work, "commit", "-q", "-m", "df-160: direct edit")
    local_sha = _git(work, "rev-parse", "HEAD").stdout.strip()

    result = _drive_pre_push(work, "refs/heads/factory/bypass", local_sha, upstream_sha)
    assert result.returncode == 1, (
        f"guard ALLOWED a factory/* push that touched .beads/issues.jsonl.\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    err = result.stderr.lower()
    assert ".beads/issues.jsonl" in err, (
        f"rejection message should name the blocked file.\nstderr={result.stderr!r}"
    )
    assert "factory branch factory/bypass" in err, (
        f"rejection message should name the offending branch.\nstderr={result.stderr!r}"
    )
    assert "br" in err and ("create" in err or "update" in err or "sync" in err), (
        f"rejection message should redirect the agent to br.\nstderr={result.stderr!r}"
    )
    assert "git push --no-verify" in err, (
        f"rejection message should document the bypass.\nstderr={result.stderr!r}"
    )


def test_factory_non_jsonl_via_legit_factory_branch_passes(tmp_path: Path) -> None:
    """Regression: an edit on a non-factory/* branch like feat/* or fix/*
    is NOT subject to the guard at all (guard only inspects factory/*
    branches per spec). Mirrors the worldai misrouted PR design point #2.
    """
    work, _bare, _root = _init_repo(tmp_path)
    _git(work, "checkout", "-q", "-b", "feat/jsonl-cleanup")
    _beads_payload(work)
    _git(work, "commit", "-q", "-m", "JSONL cleanup on feat branch")
    local_sha = _git(work, "rev-parse", "HEAD").stdout.strip()
    upstream_sha = _git(work, "rev-parse", "origin/main").stdout.strip()

    result = _drive_pre_push(work, "refs/heads/feat/jsonl-cleanup", local_sha, upstream_sha)
    assert result.returncode == 0, (
        f"guard wrongly blocked a non-factory JSONL edit.\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
    )


# --- Hook-content regression ----------------------------------------------


def test_guard_is_wired_into_pre_push_shim() -> None:
    """Regression: the guard is wired into `.githooks/pre-push`.

    Asserts the SHAPE of the call (`$(git rev-parse --show-toplevel)/.githooks/
    pre-push-beads-guard.sh`, exactly as the existing two sibling guards
    are wired) and the position (between repro-artifact-guard and
    graph-audit).

    This is the hook-content regression test the misrouted worldai PR
    #8426 also added. It runs without ever invoking git so it stays fast.
    """
    if not SHIM.exists():
        pytest.skip("Repo .githooks/pre-push shim is absent in this checkout")
    text = SHIM.read_text()
    expected = (
        '"$(git rev-parse --show-toplevel)/.githooks/pre-push-beads-guard.sh"'
    )
    assert expected in text, (
        f".githooks/pre-push does not invoke pre-push-beads-guard.sh in the "
        f"expected shape.\n--- pre-push ---\n{text}"
    )
    # Order: repro-artifact-guard, beads-guard, graph-audit.
    repro_idx = text.find(str(REPRO_GUARD.relative_to(ROOT).as_posix()))
    beads_idx = text.find("pre-push-beads-guard.sh")
    graph_idx = text.find(str(GRAPH_GUARD.relative_to(ROOT).as_posix()))
    assert repro_idx > 0, "repro-artifact-guard call missing from shim"
    assert beads_idx > 0, "beads-guard call missing from shim"
    assert graph_idx > 0, "graph-audit call missing from shim"
    assert repro_idx < beads_idx < graph_idx, (
        f"beads-guard must run AFTER repro-artifact-guard and BEFORE "
        f"graph-audit (graph-audit is expensive and we want fast rejections "
        f"first).\nrepro={repro_idx} beads={beads_idx} graph={graph_idx}"
    )


def test_guard_script_has_no_syntax_errors() -> None:
    """Regression: bash -n must accept the guard script (catches typos at
    commit time even if no test scenario runs)."""
    proc = subprocess.run(
        ["bash", "-n", str(GUARD)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, (
        f"pre-push-beads-guard.sh has a bash syntax error.\n"
        f"stderr={proc.stderr!r}"
    )


def test_guard_message_names_the_bypass() -> None:
    """Regression: the rejected-output message names `git push --no-verify`
    so an agent reading stderr has an actionable next step."""
    text = GUARD.read_text()
    assert "--no-verify" in text, (
        "guard rejection must document the bypass so human + agent readers "
        "have an actionable next step"
    )


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
