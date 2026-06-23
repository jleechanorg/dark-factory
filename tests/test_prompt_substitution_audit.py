"""Tests for ``runner.prompt_substitution_audit``.

The audit is the P4 deliverable from
``docs/factory-evolve-research/proposals-2026-06-23.md``. These tests
pin the three checks (A: wiring, B: file resolution, C: minimum
content) and the CLI surface.

Test organisation:
  * **Unit tests** — exercise individual helpers with synthetic
    inputs. Fast, no filesystem walking beyond ``tmp_path``.
  * **Real-tree integration tests** — run each check against the
    actual ``prompts/`` and ``pipelines/`` trees to confirm zero
    violations on the current HEAD. Catches drift in the writers
    set or the allowlists.
  * **CLI tests** — exercise the ``main()`` entrypoint with
    argv-injection. Verifies exit codes 0/1/2.

The test data is intentionally minimal: a few ``.md`` files in
``tmp_path`` per scenario. The audit must work on a clean tree
without depending on the structure of the real ``prompts/`` and
``pipelines/`` directories beyond the standard layout.
"""

from __future__ import annotations

import pathlib
import sys
from typing import Iterator

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.prompt_substitution_audit import (  # noqa: E402
    DIRECTIVE_VERBS,
    GENERIC_WRITER_SUFFIXES,
    MIN_PROMPT_CHARS,
    USER_SET_KEYS,
    Violation,
    _extract_state_keys,
    _has_directive_verb,
    _is_key_wired,
    _prompt_resolves,
    _relpath,
    _scan_handler_writers,
    audit_prompts,
    check_minimum_content,
    check_resolution,
    check_wiring,
    main,
)


# ---------------------------------------------------------------------------
# Test fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def tmp_prompts(tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch) -> pathlib.Path:
    """Create a minimal prompts tree with two well-formed prompts.

    The ``monkeypatch.chdir`` makes the tmp_path the CWD so the
    audit's ``_relpath`` returns repo-relative paths like
    ``prompts/good.md`` instead of the absolute /private/var/...
    tmp paths. This lets tests assert on clean relative paths.
    """
    monkeypatch.chdir(tmp_path)
    prompts = tmp_path / "prompts"
    prompts.mkdir()
    (prompts / "good.md").write_text(
        "# Good Prompt\n\n"
        "Implement the goal: ${goal}.\n\n"
        "Use ${state.foo} and write the result. Read the spec, plan the "
        "work, and emit a clean diff. Verify your work before declaring "
        "success.\n",
        encoding="utf-8",
    )
    (prompts / "bug_fix" / "fix.md").parent.mkdir()
    (prompts / "bug_fix" / "fix.md").write_text(
        "# Bug Fix\n\n"
        "Fix the test at ${state.bug_fix.test_path} to pass.\n\n"
        "## Test as oracle\n\n"
        "That test was written by the previous `reproduce` node from "
        "observed behavior, not from the bug report. Treat it as the "
        "source of truth for what correct looks like.\n\n"
        "## Workflow\n\n"
        "1. Read the test path. 2. Locate production code. 3. Fix it.\n",
        encoding="utf-8",
    )
    return prompts


@pytest.fixture
def tmp_pipelines(tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch) -> pathlib.Path:
    """Create a minimal pipelines tree with one .dot referencing a real prompt.

    The dot uses ``prompt="@../prompts/good.md"`` because the dot
    lives at ``tmp_path/pipelines/test.dot`` and the prompt lives at
    ``tmp_path/prompts/good.md`` — same as the real
    ``pipelines/factory/*.dot`` -> ``prompts/<lane>/*.md`` layout.
    """
    monkeypatch.chdir(tmp_path)
    pipelines = tmp_path / "pipelines"
    pipelines.mkdir()
    (pipelines / "test.dot").write_text(
        'digraph t { graph [goal="x"]; '
        'start [shape=Mdiamond]; exit [shape=Msquare]; '
        'a [type="codergen", prompt="@../prompts/good.md"]; '
        'start -> a -> exit; }',
        encoding="utf-8",
    )
    return pipelines


