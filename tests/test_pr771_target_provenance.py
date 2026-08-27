"""Regression tests for controller-owned target and base provenance."""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from runner.handler_core import Context, _target_worktree
from runner.handler_parallel_reviewer import (
    _controller_review_request,
    _controller_snapshot,
    _controller_snapshot_root,
    _verify_controller_workspace,
)
from runner.parser import Node


def _git(cwd: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout.strip()


def _repo(tmp_path: Path) -> tuple[Path, str, str]:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.name", "test")
    _git(repo, "config", "user.email", "test@example.invalid")
    (repo / "value.txt").write_text("base\n")
    _git(repo, "add", "value.txt")
    _git(repo, "commit", "-q", "-m", "base")
    base = _git(repo, "rev-parse", "HEAD")
    (repo / "value.txt").write_text("worker change\n")
    _git(repo, "commit", "-qam", "worker change")
    head = _git(repo, "rev-parse", "HEAD")
    return repo, base, head


def _cleanup_snapshots(repo: Path, ctx: Context) -> None:
    for entry in json.loads(ctx.state.get("_controller_review_snapshots", "[]")):
        _git(repo, "worktree", "remove", "--force", entry["snapshot_path"])


def test_controller_base_does_not_follow_mutable_origin_main(tmp_path: Path) -> None:
    """A worker-controlled origin/main ref cannot collapse the reviewed range."""
    repo, base, head = _repo(tmp_path)
    _git(repo, "update-ref", "refs/remotes/origin/main", head)
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={"base_sha": base},
        run_id="base-provenance",
    )

    try:
        request = _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
        envelope = json.loads(request.envelope_json)
        assert envelope["target"]["base_sha"] == base
        assert envelope["target"]["base_sha"] != envelope["target"]["head_sha"]
        assert envelope["snapshots"]["changed_files"] == ["value.txt"]
    finally:
        _cleanup_snapshots(repo, ctx)


def test_controller_review_fails_closed_without_authenticated_base(tmp_path: Path) -> None:
    """No target-owned ref is an acceptable implicit controller base."""
    repo, _base, head = _repo(tmp_path)
    _git(repo, "update-ref", "refs/remotes/origin/main", head)
    ctx = Context(goal="review worker change", workdir=repo, run_id="missing-base")

    with pytest.raises(ValueError, match="base SHA|target head"):
        _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)


def test_controller_preserves_lexical_symlink_parent_until_validation(tmp_path: Path) -> None:
    """The earliest target resolver must not erase a symlinked parent."""
    repo, _base, head = _repo(tmp_path)
    alias_parent = tmp_path / "alias"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    lexical_repo = alias_parent / repo.name
    ctx = Context(goal="review", workdir=lexical_repo)

    assert _target_worktree(ctx) == lexical_repo
    with pytest.raises(ValueError, match="symlink"):
        _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)


def test_controller_preserves_lexical_ao_worktree_until_validation(tmp_path: Path) -> None:
    """AO worktree aliases receive the same lexical validation boundary."""
    repo, _base, head = _repo(tmp_path)
    alias_parent = tmp_path / "ao-alias"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    lexical_repo = alias_parent / repo.name
    ctx = Context(
        goal="review",
        workdir=repo,
        state={"ao.worktree": str(lexical_repo)},
    )

    assert _target_worktree(ctx) == lexical_repo
    with pytest.raises(ValueError, match="symlink"):
        _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)


def test_controller_snapshot_root_rejects_symlink_before_git_mutation(
    tmp_path: Path, monkeypatch
) -> None:
    """A symlinked controller-snapshots root cannot redirect Git worktrees."""
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    outside = tmp_path / "outside"
    outside.mkdir(mode=0o700)
    (home / ".dark-factory").mkdir(mode=0o700)
    (home / ".dark-factory" / "controller-snapshots").symlink_to(
        outside, target_is_directory=True
    )
    monkeypatch.setenv("HOME", str(home))

    with pytest.raises(ValueError, match="symlink"):
        _controller_snapshot_root()
    assert not list(outside.iterdir())


