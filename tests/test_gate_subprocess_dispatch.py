"""Adversarial-review dispatch: priority-queue must actually invoke the
resolved backend (agy / codex / minimax / claude-sonnet), not silently
collapse every non-agy name to claude. Cursor Bugbot flagged this as a
high-severity gap after the priority-queue landed.

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import os
import pathlib
import sys
import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def test_gate_subprocess_args_routes_codex_to_codex_cli(monkeypatch):
    """backend='codex' → argv starts with `codex exec --yolo`."""
    from runner.handlers import _gate_subprocess_args, Context as HCtx
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    argv = _gate_subprocess_args("codex", "PROMPT", ctx, 300)
    assert os.path.basename(argv[0]) == "codex", f"expected codex argv, got {argv[:3]!r}"
    assert "exec" in argv
    assert "--yolo" in argv
    assert "PROMPT" in argv
    # No silent collapse to claude.
    assert "claude" not in os.path.basename(argv[0])


def test_gate_subprocess_args_routes_claude_sonnet_to_claude_cli(monkeypatch):
    """backend='claude-sonnet' → argv starts with `claude --print` (not agy)."""
    from runner.handlers import _gate_subprocess_args, Context as HCtx
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    argv = _gate_subprocess_args("claude-sonnet", "PROMPT", ctx, 300)
    assert os.path.basename(argv[0]) == "claude", f"expected claude argv, got {argv[:3]!r}"
    assert "--print" in argv
    assert "PROMPT" in argv
    assert os.path.basename(argv[0]) != "agy"


def test_gate_subprocess_args_routes_bare_claude_to_claude_cli(monkeypatch):
    """backend='claude' (run-level default) → argv starts with `claude --print`."""
    from runner.handlers import _gate_subprocess_args, Context as HCtx
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    argv = _gate_subprocess_args("claude", "PROMPT", ctx, 300)
    assert os.path.basename(argv[0]) == "claude"
    assert "--print" in argv
    assert "PROMPT" in argv


def test_gate_subprocess_env_routes_minimax_through_minimax_gateway(monkeypatch):
    """backend='minimax' → ANTHROPIC_BASE_URL is set to the minimax gateway."""
    from runner.handlers import _gate_subprocess_env
    env = _gate_subprocess_env("minimax")
    assert env.get("ANTHROPIC_BASE_URL") == "https://api.minimax.io/anthropic"


def test_gate_subprocess_env_minimax_is_sanitized(monkeypatch):
    """The minimax override must layer on _sanitized_env, not raw os.environ —
    holdout vars must never reach a reviewer subprocess (jleechan-4pa)."""
    from runner.handlers import _gate_subprocess_env
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", "/secret/holdouts")
    monkeypatch.setenv("MY_HOLDOUT_SECRET", "sealed")
    env = _gate_subprocess_env("minimax")
    assert "DARK_FACTORY_HOLDOUTS" not in env
    assert "MY_HOLDOUT_SECRET" not in env
    assert env.get("ANTHROPIC_BASE_URL") == "https://api.minimax.io/anthropic"


def test_gate_subprocess_env_does_not_set_minimax_for_other_backends(monkeypatch):
    """backend='agy' / 'codex' / 'claude-sonnet' / 'claude' → no minimax override."""
    from runner.handlers import _gate_subprocess_env
    # Stub _sanitized_env to a clean baseline so a stray ANTHROPIC_BASE_URL
    # in the test runner's environment cannot leak in.
    monkeypatch.setattr(
        "runner.handlers._sanitized_env",
        lambda: {"PATH": "/usr/bin", "HOME": "/root"},
    )
    for backend in ("agy", "codex", "claude-sonnet", "claude"):
        env = _gate_subprocess_env(backend)
        assert env.get("ANTHROPIC_BASE_URL") != "https://api.minimax.io/anthropic", (
            f"{backend!r} must not carry the minimax base URL override"
        )


def test_execute_gate_runs_codex_subprocess_when_priority_resolves_codex(
    tmp_path, monkeypatch
):
    """_execute_gate with backend='codex' must actually invoke the codex
    subprocess, not silently fall back to claude. This is the end-to-end
    counterpart of Cursor Bugbot's high-severity finding: the priority
    queue used to be decorative for every non-agy name."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx
    fake_sha = "d" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        return _sp.CompletedProcess(
            cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n",
            stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "evidence", "codex")
    assert result.outcome == "success"
    assert seen, "subprocess.run must have been called"
    assert os.path.basename(seen[0][0]) == "codex", (
        f"codex-priority gate must invoke codex subprocess; got {seen[0][:1]!r}"
    )
    assert result.metadata["reviewer_backend"] == "codex"
    # No silent claude collapse.
    assert not any(os.path.basename(c[0]) == "claude" for c in seen), (
        "codex-priority gate must not also invoke claude"
    )


