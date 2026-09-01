"""Regression tests for jleechan-7t92 (GH #226): the gap PR #228 (f59b488)
left behind after fixing the ``TypeError: replace() argument 2 must be str,
not dict`` crash in ``handler_render._substitute_placeholders``.

#228 fixed exactly one of the two ``${state.<key>}`` substitution sites in
the runner (``handler_render._substitute_placeholders``, used for prompt
templates) but left the second site (``handler_decision._substitute_state``,
used for ``tool`` node ``command=``/``cwd=`` attrs and ``holdout_eval``
``feature=``/test_path attrs) still rendering structured values via bare
``str()`` — i.e. Python ``repr()`` (single-quoted, ``True``/``None``
spelled Python-style) instead of the repository's established JSON
convention (``runner/cxdb.py`` event metadata, ``handler_dispatch.py``'s
``resolved_backend_meta``, and the now-fixed ``_substitute_placeholders``).

This file proves four things:
  1. Both substitution sites now serialize dict/list identically (JSON,
     sort_keys) via the shared ``handler_core._serialize_state_value``.
  2. A genuinely unserializable value never crashes a render; the fallback
     names the key and the value's Python type only, never its content.
  3. A real SpecGen fix-loop render (the actual production crash path:
     runs 7fffdced606f / ac53c47830b2) survives dict/list/scalar state
     sitting in ``ctx.state`` — exercised through ``runner.engine.run`` on
     the real ``pipelines/slim/spec_gen.dot`` graph, not a hand-built
     Context disconnected from the pipeline.

     NOTE on scope (caught by adversarial review of this PR): ``spec_gen.dot``
     is a spec-only lane (see ``test_spec_gen_has_no_implement_node``) with no
     ``tool``/``holdout_eval`` nodes, so test #3 only exercises the
     ``_substitute_placeholders`` (prompt-render) site that #228 already
     fixed — it is a genuine end-to-end regression guard for the literal
     "SpecGen fix-loop render" the bead names, but it does NOT exercise this
     PR's own ``handler_decision._substitute_state`` fix. Test #4 below
     closes that: it drives the real ``_tool`` node handler (subprocess and
     all) so the ``handler_decision.py`` fix gets its own engine-level,
     non-unit-test proof.
  4. The real ``tool`` node handler (``handler_control._tool``), which is
     ``_substitute_state``'s actual production call site, renders a dict
     state value as JSON in the executed shell command — not the pre-fix
     Python repr — proven by inspecting the subprocess's actual stdout.
"""

from __future__ import annotations

import json
import pathlib
import sys
import tempfile

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

# Scratch workdir in the OS tempdir — using the repo root here leaks one
# branch_* mkdtemp per fan-out test into the working tree.
SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="test_structured_state_convention_"))

from conftest import init_git_repo, register_scratch_dir  # noqa: E402

register_scratch_dir(SCRATCH)
init_git_repo(SCRATCH)

import runner.handlers as handlers_mod  # noqa: E402
from runner.engine import run  # noqa: E402
from runner.handler_core import _serialize_state_value  # noqa: E402
from runner.handler_decision import _substitute_state  # noqa: E402
from runner.handler_render import _substitute_placeholders  # noqa: E402
from runner.handlers import Context, TYPE_REGISTRY  # noqa: E402
from runner.parser import Node, parse  # noqa: E402

SPEC_GEN = ROOT / "pipelines" / "slim" / "spec_gen.dot"


class _Unserializable:
    """A value that raises from both json.dumps() and str()."""

    def __repr__(self):  # pragma: no cover - only invoked if a fallback fails
        raise RuntimeError("must never be reached")

    def __str__(self):
        raise RuntimeError("sealed content leak probe -- must not appear in output")


# ---------------------------------------------------------------------------
# 1. Shared helper: single JSON convention, safe key+type fallback
# ---------------------------------------------------------------------------