def test_controller_post_review_revalidates_mutable_source_checkout(tmp_path: Path, monkeypatch):
    """A source/index change after snapshot creation invalidates a pass."""
    repo, base, head = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setenv("HOME", str(home))
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={"base_sha": base},
        run_id="source-revalidation",
    )
    try:
        request = _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
        (repo / "value.txt").write_text("mutated after snapshot\n")
        with pytest.raises(ValueError, match="source checkout changed"):
            _verify_controller_workspace(ctx, request)
    finally:
        _cleanup_snapshots(repo, ctx)


def test_controller_post_review_revalidates_copied_untracked_file(
    tmp_path: Path, monkeypatch
) -> None:
    """A copied untracked worker artifact cannot change after review starts."""
    repo, base, head = _repo(tmp_path)
    worker_output = repo / "worker-output.txt"
    worker_output.write_text("before\n")
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setenv("HOME", str(home))
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={"base_sha": base},
        run_id="untracked-revalidation",
    )
    try:
        request = _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
        worker_output.write_text("after\n")
        with pytest.raises(ValueError, match="source checkout changed"):
            _verify_controller_workspace(ctx, request)
    finally:
        _cleanup_snapshots(repo, ctx)


def test_controller_post_review_rejects_new_untracked_product_file(
    tmp_path: Path, monkeypatch
) -> None:
    """A product file added after request binding cannot receive a pass."""
    from runner.handler_core import Result
    from runner.handler_parallel_reviewer import _contract_adjusted_result

    repo, base, head = _repo(tmp_path)
    evidence = repo / "review-evidence.json"
    evidence.write_text('{"status":"pass"}\n')
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setenv("HOME", str(home))
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={
            "base_sha": base,
            "evidence_paths": ["review-evidence.json"],
        },
        run_id="new-untracked-revalidation",
    )
    try:
        request = _controller_review_request(
            Node(name="cold_reviewer", attrs={}), ctx, head
        )
        (repo / "new-product-source.py").write_text("print('new product')\n")
        result = _contract_adjusted_result(
            Result(
                outcome="success",
                output=json.dumps(
                    {
                        "verdict": "pass",
                        "findings": [],
                        "evidence_checked": ["review-evidence.json"],
                        "commands_executed": ["python -m pytest -q"],
                        "caveats": [],
                    },
                    separators=(",", ":"),
                ),
                metadata={
                    "_controller_command_receipts": [
                        {
                            "command": "python -m pytest -q",
                            "exit_code": 0,
                            "output_sha256": "0" * 64,
                        }
                    ]
                },
            ),
            request,
            ctx,
            lane="primary",
            backend="codex",
        )
        assert result.outcome == "failure"
        assert result.metadata["review_contract_status"] == "invalid"
        assert (
            "source checkout changed during review"
            in result.metadata["review_contract_gap"]
        )
        assert "_verified_review_target" not in result.context_updates
    finally:
        _cleanup_snapshots(repo, ctx)


def test_controller_post_review_revalidates_ignored_declared_evidence(
    tmp_path: Path, monkeypatch
) -> None:
    """Ignored declared evidence remains bound and cannot change after review."""
    repo, base, head = _repo(tmp_path)
    (repo / ".gitignore").write_text("evidence/\n")
    evidence = repo / "evidence" / "worker-verification.json"
    evidence.parent.mkdir()
    evidence.write_text('{"status":"pass"}\n')
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setenv("HOME", str(home))
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={"base_sha": base, "evidence_paths": ["evidence/worker-verification.json"]},
        run_id="ignored-evidence-revalidation",
    )
    try:
        request = _controller_review_request(
            Node(name="cold_reviewer", attrs={}), ctx, head
        )
        evidence.write_text('{"status":"changed"}\n')
        with pytest.raises(ValueError, match="source checkout changed"):
            _verify_controller_workspace(ctx, request)
    finally:
        _cleanup_snapshots(repo, ctx)