def test_execute_gate_writes_exact_prompt_sidecar(tmp_path, monkeypatch):
    """Reviewer gates must log the exact prompt sent to the backend."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "a" * 40

    def _fake_run(cmd, **kwargs):
        return _sp.CompletedProcess(
            cmd,
            0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n",
            stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude", run_id="promptlog1")
    ctx.event_log_path = tmp_path / "events.jsonl"
    setattr(ctx, "_df_current_seq", 7)
    setattr(ctx, "_df_current_attempt", 2)
    setattr(ctx, "_df_current_node", "evidence")

    result = _execute_gate("PROMPT WITH REVIEW INSTRUCTIONS", fake_sha, 300, ctx, "gate_er", "codex")

    assert result.outcome == "success"
    prompt_path = pathlib.Path(result.metadata["llm_prompt_path"])
    assert prompt_path.exists()
    assert prompt_path.read_text() == "PROMPT WITH REVIEW INSTRUCTIONS"
    assert result.metadata["llm_prompt_sha256"]
    events = [line for line in ctx.event_log_path.read_text().splitlines() if line.strip()]
    assert any('"event": "node_prompt"' in line and '"node": "evidence"' in line for line in events)


def test_complete_controller_prompt_is_not_rewrapped_for_shadow(tmp_path, monkeypatch):
    """Controller-owned review bytes must be identical in every reviewer lane."""
    from runner.handlers import Context as HCtx
    from runner.handler_dispatch import _launch_shadow_gate_review

    seen: list[list[str]] = []

    class _FakePopen:
        pid = 123
        returncode = 0

        def __init__(self, cmd, **kwargs):
            seen.append(cmd)

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _FakePopen)
    ctx = HCtx(goal="untrusted goal", workdir=tmp_path, backend="codex")

    prompt = "CONTROLLER-OWNED COMPLETE PROMPT"
    shadow = _launch_shadow_gate_review(
        "adversarial_reviewer",
        prompt,
        "a" * 40,
        300,
        ctx,
        prompt_is_complete=True,
    )

    assert shadow is not None
    assert shadow.prompt_is_complete is True
    assert shadow.prompt == prompt
    assert shadow.json_transport is True
    assert seen
    assert seen[0][-1] == "-"
    assert "--json" in seen[0]
    assert "--ephemeral" in seen[0]
    assert "--sandbox" in seen[0]
    assert "read-only" in seen[0]
    assert "--yolo" not in seen[0]
    assert "--dangerously-bypass-approvals-and-sandbox" not in seen[0]
    assert "--ignore-user-config" not in seen[0]
    assert "--ephemeral" in seen[0]
    assert "Normal gate prompt for comparison" not in " ".join(seen[0])


def test_launch_shadow_gate_review_uses_controller_cwd_and_sanitized_env(tmp_path, monkeypatch):
    """Controller-complete shadow launch must run from neutral cwd and
    sanitized environment."""
    from runner.handlers import Context as HCtx
    from runner.handler_dispatch import _launch_shadow_gate_review

    observed: dict[str, object] = {}

    class _FakePopen:
        pid = 321
        returncode = 0

        def __init__(self, cmd, **kwargs):
            observed["cwd"] = kwargs.get("cwd")
            observed["env"] = kwargs.get("env", {})

    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", "/secret/holdouts")
    monkeypatch.setenv("MY_HOLDOUT_SECRET", "sealed")
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _FakePopen)
    monkeypatch.setattr(
        "runner.handlers._get_claude_executable",
        lambda: "claude",
    )

    neutral = tmp_path / "controller-cwd"
    neutral.mkdir()
    ctx = HCtx(goal="review", workdir=tmp_path / "target", backend="codex")
    ctx.state["_df_controller_review_cwd"] = str(neutral)

    review = _launch_shadow_gate_review(
        "adversarial_reviewer",
        "COMPLETE REVIEW PROMPT",
        "a" * 40,
        300,
        ctx,
        prompt_is_complete=True,
    )

    assert review is not None
    assert review.prompt_is_complete is True
    assert review.json_transport is True
    assert observed.get("cwd") == neutral
    env = observed.get("env", {})
    assert isinstance(env, dict)
    assert "DARK_FACTORY_HOLDOUTS" not in env
    assert "MY_HOLDOUT_SECRET" not in env


def test_controller_codex_args_builds_stdin_transport():
    """Controller transport must use JSON transport on stdin and a neutral cwd."""
    from runner.handler_dispatch import _controller_codex_args
    argv = [
        "sandbox-exec",
        "-p",
        "(version 1)\n(allow default)",
        "codex",
        "exec",
        "--skip-git-repo-check",
        "PROMPT",
    ]
    transformed = _controller_codex_args(argv)
    assert transformed[-1] == "-"
    assert transformed == [
        "codex",
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "-",
    ]


def test_controller_codex_args_rejects_non_codex_command():
    """Unsupported backend command builders must fail closed."""
    from runner.handler_dispatch import _controller_codex_args
    with pytest.raises(ValueError, match="codex executable"):
        _controller_codex_args(["claude", "--print", "PROMPT"])


def test_controller_codex_args_rejects_unsafe_transport_options():
    """Unsafe Codex transport options must fail closed before launch."""
    from runner.handler_dispatch import _controller_codex_args

    legacy = _controller_codex_args([
        "codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        "PROMPT",
    ])
    assert "--yolo" not in legacy

    with pytest.raises(ValueError, match="unsafe codex flags"):
        _controller_codex_args([
            "codex",
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "PROMPT",
        ])
    with pytest.raises(ValueError, match="read-only"):
        _controller_codex_args([
            "codex",
            "exec",
            "--sandbox",
            "read-write",
            "PROMPT",
        ])
    with pytest.raises(ValueError, match="read-only"):
        _controller_codex_args([
            "codex",
            "exec",
            "--sandbox=read-write",
            "PROMPT",
        ])


def test_launch_shadow_gate_review_rejects_complete_controller_prompt_non_codex_backend(
    tmp_path, monkeypatch
):
    """Controller-owned prompt lanes must be codex-only, never other backends."""
    from runner.handlers import Context as HCtx
    from runner.handler_dispatch import _launch_shadow_gate_review

    class _FakePopen:
        def __init__(self, *args, **kwargs):
            raise AssertionError("shadow controller launch must fail closed")

    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _FakePopen)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)

    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    review = _launch_shadow_gate_review(
        "gate",
        "COMPLETE CONTROLLER PROMPT",
        "a" * 40,
        300,
        ctx,
        backend="claude",
        prompt_is_complete=True,
    )

    assert review is not None
    assert review.proc is None
    assert review.launch_error
    assert "codex backend" in review.launch_error.lower()


def test_execute_gate_uses_controller_codex_transport(tmp_path, monkeypatch):
    """Controller-JSON review transport must send prompt via stdin from neutral cwd."""
    import subprocess as _sp
    import json
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "f" * 40
    observed: dict[str, object] = {}

    def _fake_run(cmd, **kwargs):
        observed["cmd"] = cmd
        observed["input"] = kwargs.get("input")
        observed["cwd"] = kwargs.get("cwd")
        observed["env"] = kwargs.get("env", {})
        transport = json.dumps({
            "type": "item.completed",
            "item": {
                "type": "agent_message",
                "text": f"head_sha: {fake_sha}\nverdict: pass\n",
            },
        }) + "\n"
        return _sp.CompletedProcess(
            cmd,
            0,
            stdout=transport,
            stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", "/secret/holdouts")
    monkeypatch.setenv("MY_HOLDOUT_SECRET", "sealed")
    monkeypatch.setattr("subprocess.run", _fake_run)

    neutral = tmp_path / "controller-cwd"
    neutral.mkdir()
    ctx = HCtx(goal="test", workdir=tmp_path / "target", backend="claude", run_id="controller")
    ctx.state["_df_controller_review_json"] = "true"
    ctx.state["_df_controller_review_cwd"] = str(neutral)

    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "gate_er", "codex")

    assert result.outcome == "success"
    assert observed["input"] == "PROMPT"
    assert observed["cwd"] == neutral
    cmd = observed["cmd"]
    assert isinstance(cmd, list)
    assert cmd[-1] == "-"
    assert "--json" in cmd
    assert "--ephemeral" in cmd
    assert "--sandbox" in cmd
    assert "read-only" in cmd
    assert "--skip-git-repo-check" in cmd
    assert "--yolo" not in cmd
    env = observed.get("env", {})
    assert isinstance(env, dict)
    assert "DARK_FACTORY_HOLDOUTS" not in env
    assert "MY_HOLDOUT_SECRET" not in env


def test_execute_gate_rejects_controller_request_for_non_codex_backend(
    tmp_path, monkeypatch
):
    """Controller transport must be codex-only and fail before subprocess launch."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "a" * 40
    launched: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        launched.append(cmd)
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n")

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)
    ctx = HCtx(goal="test", workdir=tmp_path, backend="codex")
    ctx.state["_df_controller_review_json"] = "true"

    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "gate_er", "claude")

    assert result.outcome == "error"
    assert not launched
    assert "requires codex backend" in result.output


