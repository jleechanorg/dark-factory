"""Tests for anchor comment syntax validation (Lane E/F remediation Candidate B).

Anchor comments must use the language's comment syntax:
- For Rust (*.rs): use `//`, NEVER `#` (which causes rustc syntax errors)
- For Python (*.py), Shell (*.sh), YAML (*.yml, *.yaml), TOML (*.toml): use `#`
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT_PATH = REPO_ROOT / "scripts" / "check_anchor_comments.py"


def test_script_exists_and_executable() -> None:
    assert SCRIPT_PATH.exists(), f"Missing script at {SCRIPT_PATH}"


def test_validate_rust_valid_comment() -> None:
    from scripts.check_anchor_comments import check_diff_lines

    # Valid Rust lines
    diff_lines = [
        ("daemon/src/adapters.rs", "+// PR #665: fix mergeable"),
        ("daemon/src/adapters.rs", "+    // bead jleechan-1234"),
        ("daemon/src/adapters.rs", "+#[derive(Debug)]"),
        ("daemon/src/adapters.rs", "+#![allow(unused)]"),
        ("daemon/src/adapters.rs", "+#[doc = \"some doc\"]"),
    ]
    violations = check_diff_lines(diff_lines)
    assert violations == []


def test_validate_rust_invalid_hash_comment() -> None:
    from scripts.check_anchor_comments import check_diff_lines

    # Invalid Rust lines using # instead of //
    diff_lines = [
        ("daemon/src/adapters.rs", "+# PR #665: fix mergeable"),
        ("daemon/src/verifier.rs", "+    # bead jleechan-qzr3"),
        ("daemon/src/tick.rs", "+# closes issue #670"),
    ]
    violations = check_diff_lines(diff_lines)
    assert len(violations) == 3
    assert "daemon/src/adapters.rs" in violations[0]
    assert "Rust" in violations[0]
    assert "//" in violations[0]


def test_validate_python_shell_valid_comment() -> None:
    from scripts.check_anchor_comments import check_diff_lines

    diff_lines = [
        ("runner/engine.py", "+# PR #665: fix mergeable"),
        ("scripts/check_runner.sh", "+# bead jleechan-123"),
        (".github/workflows/ci.yml", "+  # CI config"),
        ("daemon/Cargo.toml", "+# dependency comment"),
    ]
    violations = check_diff_lines(diff_lines)
    assert violations == []


def test_validate_python_shell_invalid_slash_comment() -> None:
    from scripts.check_anchor_comments import check_diff_lines

    diff_lines = [
        ("runner/engine.py", "+// PR #665: fix mergeable"),
        ("scripts/check_runner.sh", "+// bead jleechan-123"),
    ]
    violations = check_diff_lines(diff_lines)
    assert len(violations) == 2
    assert "runner/engine.py" in violations[0]
    assert "#" in violations[0]


def test_cli_execution_with_clean_git_diff() -> None:
    proc = subprocess.run(
        [sys.executable, str(SCRIPT_PATH), "--check-staged"],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
    )
    # Working tree is clean so should exit 0
    assert proc.returncode == 0