class TestSerializeStateValueConvention:
    def test_dict_renders_as_sorted_json_not_python_repr(self):
        rendered = _serialize_state_value("k", {"b": 2, "a": 1})
        assert rendered == json.dumps({"b": 2, "a": 1}, sort_keys=True)
        # Python repr would use single quotes; JSON must use double quotes.
        assert "'" not in rendered

    def test_list_renders_as_json(self):
        rendered = _serialize_state_value("k", ["b.py", "a.py"])
        assert rendered == json.dumps(["b.py", "a.py"], sort_keys=True)

    def test_scalar_renders_via_str(self):
        assert _serialize_state_value("k", 3) == "3"
        assert _serialize_state_value("k", True) == "True"

    def test_unserializable_nested_value_falls_back_to_key_and_type_only(self):
        """A dict containing a non-JSON-serializable object must not crash
        and must not leak the object's content into the rendered text."""
        payload = {"secret": _Unserializable()}
        rendered = _serialize_state_value("holdout.secret_meta", payload)
        assert "holdout.secret_meta" in rendered
        assert "_Unserializable" in rendered or "dict" in rendered
        assert "sealed content leak probe" not in rendered

    def test_unserializable_scalar_falls_back_to_key_and_type_only(self):
        rendered = _serialize_state_value("weird_key", _Unserializable())
        assert "weird_key" in rendered
        assert "_Unserializable" in rendered
        assert "sealed content leak probe" not in rendered


# ---------------------------------------------------------------------------
# 2. Parity between the two substitution sites
# ---------------------------------------------------------------------------


class TestSubstitutionSiteParity:
    """_substitute_state (tool/holdout attrs) must match _substitute_placeholders
    (prompt templates) for the same structured ctx.state value — this is the
    literal gap #228 left: only the prompt-rendering site was fixed."""

    def test_substitute_state_renders_dict_as_json_like_substitute_placeholders(self):
        ctx = Context(goal="g", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["review_main.resolved_backend_meta"] = {
            "reviewer_backend_resolution": "priority_queue",
            "adversarial_resolved": "codex",
        }
        text = "${state.review_main.resolved_backend_meta}"

        from_prompt_path = _substitute_placeholders(text, ctx)
        from_tool_path = _substitute_state(text, ctx)

        assert from_prompt_path == from_tool_path, (
            "the two ${state.*} substitution sites must serialize the same "
            "dict value identically (single repository convention), got "
            f"prompt-path={from_prompt_path!r} tool-path={from_tool_path!r}"
        )
        # And it must actually be JSON, not the pre-fix Python repr.
        assert "'" not in from_tool_path

    def test_substitute_state_no_longer_uses_python_repr_for_dict(self):
        """RED-proof regression: before this fix, _substitute_state used bare
        str(v) on dicts, producing single-quoted Python repr. Confirm the
        specific repr artifact is gone."""
        ctx = Context(goal="g", workdir=pathlib.Path("/tmp"), backend="echo")
        ctx.state["x"] = {"a": 1}
        rendered = _substitute_state("${state.x}", ctx)
        assert rendered == '{"a": 1}'
        assert rendered != "{'a': 1}"

    def test_substitute_state_unresolved_placeholder_left_intact(self):
        """Existing contract preserved: a key with no matching state entry is
        left as the literal marker (documented downstream-failure-visible
        behavior), unaffected by the serialization convention change."""
        ctx = Context(goal="g", workdir=pathlib.Path("/tmp"), backend="echo")
        rendered = _substitute_state("${state.missing_key}", ctx)
        assert rendered == "${state.missing_key}"


# ---------------------------------------------------------------------------
# 3. Real SpecGen fix-loop render: dict/list/scalar state must not crash
# ---------------------------------------------------------------------------


def test_spec_gen_fix_main_render_survives_dict_list_scalar_state(monkeypatch, tmp_path):
    """Regression test for jleechan-7t92 (GH #226), reproducing the actual
    production crash conditions from SpecGenFactory runs 7fffdced606f and
    ac53c47830b2: the pre-#228 ``_substitute_placeholders`` called
    ``text.replace(placeholder, v)`` unconditionally for *every* ctx.state
    entry regardless of whether that entry's placeholder even appeared in
    the rendered text -- so a single dict value anywhere in ctx.state (e.g.
    ``resolved_backend_meta`` from a backend_priority-resolved reviewer)
    crashed every subsequent codergen render, including fix_main's, via the
    real ``pipelines/slim/spec_gen.dot`` fix loop.

    This drives the actual engine (``runner.engine.run``) over the real
    graph so it exercises ``_codergen`` -> ``_render_prompt`` ->
    ``_substitute_placeholders`` for the real ``fix_main`` node and its real
    ``@prompts/slim/fix_spec.md`` template -- not a hand-built Context
    disconnected from the pipeline (unlike the unit tests in
    test_state_dict_substitution.py, which call _substitute_placeholders
    directly and never touch the SpecGen fix loop).

    SCOPE NOTE: this test alone does not prove this PR's own fix (the
    ``handler_decision._substitute_state`` JSON-convention alignment) did
    anything, because ``spec_gen.dot`` has no ``tool``/``holdout_eval``
    nodes for ``_substitute_state`` to run on -- it would pass unchanged even
    with ONLY #228 applied and this PR's ``handler_decision.py`` change
    reverted (verified manually while preparing this PR). It IS a genuine,
    non-tautological regression guard for the literal "SpecGen fix-loop
    render" scenario the bead names, and it does fail with a real
    ``TypeError`` against the pre-#228 code (verified against commit
    ``f59b488~1``). See
    ``test_tool_node_renders_dict_state_as_json_not_python_repr`` below for
    the engine-level proof of this PR's actual new fix.
    """
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)

    g = parse(SPEC_GEN)
    g.nodes["plan_main"].attrs["backend"] = "echo"
    g.nodes["plan_attractor"].attrs["backend"] = "echo"

    ctx = Context(
        goal="dict/list/scalar state must not crash the SpecGen fix loop",
        workdir=SCRATCH,
        backend="echo",
    )
    ctx.state["review_attractor.outcome"] = "success"

    # Structured state stashed by an upstream node, mirroring the real
    # production shape (a dict like resolved_backend_meta), plus a list and
    # a bare scalar for full acceptance-criteria coverage.
    ctx.state["review_main.resolved_backend_meta"] = {
        "reviewer_backend_resolution": "priority_queue",
        "adversarial_resolved": "codex",
    }
    ctx.state["some_list_value"] = ["a.py", "b.py"]
    ctx.state["some_scalar_value"] = 3

    call_count = {"n": 0}
    original_handler = TYPE_REGISTRY["codergen"]

    def patched_codergen(node, _ctx):
        if node.name == "review_main":
            call_count["n"] += 1
            if call_count["n"] == 1:
                from runner.handlers import Result
                return Result(outcome="failure", output="spec missing non-goals")
            from runner.handlers import Result
            return Result(outcome="success", output="spec approved after fix")
        if node.name == "review_attractor":
            from runner.handlers import Result
            return Result(outcome="success", output="attractor spec approved")
        return original_handler(node, _ctx)

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", patched_codergen)

    # The assertion of record: this must not raise TypeError. On the
    # pre-#228 handler_render.py this call reproduces the exact production
    # crash (`TypeError: replace() argument 2 must be str, not dict`).
    history = run(g, ctx, checkpoint=tmp_path / "checkpoint.json")

    assert history[-1].outcome == "success"
    node_names = [step.node for step in history]
    assert "fix_main" in node_names
    assert node_names.count("review_main") == 2