def test_execute_gate_runs_parallel_codex_shadow_review(tmp_path, monkeypatch):
    """Factory-run gates should compare the normal reviewer with a simple
    parallel Codex review and log both outputs.
    """
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx

    fake_sha = "b" * 40
    popen_cmds: list[list[str]] = []

    class _FakePopen:
        pid = 12345

        def __init__(self, cmd, **kwargs):
            popen_cmds.append(cmd)
            self.returncode = 0

        def communicate(self, timeout=None):
            return (
                f"head_sha: {fake_sha}\n"
                "## Review Verdict\nfail\n\n"
                "## Blocking Findings\n"
                "1. Severity: blocker\n"
                "   Evidence: artifact missing from bundle.\n"
                "   Why it matters: evidence reviewer should feed this to coder.\n"
                "   Fix: regenerate the bundle.\n\n"
                "verdict: fail\n",
                "",
            )

    def _fake_run(cmd, **kwargs):
        return _sp.CompletedProcess(
            cmd,
            0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n",
            stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _FakePopen)
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude", run_id="gateshadow1")
    ctx.state["_df_shadow_codex_review"] = "true"
    ctx.event_log_path = tmp_path / "events.jsonl"
    setattr(ctx, "_df_current_seq", 9)
    setattr(ctx, "_df_current_attempt", 1)
    setattr(ctx, "_df_current_node", "gate_er")

    result = _execute_gate("NORMAL GATE PROMPT", fake_sha, 300, ctx, "gate_er", "claude")

    assert result.outcome == "failure"
    assert popen_cmds
    assert popen_cmds[0][:4] == ["codex", "exec", "--yolo", "--skip-git-repo-check"]
    assert "## Parallel Codex Gate Review" in result.output
    assert "artifact missing from bundle" in result.output
    assert result.metadata["shadow_codex_gate_review"] == "true"
    assert result.metadata["shadow_codex_gate_outcome"] == "failure"
    assert result.metadata["shadow_codex_gate_verdict"] == "fail"
    prompt_path = pathlib.Path(result.metadata["shadow_codex_gate_prompt_path"])
    output_path = pathlib.Path(result.metadata["shadow_codex_gate_output_path"])
    assert prompt_path.exists()
    assert output_path.exists()
    assert "review this evidence" in prompt_path.read_text()
    assert "NORMAL GATE PROMPT" in prompt_path.read_text()
    assert "artifact missing from bundle" in output_path.read_text()
    events = ctx.event_log_path.read_text()
    assert '"event": "shadow_gate_prompt"' in events
    assert '"event": "shadow_gate_result"' in events