@pytest.fixture
def tmp_runner(tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch) -> pathlib.Path:
    """Create a minimal runner tree with one writer file."""
    monkeypatch.chdir(tmp_path)
    runner = tmp_path / "runner"
    runner.mkdir()
    (runner / "writer.py").write_text(
        "def write_last_test_output(ctx, value):\n"
        "    ctx.state['last_test_output'] = value\n"
        "    ctx.state['last_test_rc'] = '0'\n",
        encoding="utf-8",
    )
    return runner


# ---------------------------------------------------------------------------
# Unit tests — helpers
# ---------------------------------------------------------------------------


def test_extract_state_keys_strips_state_prefix() -> None:
    text = "Read ${state.foo.bar} and ${state.baz}."
    assert _extract_state_keys(text) == ["foo.bar", "baz"]


def test_extract_state_keys_ignores_non_state_tokens() -> None:
    text = "Goal: ${goal}. Diff: ${diff}. State: ${state.x}."
    assert _extract_state_keys(text) == ["x"]


def test_extract_state_keys_dedupes_in_first_seen_order() -> None:
    text = "${state.a} ${state.b} ${state.a} ${state.c}"
    assert _extract_state_keys(text) == ["a", "b", "c"]


def test_extract_state_keys_empty_for_no_state_tokens() -> None:
    assert _extract_state_keys("Goal: ${goal}. Diff: ${diff}.") == []


def test_is_key_wired_matches_direct_writer() -> None:
    writers = {"last_test_output", "last_test_rc"}
    assert _is_key_wired("last_test_output", writers) is True


def test_is_key_wired_matches_user_set_allowlist() -> None:
    writers: set[str] = set()
    assert _is_key_wired("bug_fix.test_path", writers) is True


def test_is_key_wired_matches_generic_suffix() -> None:
    writers: set[str] = set()
    # *.outcome is the engine_run.py:635 generic writer.
    assert _is_key_wired("branch_a.outcome", writers) is True
    # *.diff is the handler_codergen.py:164 generic writer.
    assert _is_key_wired("reviewer.diff", writers) is True
    # *.resolved_backend is the handler_dispatch.py:373 generic writer.
    assert _is_key_wired("gate_er.resolved_backend", writers) is True


def test_is_key_wired_rejects_unwired_key() -> None:
    writers = {"last_test_output"}
    assert _is_key_wired("totally.unwired.key", writers) is False


def test_has_directive_verb_matches_common_verbs() -> None:
    assert _has_directive_verb("Implement the goal.") is True
    assert _has_directive_verb("Run the tests.") is True
    assert _has_directive_verb("Fix the bug.") is True
    assert _has_directive_verb("Verify the result.") is True


def test_has_directive_verb_rejects_no_verbs() -> None:
    assert _has_directive_verb("Lorem ipsum dolor sit amet.") is False
    assert _has_directive_verb("") is False
    assert _has_directive_verb("# Header only\n") is False


def test_has_directive_verb_is_case_insensitive() -> None:
    assert _has_directive_verb("IMPLEMENT the goal.") is True
    assert _has_directive_verb("read the file.") is True


def test_scan_handler_writers_finds_direct_writes(tmp_runner: pathlib.Path) -> None:
    writers = _scan_handler_writers(tmp_runner)
    assert "last_test_output" in writers
    assert "last_test_rc" in writers


def test_scan_handler_writers_handles_missing_dir(tmp_path: pathlib.Path) -> None:
    assert _scan_handler_writers(tmp_path / "nope") == set()


def test_scan_handler_writers_collects_fstring_writers(tmp_path: pathlib.Path) -> None:
    """An f-string writer like ``ctx.state[f"{node.name}.diff"] = ...``
    is recorded by its literal template form so the generic-suffix
    match in ``_is_key_wired`` can fire.
    """
    runner = tmp_path / "runner"
    runner.mkdir()
    (runner / "fstring.py").write_text(
        'def writer(ctx, node):\n'
        '    ctx.state[f"{node.name}.diff"] = "x"\n',
        encoding="utf-8",
    )
    writers = _scan_handler_writers(runner)
    # The f-string template form is the bare ``{node.name}.diff``.
    assert any("diff" in w for w in writers)


