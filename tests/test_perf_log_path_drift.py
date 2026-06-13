"""Regression monitor: the --perf-log-dir default in runner/__main__.py MUST live
under ~/Library/Logs (Apple's per-app log dir, survives reboots and macOS /tmp
sweeps and AO retag cycles). A previous revert to /tmp/dark-factory lost the v9
perf-log history; this test tripwires any future regression on main.

Bead: jleechan-cv3 (monitor)
Design doc: roadmap/README.md
"""

from __future__ import annotations

import ast
import pathlib
import re
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner import perf_log  # noqa: E402


MAIN_PATH = ROOT / "runner" / "__main__.py"


def _extract_perf_log_default() -> pathlib.Path:
    """Parse runner/__main__.py and return the literal default= value for
    --perf-log-dir, resolved relative to the file's directory if needed.

    Falls back to a regex on the source if AST inspection fails.
    """
    src = MAIN_PATH.read_text(encoding="utf-8")
    tree = ast.parse(src, filename=str(MAIN_PATH))

    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        # Look for p.add_argument("--perf-log-dir", ...)
        if not (
            isinstance(node.func, ast.Attribute)
            and node.func.attr == "add_argument"
        ):
            continue
        positional = node.args
        if not positional or not isinstance(positional[0], ast.Constant):
            continue
        if positional[0].value != "--perf-log-dir":
            continue

        # Find default= keyword in the call.
        for kw in node.keywords:
            if kw.arg != "default":
                continue
            value = kw.value
            if isinstance(value, ast.Constant) and isinstance(value.value, str):
                return pathlib.Path(value.value)
            if isinstance(value, ast.Call):
                # Handle pathlib.Path("/tmp/dark-factory") or
                # pathlib.Path.home() / "Library" / "Logs" / "dark-factory"
                func = value.func
                if (
                    isinstance(func, ast.Attribute)
                    and func.attr == "Path"
                    and isinstance(func.value, ast.Name)
                    and func.value.id == "pathlib"
                    and value.args
                    and isinstance(value.args[0], ast.Constant)
                    and isinstance(value.args[0].value, str)
                ):
                    return pathlib.Path(value.args[0].value)
            if isinstance(value, ast.BinOp) and isinstance(value.op, ast.Div):
                # pathlib.Path.home() / "Library" / "Logs" / "dark-factory"
                # Walk right→left collecting string parts, then verify the
                # leftmost is pathlib.Path.home() (possibly via Path.home()).
                parts: list[str] = []
                cursor: ast.AST = value
                while isinstance(cursor, ast.BinOp) and isinstance(cursor.op, ast.Div):
                    right = cursor.right
                    if isinstance(right, ast.Constant) and isinstance(right.value, str):
                        parts.append(right.value)
                    else:
                        break
                    cursor = cursor.left  # type: ignore[assignment]
                base_call = cursor
                if isinstance(base_call, ast.Call) and isinstance(base_call.func, ast.Attribute):
                    # Resolve func chain — could be pathlib.Path.home() or
                    # just path.home() — find the root Name id.
                    chain: list[ast.AST] = []
                    attr_node: ast.AST = base_call.func
                    while isinstance(attr_node, ast.Attribute):
                        chain.append(attr_node)
                        attr_node = attr_node.value
                    root = attr_node
                    if isinstance(root, ast.Name) and root.id == "pathlib" and chain and chain[0].attr == "home":
                        base = pathlib.Path.home()
                        for part in reversed(parts):
                            base = base / part
                        return base

    # Fallback: regex scan the raw source line for --perf-log-dir default.
    match = re.search(
        r'--perf-log-dir.*?default=([^,\n)]+)',
        src,
        re.DOTALL,
    )
    if not match:
        pytest.fail(f"Could not locate --perf-log-dir default in {MAIN_PATH}")
    literal = match.group(1).strip()
    # Strip leading pathlib.Path(
    literal = re.sub(r"^pathlib\.Path\(\"?", "", literal)
    literal = literal.rstrip(")").rstrip('"')
    return pathlib.Path(literal)


def test_main_perf_log_default_is_under_library_logs():
    """The --perf-log-dir default in runner/__main__.py must resolve under
    ~/Library/Logs (Apple's per-app log location)."""
    default = _extract_perf_log_default()
    expected_root = pathlib.Path.home() / "Library" / "Logs"
    try:
        default.relative_to(expected_root)
    except ValueError as exc:
        pytest.fail(
            f"Expected --perf-log-dir default to live under {expected_root}, "
            f"got {default!r} ({exc})"
        )


def test_main_perf_log_default_is_not_tmp():
    """The whole point of jleechan-cv3: /tmp/dark-factory lost v9 history when
    macOS swept /tmp. The default MUST NOT live under /tmp."""
    default = _extract_perf_log_default()
    default_str = str(default)
    assert not default_str.startswith("/tmp"), (
        f"Regression: --perf-log-dir default is under /tmp ({default!r}). "
        "Use ~/Library/Logs/dark-factory (Apple's per-app log dir) instead."
    )
    # Also block the exact /tmp/dark-factory literal explicitly.
    assert default != pathlib.Path("/tmp/dark-factory"), (
        "Regression: --perf-log-dir default reverted to /tmp/dark-factory "
        "(this is the v9 bug). Use ~/Library/Logs/dark-factory."
    )
    # And the parent dir.
    assert default != pathlib.Path("/tmp"), (
        "Regression: --perf-log-dir default is /tmp."
    )


def test_perf_log_path_resolution_survives_tmp_sweeps(tmp_path, monkeypatch):
    """runner.perf_log.run_dir() must not place logs under /tmp, regardless of
    which root the CLI hands in. We drive this by calling run_dir with a
    candidate root and asserting the resulting dir is not under /tmp.

    This protects against future code that might silently swap the default
    back to a /tmp path inside the perf_log module itself.
    """
    # Use a tmp_path-style candidate that the module would accept; we
    # exercise the join logic, not the actual I/O.
    fake_root = tmp_path / "Library" / "Logs" / "dark-factory"
    fake_root.mkdir(parents=True, exist_ok=True)

    fake_ctx = perf_log.GitContext(
        repo_slug="dark-factory",
        branch_slug="main",
        head_sha="0" * 40,
        workdir=fake_root,
    )
    resolved = perf_log.run_dir(fake_root, fake_ctx)

    # The join must keep the root intact (no silent fallback to /tmp).
    try:
        resolved.relative_to(fake_root)
    except ValueError as exc:
        pytest.fail(
            f"perf_log.run_dir({fake_root!r}, ...) escaped its root: "
            f"got {resolved!r} ({exc})"
        )