def test_execute_gate_runs_minimax_with_correct_env(monkeypatch, tmp_path):
    """_execute_gate with backend='minimax' invokes the claude CLI but with
    ANTHROPIC_BASE_URL set to the minimax gateway. The recorded reviewer
    name stays ``minimax`` (the cross-vendor intent) even though the
    subprocess is the claude binary."""
    import subprocess as _sp
    from runner.handlers import _execute_gate, Context as HCtx
    fake_sha = "e" * 40
    seen_cmds: list[list[str]] = []
    seen_envs: list[dict] = []

    def _fake_run(cmd, **kwargs):
        seen_cmds.append(cmd)
        seen_envs.append(kwargs.get("env", {}))
        return _sp.CompletedProcess(
            cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n",
            stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "evidence", "minimax")
    assert result.outcome == "success"
    assert os.path.basename(seen_cmds[0][0]) == "claude", (
        f"minimax backend invokes the claude CLI; got {seen_cmds[0][:1]!r}"
    )
    assert seen_envs[0].get("ANTHROPIC_BASE_URL") == "https://api.minimax.io/anthropic"
    # Recorded name is the cross-vendor intent, not the underlying CLI.
    assert result.metadata["reviewer_backend"] == "minimax"


def test_resolve_adversarial_backend_falls_back_to_default_when_post_filter_empty(
    monkeypatch,
):
    """A lane naming ONLY the coder's own backend keeps that entry.

    ``prefer_adversarial`` demotes rather than drops, so the single entry
    survives and the resolver returns it even though it is uninstalled.
    Recovery is ``_execute_gate``'s job: a missing binary is an infra failure
    that triggers its agy -> claude fallback. Resolving to an installed-but-
    unrequested vendor here would silently override a controller-review
    lane's codex-only queue."""
    from runner.handlers import _resolve_gate_backend, Context as HCtx
    from runner.parser import Node
    # All non-claude-sonnet backends are uninstalled; only claude-sonnet
    # is on PATH. With the old (buggy) behavior, an empty post-filter list
    # would have hardcoded ``claude-sonnet``. With the fix, the resolver
    # falls back to the default priority — which probes codex, minimax,
    # agy, then claude-sonnet in order. None of the first three are
    # installed, so it correctly lands on ``claude-sonnet`` via the
    # default-priority probe, with the FULL skip list recorded in
    # metadata (proving the probe path was actually taken).
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "claude-sonnet",
    )
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    # Lane says `backend_priority=claude` and `prefer_adversarial=true`
    # with a `claude` coder — the prefer_adversarial filter removes
    # `claude`, leaving the post-filter list empty. The fix is in
    # _resolve_gate_backend (not _resolve_adversarial_backend), so we
    # drive the entry point that actually owns the fallback.
    node = Node(
        name="evidence",
        attrs={
            "backend_priority": "claude",
            "prefer_adversarial": "true",
        },
    )
    resolved, meta = _resolve_gate_backend(node, ctx)

    # The lane's own entry survives demotion and is returned even though it
    # is uninstalled -- the resolver never substitutes a vendor the lane did
    # not ask for. `_execute_gate` owns recovery from the missing binary.
    assert resolved == "claude", (
        f"the lane's only entry must survive demotion; got {resolved!r}"
    )
    assert meta["adversarial_priority"] == "claude", (
        "the queue must stay exactly what the lane declared; got "
        f"{meta['adversarial_priority']!r}"
    )
    # It was really probed, not assumed present.
    skipped = meta["adversarial_skipped"].split(",") if meta["adversarial_skipped"] else []
    assert skipped == ["claude"], (
        f"the single entry must be probed and recorded as skipped; got {skipped!r}"
    )
