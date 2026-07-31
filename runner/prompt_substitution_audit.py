"""Static structural audit for dark-factory ``prompts/`` files.

Catches the prompt-content regression class catalogued in
``docs/factory-evolve-research/proposals-2026-06-23.md`` P4. The
canonical incident is the PR #95 fix where ``prompts/slim/fix.md``
silently lost its ``${state.last_test_output}`` substitution — the
fix prompt rendered to empty strings at runtime, every fix attempt
returned ``success`` blindly (232s/680s/597s × $3-4 each), and the
factory exhausted its 3-visit bound without a single corrective edit.

The audit enforces three rules on every prompt file:

* **A — substitution-key wiring** — every ``${state.X}`` referenced
  in a prompt is either written by a ``runner/`` handler, matches a
  generic-writer pattern (``*.outcome`` / ``*.diff`` / ``*.resolved_*``),
  or is an explicit user-set key (allowlisted). An unwired key renders
  to ``""`` at runtime, which is the exact PR #95 failure mode.

* **B — prompt file resolution** — every ``prompt="@prompts/..."``
  reference in every ``pipelines/*.dot`` resolves to an existing
  prompt file. A renamed or deleted prompt breaks the engine's
  prompt loader with a less-helpful error; this check catches the
  missing file at unit-test time.

* **C — minimum content gate** — every prompt is >= ``MIN_PROMPT_CHARS``
  characters, contains ``${goal}`` (or is allowlisted as not needing
  it), and contains at least one directive verb from a conservative
  allowlist. Catches the "templated one-liner" regression class
  (e.g. a 12-character placeholder prompt that was a stub during
  refactoring and got committed by accident).

The audit is invoked three ways:

1. ``audit_prompts(prompts_dir, pipelines_dir, runner_dir, repo_root)``
   — returns ``list[Violation]`` for the full audit.
2. ``audit_prompts_check(check, ...)`` — run a single check.
3. ``python -m runner.prompt_substitution_audit [prompts_dir]
   [pipelines_dir] [runner_dir]`` — CLI; exit 0 if clean, 1 otherwise.

This module is **pure metadata analysis** — no LLM calls, no NL
classification. The wiring check is a static scan of ``runner/`` for
``ctx.state["X"] = ...`` assignments; the directive-verb check uses
a small allowlist of common English imperative verbs.

See ``docs/factory-evolve-research/proposals-2026-06-23.md`` P4 for
the full rationale and the two prior incidents this would have
caught (PR #95 ``fix.md`` regression, the original PR-B''
goal_gate incident ``7aa7695b1cf6``).
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys
from dataclasses import dataclass
from typing import Iterable, Literal, Optional


# ---------------------------------------------------------------------------
# Tunables — conservative thresholds that pass on all 35 existing prompts.
# ---------------------------------------------------------------------------

# Minimum prompt content length. 100 chars is well below the shortest
# real prompt (the ``prompts/slim/fix.md`` body itself is 1100+ chars)
# but large enough to flag a "TODO" or "stub" placeholder.
MIN_PROMPT_CHARS = 100

# Conservative directive-verb allowlist. Matched in lowercase form,
# against any whitespace-separated word in the prompt. Common English
# imperative verbs that appear in any non-trivial instruction. The
# list is intentionally broad; an over-broad allowlist produces false
# negatives (missed "templated one-liner" detection), not false
# positives. A "templated one-liner" that happens to contain one of
# these words is still arguably a valid prompt anyway.
DIRECTIVE_VERBS: frozenset[str] = frozenset({
    "add", "analyze", "apply", "assess", "build", "check", "choose",
    "clarify", "compare", "compile", "create", "decide", "define",
    "delete", "describe", "design", "determine", "develop", "diagnose",
    "document", "edit", "emit", "ensure", "evaluate", "examine",
    "explain", "explore", "extract", "find", "fix", "follow", "gather",
    "generate", "identify", "implement", "improve", "include", "inspect",
    "introduce", "investigate", "list", "locate", "maintain", "make",
    "modify", "move", "name", "note", "observe", "optimize", "organize",
    "outline", "perform", "pin", "plan", "prepare", "preserve", "print",
    "produce", "propose", "provide", "read", "record", "refactor",
    "remove", "replace", "report", "research", "resolve", "restate",
    "restrict", "review", "rewrite", "run", "search", "select", "set",
    "show", "specify", "split", "start", "state", "stop", "study",
    "submit", "summarize", "test", "trace", "treat", "update", "use",
    "validate", "verify", "write",
})

# Generic-writer suffixes — keys matching any of these are wired by a
# parameterised writer (``ctx.state[f"{node.name}.X"]``) and therefore
# count as "wired" for any value of the prefix. Mirrors the writers
# found in ``runner/engine_run.py:635,671,779`` (per-node ``.outcome``)
# and ``runner/handler_codergen.py:164-165`` (per-node ``.diff``) and
# ``runner/handler_dispatch.py:373-376`` (per-node ``.resolved_*``).
GENERIC_WRITER_SUFFIXES: tuple[str, ...] = (
    ".outcome",
    ".diff",
    ".changed_files",
    ".resolved_backend",
    ".resolved_backend_meta",
    ".output",
    ".output_head",
)

# Keys that the user supplies via ``--state KEY=VALUE`` or via the
# graph's ``state`` attribute at pipeline invocation time. They have
# no ``runner/`` writer because the engine does not produce them;
# the user does. The pipeline that owns the key is named in the
# allowlist entry for documentation. An entry maps ``key -> (pipeline,
# justification)``.
USER_SET_KEYS: dict[str, str] = {
    # bug_fix.dot accepts ``--state bug_fix.test_path=...`` from the
    # operator. See runner/handler_special_gates.py:216.
    "bug_fix.test_path": "pipelines/bug_fix.dot (user-supplied via --state)",
}

# The default codergen prompt is allowlisted from the ``${goal}``
# presence check because it documents the goal-substitution contract
# for ALL codergen nodes and may legitimately exist without an inline
# goal reference. (The engine injects ``${goal}`` from the graph's
# goal attribute at dispatch time.)
#
# ``prompts/bug_fix/fix.md`` is also allowlisted: the bug-fix lane's
# "goal" is implicit in the test failure state, and the prompt encodes
# that goal via the ``${state.bug_fix.test_path}`` injection. There is
# no useful ``${goal}`` text to include — the test oracle IS the goal.
PROMPTS_WITHOUT_GOAL_OK: frozenset[str] = frozenset({
    "prompts/codergen.md",
    "prompts/bug_fix/fix.md",
    # The controller review prompt is bound to a static, source-owned
    # contract — its only goal is to grade the controller-bound diff
    # against the C0–C6 / E0–E13 checklist. The contract's task text
    # arrives via the controller-bound envelope (Base64), not via
    # ${goal}, so the prompt itself does not need ${goal}.
    "prompts/catalog/controller_cold_review_v1.md",
})


# ---------------------------------------------------------------------------
# Public types
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Violation:
    """A single prompt-substitution audit finding.

    ``kind``        — "A" (unwired ``${state.X}``), "B" (broken
                       ``prompt="@..."`` reference), or "C" (prompt
                       content below the minimum-content gate).
    ``prompt``      — repo-relative POSIX path of the offending prompt
                       (or pipeline, for kind B).
    ``location``    — token or section locator so a human can grep
                       without re-reading the file.
    ``message``     — human-readable explanation; one short line.
    """

    kind: Literal["A", "B", "C"]
    prompt: str
    location: str
    message: str


# ---------------------------------------------------------------------------
# Regexes
# ---------------------------------------------------------------------------

# Match a ``${...}`` substitution token. Captures the full body inside
# the braces (including dotted keys like ``state.foo.bar``).
_SUBSTITUTION_RE = re.compile(r"\$\{([a-zA-Z_][a-zA-Z0-9_.]*)\}")

# Match the dot-prefixed relative path inside a ``prompt="@..."``
# attribute. Mirrors the format produced by Graphviz for quoted
# attribute values.
_PROMPT_ATTR_RE = re.compile(r'prompt\s*=\s*"?@([^"\s]+)"?')


# ---------------------------------------------------------------------------
# Check A — substitution-key wiring
# ---------------------------------------------------------------------------


def _scan_handler_writers(runner_dir: pathlib.Path) -> set[str]:
    """Walk every ``.py`` file under ``runner/`` and collect every key
    that a handler writes via ``ctx.state["X"] = ...`` or
    ``ctx.state[f"...{...}..."]`` (a parameterised writer).

    The set is then used by ``_is_key_wired`` to classify each
    ``${state.X}`` token. We intentionally scan the source text (not
    an import + inspect) so the audit does not need the runner's
    dependencies to be installed in a test environment, and so the
    set of writers is exactly the set of writers present at HEAD.
    """
    writers: set[str] = set()
    if not runner_dir.exists():
        return writers
    for path in sorted(runner_dir.rglob("*.py")):
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for match in re.finditer(
            r'ctx\.state\[\s*([fFrR]?["\'][^"\']*["\'])\s*\]', text
        ):
            literal = match.group(1)
            # Strip the f/r-string prefix and surrounding quotes.
            inner = literal[1:] if literal[0] in "fFrR" else literal
            if inner[0] in "\"'":
                inner = inner[1:-1]
            # Look for embedded ``{name}`` placeholders (f-strings).
            placeholders = re.findall(r"\{([a-zA-Z_][a-zA-Z0-9_.]*)\}", inner)
            if placeholders:
                # Parameterised writer: ``state["<node>.<key>"]``. The
                # generic suffix (after the last ``.``) is the
                # writer's contribution; the prefix is a node name.
                # Record the literal inner form so the match is
                # exact — the generic-suffix check in
                # ``_is_key_wired`` handles ``*.outcome`` etc.
                writers.add(inner)
            else:
                writers.add(inner)
    return writers


def _is_key_wired(key: str, writers: set[str]) -> bool:
    """Return True iff ``key`` is known to be populated at runtime.

    A key is wired when any of the following holds:

    1. The literal key appears in the writers set (a direct
       ``ctx.state[key] = ...`` assignment somewhere in ``runner/``).
    2. The key ends with one of the ``GENERIC_WRITER_SUFFIXES``
       (a parameterised writer like ``ctx.state[f"{node.name}.diff"]``).
    3. The key is in the ``USER_SET_KEYS`` allowlist (the user
       supplies the value via ``--state KEY=VALUE``).
    """
    if key in writers:
        return True
    if key in USER_SET_KEYS:
        return True
    if any(key.endswith(suffix) for suffix in GENERIC_WRITER_SUFFIXES):
        return True
    return False


def _extract_state_keys(text: str) -> list[str]:
    """Return every unique ``${state.X}`` substitution key in ``text``.

    ``${state.foo.bar}`` and ``${state.foo}`` are distinct keys.
    Order is preserved by first-occurrence. The ``state.`` prefix
    is stripped so callers can compare against the bare key.
    """
    out: list[str] = []
    seen: set[str] = set()
    for match in _SUBSTITUTION_RE.finditer(text):
        body = match.group(1)
        if not body.startswith("state."):
            continue
        key = body[len("state."):]
        if key and key not in seen:
            seen.add(key)
            out.append(key)
    return out


def _read_prompt(path: pathlib.Path) -> Optional[str]:
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None


def check_wiring(
    prompts_dir: pathlib.Path,
    writers: set[str],
) -> list[Violation]:
    """Check A — every ``${state.X}`` in every prompt is wired.

    ``writers`` is the set returned by ``_scan_handler_writers``; pass
    it in (instead of re-scanning ``runner/`` here) so the caller can
    share one scan across multiple checks. Returns one Violation per
    unwired key.
    """
    violations: list[Violation] = []
    if not prompts_dir.exists():
        return violations
    for path in sorted(prompts_dir.rglob("*.md")):
        relpath = _relpath(path)
        text = _read_prompt(path)
        if text is None:
            continue
        for key in _extract_state_keys(text):
            if not _is_key_wired(key, writers):
                violations.append(
                    Violation(
                        kind="A",
                        prompt=relpath,
                        location=f"${{state.{key}}}",
                        message=(
                            f"prompt references ${{state.{key}}} but no "
                            f"runner handler writes it (no direct writer, "
                            f"no generic-suffix match, not in the "
                            f"user-set allowlist). At runtime this "
                            f"renders to an empty string, which is the "
                            f"PR #95 fix.md regression class."
                        ),
                    )
                )
    return violations


# ---------------------------------------------------------------------------
# Check B — prompt file resolution
# ---------------------------------------------------------------------------


def _collect_dot_prompt_refs(
    pipelines_dir: pathlib.Path,
) -> list[tuple[pathlib.Path, str]]:
    """Walk every ``.dot`` file and collect every ``prompt="@..."``
    reference. Returns ``(dot_file, relpath_within_dot_dir)`` pairs.

    The relpath is what the engine will try to resolve at runtime —
    dot-file-dir-relative by default, with the workdir / factory_home
    fallback handled by ``runner.handlers._render_prompt``. The audit
    checks all three resolution locations so a prompt that lives in
    any of them passes.
    """
    out: list[tuple[pathlib.Path, str]] = []
    if not pipelines_dir.exists():
        return out
    for dot in sorted(pipelines_dir.rglob("*.dot")):
        try:
            text = dot.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for match in _PROMPT_ATTR_RE.finditer(text):
            out.append((dot, match.group(1)))
    return out


def _prompt_resolves(
    dot_file: pathlib.Path,
    relpath: str,
    repo_root: pathlib.Path,
) -> bool:
    """Return True iff ``relpath`` (a ``@prompts/...`` reference)
    resolves to an existing file under any of the engine's lookup
    locations.

    Mirrors ``runner.handlers._render_prompt``'s resolution order:
    workdir-relative first, then ``factory_home``-relative (== repo
    root in the test environment). For include-fragments (dot files
    that reference prompts via ``include="@..."``), the dot-dir
    itself is the workdir.
    """
    candidates = [
        dot_file.parent / relpath,
        pathlib.Path.cwd() / relpath,
        repo_root / relpath,
    ]
    return any(c.is_file() for c in candidates)


def check_resolution(
    pipelines_dir: pathlib.Path,
    repo_root: pathlib.Path,
) -> list[Violation]:
    """Check B — every ``prompt="@..."`` reference resolves to a file."""
    violations: list[Violation] = []
    for dot_file, relpath in _collect_dot_prompt_refs(pipelines_dir):
        if not _prompt_resolves(dot_file, relpath, repo_root):
            violations.append(
                Violation(
                    kind="B",
                    prompt=_relpath(dot_file),
                    location=f"prompt=@{relpath}",
                    message=(
                        f"pipeline references prompt=@{relpath!r} but no "
                        f"file exists at that path under the dot's "
                        f"directory, the workdir, or the repo root. The "
                        f"engine's prompt loader will fail at runtime."
                    ),
                )
            )
    return violations


# ---------------------------------------------------------------------------
# Check C — minimum content gate
# ---------------------------------------------------------------------------


def _has_directive_verb(text: str) -> bool:
    """Return True iff ``text`` contains at least one directive verb.

    Matched as a case-insensitive whole word, allowing leading/trailing
    punctuation. The list (``DIRECTIVE_VERBS``) is conservative; the
    intent is to flag templated one-liners, not to enforce style.
    """
    lowered = text.lower()
    for verb in DIRECTIVE_VERBS:
        if re.search(rf"\b{re.escape(verb)}\b", lowered):
            return True
    return False


def check_minimum_content(prompts_dir: pathlib.Path) -> list[Violation]:
    """Check C — every prompt is >= MIN_PROMPT_CHARS, contains
    ``${goal}`` (or is allowlisted), and contains a directive verb.

    The ``${goal}`` rule pins the contract that the engine injects the
    pipeline's top-level goal into every prompt. A prompt that omits
    ``${goal}`` is either a special-case (allowlisted) or a bug.
    """
    violations: list[Violation] = []
    if not prompts_dir.exists():
        return violations
    for path in sorted(prompts_dir.rglob("*.md")):
        relpath = _relpath(path)
        text = _read_prompt(path)
        if text is None:
            continue
        stripped = text.strip()
        if len(stripped) < MIN_PROMPT_CHARS:
            violations.append(
                Violation(
                    kind="C",
                    prompt=relpath,
                    location=f"<{len(stripped)} chars>",
                    message=(
                        f"prompt is only {len(stripped)} chars "
                        f"(minimum is {MIN_PROMPT_CHARS}). Templated "
                        f"one-liner or placeholder stub; the PR #95 "
                        f"fix.md regression had a similarly short body."
                    ),
                )
            )
            continue
        if relpath not in PROMPTS_WITHOUT_GOAL_OK and "${goal}" not in text:
            violations.append(
                Violation(
                    kind="C",
                    prompt=relpath,
                    location="<no ${goal}>",
                    message=(
                        f"prompt does not contain ${{goal}}. The engine "
                        f"injects the pipeline's top-level goal into "
                        f"every prompt; omitting it is a bug unless the "
                        f"prompt is in PROMPTS_WITHOUT_GOAL_OK."
                    ),
                )
            )
            continue
        if not _has_directive_verb(text):
            violations.append(
                Violation(
                    kind="C",
                    prompt=relpath,
                    location="<no directive verb>",
                    message=(
                        f"prompt does not contain any directive verb "
                        f"from DIRECTIVE_VERBS. May be a non-instruction "
                        f"document (header-only prompt); flag for review."
                    ),
                )
            )
    return violations


# ---------------------------------------------------------------------------
# Public audit entrypoint
# ---------------------------------------------------------------------------


def audit_prompts(
    prompts_dir: pathlib.Path,
    pipelines_dir: pathlib.Path,
    runner_dir: pathlib.Path,
    repo_root: pathlib.Path,
) -> list[Violation]:
    """Run all three checks and return a deduplicated ``list[Violation]``.

    Order of checks does not affect the output (violations are
    returned grouped by kind); the order within a kind is the sorted
    prompt-path order from the underlying walk.
    """
    writers = _scan_handler_writers(runner_dir)
    return (
        check_wiring(prompts_dir, writers)
        + check_resolution(pipelines_dir, repo_root)
        + check_minimum_content(prompts_dir)
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _relpath(path: pathlib.Path) -> str:
    """Return the path as a repo-root-relative POSIX string for
    consistent reporting. Falls back to absolute when the path is
    not under the current working directory.
    """
    try:
        return path.resolve().relative_to(pathlib.Path.cwd()).as_posix()
    except ValueError:
        return path.as_posix()


def _format_report(violations: Iterable[Violation]) -> str:
    lines = []
    for v in violations:
        lines.append(f"{v.kind} | {v.prompt} | {v.location} | {v.message}")
    return "\n".join(lines)


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m runner.prompt_substitution_audit",
        description=(
            "Static structural audit for dark-factory prompts. "
            "Exits 0 if all three checks (A: wiring, B: file "
            "resolution, C: minimum content) pass, 1 otherwise."
        ),
    )
    parser.add_argument(
        "prompts_dir",
        nargs="?",
        default="prompts",
        help="Root directory to walk for prompt *.md files (default: ./prompts).",
    )
    parser.add_argument(
        "pipelines_dir",
        nargs="?",
        default="pipelines",
        help="Root directory to walk for *.dot files (default: ./pipelines).",
    )
    parser.add_argument(
        "runner_dir",
        nargs="?",
        default="runner",
        help="Root directory to walk for handler *.py files (default: ./runner).",
    )
    args = parser.parse_args(argv)

    prompts_dir = pathlib.Path(args.prompts_dir)
    pipelines_dir = pathlib.Path(args.pipelines_dir)
    runner_dir = pathlib.Path(args.runner_dir)
    repo_root = pathlib.Path.cwd()

    if not prompts_dir.exists():
        print(f"error: prompts dir does not exist: {prompts_dir}", file=sys.stderr)
        return 2
    if not pipelines_dir.exists():
        print(f"error: pipelines dir does not exist: {pipelines_dir}", file=sys.stderr)
        return 2
    if not runner_dir.exists():
        print(f"error: runner dir does not exist: {runner_dir}", file=sys.stderr)
        return 2

    violations = audit_prompts(prompts_dir, pipelines_dir, runner_dir, repo_root)
    if not violations:
        print("OK: no prompt substitution violations found.")
        return 0
    print(_format_report(violations))
    return 1


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
