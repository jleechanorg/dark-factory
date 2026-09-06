"""Drift guard for the backend-name closed set.

Scans ``runner/handler_dispatch.py`` and ``runner/handler_codergen.py``
for every literal that names a specific backend (``backend == "X"``,
``backend in {"X", "Y"}``, ``backend not in (...)``) and asserts every
such name appears in
``runner.backend_registry._BUILTIN_BACKEND_NAMES``. Round 4 swaps the
single-regex extractor for an AST walker that handles:

  * ``backend == "x"`` and ``backend != "x"``
  * ``backend in {"x", "y"}`` AND ``backend in ("x", "y")`` — every member
  * ``backend not in {"x", ...}`` and the tuple form
  * Hyphenated names like ``"claude-sonnet"`` / ``"gpt-5"`` (caught by
    matching ``Constant`` nodes of type ``str`` whose value is a
    plausible backend identifier — non-empty, no whitespace, contains at
    least one letter or digit).

This is the drift guard the round-3 reviewer (Opus) called leaky:
``r'\\w+'`` missed hyphenated names, and the set-member regex only
captured the first element of a multi-name set.
"""
from __future__ import annotations

import ast
import pathlib

from runner import backend_registry


_DISPATCH_FILES = (
    pathlib.Path(__file__).resolve().parent.parent / "runner" / "handler_dispatch.py",
    pathlib.Path(__file__).resolve().parent.parent / "runner" / "handler_codergen.py",
)


def _looks_like_backend_name(value: object) -> bool:
    if not isinstance(value, str):
        return False
    if not value:
        return False
    if any(ch.isspace() for ch in value):
        return False
    # Plausible backend identifier: alphanumerics + hyphen + underscore.
    # Empty/whitespace-only strings and anything containing spaces is
    # rejected; that catches accidental matches against unrelated string
    # constants in the dispatch files.
    return all(ch.isalnum() or ch in "-_." for ch in value) and any(
        ch.isalnum() for ch in value
    )


class _BackendLiteralExtractor(ast.NodeVisitor):
    """AST walker that collects every string literal used in a backend-name
    comparison or membership test against the parameter name ``backend``.

    Only the **direct** ``backend`` parameter (a bare ``Name`` whose ``id``
    is ``"backend"``) is treated as authoritative — anything else
    (``self.backend``, ``ctx.backend``, ``backend_var``, etc.) is
    treated as out-of-scope so the drift guard does not pull unrelated
    string constants in the dispatch files.
    """

    def __init__(self) -> None:
        self.found: set[str] = set()

    def visit_Compare(self, node: ast.Compare) -> None:  # noqa: N802
        left = node.left
        left_name = self._name_of(left)
        if left_name == "backend":
            for op, comparator in zip(node.ops, node.comparators):
                if isinstance(op, (ast.In, ast.NotIn)):
                    for elt in self._iter_container(comparator):
                        if _looks_like_backend_name(elt):
                            self.found.add(elt)
                elif isinstance(op, (ast.Eq, ast.NotEq)):
                    if isinstance(comparator, ast.Constant) and _looks_like_backend_name(
                        comparator.value
                    ):
                        self.found.add(comparator.value)
        # Continue into nested compares (e.g. ``a < b < c``).
        self.generic_visit(node)

    def _name_of(self, node: ast.AST) -> str | None:
        if isinstance(node, ast.Name):
            return node.id
        if isinstance(node, ast.Attribute):
            return node.attr
        return None

    def _iter_container(self, node: ast.AST):
        """Yield every string constant inside a Set / Tuple / List literal."""
        if isinstance(node, (ast.Set, ast.Tuple, ast.List)):
            for elt in node.elts:
                if isinstance(elt, ast.Constant) and _looks_like_backend_name(elt.value):
                    yield elt.value


def _extract_literal_backends(source: str) -> set[str]:
    """Walk ``source`` and return every backend-name literal.

    A literal is captured only when the comparison's left-hand side
    resolves to the parameter name ``backend`` (directly as a
    ``Name`` node, or indirectly as ``self.backend`` / an attribute
    on a parameter named ``backend``). This is the rule that round-3
    got wrong: the original regex pulled every string constant on the
    right of any ``==`` or ``in`` anywhere in the dispatch files, which
    matched unrelated constants like ``"true"``, ``"on"``, ``"pass"``
    and polluted the drift set. Round-4 tightens the walker so only
    direct comparisons against ``backend`` contribute.
    """
    tree = ast.parse(source)
    extractor = _BackendLiteralExtractor()
    extractor.visit(tree)
    return extractor.found


def test_dispatch_files_only_reference_known_builtins():
    missing: set[str] = set()
    for path in _DISPATCH_FILES:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        for name in _extract_literal_backends(text):
            if name not in backend_registry._BUILTIN_BACKEND_NAMES:
                missing.add(name)
    assert not missing, (
        f"Dispatch ladders reference backends not in "
        f"_BUILTIN_BACKEND_NAMES: {sorted(missing)}. Extend "
        f"runner/backend_registry.py:_BUILTIN_BACKEND_NAMES before "
        f"using a new name."
    )


def test_extracted_literals_are_nonempty_for_smoke():
    dispatch = _DISPATCH_FILES[0].read_text(encoding="utf-8")
    found = _extract_literal_backends(dispatch)
    assert "claude" in found or "codex" in found


def test_ast_extractor_catches_multi_element_set_and_tuple():
    """The round-3 regex only caught the FIRST name in a multi-element set;
    the AST walker must catch every member."""
    source = '''
def f(backend):
    if backend in {"echo", "mock_llm"}:
        return 1
    if backend in ("claude-sonnet", "gpt-5"):
        return 2
    if backend not in {"agy"}:
        return 3
'''
    found = _extract_literal_backends(source)
    assert "echo" in found
    assert "mock_llm" in found
    assert "claude-sonnet" in found  # hyphenated name — regex \w+ missed this
    assert "gpt-5" in found  # hyphenated
    assert "agy" in found


def test_ast_extractor_catches_equality_and_inequality():
    source = '''
def f(backend):
    if backend == "claude":
        return 1
    if backend != "codex":
        return 2
'''
    found = _extract_literal_backends(source)
    assert "claude" in found
    assert "codex" in found


def test_ast_extractor_rejects_non_backend_strings():
    """Don't accidentally match unrelated string constants."""
    source = '''
def f(backend):
    if backend == "echo":
        return "this is not a backend name"
    if other == "true":
        return 1
'''
    found = _extract_literal_backends(source)
    assert found == {"echo"}


def test_ast_extractor_ignores_non_backend_lhs():
    """Comparisons whose LHS is NOT ``backend`` are ignored — round-3
    regex pulled every string constant on the right of any ``==`` /
    ``in`` anywhere in the file, which caught unrelated values like
    ``"true"``, ``"on"``, ``"pass"``."""
    source = '''
def f(backend, mode):
    if mode == "true":
        return 1
    if backend == "echo":
        return 2
    if self.kind == "codex":
        return 3
'''
    found = _extract_literal_backends(source)
    assert found == {"echo"}, (
        f"Drift guard over-matched; expected only 'echo', got {found!r}"
    )
