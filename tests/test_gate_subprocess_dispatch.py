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
    """When prefer_adversarial empties the post-filter priority list, the
    resolver must NOT short-circuit to ``claude-sonnet``; it must probe the
    default priority (codex, minimax, agy, claude-sonnet) so cross-vendor
    review is a real subprocess, not a label."""
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
    assert resolved == "claude-sonnet"
    # If the resolver had used the empty-list short-circuit, the skip
    # list would be empty (nothing was probed). With the default-priority
    # fallback, the skip list records codex, minimax, agy, and any
    # earlier default entries that were probed and skipped.
    skipped = meta["adversarial_skipped"].split(",") if meta["adversarial_skipped"] else []
    assert "codex" in skipped, (
        f"empty-list fallback must probe the default priority; "
        f"skipped list missing 'codex': {skipped!r}"
    )
    assert "minimax" in skipped
    assert "agy" in skipped