@pytest.mark.parametrize("untracked", [False, True])
def test_controller_rejects_source_mutation_between_snapshot_and_binding(
    tmp_path: Path, monkeypatch, untracked: bool
) -> None:
    """Snapshot-to-binding races cannot leave a stale review target accepted."""
    import runner.handler_parallel_reviewer as reviewer

    repo, base, head = _repo(tmp_path)
    if untracked:
        (repo / "worker-output.txt").write_text("before\n")
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setenv("HOME", str(home))
    ctx = Context(
        goal="review worker change",
        workdir=repo,
        state={"base_sha": base},
        run_id="snapshot-binding-race",
    )
    original_snapshot = reviewer._controller_snapshot
    captured: dict[str, Path] = {}

    def snapshot_then_mutate(source, expected_sha, evidence, **kwargs):
        result = original_snapshot(source, expected_sha, evidence, **kwargs)
        captured["snapshot"] = result[0]
        if untracked:
            (repo / "worker-output.txt").write_text("after\n")
        else:
            (repo / "value.txt").write_text("tracked race\n")
        return result

    monkeypatch.setattr(reviewer, "_controller_snapshot", snapshot_then_mutate)
    with pytest.raises(ValueError, match="changed during snapshot creation"):
        _controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
    assert not captured["snapshot"].exists()


def test_cleanup_skips_git_when_source_ancestor_becomes_symlink(tmp_path: Path, monkeypatch):
    """Cleanup must not hand Git a source path whose parent was swapped."""
    from runner.engine_run import _cleanup_controller_snapshot

    repo, _base, head = _repo(tmp_path)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    snapshot = _controller_snapshot(repo, head, ())[0]
    alias_parent = tmp_path / "alias"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    source_alias = alias_parent / repo.name
    monkeypatch.setattr(
        "runner.handler_sandbox._holdout_denied_paths", list
    )
    calls: list[list[str]] = []

    def unexpected_git(command, **kwargs):
        calls.append(command)
        raise AssertionError("cleanup invoked Git with a symlinked source ancestor")

    monkeypatch.setattr("runner.engine_run.subprocess.run", unexpected_git)
    state = {
        "_controller_review_snapshots": json.dumps(
            [{"snapshot_path": str(snapshot), "source_worktree": str(source_alias)}]
        )
    }
    _cleanup_controller_snapshot(Context(goal="", workdir=repo, state=state))

    assert calls == []
    assert snapshot.exists()


def test_cleanup_revalidates_source_before_prune(tmp_path: Path, monkeypatch):
    """A parent swap after remove must prevent a later prune invocation."""
    from runner.engine_run import _cleanup_controller_snapshot

    source_parent = tmp_path / "source-parent"
    source_parent.mkdir(mode=0o700)
    repo, _base, head = _repo(source_parent)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    snapshot = _controller_snapshot(repo, head, ())[0]
    monkeypatch.setattr("runner.handler_sandbox._holdout_denied_paths", list)
    calls: list[list[str]] = []

    def fake_git(command, **kwargs):
        calls.append(command)
        if command[-4:-2] == ["worktree", "remove"]:
            shutil.rmtree(snapshot)
            real_parent = tmp_path / "real-source-parent"
            source_parent.rename(real_parent)
            source_parent.symlink_to(real_parent, target_is_directory=True)
            return subprocess.CompletedProcess(command, 0, "", "")
        raise AssertionError("worktree prune ran after source parent was swapped")

    monkeypatch.setattr("runner.engine_run.subprocess.run", fake_git)
    state = {
        "_controller_review_snapshots": json.dumps(
            [{"snapshot_path": str(snapshot), "source_worktree": str(repo)}]
        )
    }
    _cleanup_controller_snapshot(Context(goal="", workdir=repo, state=state))

    assert len(calls) == 1
    assert calls[0][-4:-2] == ["worktree", "remove"]


def test_snapshot_failure_skips_cleanup_after_source_parent_swap(
    tmp_path: Path, monkeypatch
) -> None:
    """Snapshot failure must not remove through an ancestor swapped to a symlink."""
    import runner.handler_parallel_reviewer as reviewer

    source_parent = tmp_path / "source-parent"
    source_parent.mkdir(mode=0o700)
    repo, _base, head = _repo(source_parent)
    raw_repo = source_parent / repo.name / ".." / repo.name
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.setattr("runner.handler_sandbox._holdout_denied_paths", list)
    real_run = reviewer.subprocess.run
    cleanup_calls: list[list[str]] = []
    swapped = False

    def git_with_swap(command, *args, **kwargs):
        nonlocal swapped
        is_add = command[0:2] == ["git", "-C"] and "worktree" in command and "add" in command
        if is_add:
            result = real_run(command, *args, **kwargs)
            real_parent = tmp_path / "real-source-parent"
            source_parent.rename(real_parent)
            source_parent.symlink_to(real_parent, target_is_directory=True)
            swapped = True
            return result
        if "worktree" in command and "remove" in command:
            cleanup_calls.append(command)
            raise AssertionError("snapshot cleanup followed a swapped source parent")
        return real_run(command, *args, **kwargs)

    monkeypatch.setattr(reviewer.subprocess, "run", git_with_swap)
    real_git_output = reviewer._git_output
    calls_to_output = 0

    def fail_after_add(workdir, *args, **kwargs):
        nonlocal calls_to_output
        calls_to_output += 1
        if swapped and args[:1] == ("diff",):
            raise ValueError("forced snapshot failure")
        return real_git_output(workdir, *args, **kwargs)

    monkeypatch.setattr(reviewer, "_git_output", fail_after_add)
    with pytest.raises(ValueError, match="forced snapshot failure"):
        reviewer._controller_snapshot(repo, head, (), cleanup_source=raw_repo)

    assert calls_to_output >= 2
    assert cleanup_calls == []


