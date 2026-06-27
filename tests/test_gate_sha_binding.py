"""Regression tests for gate HEAD-SHA binding.

The `_gate_*` handlers must inject the worktree HEAD SHA into the prompt
passed to `claude --print` and require the gate response to echo back a
matching `head_sha: <40-hex>` line. Without that binding a late-arriving
verdict could be applied to a different commit than the one it was meant
to review.

Design choice: the expected SHA is surfaced via PROMPT TEXT (the final
argv to `claude --print`), not via env. The prompt is the contract the
gate reads; env vars can be silently ignored. The fake claude binaries
below extract `expected_head_sha:` from their argv and conditionally
echo it back to simulate gate behaviour.
"""

from __future__ import annotations

import pathlib
import stat
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402

from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.parser import parse  # noqa: E402


def _head_sha() -> str:
    proc = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    )
    return proc.stdout.strip().lower()


def _install_fake_claude(tmp_path: pathlib.Path, monkeypatch, script: str) -> None:
    """Drop a fake `claude` executable on PATH for the duration of one test.

    Also monkeypatches `_get_claude_executable` so the runner does not skip
    PATH lookup in favour of a real NVM-installed claude binary on the dev
    machine.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    claude = bin_dir / "claude"
    claude.write_text(script)
    claude.chmod(claude.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("PATH", f"{bin_dir}:/usr/bin:/bin")
    # Force the runner to use the bare `claude` name so PATH lookup hits
    # our fake binary instead of the NVM-installed one.
    from runner import handlers as _h
    monkeypatch.setattr(_h, "_get_claude_executable", lambda: "claude")
    # Disable sandbox-exec so our fake binary runs unsandboxed (sandbox-exec
    # may not be available in all CI environments and we need predictable
    # subprocess output).
    monkeypatch.setenv("DISABLE_SANDBOX", "1")


def _seed_other_gates(ctx: Context) -> None:
    """Force the gates pipeline to flow through gate_es deterministically."""
    # gate_es is the first gate; we don't pre-seed it (we want to exercise
    # the real claude-backend path). gate_er / gate_cs are seeded so the
    # pipeline can complete without spawning more fake binaries.
    # gate_skeptic and adversarial_reviewer are pre-seeded because they now
    # sit between holdout and gate_es in the Level-5 topology.
    ctx.state["gate_skeptic.outcome"] = "success"
    ctx.state["adversarial_reviewer.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "success"
    ctx.state["gate_cs.outcome"] = "success"


def _mock_adversarial_reviewer(monkeypatch) -> None:
    """Replace gate_skeptic so SHA tests focus on the target gate."""
    # gate_skeptic has no registered handler; default _codergen only honors
    # ctx.state seeding in echo backend. Register an echo-seeded fake so
    # backend=claude tests don't try to call a real LLM. adversarial_reviewer
    # is now type=parallel_reviewer and honors ctx.state directly.
    def fake_skeptic(node, ctx):
        pre = ctx.state.get(f"{node.name}.outcome")
        return Result(outcome=pre or "success", output=f"fake_skeptic({node.name})")
    monkeypatch.setitem(TYPE_REGISTRY, "gate_skeptic", fake_skeptic)


def test_gate_missing_head_sha_echo_is_error(tmp_path, monkeypatch):
    """Verdict PASS but no head_sha line → outcome=error (NOT success)."""
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    _mock_adversarial_reviewer(monkeypatch)

    # Fake claude: prints a PASS verdict but does NOT echo head_sha.
    # Exit 0 so the rc!=0 fail-closed branch does NOT swallow the bug —
    # this isolates the SHA-binding check.
    script = "#!/bin/sh\nprintf 'VERDICT: PASS\\n'\nexit 0\n"
    _install_fake_claude(tmp_path, monkeypatch, script)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="claude")
    _seed_other_gates(ctx)

    history = run(g, ctx, max_steps=20)

    gate_es_step = next(r for r in history if r.node == "gate_es")
    assert gate_es_step.outcome == "error", (
        f"missing head_sha must collapse to error, got {gate_es_step.outcome}"
    )
    # Pipeline should also terminate non-successfully because gate_es errored.
    assert history[-1].outcome != "success"


def test_gate_wrong_head_sha_echo_is_error(tmp_path, monkeypatch):
    """Verdict PASS with head_sha mismatching expected → outcome=error."""
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    _mock_adversarial_reviewer(monkeypatch)

    # 40-hex SHA that is guaranteed not to match the real worktree HEAD.
    wrong_sha = "0" * 40
    script = (
        "#!/bin/sh\n"
        "printf 'VERDICT: PASS\\n'\n"
        f"printf 'head_sha: {wrong_sha}\\n'\n"
        "exit 0\n"
    )
    _install_fake_claude(tmp_path, monkeypatch, script)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="claude")
    _seed_other_gates(ctx)

    history = run(g, ctx, max_steps=20)

    gate_es_step = next(r for r in history if r.node == "gate_es")
    assert gate_es_step.outcome == "error", (
        f"mismatched head_sha must collapse to error, got {gate_es_step.outcome}"
    )
    assert history[-1].outcome != "success"


def test_gate_correct_head_sha_echo_is_success(tmp_path, monkeypatch):
    """Verdict PASS + correct head_sha echo → outcome=success.

    The fake claude binary extracts `expected_head_sha:` from its argv (the
    prompt text the runner passes) and echoes it back, simulating a gate
    that honoured the SHA-binding directive in the prompt.
    """
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    _mock_adversarial_reviewer(monkeypatch)

    # The fake binary parses its own argv for `expected_head_sha:` and
    # echoes that value back as `head_sha:`. This is exactly what a real
    # gate-aware LLM would do when it sees the directive in the prompt.
    script = (
        "#!/bin/sh\n"
        "# Concatenate all argv into one string and grep out the expected SHA.\n"
        "prompt=\"$*\"\n"
        "sha=$(printf '%s' \"$prompt\" | grep -oE 'expected_head_sha: [0-9a-fA-F]{40}' | head -1 | awk '{print $2}')\n"
        "printf 'VERDICT: PASS\\n'\n"
        "printf 'head_sha: %s\\n' \"$sha\"\n"
        "exit 0\n"
    )
    _install_fake_claude(tmp_path, monkeypatch, script)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="claude")
    _seed_other_gates(ctx)

    history = run(g, ctx, max_steps=20)

    gate_es_step = next(r for r in history if r.node == "gate_es")
    assert gate_es_step.outcome == "success", (
        f"correct head_sha echo must yield success, got {gate_es_step.outcome}"
    )
    # Sanity-check the expected SHA is the real worktree HEAD.
    assert _head_sha()  # 40-hex, raises if not in a repo


def test_gate_sha_binding_preserves_rc_nonzero_fail_closed(tmp_path, monkeypatch):
    """rc!=0 + PASS verdict must still collapse to error even when the
    fake binary echoes a correct head_sha (preserves existing fail-closed
    behaviour from PR for `_parse_verdict`)."""
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    _mock_adversarial_reviewer(monkeypatch)

    script = (
        "#!/bin/sh\n"
        "prompt=\"$*\"\n"
        "sha=$(printf '%s' \"$prompt\" | grep -oE 'expected_head_sha: [0-9a-fA-F]{40}' | head -1 | awk '{print $2}')\n"
        "printf 'VERDICT: PASS\\n'\n"
        "printf 'head_sha: %s\\n' \"$sha\"\n"
        "exit 23\n"  # non-zero: claude crashed despite emitting a pass line
    )
    _install_fake_claude(tmp_path, monkeypatch, script)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="claude")
    _seed_other_gates(ctx)

    history = run(g, ctx, max_steps=20)

    gate_es_step = next(r for r in history if r.node == "gate_es")
    assert gate_es_step.outcome == "error", (
        f"rc!=0 + pass must collapse to error even with correct SHA, "
        f"got {gate_es_step.outcome}"
    )
