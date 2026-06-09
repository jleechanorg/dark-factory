"""Gate handler + CXDB + Healer smoke tests."""

from __future__ import annotations

import os
import pathlib
import stat
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.cxdb import CXDB  # noqa: E402
from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.handlers import _parse_verdict  # noqa: E402
from runner.healer import report  # noqa: E402
from runner.parser import parse  # noqa: E402


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


def test_parse_verdict_pass_warn_fail():
    assert _parse_verdict("blah\nVERDICT: PASS\n")[1] == "success"
    assert _parse_verdict("Overall: WARN — minor")[1] == "success"
    assert _parse_verdict("verdict: FAIL")[1] == "failure"
    assert _parse_verdict("Verdict: PARTIAL")[1] == "failure"
    assert _parse_verdict("verdict: INCONCLUSIVE")[1] == "failure"
    # Standalone-line fallback fires when no marker is present.
    assert _parse_verdict("everything is fine\nPASS\n")[1] == "success"
    # Prose that contains the word "pass" inside another phrase is NOT a verdict.
    assert _parse_verdict("everything is fine\nresult: pass needed")[1] == "failure"


def test_gate_echo_seeded_outcome(monkeypatch):
    """Gate handlers in echo mode pull outcome from ctx.state."""
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "success"
    ctx.state["gate_cs.outcome"] = "success"

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    assert nodes == ["start", "holdout", "gate_es", "gate_er", "gate_cs", "exit"]
    assert history[-1].outcome == "success"