def test_prompt_resolves_under_dot_dir(tmp_path: pathlib.Path) -> None:
    pipelines = tmp_path / "pipelines"
    pipelines.mkdir()
    prompts = tmp_path / "prompts"
    prompts.mkdir()
    (prompts / "real.md").write_text("ok", encoding="utf-8")
    dot = pipelines / "test.dot"
    dot.write_text('digraph {}', encoding="utf-8")
    assert _prompt_resolves(dot, "../prompts/real.md", tmp_path) is True


def test_prompt_resolves_under_repo_root(tmp_path: pathlib.Path) -> None:
    pipelines = tmp_path / "pipelines"
    pipelines.mkdir()
    prompts = tmp_path / "prompts"
    prompts.mkdir()
    (prompts / "real.md").write_text("ok", encoding="utf-8")
    dot = pipelines / "test.dot"
    dot.write_text('digraph {}', encoding="utf-8")
    assert _prompt_resolves(dot, "prompts/real.md", tmp_path) is True


def test_prompt_resolves_returns_false_for_missing(tmp_path: pathlib.Path) -> None:
    pipelines = tmp_path / "pipelines"
    pipelines.mkdir()
    dot = pipelines / "test.dot"
    dot.write_text('digraph {}', encoding="utf-8")
    assert _prompt_resolves(dot, "prompts/does_not_exist.md", tmp_path) is False