def test_tool_node_renders_dict_state_as_json_not_python_repr(monkeypatch, tmp_path):
    """Engine-level proof of THIS PR's actual new fix: the real ``tool`` node
    handler (``handler_control._tool``) is ``_substitute_state``'s only real
    production call site (alongside ``handler_holdout`` and
    ``handler_special_gates``). Drive it end-to-end -- real ``Node``, real
    ``_tool(node, ctx)``, real subprocess -- with a dict-valued ctx.state
    entry referenced by the node's ``command=`` attribute, and inspect the
    subprocess's actual captured stdout.

    Before this PR, ``_substitute_state`` rendered the dict via bare
    ``str(v)``, so the shell would have received the Python-repr text
    ``{'a': 1, 'b': 2}`` (single-quoted -- not valid JSON, and inconsistent
    with the repository's structured-data convention). After this PR it
    receives ``{"a": 1, "b": 2}`` (sort-keyed JSON, matching the now-fixed
    ``_substitute_placeholders`` prompt-rendering path exactly).
    """
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)

    tool_handler = TYPE_REGISTRY["tool"]
    node = Node(
        name="probe_tool",
        attrs={
            "type": "tool",
            # Single-quote the placeholder: the substituted JSON value itself
            # contains double quotes, so wrapping it in double quotes would
            # confuse shlex.split's posix tokenizer. Single quotes let shlex
            # pass the whole JSON string through as one verbatim argument.
            "command": "echo '${state.review_main.resolved_backend_meta}'",
            "timeout": "30",
        },
    )
    ctx = Context(goal="tool node dict-state JSON convention", workdir=SCRATCH, backend="echo")
    ctx.state["review_main.resolved_backend_meta"] = {"b": 2, "a": 1}

    result = tool_handler(node, ctx)

    assert result.outcome == "success", result.output
    # Must be valid JSON, sort-keyed, matching _serialize_state_value exactly.
    assert json.loads(result.output.strip()) == {"a": 1, "b": 2}
    assert result.output.strip() == '{"a": 1, "b": 2}'
    # The pre-fix Python-repr artifact must be gone from the real subprocess output.
    assert "'a': 1" not in result.output
    assert "'b': 2" not in result.output
