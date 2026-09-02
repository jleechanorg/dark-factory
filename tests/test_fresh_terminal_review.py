from __future__ import annotations

import base64
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402


def _repo(tmp_path: pathlib.Path) -> pathlib.Path:
    repo = tmp_path / "target"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "config", "user.email", "jleechan2015@users.noreply.github.com"],
        check=True,
    )
    (repo / "value.txt").write_text("before\n")
    subprocess.run(["git", "-C", str(repo), "add", "value.txt"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
    return repo


def _review_node(prompt: pathlib.Path):
    node = make_node(
        name="cold_reviewer",
        type="codergen",
        backend="codex",
        class_="review",
        prompt=f"@{prompt}",
        verdict_gate="true",
        fresh_session="true",
    )
    node.attrs["class"] = "review"
    node.attrs.pop("class_", None)
    return node


def _head_sha(repo: pathlib.Path) -> str:
    proc = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"],
        capture_output=True, text=True, check=True,
    )
    return proc.stdout.strip()


def _target_for(repo: pathlib.Path) -> str:
    """Mint a ``git-commit://`` locator matching what the runner's D3 mint
    path (`_mint_post_worker_target`) would have written to
    ``ctx.state["target"]`` after a worker success visit."""
    return f"git-commit://{repo}@{_head_sha(repo)}"


def _target_state(repo: pathlib.Path) -> dict[str, str]:
    """``ctx.state`` keys a real worker-success mint would have set: the
    target locator plus a pin chain whose last entry matches it. The
    pin-chain consistency check (fail-closed finding) refuses any
    verdict-gated visit where these two are out of sync, so every test that
    seeds a reviewer-ready ``target`` directly (bypassing
    ``_mint_post_worker_target``) must seed both together."""
    target = _target_for(repo)
    return {"target": target, "_target_pin_chain": json.dumps([target])}


def _use_tmp_snapshot_root(tmp_path: pathlib.Path, monkeypatch) -> pathlib.Path:
    """Redirect the fresh-reviewer snapshot root under `tmp_path` so tests
    never touch the real `~/.dark-factory/review-snapshots`."""
    snapshot_root = tmp_path / "review-snapshots"
    monkeypatch.setattr(
        "runner.review_snapshot._default_snapshot_root", lambda: snapshot_root
    )
    return snapshot_root


def _run_review(
    tmp_path, monkeypatch, output: str, *, mutate: bool = False, stderr: str = "",
    returncode: int = 0,
):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text(
        "Review target: ${target}\nEnd with Verdict: PASS or Verdict: FAIL.\n"
    )
    node = _review_node(prompt)
    _use_tmp_snapshot_root(tmp_path, monkeypatch)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo), **_target_state(repo)},
    )
    calls: list[tuple[list[str], pathlib.Path]] = []
    real_run = subprocess.run

    def fake_run(args, **kwargs):
        if args and args[0] == "codex":
            snapshot_cwd = pathlib.Path(kwargs["cwd"])
            calls.append((list(args), snapshot_cwd))
            # Snapshot content matches the pinned commit at launch time
            # (checked here, not after `_codergen` returns, because
            # cleanup removes the snapshot directory before this function's
            # caller gets control back).
            assert (snapshot_cwd / "value.txt").read_text() == "before\n"
            if mutate:
                # Mutates the isolated snapshot the reviewer actually ran
                # in, never the live `repo` — that isolation is the point
                # of the fresh-reviewer snapshot (design item 6).
                (snapshot_cwd / "value.txt").write_text("reviewer changed this\n")
            return subprocess.CompletedProcess(args, returncode, stdout=output, stderr=stderr)
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)
    return _codergen(node, ctx), calls, repo


def test_fresh_reviewer_runs_codex_ephemeral_in_target_worktree(tmp_path, monkeypatch):
    result, calls, repo = _run_review(
        tmp_path,
        monkeypatch,
        "No blocking findings.\nReview completeness: COMPLETE\nVerdict: PASS\n",
    )

    assert result.outcome == "success"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["review_completeness"] == "complete"
    assert len(calls) == 1
    argv, cwd = calls[0]
    assert argv[:5] == ["codex", "exec", "--ephemeral", "--yolo", "--skip-git-repo-check"]
    assert not {"--disable", "--ignore-rules"}.intersection(argv)
    # Design item 6: the reviewer runs against an isolated snapshot, never
    # the live coder workdir. `cwd` was captured while codex "ran" (inside
    # `fake_run`, before cleanup); by now cleanup has already removed it.
    assert cwd != repo
    assert not cwd.exists()
    assert "snapshot_cleanup_failed" not in result.metadata


