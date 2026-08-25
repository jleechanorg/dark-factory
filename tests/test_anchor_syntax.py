"""Tests for Item B: Anchor-comment syntax validation and code standards.

Validates that:
1. Anchor comments in files match the language's native comment syntax:
   - Rust (.rs): `//` (NEVER `#`)
   - Python (.py): `#` (NEVER `//`)
   - Shell (.sh): `#` (NEVER `//`)
   - SQL (.sql): `--` (NEVER `#` or `//`)
   - YAML (.yml, .yaml): `#` (NEVER `//`)
   - JS/TS (.js, .ts, .mjs): `//` (NEVER `#`)
2. `docs/code-standards.md` exists and contains the required anchor comment syntax rules.
3. `scripts/check_anchor_comment_syntax.py` correctly detects and rejects malformed anchor comments.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
CHECK_SCRIPT = REPO_ROOT / "scripts" / "check_anchor_comment_syntax.py"
CODE_STANDARDS_DOC = REPO_ROOT / "docs" / "code-standards.md"


def test_code_standards_doc_exists_and_documents_anchor_pattern() -> None:
    """Verify docs/code-standards.md exists and documents the anchor comment rule."""
    assert CODE_STANDARDS_DOC.exists(), (
        f"docs/code-standards.md must exist at {CODE_STANDARDS_DOC}"
    )
    content = CODE_STANDARDS_DOC.read_text(encoding="utf-8")
    assert "anchor comments must use the language's comment syntax" in content.lower() or (
        "anchor comment" in content.lower() and "//" in content and "#" in content
    ), "docs/code-standards.md must document anchor comment syntax conventions"
    assert "Rust" in content and "//" in content, "Must document // for Rust"
    assert "Python" in content and "#" in content, "Must document # for Python"
    assert "shell" in content.lower() and "#" in content, "Must document # for shell"


def test_validator_rejects_hash_comment_in_rust(tmp_path: pathlib.Path) -> None:
    """Rust files with `# PR #N` anchor comments must be rejected."""
    assert CHECK_SCRIPT.exists(), f"{CHECK_SCRIPT} must exist"
    rs_file = tmp_path / "test.rs"
    rs_file.write_text("fn main() {}\n# PR #666\n", encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(rs_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0, "Validator must reject '#' comment in Rust file"
    assert "Rust" in proc.stdout or "Rust" in proc.stderr or "syntax" in proc.stdout or "syntax" in proc.stderr


def test_validator_accepts_slash_comment_in_rust(tmp_path: pathlib.Path) -> None:
    """Rust files with `// PR #N` anchor comments must be accepted."""
    assert CHECK_SCRIPT.exists(), f"{CHECK_SCRIPT} must exist"
    rs_file = tmp_path / "test.rs"
    rs_file.write_text("fn main() {}\n// PR #666\n", encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(rs_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"Validator must accept '//' comment in Rust: {proc.stdout} {proc.stderr}"


@pytest.mark.parametrize(
    "line",
    ["let value = 1; # PR #666\n", "#[cfg(test)] # PR #666\n"],
)
def test_validator_rejects_inline_hash_anchor_in_rust(
    tmp_path: pathlib.Path, line: str
) -> None:
    rs_file = tmp_path / "test.rs"
    rs_file.write_text(line, encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(rs_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0


def test_validator_accepts_inline_slash_anchor_in_rust(tmp_path: pathlib.Path) -> None:
    rs_file = tmp_path / "test.rs"
    rs_file.write_text("let value = 1; // PR #666\n", encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(rs_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr


def test_validator_rejects_inline_bare_anchor_after_statement(
    tmp_path: pathlib.Path,
) -> None:
    rs_file = tmp_path / "test.rs"
    rs_file.write_text("let value = 1; PR #666\n", encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(rs_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0


def test_generic_anchor_word_in_string_is_not_a_marker(tmp_path: pathlib.Path) -> None:
    shell_file = tmp_path / "test.sh"
    shell_file.write_text('echo "// anchor text"\n', encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(shell_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr


@pytest.mark.parametrize(
    "line",
    [
        'message = "// PR #666"\n',
        "message = '# PR #666'\n",
        'message = "let value = 1; PR #666"\n',
    ],
)
def test_anchor_examples_inside_python_strings_are_not_comments(
    tmp_path: pathlib.Path, line: str
) -> None:
    py_file = tmp_path / "test.py"
    py_file.write_text(line, encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(py_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr


def test_validator_rejects_slash_comment_in_python(tmp_path: pathlib.Path) -> None:
    """Python files with `// PR #N` anchor comments must be rejected."""
    assert CHECK_SCRIPT.exists(), f"{CHECK_SCRIPT} must exist"
    py_file = tmp_path / "test.py"
    py_file.write_text("def foo():\n    pass\n// PR #666\n", encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(py_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0, "Validator must reject '//' comment in Python file"


def test_validator_accepts_hash_comment_in_python(tmp_path: pathlib.Path) -> None:
    """Python files with `# PR #N` anchor comments must be accepted."""
    assert CHECK_SCRIPT.exists(), f"{CHECK_SCRIPT} must exist"
    py_file = tmp_path / "test.py"
    py_file.write_text("def foo():\n    pass\n# PR #666\n", encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(py_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"Validator must accept '#' in Python: {proc.stdout} {proc.stderr}"


def test_validator_rejects_slash_comment_in_shell(tmp_path: pathlib.Path) -> None:
    """Shell files with `// PR #N` anchor comments must be rejected."""
    assert CHECK_SCRIPT.exists(), f"{CHECK_SCRIPT} must exist"
    sh_file = tmp_path / "test.sh"
    sh_file.write_text("#!/bin/bash\n// PR #666\n", encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(sh_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0, "Validator must reject '//' in shell script"


def test_validator_accepts_dash_comment_in_sql(tmp_path: pathlib.Path) -> None:
    """SQL files with `-- PR #N` anchor comments must be accepted."""
    assert CHECK_SCRIPT.exists(), f"{CHECK_SCRIPT} must exist"
    sql_file = tmp_path / "test.sql"
    sql_file.write_text("SELECT 1;\n-- PR #666\n", encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(sql_file)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"Validator must accept '--' in SQL: {proc.stdout} {proc.stderr}"


@pytest.mark.parametrize("suffix", [".c", ".h", ".cpp", ".hpp"])
def test_validator_rejects_hash_anchor_in_c_family(
    tmp_path: pathlib.Path, suffix: str
) -> None:
    source = tmp_path / f"anchor{suffix}"
    source.write_text("# PR #742\n", encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(source)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0


def test_validator_rejects_bare_markdown_anchor(tmp_path: pathlib.Path) -> None:
    markdown = tmp_path / "evidence.md"
    markdown.write_text("PR #742\n", encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(markdown)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0


def test_validator_accepts_markdown_comment_anchor(tmp_path: pathlib.Path) -> None:
    markdown = tmp_path / "evidence.md"
    markdown.write_text("<!-- PR #742 -->\n", encoding="utf-8")
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--file", str(markdown)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr


def test_diff_option_is_implemented() -> None:
    proc = subprocess.run(
        [sys.executable, str(CHECK_SCRIPT), "--diff", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr


def test_web_advice_failopen_e2e_log_section_5_triages_all_candidates() -> None:
    """Verify docs/web-advice-failopen-e2e-log.md §5 triages all Lane E/F candidates (A-D)."""
    e2e_log = REPO_ROOT / "docs" / "web-advice-failopen-e2e-log.md"
    assert e2e_log.exists(), f"{e2e_log} must exist"
    content = e2e_log.read_text(encoding="utf-8")
    assert "## 5. Operator actions" in content, "Must contain section 5"
    assert "### 5.2 Lane E/F Remediation Candidates" in content, "Must contain section 5.2 triage"

    # Candidate A
    assert "Candidate A" in content or "**A**" in content
    assert "runner outage" in content.lower()
    assert "FIX" in content

    # Candidate B
    assert "Candidate B" in content or "**B**" in content
    assert "anchor-comment" in content.lower() or "anchor comment" in content.lower()
    assert "docs/code-standards.md" in content

    # Candidate C
    assert "Candidate C" in content or "**C**" in content
    assert "ACCEPT-AS-DEGRADED" in content
    assert "external_ref" in content

    # Candidate D
    assert "Candidate D" in content or "**D**" in content
    assert "Evidence Gate workflow" in content or "commit message" in content
