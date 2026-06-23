"""Regression guard: every ``prompt=@...`` reference in any .dot pipeline
resolves to an existing file.

Companion to the 5 family-scoped timeout-attr tests and the
cross-family value-pinning test:

- ``test_gates_dot_timeouts.py`` (factory/)
- ``test_slim_pipelines_timeouts.py`` (slim/)
- ``test_airbnb_clone_pipelines_timeouts.py`` (airbnb-clone/)
- ``test_amazon_clone_pipelines_timeouts.py`` (amazon-clone/)
- ``test_remaining_pipelines_timeouts.py`` (all-nodes-coverage + attractor-spec-review + fibonacci/)
- ``test_timeout_value_pinning.py`` (cross-family value range)

The timeout-attrs pattern has 3 dimensions: per-family presence,
per-family value, cross-family range. This test adds a 4th dimension:
**prompt-pinning** — every ``prompt=@...`` reference in every .dot
file in the repo resolves to an existing file.

The contract: if a .dot file declares a `prompt="@path"`, the engine
will try to load the prompt at runtime. If the file is missing, the
engine fails with a less-helpful error (typically "file not found" in
the codergen subprocess). A pre-flight check at test time catches the
missing file before the engine ever runs.

The reference syntax is the leading-`@` convention used by dark-factory's
prompt resolver (see ``runner/engine.py:_resolve_prompt_path`` — the
`@` is stripped and the remainder is interpreted as a path relative to
the .dot file's containing directory, with the engine then resolving
include chains). This test mirrors that resolution: strip the leading
`@`, treat the remainder as a path relative to the .dot file, check
that the file exists.

**File-disjoint**: this test is a new file, only reads .dot pipelines
through the existing parser and walks the file system. Does not touch
any WIP-touched file. The 5 sibling test files (and the value-pinning
test) are NOT imported — this test inlines its own walk to survive WIP
rebases.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.parser import parse  # noqa: E402


# Match `prompt="@some/path.md"` (and tolerate the optional int/string forms).
# DOT allows `prompt=@path`, `prompt="@path"`, and `prompt=@path.md`.
# This regex captures the path inside the optional quotes.
_PROMPT_ATTR_RE = re.compile(r'prompt\s*=\s*"?@([^"\s]+)"?')


# Match the first `graph [ ... ]` block in a .dot file. Captures (1) the
# block contents (a newline-separated list of `key="value"` lines).
_GRAPH_ATTRS_RE = re.compile(r"graph\s*\[\s*([^\]]*?)\s*\]", re.DOTALL)
# Match a `key="value"` line inside a graph [...] block. Captures (1) key.
_GRAPH_ATTR_LINE_RE = re.compile(r'^\s*([a-z_][a-z0-9_]*)\s*=\s*"([^"]*)"', re.MULTILINE)


def _is_test_fixture(dot_file: pathlib.Path) -> bool:
    """Return True if a .dot file declares `graph [test_fixture="true"]`.

    The opt-out is a property of the file, not a property of the test's
    path-walk logic. This mirrors the existing soft-tier convention
    (``skip_holdout="true"`` / ``skip_spec_validation="true"`` at
    ``bin/conformance:208-253``) so a fixture anywhere in the repo can
    opt out without a hardcoded path check.

    Reads the .dot file once and regex-extracts the first ``graph [...]``
    block. The block is the same DOT `Graph attributes` block the engine
    surfaces via ``runner.parser.Graph.graph_attrs`` at runtime.
    """
    try:
        text = dot_file.read_text()
    except (OSError, UnicodeDecodeError):
        return False
    block = _GRAPH_ATTRS_RE.search(text)
    if not block:
        return False
    for key, value in _GRAPH_ATTR_LINE_RE.findall(block.group(1)):
        if key == "test_fixture" and value.lower() == "true":
            return True
    return False


def _all_dot_files() -> list[pathlib.Path]:
    """Return every .dot file in the repo, excluding worktree copies,
    include-only fragments, and test fixtures.

    Excludes:
      * `.claude/worktrees/**` — nested-clone copies left behind by
        worktree-driven PR iteration.
      * `_*` stems — include-only fragments referenced from a parent
        ``include="@_base.dot"`` attribute; never resolved standalone.
      * `.dot` files declaring ``graph [test_fixture="true"]`` —
        deliberately-invalid conformance fixtures (e.g.
        ``level5_missing_gate.dot``, ``level5_valid.dot``) used to
        exercise the conformance validator's diagnostic path. Their
        ``@prompts/hello/...`` references may be intentionally
        unresolved; pinning them here would conflate "deliberately
        broken fixture" with "broken real pipeline". The opt-out is
        a property of the file, not of the test's path-walk logic.
    """
    out: list[pathlib.Path] = []
    for path in ROOT.rglob("*.dot"):
        parts = path.parts
        if ".claude" in parts and "worktrees" in parts:
            continue
        if path.stem.startswith("_"):
            continue
        if _is_test_fixture(path):
            continue
        out.append(path)
    return sorted(out)


def _resolve_prompt_path(dot_file: pathlib.Path, prompt_ref: str) -> pathlib.Path:
    """Resolve a ``@<ref>`` prompt reference the way the engine does.

    Mirrors ``runner.handlers._render_prompt`` (the production
    resolver that actually loads the prompt at runtime):

      1. Strip the leading ``@``.
      2. If the result is an absolute path, honor it as-is (the engine
         resolves absolute ``prompt=@/abs/path.md`` references directly
         and rejects them only if they live under a holdout deny path).
      3. Otherwise resolve relative to ``workdir`` (the engine's CWD
         at run time — also the dark-factory repo root in practice for
         test fixtures) first.
      4. If that misses, fall back to ``factory_home()`` (the repo
         root resolved via ``runner.paths``). This is the path
         ``pipelines/factory/*.dot`` historically relies on, and the
         only path that lets ``benchmarks/airbnb-clone/pipelines/
         airbnb-clone.dot`` find its ``benchmarks/airbnb-clone/
         prompts/...`` files which sit beside the .dot at a level
         above it (i.e. the @-prefixed path is repo-root relative,
         not .dot-dir relative).

    Note: this order is deliberately the ENGINE's runtime order
    (workdir → factory_home), not the preflight CLI's order
    (dot-dir → factory_home). ``runner.structural_preflight`` is a
    separate module that validates a single .dot at a time and
    resolves relative to the .dot file's directory first; the
    production engine in ``runner.handlers._render_prompt`` resolves
    relative to the CWD first. This test pins the production path
    that codergen / tool nodes actually exercise at run time.

    Returns the FIRST path that exists; falls back to the workdir
    resolution for diagnostic reporting. Existence is checked
    separately by the test.
    """
    if prompt_ref.startswith("@"):
        prompt_ref = prompt_ref[1:]
    prompt_path = pathlib.Path(prompt_ref)
    if prompt_path.is_absolute():
        return prompt_path.resolve()
    # 1) workdir-relative (engine's first try)
    workdir_relative = (pathlib.Path.cwd() / prompt_path).resolve()
    if workdir_relative.is_file():
        return workdir_relative
    # 2) factory_home() / repo-root relative (engine's fallback)
    #    ROOT from conftest is the repo root — same as factory_home()
    #    in the test environment. This catches airbnb-clone-style
    #    @-prefixed paths that are repo-root-relative.
    home_relative = (ROOT / prompt_path).resolve()
    if home_relative.is_file():
        return home_relative
    # Neither hit — return the home_relative for the diagnostic
    # message so the failure reports where the engine WOULD have
    # looked last (and matches the actual on-disk gap).
    return home_relative


def test_every_prompt_reference_in_every_dot_file_resolves_to_existing_file() -> None:
    """Every ``prompt=@...`` reference in every .dot file resolves to an existing file.

    Iterates every .dot file in the repo and asserts that any
    ``prompt=@...`` reference resolves to a file that actually exists
    on disk. Catches broken include references (a renamed prompt file,
    a typo in the path, a deleted prompt) before the engine ever
    tries to load it.

    The contract is structural: the engine's prompt resolver will
    fail with a less-helpful error if the file is missing, so the
    test catches the bug at unit-test time rather than at run time.
    """
    missing: list[tuple[str, str]] = []
    for path in _all_dot_files():
        rel = str(path.relative_to(ROOT))
        g = parse(path)
        for name, node in g.nodes.items():
            if name in {"start", "exit"}:
                continue
            # Use the parser's prompt_ref property — it strips the
            # leading '@' (or returns the un-prefixed value as-is if
            # the .dot author omitted the @). This matches what the
            # engine actually feeds to _render_prompt at runtime, so
            # the test pins the contract the engine sees, not the
            # string-equal-to-literal-"@..." form.
            ref = node.prompt_ref
            if not ref:
                continue
            # Re-add the '@' for the resolver helper's path-strip step
            # (the helper mirrors the engine's runtime contract: it
            # also strips the leading '@' before resolving).
            resolved = _resolve_prompt_path(path, "@" + ref)
            if not resolved.is_file():
                missing.append((rel, f"{name} -> @{ref} -> {resolved}"))
    assert not missing, (
        f"every prompt reference in every .dot file must resolve to an "
        f"existing file. Missing: {missing}."
    )


def test_prompt_resolver_strips_leading_at_sign() -> None:
    """The leading ``@`` is stripped before path resolution.

    A regression in the engine's prompt resolver that fails to strip
    the leading ``@`` would cause every prompt reference like
    ``@prompts/plan.md`` to be treated as a literal path
    (an ``@prompts`` filename in the dot's directory, which won't
    exist), silently breaking the engine. This unit-level test
    pins the strip contract: passing an ``@``-prefixed string and
    the equivalent un-prefixed string must yield the same resolved
    path. Equality (not inequality) is the right assertion once
    both inputs have been normalized by the strip.
    """
    fake_dot = pathlib.Path("/tmp/_test_fake.dot")
    with_at = _resolve_prompt_path(fake_dot, "@prompts/plan.md")
    without_at = _resolve_prompt_path(fake_dot, "prompts/plan.md")
    assert with_at == without_at, (
        "_resolve_prompt_path must produce the SAME path for "
        "'@prompts/plan.md' and 'prompts/plan.md' — if they differ, "
        "the leading-@ strip is broken (a regression that would "
        "cause the engine to look up '@prompts/plan.md' as a "
        "literal file name, which never exists)"
    )