def test_fresh_reviewer_failure_relays_message_when_unparseable(tmp_path, monkeypatch):
    """D8 (factory two-node redesign): raw reviewer prose never crosses to
    the worker prompt (`result.output` feeds `_last_review_feedback`).
    Free-form FAIL prose with no typed findings JSON degrades to the
    fixed re-run message rather than leaking the prose verbatim."""
    review = "Blocking: app.py:12 returns the wrong value.\nVerdict: FAIL\n"
    result, _, _ = _run_review(tmp_path, monkeypatch, review)

    assert result.outcome == "failure"
    assert result.output == "review did not produce valid findings; re-run against current pin"
    assert result.metadata["verdict"] == "fail"


def test_fresh_reviewer_failure_relays_typed_findings_base64_fenced(tmp_path, monkeypatch):
    """A FAIL verdict with a valid typed-findings JSON list relays as a
    Base64-fenced, explicitly-untrusted block instead of raw prose (D8)."""
    findings = [
        {"path": "app.py", "claim": "returns the wrong value", "required_fix": "fix the return"}
    ]
    review = (
        "Blocking findings:\n```json\n" + json.dumps(findings) + "\n```\nVerdict: FAIL\n"
    )
    result, _, _ = _run_review(tmp_path, monkeypatch, review)

    assert result.outcome == "failure"
    assert "BEGIN REVIEWER FINDINGS" in result.output
    assert "app.py" not in result.output  # raw prose text is not present verbatim
    encoded = result.output.splitlines()[1]
    decoded = json.loads(base64.b64decode(encoded).decode("utf-8"))
    assert decoded == findings


def test_fresh_reviewer_real_prompt_fail_with_fenced_json_relays_typed_findings(
    tmp_path, monkeypatch
):
    """Round-5 finding: the shipped static prompt (`prompts/slim/
    fresh_review.md`) now asks the reviewer to emit a fenced JSON findings
    block on FAIL. A compliant FAIL transcript rendered from the REAL
    prompt file must parse into typed findings, not degrade to the "no
    valid findings" message — the live RED run demonstrated every
    compliant FAIL degrading because the old prompt never asked for JSON."""
    repo = _repo(tmp_path)
    _use_tmp_snapshot_root(tmp_path, monkeypatch)
    monkeypatch.setenv("DARK_FACTORY_HOME", str(ROOT))
    node = make_node(
        name="cold_reviewer",
        type="codergen",
        backend="codex",
        class_="review",
        prompt="@prompts/slim/fresh_review.md",  # the real, shipped prompt
        verdict_gate="true",
        fresh_session="true",
    )
    node.attrs["class"] = "review"
    node.attrs.pop("class_", None)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo), **_target_state(repo)},
    )
    findings = [
        {"path": "app.py", "claim": "returns the wrong value", "required_fix": "fix the return"}
    ]
    review = (
        "Blocking findings:\n```json\n" + json.dumps(findings) + "\n```\n"
        "Review completeness: COMPLETE\nVerdict: FAIL\n"
    )
    real_run = subprocess.run

    def fake_run(args, **kwargs):
        if args and args[0] == "codex":
            return subprocess.CompletedProcess(args, 0, stdout=review, stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)

    result = _codergen(node, ctx)

    assert result.outcome == "failure"
    assert "BEGIN REVIEWER FINDINGS" in result.output
    assert result.output != "review did not produce valid findings; re-run against current pin"


def test_fresh_reviewer_unknown_verdict_fails_closed(tmp_path, monkeypatch):
    result, _, _ = _run_review(tmp_path, monkeypatch, "Looks plausible.\n")

    assert result.outcome == "error"
    assert result.metadata["verdict"] == "unknown"


def test_fresh_reviewer_non_terminal_pass_fails_closed(tmp_path, monkeypatch):
    """CRITICAL-4 (external review, round 3): a `Verdict: PASS` line buried
    mid-output, with more text after it, is not a terminal verdict — even
    though it would otherwise parse as a valid PASS. The transcript must
    END there, or the visit is treated as an unparseable/untrusted verdict
    (fail closed), not a validated success."""
    review = (
        "Review completeness: COMPLETE\nVerdict: PASS\n"
        "Actually wait, let me reconsider one more thing...\n"
    )
    result, _, _ = _run_review(tmp_path, monkeypatch, review)

    assert result.outcome == "error"
    assert result.metadata["verdict"] == "unknown"
    assert result.metadata["terminal_report_valid"] == "false"


