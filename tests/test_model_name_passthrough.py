"""Regression tests for the `model_name` coder-model pin on the claude backend.

Spec: specs/workflow_graphgen.md §7 / benchmarks/.../workflow_graphgen.feature.md
"Model pin prerequisite". The benchmark pins the coder to Sonnet via a new node
attribute named `model_name` (NOT `model`, which handlers.py:246 already treats
as a backend alias). The claude branch must:
  1. emit `--model <value>` iff `model_name` is set, and
  2. still dispatch a node that sets `model_name` but no `backend` to the claude
     branch (the model string must never become a backend name).
"""

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

import runner.handlers as handlers_mod  # noqa: E402
from runner.handlers import Context, Node, _codergen  # noqa: E402


def _patch_claude(monkeypatch, commands):
    """Wire the claude backend to a deterministic capture seam."""

    def fake_sandbox(args, workdir=None):
        # `_sandboxed_args(args)` (legacy AO/shadow path) and
        # `_sandboxed_args_for_workdir(args, ctx.workdir)` (Lane H coder path)
        # both delegate to the same fake seam in this test — the workdir is
        # only used by the real implementation to enumerate sealed doc paths.
        return ["sandboxed", *args]

    def fake_run(args, **kwargs):
        commands.append(args)

        class Proc:
            returncode = 0
            stdout = "done"
            stderr = ""

        return Proc()

    monkeypatch.setattr(handlers_mod, "_get_claude_executable", lambda: "claude")
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", fake_sandbox)
    # Lane H (jleechan-113) routes claude/codex/agy coder subprocesses through
    # `_sandboxed_args_for_workdir` so the deny list also covers the
    # implementing agent's own `benchmarks/*/{README,DESIGN,SCORING,SCENARIOS}.md`.
    # Patch the new helper too so the test can assert on the dispatch layout
    # without depending on the real sandbox-exec profile.
    monkeypatch.setattr(handlers_mod, "_sandboxed_args_for_workdir", fake_sandbox)
    monkeypatch.setattr(handlers_mod.subprocess, "run", fake_run)


def test_model_flag_present_when_model_name_set(monkeypatch, tmp_path):
    commands = []
    (tmp_path / "prompt.md").write_text("build it")
    _patch_claude(monkeypatch, commands)

    ctx = Context(goal="t", workdir=tmp_path, backend="claude", state={})
    result = _codergen(
        Node(name="implement", attrs={"prompt": "@prompt.md", "model_name": "claude-sonnet-4-6"}),
        ctx,
    )

    assert result.outcome == "success"
    assert commands, "claude subprocess never invoked"
    argv = commands[0]
    # `--model claude-sonnet-4-6` present as an adjacent pair.
    assert "--model" in argv
    assert argv[argv.index("--model") + 1] == "claude-sonnet-4-6"
    # Prompt text stays the final positional arg for `claude --print`.
    assert argv[-1] == "build it"


def test_model_flag_absent_when_model_name_unset(monkeypatch, tmp_path):
    commands = []
    (tmp_path / "prompt.md").write_text("build it")
    _patch_claude(monkeypatch, commands)

    ctx = Context(goal="t", workdir=tmp_path, backend="claude", state={})
    result = _codergen(Node(name="implement", attrs={"prompt": "@prompt.md"}), ctx)

    assert result.outcome == "success"
    assert commands, "claude subprocess never invoked"
    assert "--model" not in commands[0]


def test_model_name_without_backend_still_dispatches_to_claude(monkeypatch, tmp_path):
    """A node with `model_name` but no `backend` must route to the claude branch
    via ctx.backend — the model string must never be read as a backend name."""
    commands = []
    (tmp_path / "prompt.md").write_text("build it")
    _patch_claude(monkeypatch, commands)

    ctx = Context(goal="t", workdir=tmp_path, backend="claude", state={})
    result = _codergen(
        Node(name="implement", attrs={"prompt": "@prompt.md", "model_name": "claude-sonnet-4-6"}),
        ctx,
    )

    assert result.outcome == "success"
    assert commands, "claude subprocess never invoked"
    argv = commands[0]
    # Claude-branch signature: the `--print` headless flag is present, proving we
    # dispatched to the claude backend and not to a backend named after the model.
    assert "--print" in argv
    assert argv[1] == "claude"
    assert argv[argv.index("--model") + 1] == "claude-sonnet-4-6"