def test_review_persists_raw_source_for_mutation_and_engine_cleanup(
    tmp_path: Path, monkeypatch
) -> None:
    """Accepted lexical aliases remain bound for both later cleanup paths."""
    import runner.handler_parallel_reviewer as reviewer
    from runner.engine_run import _cleanup_controller_snapshot

    source_parent = tmp_path / "source-parent"
    source_parent.mkdir(mode=0o700)
    repo, base, head = _repo(source_parent)
    raw_repo = source_parent / repo.name / ".." / repo.name
    assert str(raw_repo) != str(repo)
    home = tmp_path / "home"
    home.mkdir(mode=0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.setattr("runner.handler_sandbox._holdout_denied_paths", list)

    # The source-mutation cleanup must validate the raw spelling, not the
    # already-resolved source path used for reads and fingerprinting.
    ctx = Context(
        goal="review worker change",
        workdir=raw_repo,
        state={"base_sha": base},
        run_id="raw-source-cleanup",
    )
    original_snapshot = reviewer._controller_snapshot
    captured: dict[str, Path] = {}

    def snapshot_then_swap(source, expected_sha, evidence, **kwargs):
        result = original_snapshot(source, expected_sha, evidence, **kwargs)
        captured["snapshot"] = result[0]
        (repo / "value.txt").write_text("changed after snapshot\n")
        real_parent = tmp_path / "real-source-parent"
        source_parent.rename(real_parent)
        source_parent.symlink_to(real_parent, target_is_directory=True)
        return result

    cleanup_calls: list[list[str]] = []
    real_run = reviewer.subprocess.run

    def observe_cleanup(command, *args, **kwargs):
        if "worktree" in command and "remove" in command:
            cleanup_calls.append(command)
        return real_run(command, *args, **kwargs)

    monkeypatch.setattr(reviewer, "_controller_snapshot", snapshot_then_swap)
    monkeypatch.setattr(reviewer.subprocess, "run", observe_cleanup)
    with pytest.raises(ValueError, match="changed during snapshot creation"):
        reviewer._controller_review_request(Node(name="cold_reviewer", attrs={}), ctx, head)
    assert cleanup_calls == []
    assert captured["snapshot"].exists()

    # Restore the parent before deterministic test cleanup.
    source_parent.unlink()
    (tmp_path / "real-source-parent").rename(source_parent)
    _git(repo, "worktree", "remove", "--force", str(captured["snapshot"]))
    (repo / "value.txt").write_text("worker change\n")
    monkeypatch.setattr(reviewer, "_controller_snapshot", original_snapshot)

    # A successful request records the same raw spelling for engine-owned
    # cleanup. After a parent swap, the engine must reject remove and prune.
    clean_ctx = Context(
        goal="review worker change",
        workdir=raw_repo,
        state={"base_sha": base},
        run_id="raw-source-engine-cleanup",
    )
    reviewer._controller_review_request(
        Node(name="cold_reviewer", attrs={}), clean_ctx, head
    )
    state_entries = json.loads(clean_ctx.state["_controller_review_snapshots"])
    binding_entries = json.loads(clean_ctx.state["_controller_review_source_bindings"])
    assert state_entries[-1]["source_worktree"] == str(raw_repo)
    assert binding_entries[-1]["source_worktree"] == str(raw_repo)
    clean_snapshot = Path(state_entries[-1]["snapshot_path"])
    real_parent = tmp_path / "real-source-parent"
    source_parent.rename(real_parent)
    source_parent.symlink_to(real_parent, target_is_directory=True)

    engine_calls: list[list[str]] = []

    def unexpected_engine_git(command, **kwargs):
        engine_calls.append(command)
        raise AssertionError("engine cleanup followed a swapped raw source parent")

    monkeypatch.setattr("runner.engine_run.subprocess.run", unexpected_engine_git)
    _cleanup_controller_snapshot(Context(goal="", workdir=repo, state=clean_ctx.state))
    assert engine_calls == []
    assert clean_snapshot.exists()


def test_default_two_node_seeds_base_before_worker_mutation(tmp_path: Path, monkeypatch) -> None:
    """The production default graph binds its base before creating a review snapshot."""
    import runner.handler_parallel_reviewer as reviewer
    from runner import handlers
    from runner.__main__ import main
    from runner.handler_core import Result

    repo, base, _head = _repo(tmp_path)
    _git(repo, "reset", "--hard", base)
    (repo / "evidence").mkdir()
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))

    def worker(node, ctx):
        (repo / "worker.txt").write_text("worker mutation\n")
        (repo / "evidence" / "worker-verification.json").write_text(
            '{"status":"pass"}\n'
        )
        return Result(outcome="success", output="worker complete")

    monkeypatch.setitem(handlers.TYPE_REGISTRY, "codergen", worker)
    captured: dict[str, object] = {}
    original_request = reviewer._controller_review_request

    def capture_request(node, ctx, expected_sha):
        request = original_request(node, ctx, expected_sha)
        captured["request"] = request
        captured["base_state"] = ctx.state.get("_controller_base_sha")
        return request

    monkeypatch.setattr(reviewer, "_controller_review_request", capture_request)
    monkeypatch.setattr(
        reviewer,
        "_run_primary_review",
        lambda *args, **kwargs: Result(
            outcome="success",
            output=json.dumps(
                {
                    "verdict": "pass",
                    "findings": [],
                    "evidence_checked": ["worker-verification.json"],
                    "commands_executed": ["verification command"],
                    "caveats": [],
                }
            ),
            metadata={
                "returncode": "0",
                "_controller_command_receipts": [
                    {
                        "command": "verification command",
                        "exit_code": 0,
                        "output_sha256": "1" * 64,
                    }
                ],
            },
        ),
    )

    result = main(
        [
            "--goal",
            "review worker mutation",
            "--workdir",
            str(repo),
            "--ao-worktree",
            str(repo),
            "--backend",
            "echo",
            "--no-perf-log",
            "--max-steps",
            "10",
        ]
    )

    request = captured["request"]
    envelope = json.loads(request.envelope_json)
    assert result == 0
    assert captured["base_state"] == base
    assert envelope["target"]["base_sha"] == base
    assert envelope["target"]["base_sha"] != envelope["target"]["head_sha"]
    assert "worker.txt" in envelope["snapshots"]["changed_files"]


