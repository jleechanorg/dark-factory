#!/usr/bin/env python3
"""check_anchor_comment_syntax.py — Validate anchor comment syntax by file extension.

Rule (from docs/code-standards.md):
  Anchor comments must use the language's native comment syntax:
    - Rust (.rs): `//`
    - Python (.py): `#`
    - Shell (.sh, .bash, .zsh): `#`
    - YAML (.yml, .yaml): `#`
    - TOML (.toml): `#`
    - SQL (.sql): `--`
    - JavaScript / TypeScript (.js, .mjs, .ts): `//`
    - C / C++ (.c, .h, .cpp, .hpp): `//`
    - Markdown (.md): `<!-- ... -->` or `#`

Usage:
  python3 scripts/check_anchor_comment_syntax.py --file <path>
  python3 scripts/check_anchor_comment_syntax.py --staged
  python3 scripts/check_anchor_comment_syntax.py --diff [ref]
  python3 scripts/check_anchor_comment_syntax.py --check-doc
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys

# Match only concrete PR/evidence markers. The generic English word "anchor" is
# intentionally excluded: it appears legitimately in strings, prose, and heredocs.
ANCHOR_PATTERN = re.compile(r"(?:PR\s*#?\d+|Evidence:)", re.IGNORECASE)

# Mapping of file extension to allowed comment prefix tokens and human-readable name
SYNTAX_RULES: dict[str, dict[str, object]] = {
    ".rs": {
        "lang": "Rust",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".py": {
        "lang": "Python",
        "valid_prefixes": ("#",),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "# PR #<N>",
    },
    ".sh": {
        "lang": "Shell",
        "valid_prefixes": ("#",),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "# PR #<N>",
    },
    ".bash": {
        "lang": "Shell",
        "valid_prefixes": ("#",),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "# PR #<N>",
    },
    ".zsh": {
        "lang": "Shell",
        "valid_prefixes": ("#",),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "# PR #<N>",
    },
    ".yml": {
        "lang": "YAML",
        "valid_prefixes": ("#",),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "# PR #<N>",
    },
    ".yaml": {
        "lang": "YAML",
        "valid_prefixes": ("#",),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "# PR #<N>",
    },
    ".toml": {
        "lang": "TOML",
        "valid_prefixes": ("#",),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "# PR #<N>",
    },
    ".sql": {
        "lang": "SQL",
        "valid_prefixes": ("--", "/*"),
        "invalid_prefixes": ("#", "//"),
        "expected_example": "-- PR #<N>",
    },
    ".js": {
        "lang": "JavaScript",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".ts": {
        "lang": "TypeScript",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".mjs": {
        "lang": "JavaScript (ESM)",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".cjs": {
        "lang": "JavaScript (CJS)",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".c": {
        "lang": "C",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".h": {
        "lang": "C/C++ header",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".cpp": {
        "lang": "C++",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".hpp": {
        "lang": "C++ header",
        "valid_prefixes": ("//", "/*"),
        "invalid_prefixes": ("#", "--"),
        "expected_example": "// PR #<N>",
    },
    ".md": {
        "lang": "Markdown",
        "valid_prefixes": ("<!--", "#"),
        "invalid_prefixes": ("//", "--"),
        "expected_example": "<!-- PR #<N> -->",
    },
}


def _inside_quoted_string(text: str, index: int) -> bool:
    """Return whether ``index`` is inside a simple single/double-quoted string."""
    quote: str | None = None
    escaped = False
    for char in text[:index]:
        if escaped:
            escaped = False
            continue
        if char == "\\":
            escaped = True
            continue
        if quote is None and char in {"'", '"'}:
            quote = char
        elif quote == char:
            quote = None
    return quote is not None


def check_line(file_path: str, line_no: int, line_text: str) -> list[str]:
    """Check a single line in a file. Returns error messages if invalid."""
    errors: list[str] = []
    ext = pathlib.Path(file_path).suffix.lower()
    if ext not in SYNTAX_RULES:
        return errors

    rule = SYNTAX_RULES[ext]
    stripped = line_text.strip()
    if not stripped:
        return errors

    # Check if line contains an anchor comment pattern
    if not ANCHOR_PATTERN.search(stripped):
        return errors

    valid_prefixes: tuple[str, ...] = rule["valid_prefixes"]  # type: ignore
    invalid_prefixes: tuple[str, ...] = rule["invalid_prefixes"]  # type: ignore
    lang: str = rule["lang"]  # type: ignore
    expected_example: str = rule["expected_example"]  # type: ignore

    # Inspect the token immediately before every concrete marker, rather than
    # only the start of the line. This catches `let x = 1; # PR #742` and a
    # second invalid marker after a valid Rust attribute.
    for marker in ANCHOR_PATTERN.finditer(stripped):
        if _inside_quoted_string(stripped, marker.start()):
            continue
        prefix = stripped[: marker.start()].rstrip()
        token = next(
            (
                candidate
                for candidate in sorted(
                    set(valid_prefixes + invalid_prefixes), key=len, reverse=True
                )
                if prefix.endswith(candidate)
            ),
            None,
        )
        if token in invalid_prefixes:
            errors.append(
                f"{file_path}:{line_no}: Invalid comment syntax for {lang} file. "
                f"Line uses '{token}' comment syntax instead of '{valid_prefixes[0]}'. "
                f"Expected format: '{expected_example}' (found: {stripped!r})"
            )
            return errors
        if token in valid_prefixes:
            continue
        # Bare markers at the start of any line are malformed anchors. In code
        # files, also reject a bare marker appended after a statement/block
        # delimiter. Markdown prose such as "see PR #742" remains untouched.
        if marker.start() == 0 or (
            ext != ".md" and prefix and prefix[-1] in ";{}"
        ):
            errors.append(
                f"{file_path}:{line_no}: Anchor marker in {lang} file is missing "
                f"a comment delimiter. Expected format: '{expected_example}' "
                f"(found: {stripped!r})"
            )
            return errors

    return errors


def check_file(path: pathlib.Path) -> list[str]:
    """Check an entire file for invalid anchor comments."""
    errors: list[str] = []
    try:
        content = path.read_text(encoding="utf-8")
    except Exception as e:
        return [f"Could not read {path}: {e}"]

    for idx, line in enumerate(content.splitlines(), start=1):
        line_errors = check_line(str(path), idx, line)
        errors.extend(line_errors)
    return errors


def check_staged_diff() -> list[str]:
    """Check staged git changes for invalid anchor comments."""
    errors: list[str] = []
    proc = subprocess.run(
        ["git", "diff", "--cached", "-U0"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return [f"git diff --cached failed: {proc.stderr}"]

    current_file = None
    current_line_no = 0
    for line in proc.stdout.splitlines():
        if line.startswith("+++ b/"):
            current_file = line[6:]
        elif line.startswith("@@ "):
            # Parse line number from @@ -old,count +new,count @@
            m = re.search(r"\+(\d+)", line)
            if m:
                current_line_no = int(m.group(1)) - 1
        elif line.startswith("+") and not line.startswith("+++"):
            current_line_no += 1
            if current_file:
                added_text = line[1:]
                errors.extend(check_line(current_file, current_line_no, added_text))
    return errors


def check_diff(ref: str) -> list[str]:
    """Check added lines in the working-tree diff against ``ref``."""
    errors: list[str] = []
    proc = subprocess.run(
        ["git", "diff", "-U0", ref, "--"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return [f"git diff {ref} failed: {proc.stderr}"]

    current_file = None
    current_line_no = 0
    for line in proc.stdout.splitlines():
        if line.startswith("+++ b/"):
            current_file = line[6:]
        elif line.startswith("@@ "):
            match = re.search(r"\+(\d+)", line)
            if match:
                current_line_no = int(match.group(1)) - 1
        elif line.startswith("+") and not line.startswith("+++"):
            current_line_no += 1
            if current_file:
                errors.extend(check_line(current_file, current_line_no, line[1:]))
    return errors


def check_doc() -> list[str]:
    """Verify docs/code-standards.md exists and contains required rules."""
    doc_path = pathlib.Path(__file__).resolve().parents[1] / "docs" / "code-standards.md"
    if not doc_path.exists():
        return [f"Missing code standards doc at {doc_path}"]
    content = doc_path.read_text(encoding="utf-8")
    if "anchor comments must use the language's comment syntax" not in content.lower():
        if not ("anchor comment" in content.lower() and "//" in content and "#" in content):
            return ["docs/code-standards.md does not document anchor comment syntax"]
    return []


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate anchor comment syntax by file extension.")
    parser.add_argument("--file", type=pathlib.Path, help="Check a specific file.")
    parser.add_argument("--staged", action="store_true", help="Check staged git diff.")
    parser.add_argument(
        "--diff",
        nargs="?",
        const="HEAD",
        metavar="REF",
        help="Check added working-tree lines against REF (default: HEAD).",
    )
    parser.add_argument("--check-doc", action="store_true", help="Check code standards document.")
    args = parser.parse_args()

    all_errors: list[str] = []

    if args.file:
        all_errors.extend(check_file(args.file))
    elif args.staged:
        all_errors.extend(check_staged_diff())
    elif args.diff:
        all_errors.extend(check_diff(args.diff))
    elif args.check_doc:
        all_errors.extend(check_doc())
    else:
        # Default: check staged diff and doc
        all_errors.extend(check_doc())
        all_errors.extend(check_staged_diff())

    if all_errors:
        print("Anchor Comment Syntax Errors:", file=sys.stderr)
        for err in all_errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    print("Anchor comment syntax check: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
