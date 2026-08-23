#!/usr/bin/env python3
"""Check anchor comment syntax across staged or pushed files.

Anchor comments must use the language's comment syntax (Candidate B):
- For Rust (*.rs): use `//`, NEVER `#` (which causes rustc syntax errors)
- For Python (*.py), Shell (*.sh), YAML (*.yml, *.yaml), TOML (*.toml): use `#`, NEVER `//`
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys


# Rust attributes: #[...] or #![...]
RUST_ATTR_RE = re.compile(r"^\s*#\s*!?\s*\[")

# Extension groups
RUST_EXTS = {".rs"}
HASH_COMMENT_EXTS = {".py", ".sh", ".bash", ".zsh", ".yml", ".yaml", ".toml", ".ini"}


def check_diff_lines(diff_lines: list[tuple[str, str]]) -> list[str]:
    """Check a list of (filename, added_line) tuples for comment syntax violations."""
    violations: list[str] = []

    for file_path, line in diff_lines:
        path = pathlib.Path(file_path)
        ext = path.suffix.lower()
        content = line[1:] if line.startswith("+") else line
        trimmed = content.strip()

        if ext in RUST_EXTS:
            # Rust: if line starts with '#' and is NOT an attribute (#[ or #![), flag as error
            if trimmed.startswith("#") and not RUST_ATTR_RE.match(trimmed):
                violations.append(
                    f"{file_path}: invalid comment syntax '{trimmed}'. "
                    f"Rust anchor comments must use '//', not '#' (which breaks Rust compilation)."
                )
        elif ext in HASH_COMMENT_EXTS:
            # Python/Shell/YAML/TOML: if line starts with '//', flag as error
            if trimmed.startswith("//"):
                violations.append(
                    f"{file_path}: invalid comment syntax '{trimmed}'. "
                    f"Anchor comments in {ext} files must use '#', not '//'."
                )

    return violations


def extract_added_lines_from_diff(diff_text: str) -> list[tuple[str, str]]:
    """Parse git diff output into (file_path, added_line) tuples."""
    diff_lines: list[tuple[str, str]] = []
    current_file = ""

    for line in diff_text.splitlines():
        if line.startswith("+++ b/"):
            current_file = line[6:].strip()
        elif line.startswith("+++ "):
            current_file = line[4:].strip()
        elif line.startswith("+") and not line.startswith("+++"):
            if current_file:
                diff_lines.append((current_file, line))

    return diff_lines


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-staged", action="store_true", help="Check git staged changes")
    parser.add_argument("--diff-range", help="Git revision range to diff (e.g. origin/main...HEAD)")
    args = parser.parse_args(argv)

    if args.diff_range:
        cmd = ["git", "--no-pager", "diff", "-U0", args.diff_range]
    elif args.check_staged:
        cmd = ["git", "--no-pager", "diff", "--cached", "-U0"]
    else:
        # Default: staged + unstaged
        cmd = ["git", "--no-pager", "diff", "-U0", "HEAD"]

    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        # If HEAD doesn't resolve (e.g. empty repo or diff against empty tree), try git diff --cached
        proc = subprocess.run(["git", "--no-pager", "diff", "--cached", "-U0"], capture_output=True, text=True)
        if proc.returncode != 0:
            print(f"git diff failed: {proc.stderr.strip()}", file=sys.stderr)
            return 0

    diff_lines = extract_added_lines_from_diff(proc.stdout)
    violations = check_diff_lines(diff_lines)

    if violations:
        print("Anchor comment syntax check FAILED:", file=sys.stderr)
        for v in violations:
            print(f"  - {v}", file=sys.stderr)
        print(
            "\nHint: anchor comments must use the language's comment syntax:\n"
            "  - For Rust (*.rs): use '//'\n"
            "  - For Python (*.py), Shell (*.sh), YAML (*.yml), TOML (*.toml): use '#'\n",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