def test_fresh_reviewer_stdout_terminal_pass_accepted_despite_stderr(tmp_path, monkeypatch):
    """Round-4 finding: codex routinely writes to stderr even on a clean
    PASS (progress/warnings). The combined stdout+stderr `output` used to
    feed the terminal-line-exact check, so the appended "STDERR:" block
    made a real terminal `Verdict: PASS` in stdout permanently unparseable
    — fail-closed became fail-always. Verdict/terminal parsing must read
    stdout only; a non-empty stderr must not block a real PASS."""
    review = "No blocking findings.\nReview completeness: COMPLETE\nVerdict: PASS\n"
    result, _, _ = _run_review(
        tmp_path, monkeypatch, review, stderr="codex: some progress/warning noise\n"
    )

    assert result.outcome == "success"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["terminal_report_valid"] == "true"


def test_fresh_reviewer_verdict_only_in_stderr_is_rejected(tmp_path, monkeypatch):
    """A `Verdict: PASS` line that only appears in stderr (never in
    stdout — the reviewer's actual transcript) must not be accepted: the
    reviewer contract is about what the reviewer said, not what leaked to
    the process's diagnostic stream."""
    result, _, _ = _run_review(
        tmp_path,
        monkeypatch,
        "No blocking findings.\nReview completeness: COMPLETE\n",
        stderr="Verdict: PASS\n",
    )

    assert result.outcome == "error"
    assert result.metadata["verdict"] == "unknown"
    assert result.metadata["terminal_report_valid"] == "false"


def test_fresh_reviewer_completeness_only_in_stderr_normalizes_pass_to_failure(
    tmp_path, monkeypatch
):
    """Round-5 finding: mirrors the verdict stdout-only fix for the
    completeness marker. A stdout with a real terminal `Verdict: PASS` but
    NO completeness marker, plus a stray "Review completeness: COMPLETE"
    string in stderr, must not count as a validated PASS — the marker is
    read from stdout only, same as the verdict itself."""
    result, _, _ = _run_review(
        tmp_path,
        monkeypatch,
        "No blocking findings.\nVerdict: PASS\n",
        stderr="Review completeness: COMPLETE\n",
    )

    assert result.outcome == "failure"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["review_completeness"] == "unknown"


def test_fresh_reviewer_unfinished_pass_normalizes_to_failure(tmp_path, monkeypatch):
    """D7 (v3.1 delta): `Review completeness: UNFINISHED` + `Verdict: PASS`
    is not a validated PASS — the reviewer ran out of time and must not be
    treated as a clean bill of health."""
    review = "Ran out of time, reviewed half the diff.\nReview completeness: UNFINISHED\nVerdict: PASS\n"
    result, _, _ = _run_review(tmp_path, monkeypatch, review)

    assert result.outcome == "failure"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["review_completeness"] == "unfinished"


def test_fresh_reviewer_missing_completeness_marker_normalizes_pass_to_failure(
    tmp_path, monkeypatch
):
    """A PASS verdict with no completeness marker at all fails closed the
    same way as an explicit UNFINISHED — the marker is required, not
    optional, for a PASS to be machine-validated."""
    result, _, _ = _run_review(tmp_path, monkeypatch, "No blocking findings.\nVerdict: PASS\n")

    assert result.outcome == "failure"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["review_completeness"] == "unknown"


def test_fresh_reviewer_unfinished_fail_stays_failure(tmp_path, monkeypatch):
    """The completeness marker only gates PASS; a FAIL verdict is failure
    either way and is not affected by the completeness check."""
    review = "Blocking: found a real bug.\nReview completeness: UNFINISHED\nVerdict: FAIL\n"
    result, _, _ = _run_review(tmp_path, monkeypatch, review)

    assert result.outcome == "failure"
    assert result.metadata["verdict"] == "fail"
    # The completeness field is only computed on the success path (D7 only
    # gates PASS); metadata reflects that it was never evaluated.
    assert result.metadata["review_completeness"] == "n/a"


def test_fresh_reviewer_timeout_fails_closed(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text(
        "Review target: ${target}\nEnd with Verdict: PASS or Verdict: FAIL.\n"
    )
    _use_tmp_snapshot_root(tmp_path, monkeypatch)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo), **_target_state(repo)},
    )

    real_run = subprocess.run

    def time_out(args, **kwargs):
        if args and args[0] == "codex":
            raise subprocess.TimeoutExpired(args, kwargs["timeout"])
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", time_out)
    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert result.metadata["timed_out"] == "true"
    assert "timed out" in result.output