def test_gate_failure_short_circuits(monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "failure"  # fail at /er
    ctx.state["gate_cs.outcome"] = "success"

    history = run(g, ctx, max_steps=20)
    nodes = [r.node for r in history]
    assert nodes == ["start", "holdout", "gate_es", "gate_er", "exit"]
    assert history[-1].outcome == "failure"


def test_gate_nonzero_returncode_cannot_spoof_pass(monkeypatch, tmp_path):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    claude = bin_dir / "claude"
    claude.write_text("#!/bin/sh\nprintf 'VERDICT: PASS\\n'\nexit 19\n")
    claude.chmod(claude.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setenv("PATH", f"{bin_dir}:{pathlib.Path('/usr/bin')}")

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="claude")

    history = run(g, ctx, max_steps=20)

    assert history[-1].outcome != "success"
    assert any(r.node == "gate_es" and r.outcome == "error" for r in history)


def test_cxdb_records_steps(tmp_path, monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    ctx.state.update(
        {"gate_es.outcome": "success", "gate_er.outcome": "success", "gate_cs.outcome": "success"}
    )
    run(g, ctx, max_steps=20)
    assert db_path.exists()

    db = CXDB(db_path)
    rows = list(db._conn.execute("SELECT node FROM steps ORDER BY seq").fetchall())
    db.close()
    assert [r[0] for r in rows] == [
        "start",
        "holdout",
        "gate_es",
        "gate_er",
        "gate_cs",
        "exit",
    ]


def test_healer_reports_failures(tmp_path, monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="fail", output="boom")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    run(g, ctx, max_steps=20)

    text = report(db_path)
    assert "holdout" in text
    assert "fail" in text.lower()
    assert "Prescription" in text or "prescription" in text.lower()


def test_healer_reports_gate_infra_errors(tmp_path, monkeypatch):
    """Gate infra errors are terminal failures and must be diagnosable."""
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=tmp_path / "cxdb.sqlite")
    ctx.state["gate_es.outcome"] = "success"
    ctx.state["gate_er.outcome"] = "error"

    history = run(g, ctx, max_steps=20)
    assert any(r.outcome == "error" for r in history)

    text = report(ctx.cxdb_path)
    assert "gate_er" in text
    assert "error" in text.lower()


def test_healer_no_failures(tmp_path, monkeypatch):
    fake_holdout = lambda node, ctx: Result(outcome="success", output="ok")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(_pipeline("gates.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    ctx.state.update(
        {"gate_es.outcome": "success", "gate_er.outcome": "success", "gate_cs.outcome": "success"}
    )
    run(g, ctx, max_steps=20)

    text = report(db_path)
    assert "Nothing to diagnose" in text


# ---------------------------------------------------------------------------
# Bug fix: gate handlers must fall back to universal prompt when local command
# file is absent from the workdir's .claude/commands/ directory.
# ---------------------------------------------------------------------------

def test_gate_es_uses_universal_prompt_when_local_es_md_absent(tmp_path, monkeypatch):
    """_gate_es must fall back to the embedded universal prompt when
    .claude/commands/es.md is absent from the workdir.

    RED: current code is `_gate_es = _slash_gate("es")` which always builds
    a "/es ..." prompt regardless of whether es.md exists locally.

    GREEN: _gate_es checks for local es.md; when absent it calls
    _run_universal_prompt_gate with UNIVERSAL_EVIDENCE_REVIEW_PROMPT.
    """
    import subprocess as _sp
    from runner.handlers import _gate_es, Context as HCtx

    node = type("Node", (), {"name": "gate_es", "attrs": {}, "shape": ""})()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    # tmp_path has no .claude/commands/es.md
    assert not (tmp_path / ".claude" / "commands" / "es.md").exists()

    called_prompts: list[str] = []

    fake_sha = "a" * 40

    def _fake_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_es(node, ctx)
    assert result.outcome == "success"
    assert called_prompts, "subprocess.run must have been called"

    prompt_used = called_prompts[0]
    # Universal prompt path: starts with "You are performing..." not "/es "
    assert not prompt_used.startswith("/es "), (
        f"When es.md is absent, _gate_es must use universal prompt, not /es slash. "
        f"Got prompt starting with: {prompt_used[:60]!r}"
    )


def test_gate_code_standards_uses_universal_prompt_when_local_file_absent(tmp_path, monkeypatch):
    """_gate_code_standards must fall back to embedded prompt when
    .claude/commands/code-standards.md is absent from workdir.

    RED: current code is `_gate_code_standards = _slash_gate("code-standards")`
    which always invokes /code-standards regardless of file presence.

    GREEN: _gate_code_standards checks for local code-standards.md and falls
    back to UNIVERSAL_CODE_STANDARDS_PROMPT when absent.
    """
    import subprocess as _sp
    from runner.handlers import _gate_code_standards, Context as HCtx

    node = type("Node", (), {"name": "gate_cs", "attrs": {}, "shape": ""})()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")

    assert not (tmp_path / ".claude" / "commands" / "code-standards.md").exists()

    called_prompts: list[str] = []
    fake_sha = "b" * 40

    def _fake_run(cmd, **kwargs):
        called_prompts.append(cmd[-1])
        return _sp.CompletedProcess(cmd, 0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_code_standards(node, ctx)
    assert result.outcome == "success"
    assert called_prompts

    prompt_used = called_prompts[0]
    assert not prompt_used.startswith("/code-standards "), (
        f"When code-standards.md is absent, _gate_code_standards must use "
        f"universal prompt. Got: {prompt_used[:60]!r}"
    )


# ---------------------------------------------------------------------------
# agy reviewer backend + claude fallback (review_pr.dot evidence gate).
#
# A reviewer gate node with backend="agy" (explicit or via a .review model
# stylesheet) must (a) actually invoke agy, (b) fall back to claude only on agy
# *infrastructure* failure, and (c) NEVER reviewer-shop a real agy fail/partial
# verdict onto claude. These three properties are what make "agy reviews, falls
# back to claude if it doesn't work" honest rather than a label.
# ---------------------------------------------------------------------------

def _agy_gate_node():
    return type("Node", (), {"name": "evidence", "attrs": {"backend": "agy"}, "shape": ""})()


def test_gate_er_runs_agy_when_backend_agy(tmp_path, monkeypatch):
    """backend=agy → the reviewer subprocess is `agy --print ...`, not claude."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    # ctx.backend is the run-level CLI backend; the per-node backend=agy must win.
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    fake_sha = "a" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)

    assert result.outcome == "success"
    assert seen, "subprocess.run must have been called"
    assert seen[0][0] == "agy", f"expected agy reviewer argv, got {seen[0][:1]!r}"
    assert result.metadata["reviewer_backend"] == "agy"
    assert result.metadata["fallback_used"] == "false"
    # No reviewer-shopping: a passing agy verdict must not also call claude.
    assert not any("claude" in c[0] for c in seen)


def test_gate_agy_falls_back_to_claude_on_infra_failure(tmp_path, monkeypatch):
    """agy missing (FileNotFoundError) → fall back to claude; result is the claude verdict."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    fake_sha = "c" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        if cmd[0] == "agy":
            raise FileNotFoundError("agy: command not found")
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)

    assert result.outcome == "success"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "agy"
    assert result.metadata["reviewer_backend"] == "claude"
    # agy was tried first, then claude.
    assert seen[0][0] == "agy"
    assert any("claude" in c[0] for c in seen), "claude fallback must have been invoked"


def test_gate_agy_real_fail_verdict_not_retried(tmp_path, monkeypatch):
    """A genuine agy `verdict: fail` (matching SHA) is kept — claude is never called."""
    import subprocess as _sp
    from runner.handlers import _gate_er, Context as HCtx

    node = _agy_gate_node()
    ctx = HCtx(goal="test", workdir=tmp_path, backend="claude")
    fake_sha = "d" * 40
    seen: list[list[str]] = []

    def _fake_run(cmd, **kwargs):
        seen.append(cmd)
        # agy returns a real review verdict (rc 0, SHA echoed): this is NOT an
        # infra failure, so the fallback must not fire.
        return _sp.CompletedProcess(cmd, 0, stdout=f"head_sha: {fake_sha}\nverdict: fail\n", stderr="")

    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: a)
    monkeypatch.setattr("subprocess.run", _fake_run)

    result = _gate_er(node, ctx)

    assert result.outcome == "failure"
    assert result.metadata["reviewer_backend"] == "agy"
    assert result.metadata["fallback_used"] == "false"
    # Reviewer-shopping guard: claude must NEVER be consulted for a real verdict.
    assert all(c[0] == "agy" for c in seen), f"claude must not be retried; saw {[c[0] for c in seen]!r}"


# ---------------------------------------------------------------------------
# Adversarial-review priority queue (bead jleechan-qb7).
#
# The dark-factory default is `codex > minimax > agy > claude-sonnet`. The
# queue is the FIRST adversarial pass selector — *not* a retry cascade. A real
# fail|partial from the chosen backend is kept (no-reviewer-shopping rule,
# feedback_2026-05-31_runner_resilience_reviewer_gates.md).
# ---------------------------------------------------------------------------

def _priority_node(priority, *, prefer_adversarial=False, name="evidence"):
    attrs = {"backend_priority": ",".join(priority)}
    if prefer_adversarial:
        attrs["prefer_adversarial"] = "true"
    return type("Node", (), {"name": name, "attrs": attrs, "shape": ""})()


def test_adversarial_priority_picks_first_installed(monkeypatch):
    """When the head of the priority list is installed, it is chosen."""
    from runner.handlers import (
        _resolve_adversarial_backend,
        _resolve_gate_backend,
        Context as HCtx,
    )

    node = _priority_node(["definitely-not-installed-aaa", "codex"])
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")

    # `codex` is the only entry we expect to probe-true. Stub the probe so
    # the test is hermetic and doesn't depend on what's on PATH right now.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "codex",
    )

    resolved, meta = _resolve_adversarial_backend(
        ["definitely-not-installed-aaa", "codex"], ctx
    )
    assert resolved == "codex"
    assert meta["adversarial_resolved"] == "codex"
    assert meta["adversarial_skipped"] == "definitely-not-installed-aaa"

    # The gate resolver also returns the priority-queue audit metadata.
    backend, gate_meta = _resolve_gate_backend(node, ctx)
    assert backend == "codex"
    assert gate_meta["adversarial_resolved"] == "codex"
    assert gate_meta["reviewer_backend_resolution"] == "priority_queue"
    assert gate_meta["prefer_adversarial"] == "false"