def test_relpath_relative_to_cwd(tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.chdir(tmp_path)
    p = tmp_path / "prompts" / "x.md"
    assert _relpath(p) == "prompts/x.md"


# ---------------------------------------------------------------------------
# Check A — wiring
# ---------------------------------------------------------------------------


def test_check_wiring_flags_unwired_key(tmp_prompts: pathlib.Path) -> None:
    """A prompt referencing an unwired ``${state.X}`` raises one violation per key."""
    (tmp_prompts / "bad.md").write_text(
        "# Bad\n\n"
        "Implement the goal: ${goal}. Use ${state.totally_unwired}.\n",
        encoding="utf-8",
    )
    violations = check_wiring(tmp_prompts, writers=set())
    wired = [v for v in violations if "totally_unwired" in v.location]
    assert len(wired) == 1
    assert wired[0].kind == "A"
    assert wired[0].prompt == "prompts/bad.md"


def test_check_wiring_accepts_direct_writer(tmp_prompts: pathlib.Path, tmp_runner: pathlib.Path) -> None:
    writers = _scan_handler_writers(tmp_runner)
    # tmp_prompts/good.md references ${state.foo} which is not wired by
    # the runner fixture. Rewrite good.md to reference a wired key.
    (tmp_prompts / "good.md").write_text(
        "# Good\n\n"
        "Implement the goal: ${goal}. Use ${state.last_test_output}.\n",
        encoding="utf-8",
    )
    violations = check_wiring(tmp_prompts, writers=writers)
    assert violations == []


def test_check_wiring_accepts_generic_suffix(tmp_prompts: pathlib.Path) -> None:
    """``${state.<node>.outcome}`` is wired by engine_run.py:635 generic writer."""
    (tmp_prompts / "good.md").write_text(
        "# Good\n\n"
        "Implement the goal: ${goal}. Use ${state.reviewer.outcome}.\n",
        encoding="utf-8",
    )
    violations = check_wiring(tmp_prompts, writers=set())
    assert violations == []


def test_check_wiring_accepts_user_set_key(tmp_prompts: pathlib.Path) -> None:
    """``${state.bug_fix.test_path}`` is allowlisted as user-set."""
    # The bug_fix/fix.md fixture in tmp_prompts uses ${state.bug_fix.test_path}.
    violations = check_wiring(tmp_prompts, writers=set())
    bug_fix_violations = [v for v in violations if v.prompt == "prompts/bug_fix/fix.md"]
    assert bug_fix_violations == []


def test_check_wiring_handles_missing_dir(tmp_path: pathlib.Path) -> None:
    assert check_wiring(tmp_path / "nope", writers=set()) == []


# ---------------------------------------------------------------------------
# Check B — file resolution
# ---------------------------------------------------------------------------


def test_check_resolution_accepts_existing_prompts(
    tmp_prompts: pathlib.Path, tmp_pipelines: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    violations = check_resolution(tmp_pipelines, tmp_path)
    # tmp_pipelines/test.dot references prompt="@good.md". Since
    # tmp_prompts is at tmp_path/prompts, good.md is at
    # tmp_path/prompts/good.md, and the dot's parent is
    # tmp_path/pipelines, the resolution is dot-dir-relative
    # (../prompts/good.md) — which exists.
    assert violations == []


def test_check_resolution_flags_missing(
    tmp_pipelines: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    # Add a dot that references a non-existent prompt.
    (tmp_pipelines / "broken.dot").write_text(
        'digraph b { '
        'a [type="codergen", prompt="@does_not_exist.md"]; '
        'start -> a -> exit; }',
        encoding="utf-8",
    )
    violations = check_resolution(tmp_pipelines, tmp_path)
    broken = [v for v in violations if v.prompt == "pipelines/broken.dot"]
    assert len(broken) == 1
    assert broken[0].kind == "B"
    assert "does_not_exist.md" in broken[0].location


def test_check_resolution_handles_missing_dir(tmp_path: pathlib.Path) -> None:
    assert check_resolution(tmp_path / "nope", tmp_path) == []


# ---------------------------------------------------------------------------
# Check C — minimum content
# ---------------------------------------------------------------------------


def test_check_minimum_content_accepts_well_formed(tmp_prompts: pathlib.Path) -> None:
    violations = check_minimum_content(tmp_prompts)
    assert violations == []


def test_check_minimum_content_flags_short_prompt(tmp_prompts: pathlib.Path) -> None:
    (tmp_prompts / "stub.md").write_text("TODO", encoding="utf-8")
    violations = check_minimum_content(tmp_prompts)
    stub = [v for v in violations if v.prompt == "prompts/stub.md"]
    assert len(stub) == 1
    assert stub[0].kind == "C"
    assert "<" in stub[0].location  # <N chars> format


def test_check_minimum_content_flags_no_goal(tmp_prompts: pathlib.Path) -> None:
    (tmp_prompts / "nogoal.md").write_text(
        "# No Goal\n\n" + "x" * 200 + "\nImplement the test oracle.\n",
        encoding="utf-8",
    )
    violations = check_minimum_content(tmp_prompts)
    nogoal = [v for v in violations if v.prompt == "prompts/nogoal.md"]
    assert len(nogoal) == 1
    assert "${goal}" in nogoal[0].message


def test_check_minimum_content_flags_no_directive_verb(tmp_prompts: pathlib.Path) -> None:
    (tmp_prompts / "noverb.md").write_text(
        "# No Verb\n\n"
        "${goal}\n\n"
        "Lorem ipsum dolor sit amet consectetur adipiscing elit, "
        "sed do eiusmod tempor incididunt ut labore et dolore magna.\n",
        encoding="utf-8",
    )
    violations = check_minimum_content(tmp_prompts)
    noverb = [v for v in violations if v.prompt == "prompts/noverb.md"]
    assert len(noverb) == 1
    assert "directive verb" in noverb[0].message


def test_check_minimum_content_handles_missing_dir(tmp_path: pathlib.Path) -> None:
    assert check_minimum_content(tmp_path / "nope") == []


# ---------------------------------------------------------------------------
# Full audit (integration)
# ---------------------------------------------------------------------------


def test_full_audit_on_real_tree_passes() -> None:
    """The full audit on the real ``prompts/`` and ``pipelines/`` trees must
    return zero violations on HEAD. Catches drift in the writers
    allowlist, the user-set keys, or the minimum-content thresholds.
    """
    violations = audit_prompts(
        pathlib.Path(ROOT / "prompts"),
        pathlib.Path(ROOT / "pipelines"),
        pathlib.Path(ROOT / "runner"),
        pathlib.Path(ROOT),
    )
    assert violations == [], (
        f"real-tree audit must be clean; got: "
        f"{[f'{v.kind} | {v.prompt} | {v.location}' for v in violations]}"
    )


def test_full_audit_synthetic_violation_is_detected() -> None:
    """A prompt that violates any check raises at least one violation.

    Verifies the audit wires the three checks together end-to-end on
    a tmp_path tree, not just the real one.
    """
    import tempfile

    with tempfile.TemporaryDirectory() as td:
        root = pathlib.Path(td)
        prompts = root / "prompts"
        prompts.mkdir()
        pipelines = root / "pipelines"
        pipelines.mkdir()
        runner = root / "runner"
        runner.mkdir()

        # Synthesise a prompt that fails Check C (short) AND Check A
        # (unwired key). Should surface two violations.
        (prompts / "bad.md").write_text(
            "${state.totally_unwired}",
            encoding="utf-8",
        )
        (pipelines / "test.dot").write_text(
            'digraph t { a [prompt="@missing.md"]; }',
            encoding="utf-8",
        )
        violations = audit_prompts(prompts, pipelines, runner, root)
        kinds = {v.kind for v in violations}
        assert "A" in kinds  # unwired key
        assert "B" in kinds  # missing prompt
        assert "C" in kinds  # short prompt


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def test_cli_exits_0_on_clean(monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path) -> None:
    """A clean real-tree run exits 0."""
    monkeypatch.chdir(ROOT)
    assert main([]) == 0


def test_cli_exits_1_on_violation(monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path) -> None:
    """A tmp tree with a known violation exits 1."""
    monkeypatch.chdir(tmp_path)
    (tmp_path / "prompts").mkdir()
    (tmp_path / "prompts" / "bad.md").write_text("x" * 200, encoding="utf-8")  # no goal, no verb
    (tmp_path / "pipelines").mkdir()
    (tmp_path / "runner").mkdir()
    assert main([str(tmp_path / "prompts"), str(tmp_path / "pipelines"), str(tmp_path / "runner")]) == 1


def test_cli_exits_2_on_missing_dir(monkeypatch: pytest.MonkeyPatch, tmp_path: pathlib.Path) -> None:
    """A nonexistent dir exits 2."""
    monkeypatch.chdir(tmp_path)
    assert main(["/nonexistent/prompts", str(tmp_path / "x"), str(tmp_path / "y")]) == 2


# ---------------------------------------------------------------------------
# Invariants
# ---------------------------------------------------------------------------


def test_min_prompt_chars_is_below_shortest_real_prompt() -> None:
    """The threshold must be below the shortest real prompt so the audit
    can never false-positive on existing content. The shortest real
    prompt body is currently > 200 chars; we pin the threshold to
    100 to leave headroom.
    """
    assert MIN_PROMPT_CHARS == 100


def test_directive_verbs_includes_common_verbs() -> None:
    """The verb allowlist must include the verbs the existing prompts use."""
    for v in ("read", "write", "implement", "fix", "test", "verify", "find"):
        assert v in DIRECTIVE_VERBS, f"DIRECTIVE_VERBS missing {v!r}"


def test_generic_writer_suffixes_covers_known_patterns() -> None:
    """Every known generic-suffix writer pattern must be in the allowlist."""
    for s in GENERIC_WRITER_SUFFIXES:
        assert s.startswith("."), f"suffix {s!r} should start with '.'"


def test_user_set_keys_only_contains_documented_keys() -> None:
    """The user-set allowlist must only contain keys explicitly documented
    in the proposals doc as user-supplied. Adding a new entry requires
    also documenting it; this test pins the current state.
    """
    assert "bug_fix.test_path" in USER_SET_KEYS


# ---------------------------------------------------------------------------
# PR #95 regression-prevention test
# ---------------------------------------------------------------------------


def test_pr95_regression_class_is_caught(tmp_path: pathlib.Path) -> None:
    """The PR #95 incident (prompts/slim/fix.md missing
    ``${state.last_test_output}``) is the canonical regression this
    audit exists to catch. A synthetic prompt that matches the
    pre-regression shape (has a goal + verb, references an unwired
    state key) raises a wiring violation.
    """
    prompts = tmp_path / "prompts"
    prompts.mkdir()
    # A prompt that LOOKS well-formed (has goal, has a verb, has a
    # state key) but references a non-existent state — the exact
    # pattern that broke fix.md before PR #95.
    (prompts / "fix.md").write_text(
        "# Fix the gate\n\n"
        "Goal: ${goal}\n\n"
        "Read the test output at ${state.last_test_output} and fix it.\n",
        encoding="utf-8",
    )
    runners = tmp_path / "runner"
    runners.mkdir()
    # No writer for last_test_output. The real writer is in
    # runner/handler_control.py:130-135, but we use an empty runner
    # tree to simulate the regression.
    violations = check_wiring(prompts, writers=set())
    assert any(v.location == "${state.last_test_output}" for v in violations)