def test_fresh_reviewer_tracked_mutation_fails_closed(tmp_path, monkeypatch):
    result, calls, repo = _run_review(
        tmp_path,
        monkeypatch,
        "No blocking findings.\nVerdict: PASS\n",
        mutate=True,
    )

    assert result.outcome == "error"
    assert result.metadata["reviewer_mutated_tracked_files"] == "true"
    # D8: the mutation notice is infrastructure-error prose and must not
    # leak into the worker-facing relay (`result.output`) — it stays in the
    # metadata assertion above (manifest-only per the two-node redesign).
    assert result.output == "review did not produce valid findings; re-run against current pin"
    # Design item 6: the reviewer mutated the isolated snapshot, not the
    # live coder workdir — `repo` is untouched even though the mutation was
    # detected and fails the visit closed.
    assert (repo / "value.txt").read_text() == "before\n"
    _, snapshot_cwd = calls[0]
    assert snapshot_cwd != repo


def test_fresh_reviewer_errors_before_codex_when_no_target_minted(tmp_path, monkeypatch):
    """Design item 6 replaces `_fresh_review_workdir`'s live-workdir symlink
    check with snapshot materialization from `ctx.state["target"]` (D3/D8a).
    A verdict-gated node with no minted target has nothing to snapshot from
    and must fail closed before codex is ever launched — mirroring the old
    contract's "never launch codex against an unsafe target" guarantee."""
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text(
        "Review target: ${target}\nEnd with Verdict: PASS or Verdict: FAIL.\n"
    )
    _use_tmp_snapshot_root(tmp_path, monkeypatch)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},  # no "target" key: mint never ran
    )
    real_run = subprocess.run

    def unexpected_run(args, **kwargs):
        if args and args[0] == "codex":
            raise AssertionError("Codex launched without a minted review target")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", unexpected_run)
    result = _codergen(_review_node(prompt), ctx)

    # External-review finding: the render-time assertion (handler_render.py)
    # is now the outermost fail-closed gate — it rejects the rendered
    # "(no target minted)" placeholder before `_codergen` even dispatches to
    # a backend, so this aborts as a render failure, not a codex-branch
    # "error" outcome.
    assert result.outcome == "failure"
    assert result.metadata.get("review_render_aborted") == "true"
    assert "no target minted" in result.output


def test_fresh_reviewer_snapshot_resolves_through_symlinked_target_locator(
    tmp_path, monkeypatch
):
    """A target locator built from a symlinked path still canonicalizes to
    the real repository at parse time (`target_locator._canon_path`), so the
    snapshot is materialized from the real repo — never from, or leaking
    into, the symlinked alias."""
    real_parent = tmp_path / "real"
    real_parent.mkdir()
    repo = _repo(real_parent)
    alias_parent = tmp_path / "alias"
    alias_parent.symlink_to(real_parent, target_is_directory=True)
    aliased_repo = alias_parent / repo.name
    prompt = tmp_path / "review.md"
    prompt.write_text(
        "Review target: ${target}\nEnd with Verdict: PASS or Verdict: FAIL.\n"
    )
    snapshot_root = _use_tmp_snapshot_root(tmp_path, monkeypatch)
    aliased_target = f"git-commit://{aliased_repo}@{_head_sha(repo)}"
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"target": aliased_target, "_target_pin_chain": json.dumps([aliased_target])},
    )
    calls: list[pathlib.Path] = []
    real_run = subprocess.run

    def pass_review(args, **kwargs):
        if args and args[0] == "codex":
            calls.append(pathlib.Path(kwargs["cwd"]))
            return subprocess.CompletedProcess(
                args, 0, stdout="Review completeness: COMPLETE\nVerdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args
    )
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", pass_review)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "success"
    assert len(calls) == 1
    cwd = calls[0]
    assert cwd != repo
    assert cwd != aliased_repo
    assert cwd.is_relative_to(snapshot_root)


def test_default_graph_does_not_initialize_controller_state(tmp_path, monkeypatch):
    from runner import engine_run
    from runner.handler_core import Result
    from runner.handlers import TYPE_REGISTRY
    from runner.parser import parse

    def unexpected_controller_call(*args, **kwargs):
        raise AssertionError("default fresh-terminal graph touched controller state")

    monkeypatch.setattr(
        engine_run, "_load_controller_snapshot_journal", unexpected_controller_call
    )
    monkeypatch.setattr(engine_run, "_seed_controller_base_sha", unexpected_controller_call)
    monkeypatch.setitem(
        TYPE_REGISTRY,
        "codergen",
        lambda node, ctx: Result(outcome="success", output="Verdict: PASS\n"),
    )

    graph = parse(ROOT / "pipelines/slim/two_node.dot")
    history = engine_run.run(
        graph,
        Context(goal="review the default", workdir=tmp_path, backend="echo"),
    )

    assert history[-1].outcome == "success"