def test_adversarial_priority_skips_coder_backend_when_prefer_adversarial(monkeypatch):
    """prefer_adversarial: true drops the run-level coder backend from the
    queue so a `claude` coder run never gets a `claude` reviewer."""
    from runner.handlers import (
        _resolve_adversarial_backend,
        _resolve_gate_backend,
        Context as HCtx,
    )

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")

    # `claude` is installed (the coder), `codex` is installed, `agy` is not.
    # The queue is `[claude, codex, agy]`. With prefer_adversarial the
    # `claude` entry should be dropped, then `codex` should win.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name in ("claude", "codex"),
    )

    resolved, meta = _resolve_adversarial_backend(
        ["claude", "codex", "agy"], ctx
    )
    # `claude` was the coder, prefer_adversarial drops it; the resolver
    # was given the *post-filter* queue (the gate resolver applies the
    # filter before calling _resolve_adversarial_backend). Verify the
    # gate-level filter is the one that drops the coder backend.
    assert resolved == "claude"  # the resolver picks from what it got
    assert meta["adversarial_resolved"] == "claude"

    # The gate-level resolver, however, must apply prefer_adversarial BEFORE
    # calling the priority resolver.
    node = _priority_node(
        ["claude", "codex", "agy"], prefer_adversarial=True, name="evidence"
    )
    backend, gate_meta = _resolve_gate_backend(node, ctx)
    assert backend == "codex", (
        f"prefer_adversarial must drop the coder backend; got {backend!r}"
    )
    assert "claude" not in gate_meta["adversarial_priority"].split(","), (
        f"the post-filter priority list must not contain 'claude'; got {gate_meta['adversarial_priority']!r}"
    )
    assert gate_meta["prefer_adversarial"] == "true"


