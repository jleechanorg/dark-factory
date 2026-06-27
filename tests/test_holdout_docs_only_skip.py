"""Docs-only PR skip path for the sealed holdout evaluator.

Lane-I of the 2026-06-27 audit ships a docs-only change (markdown-only
diff). The holdout handler now detects that pattern via ``git diff
origin/main...HEAD --name-only`` and exits success with a redacted
verdict, rather than failing with "no feature attribute or state" when
no ``--feature`` was supplied.

These tests pin both branches:
  * Docs-only workdir → success + skipped=docs-only metadata.
  * Code-touching workdir → original "no feature attribute or state"
    failure is preserved (so the fix loop still gets a real failure for
    behavioral PRs that legitimately forgot ``--feature``).
"""
from __future__ import annotations

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

# Import via the assembled shim so the circular runner.handlers ↔
# runner.handler_holdout import is already resolved (handlers.py pulls
# the symbols in at module load).
from runner.handlers import Context, Node, _holdout_eval  # noqa: E402
from runner.handler_holdout import (  # noqa: E402
    _DOCS_ONLY_EXTS,
    _workdir_diff_is_docs_only,
)


def _make_commit_worktree(tmp_path: pathlib.Path, files: list[str]) -> pathlib.Path:
    """Build a git worktree where ``git diff origin/main...HEAD`` returns
    only ``files``. Uses a real bare remote so ``origin/main`` resolves
    under the triple-dot merge-base syntax.
    """
    base = tmp_path / "repo"
    base.mkdir()
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(base)], check=True)
    work = tmp_path / "wt"
    subprocess.run(["git", "clone", "-q", str(base), str(work)], check=True)
    subprocess.run(["git", "config", "user.email", "t@t"], cwd=work, check=True)
    subprocess.run(["git", "config", "user.name", "t"], cwd=work, check=True)
    # Seed main with a placeholder so origin/main is non-empty.
    (work / "README.md").write_text("seed\n")
    subprocess.run(["git", "add", "-A"], cwd=work, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "init"], cwd=work, check=True)
    subprocess.run(["git", "push", "-q", "origin", "main"], cwd=work, check=True)
    # Feature branch diverged from main.
    subprocess.run(["git", "checkout", "-q", "-b", "feat"], cwd=work, check=True)
    for rel in files:
        p = work / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("hello\n")
    subprocess.run(["git", "add", "-A"], cwd=work, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "feat"], cwd=work, check=True)
    return work


def test_docs_only_extension_set():
    """Pinned set — adding a new docs extension is a deliberate policy change."""
    assert ".md" in _DOCS_ONLY_EXTS
    assert ".markdown" in _DOCS_ONLY_EXTS
    assert ".txt" in _DOCS_ONLY_EXTS
    assert ".rst" in _DOCS_ONLY_EXTS
    # Anything executable / code-shaped is NOT in the set.
    assert ".py" not in _DOCS_ONLY_EXTS
    assert ".ts" not in _DOCS_ONLY_EXTS
    assert ".json" not in _DOCS_ONLY_EXTS
    assert ".sh" not in _DOCS_ONLY_EXTS


def test_workdir_diff_is_docs_only_true(tmp_path):
    work = _make_commit_worktree(
        tmp_path,
        [
            "benchmarks/airbnb-clone/visible_acceptance.md",
            "benchmarks/amazon-clone/spec.md",
            "docs/notes.txt",
        ],
    )
    assert _workdir_diff_is_docs_only(work) is True


def test_workdir_diff_is_docs_only_false_for_code(tmp_path):
    work = _make_commit_worktree(
        tmp_path,
        [
            "benchmarks/airbnb-clone/visible_acceptance.md",
            "runner/new_code.py",  # mixed diff → not docs-only
        ],
    )
    assert _workdir_diff_is_docs_only(work) is False


def test_workdir_diff_is_docs_only_false_for_json(tmp_path):
    """Configs / data files (e.g. package.json, tsconfig.json) are NOT docs."""
    work = _make_commit_worktree(
        tmp_path,
        ["package.json"],
    )
    assert _workdir_diff_is_docs_only(work) is False


def test_workdir_diff_is_docs_only_false_when_no_origin(tmp_path):
    """If git can't find origin/main, the helper must return False (not crash)
    so non-docs PRs still hit the normal failure path."""
    work = tmp_path / "wt"
    work.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main", str(work)], check=True)
    # No remote configured.
    assert _workdir_diff_is_docs_only(work) is False


def test_holdout_eval_skips_on_docs_only(tmp_path):
    """The handler returns success+skipped=docs-only when feature is missing
    AND the workdir diff is docs-only. This is the lane-I unblock path."""
    work = _make_commit_worktree(
        tmp_path,
        ["benchmarks/amazon-clone/spec.md"],
    )
    node = Node(name="holdout", attrs={"type": "holdout_eval"})
    ctx = Context(goal="docs-only PR", workdir=work, backend="echo")
    # No --feature injected → ctx.state["feature"] is empty.
    result = _holdout_eval(node, ctx)
    assert result.outcome == "success", result.output
    payload = json.loads(result.output)
    assert payload["verdict"] == "pass"
    assert payload["skipped"] == "docs-only"
    assert result.metadata.get("skipped") == "docs-only"


def test_holdout_eval_still_fails_when_feature_missing_and_code_changes(tmp_path):
    """Regression: code-changing PRs that forget --feature must still fail
    with the original 'no feature attribute or state' message, so the fix
    loop can correct the missing arg."""
    work = _make_commit_worktree(
        tmp_path,
        ["runner/new_module.py"],
    )
    node = Node(name="holdout", attrs={"type": "holdout_eval"})
    ctx = Context(goal="code PR", workdir=work, backend="echo")
    result = _holdout_eval(node, ctx)
    assert result.outcome == "failure", result.output
    assert result.output == "no feature attribute or state"