def test_cli_ao_worktree_symlink_parent_rejected_before_snapshot(
    tmp_path: Path, monkeypatch
) -> None:
    """CLI AO worktree ingestion must retain aliases for the lexical guard."""
    from runner import handlers
    from runner.__main__ import main
    from runner.handler_core import Result

    repo, _base, _head = _repo(tmp_path)
    alias_parent = tmp_path / "ao-alias"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    lexical_worktree = alias_parent / repo.name
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("HOME", str(home))
    head_lookup_attempted = False

    def worker(node, ctx):
        return Result(outcome="success", output="worker complete")

    def unexpected_head(*args, **kwargs):
        nonlocal head_lookup_attempted
        head_lookup_attempted = True
        raise AssertionError("symlinked AO worktree reached a Git HEAD lookup")

    monkeypatch.setitem(handlers.TYPE_REGISTRY, "codergen", worker)
    monkeypatch.setattr(handlers, "_worktree_head_sha", unexpected_head)
    result = main(
        [
            "--goal",
            "reject symlinked AO worktree",
            "--workdir",
            str(repo),
            "--ao-worktree",
            str(lexical_worktree),
            "--backend",
            "echo",
            "--no-perf-log",
            "--max-steps",
            "10",
        ]
    )

    assert result != 0
    assert head_lookup_attempted is False
