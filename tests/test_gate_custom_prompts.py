"""Custom prompt gates — gate_er/es/cs with a node-level prompt="@path" attr
route the authored template (not the /er slash command or universal
evidence prompt) to the reviewer backend. PR #39 Bugbot HIGH regression.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def test_gate_er_with_prompt_attr_sends_custom_prompt_to_reviewer(monkeypatch, tmp_path):
    """gate_er must render the node's own prompt template and dispatch it to
    the resolved reviewer backend with the SHA + verdict contract appended."""
    import runner.handlers as handlers_mod
    from runner.handlers import Context as HCtx, _gate_er
    from runner.parser import Node

    prompt_dir = tmp_path / "prompts" / "slim"
    prompt_dir.mkdir(parents=True)
    (prompt_dir / "spec_review.md").write_text(
        "You are an independent spec reviewer.\nGoal:\n${goal}\n"
    )
    # A local /er command file exists — the custom prompt must still win.
    cmd_dir = tmp_path / ".claude" / "commands"
    cmd_dir.mkdir(parents=True)
    (cmd_dir / "er.md").write_text("evidence review slash command")

    captured = {}

    def fake_execute_gate(prompt, expected_sha, timeout, ctx, name, backend):
        captured["prompt"] = prompt
        captured["sha"] = expected_sha
        captured["timeout"] = timeout
        captured["name"] = name
        captured["backend"] = backend
        return handlers_mod.Result(
            outcome="success", output="verdict: pass", metadata={"verdict": "pass"}
        )

    monkeypatch.setattr(handlers_mod, "_worktree_head_sha", lambda wd: "abc123def")
    monkeypatch.setattr(
        handlers_mod, "_resolve_gate_backend", lambda node, ctx: ("codex", {"adversarial_resolved": "codex"})
    )
    monkeypatch.setattr(handlers_mod, "_execute_gate", fake_execute_gate)

    node = Node(
        name="spec_review",
        attrs={
            "type": "gate_er",
            "prompt": "@prompts/slim/spec_review.md",
            "timeout": "600",
        },
    )
    ctx = HCtx(goal="review the spec", workdir=tmp_path, backend="claude")
    result = _gate_er(node, ctx)

    assert result.outcome == "success"
    # The authored review instructions reached the reviewer — not the /er
    # slash command and not the universal evidence template.
    assert "independent spec reviewer" in captured["prompt"]
    assert "Goal:\nreview the spec" in captured["prompt"]
    assert "evidence review slash command" not in captured["prompt"]
    # The runner-owned machine contract is appended.
    assert "head_sha: abc123def" in captured["prompt"]
    assert "verdict: <pass|fail>" in captured["prompt"]
    assert captured["sha"] == "abc123def"
    assert captured["timeout"] == 600
    assert captured["backend"] == "codex"
    # Priority-queue audit metadata merged onto the result.
    assert result.metadata["adversarial_resolved"] == "codex"


def test_gate_er_with_missing_prompt_template_errors(monkeypatch, tmp_path):
    """A review gate must not silently grade with stub instructions when the
    authored template is missing — it must surface an infra error."""
    import runner.handlers as handlers_mod
    from runner.handlers import Context as HCtx, _gate_er
    from runner.parser import Node

    monkeypatch.setattr(handlers_mod, "_worktree_head_sha", lambda wd: "abc123def")
    monkeypatch.setattr(
        handlers_mod,
        "_execute_gate",
        lambda *a, **k: (_ for _ in ()).throw(AssertionError("must not dispatch")),
    )

    node = Node(
        name="spec_review",
        attrs={"type": "gate_er", "prompt": "@prompts/does/not/exist.md"},
    )
    ctx = HCtx(goal="review the spec", workdir=tmp_path, backend="claude")
    result = _gate_er(node, ctx)

    assert result.outcome == "error"
    assert result.metadata.get("prompt_status") == "missing"


def test_gate_er_with_prompt_attr_echo_preseed(tmp_path):
    """Echo backend still honors the ctx.state pre-seed on custom-prompt gates."""
    from runner.handlers import Context as HCtx, _gate_er
    from runner.parser import Node

    node = Node(
        name="spec_review",
        attrs={"type": "gate_er", "prompt": "@prompts/slim/spec_review.md"},
    )
    ctx = HCtx(goal="g", workdir=tmp_path, backend="echo")
    ctx.state["spec_review.outcome"] = "failure"
    result = _gate_er(node, ctx)
    assert result.outcome == "failure"


def test_gate_es_and_cs_with_prompt_attr_route_custom_prompt(monkeypatch, tmp_path):
    """The prompt_ref routing applies to all three review gate types."""
    import runner.handlers as handlers_mod
    from runner.handlers import Context as HCtx, _gate_es, _gate_code_standards
    from runner.parser import Node

    prompt_dir = tmp_path / "prompts"
    prompt_dir.mkdir(parents=True)
    (prompt_dir / "custom.md").write_text("CUSTOM REVIEW BODY ${goal}")

    seen = []
    monkeypatch.setattr(handlers_mod, "_worktree_head_sha", lambda wd: "feedc0de")
    monkeypatch.setattr(
        handlers_mod, "_resolve_gate_backend", lambda node, ctx: ("claude", {})
    )
    monkeypatch.setattr(
        handlers_mod,
        "_execute_gate",
        lambda prompt, sha, timeout, ctx, name, backend: (
            seen.append((name, prompt)),
            handlers_mod.Result(outcome="success", output="verdict: pass", metadata={}),
        )[1],
    )

    node = Node(name="g1", attrs={"prompt": "@prompts/custom.md"})
    ctx = HCtx(goal="xyz", workdir=tmp_path, backend="claude")
    assert _gate_es(node, ctx).outcome == "success"
    assert _gate_code_standards(node, ctx).outcome == "success"
    assert [n for n, _ in seen] == ["gate_es", "gate_code_standards"]
    for _, prompt in seen:
        assert "CUSTOM REVIEW BODY xyz" in prompt
        assert "head_sha: feedc0de" in prompt