def test_adversarial_priority_env_override_honored(monkeypatch):
    """DARK_FACTORY_ADVERSARIAL_PRIORITY env var overrides the default queue."""
    from runner.handlers import _resolve_adversarial_backend, Context as HCtx

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    monkeypatch.setenv("DARK_FACTORY_ADVERSARIAL_PRIORITY", "minimax,codex,agy")
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name in ("codex", "agy"),  # minimax is NOT installed
    )

    resolved, meta = _resolve_adversarial_backend(None, ctx)
    assert resolved == "codex", (
        f"with minimax uninstalled and codex installed, the resolver must "
        f"fall through to codex; got {resolved!r}"
    )
    assert meta["adversarial_priority"] == "minimax,codex,agy"
    assert meta["adversarial_skipped"] == "minimax"


def test_adversarial_priority_falls_through_to_claude_sonnet_when_nothing_else(monkeypatch):
    """When no priority entry is installed, the resolver returns the last
    entry (claude-sonnet) so the gate still runs and surfaces the missing
    binary honestly. The gate's backend_missing=true path is the real
    signal that nothing was installed — the resolver does not silently
    downgrade or shop reviewers."""
    from runner.handlers import _resolve_adversarial_backend, Context as HCtx

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    monkeypatch.delenv("DARK_FACTORY_ADVERSARIAL_PRIORITY", raising=False)
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: False,  # nothing on PATH
    )

    resolved, meta = _resolve_adversarial_backend(None, ctx)
    # claude-sonnet is the tail of the default queue; the resolver falls
    # through to it so the gate can run and the missing-binary path fires.
    assert resolved == "claude-sonnet", (
        f"with nothing installed, resolver must fall through to claude-sonnet; got {resolved!r}"
    )
    # The full default queue is recorded as skipped — operator can see
    # why the gate is running on a tail-end entry. The order is
    # probe-then-fallthrough, so the tail entry is also marked skipped
    # (the gate's missing-binary path is the real signal).
    assert meta["adversarial_skipped"] == "codex,minimax,agy,claude-sonnet"
    assert meta["adversarial_priority"] == "codex,minimax,agy,claude-sonnet"


def test_adversarial_priority_pinned_across_visits(monkeypatch):
    """Cross-visit pin: once `_resolve_gate_backend` resolves a node via the
    priority queue, re-visits to the same node name return the same backend
    even when `_probe_backend_installed` would resolve differently. This
    honors the design-doc promise in
    `roadmap/agy-reviewer-and-base-dot-2026-06-09.md` §5.2 ("the runner
    pins the reviewer for the entire run") and the no-reviewer-shopping
    rule (a real fail from one backend is never re-resolved onto a
    different one on a re-visit). Regression test for the verifier's
    Concern 1 in `agy-task-review.md`."""
    from runner.handlers import _resolve_gate_backend, Context as HCtx

    node = _priority_node(["codex", "minimax", "agy", "claude-sonnet"], name="evidence")
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")

    # First visit: `codex` is the only entry installed → it is the
    # chosen backend. The pin is recorded in ctx.state.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "codex",
    )
    first, first_meta = _resolve_gate_backend(node, ctx)
    assert first == "codex"
    assert first_meta["reviewer_backend_resolution"] == "priority_queue"
    assert ctx.state["evidence.resolved_backend"] == "codex"

    # Now `codex` disappears from PATH (e.g. uninstalled mid-run).
    # Only `agy` is installed. Without the cross-visit pin, the
    # resolver would fall through to `agy` — a different vendor, a
    # different verdict. With the pin, the second visit returns the
    # *same* backend (`codex`) and the same metadata, honoring the
    # "pinned for the entire run" promise.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "agy",
    )
    second, second_meta = _resolve_gate_backend(node, ctx)
    assert second == "codex", (
        f"cross-visit pin broken: re-visit must return pinned backend, got {second!r}"
    )
    assert second_meta["reviewer_backend_resolution"] == "priority_queue"
# ---------------------------------------------------------------------------
# Adversarial-review dispatch — the priority-queue must actually invoke the
# resolved backend, not silently collapse every non-agy name to claude. Cursor
# Bugbot flagged this as a high-severity gap after the priority-queue landed;
# the dispatch now covers agy / codex / minimax / claude-sonnet end-to-end.
# ---------------------------------------------------------------------------

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
