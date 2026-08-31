from __future__ import annotations

import pathlib
import shutil
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402
from runner.handler_codergen import _git_ignored_snapshot_paths  # noqa: E402


def _repo(tmp_path: pathlib.Path) -> pathlib.Path:
    repo = tmp_path / "target"
    repo.mkdir(parents=True, exist_ok=True)
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


def test_real_slim_prompt_link_remains_a_symlink_in_isolated_snapshot(
    tmp_path, monkeypatch
):
    source_link = ROOT / "pipelines/slim/prompts/prompts"
    repo = _repo(tmp_path)
    (repo / "prompts").mkdir()
    (repo / "prompts" / "worker.md").write_text("worker prompt\n")
    snapshot_link = repo / source_link.relative_to(ROOT)
    snapshot_link.parent.mkdir(parents=True)
    snapshot_link.symlink_to(source_link.readlink(), target_is_directory=True)
    subprocess.run(["git", "-C", str(repo), "add", "prompts", "pipelines"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "add slim prompt link"], check=True)

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    calls: list[pathlib.Path] = []
    real_run = subprocess.run

    def check_snapshot_link(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            copied_link = cwd / source_link.relative_to(ROOT)
            calls.append(copied_link)
            assert copied_link.is_symlink()
            assert copied_link.readlink() == source_link.readlink()
            assert copied_link.resolve(strict=True) == cwd.resolve(strict=True) / "prompts"
            assert (copied_link / "worker.md").read_text() == "worker prompt\n"
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", check_snapshot_link)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "success", result.output
    assert len(calls) == 1


def _run_review(tmp_path, monkeypatch, output: str, *, mutate: bool = False):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    calls: list[tuple[list[str], pathlib.Path]] = []
    real_run = subprocess.run

    def fake_run(args, **kwargs):
        if args and args[0] == "codex":
            review_dir = pathlib.Path(kwargs["cwd"])
            calls.append((list(args), review_dir))
            assert (review_dir / "value.txt").read_text() == "before\n"
            if mutate:
                (review_dir / "value.txt").write_text("reviewer changed this\n")
            return subprocess.CompletedProcess(args, 0, stdout=output, stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)
    return _codergen(node, ctx), calls, repo


def test_slim_prompt_link_resolves_to_repository_prompt_root() -> None:
    link = ROOT / "pipelines/slim/prompts/prompts"
    repository_root = ROOT.resolve(strict=True)
    prompt_root = (ROOT / "prompts").resolve(strict=True)

    assert link.is_symlink(), f"expected tracked prompt link at {link}"
    try:
        resolved = link.resolve(strict=True)
    except OSError as exc:
        pytest.fail(f"tracked prompt link must not be dangling: {link}: {exc}")

    assert resolved == prompt_root
    assert resolved.is_relative_to(repository_root)


def test_fresh_reviewer_runs_codex_ephemeral_in_isolated_review_workdir(tmp_path, monkeypatch):
    result, calls, repo = _run_review(
        tmp_path,
        monkeypatch,
        "No blocking findings.\nVerdict: PASS\n",
    )

    assert result.outcome == "success", result.output
    assert result.metadata["verdict"] == "pass"
    assert len(calls) == 1
    argv, cwd = calls[0]
    assert argv[:5] == ["codex", "exec", "--ephemeral", "--yolo", "--skip-git-repo-check"]
    assert not {"--disable", "--ignore-rules"}.intersection(argv)
    assert cwd != repo
    # Review workdir was temporary and cleaned up on exit
    assert not cwd.exists()


def test_fresh_reviewer_failure_is_relayable_output(tmp_path, monkeypatch):
    review = "Blocking: app.py:12 returns the wrong value.\nVerdict: FAIL\n"
    result, _, _ = _run_review(tmp_path, monkeypatch, review)

    assert result.outcome == "failure"
    assert result.output == review
    assert result.metadata["verdict"] == "fail"


def test_fresh_reviewer_unknown_verdict_fails_closed(tmp_path, monkeypatch):
    result, _, _ = _run_review(tmp_path, monkeypatch, "Looks plausible.\n")

    assert result.outcome == "error"
    assert result.metadata["verdict"] == "unknown"


def test_fresh_reviewer_timeout_fails_closed(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
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
    result, _, repo = _run_review(
        tmp_path,
        monkeypatch,
        "No blocking findings.\nVerdict: PASS\n",
        mutate=True,
    )

    assert result.outcome == "error"
    assert result.metadata["reviewer_mutated_tracked_files"] == "true"
    assert "reviewer changed tracked files" in result.output.lower()
    assert (repo / "value.txt").read_text() == "before\n"


def test_fresh_reviewer_preserves_untracked_artifacts_and_unstaged_edits(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    (repo / "value.txt").write_text("worker modified this\n")
    (repo / "artifact.json").write_text('{"tests": "passed"}\n')
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    calls: list[pathlib.Path] = []
    real_run = subprocess.run

    def check_review_tree(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append(cwd)
            assert (cwd / "value.txt").read_text() == "worker modified this\n"
            assert (cwd / "artifact.json").read_text() == '{"tests": "passed"}\n'
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", check_review_tree)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "success"
    assert len(calls) == 1
    assert calls[0] != repo
    # Coder worktree is untouched and retains its edits/artifacts
    assert (repo / "value.txt").read_text() == "worker modified this\n"
    assert (repo / "artifact.json").read_text() == '{"tests": "passed"}\n'


def test_fresh_reviewer_excludes_git_ignored_runtime_directory(tmp_path, monkeypatch):
    repo = _repo(tmp_path)

    interpreter = tmp_path / "python3"
    interpreter.write_text("external interpreter\n")
    (repo / ".venv" / "bin").mkdir(parents=True)
    (repo / ".venv" / ".gitignore").write_text("*\n")
    (repo / ".venv" / "bin" / "python3").symlink_to(interpreter)

    assert _git_ignored_snapshot_paths(repo) == {pathlib.Path(".venv")}

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    calls: list[pathlib.Path] = []
    real_run = subprocess.run

    def check_review_tree(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append(cwd)
            assert not (cwd / ".venv").exists()
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", check_review_tree)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "success"
    assert len(calls) == 1


def test_fresh_reviewer_rejects_tracked_alias_to_git_ignored_directory_before_copy(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    runtime = repo / "runtime-cache"
    (runtime / "bin").mkdir(parents=True)
    (runtime / ".gitignore").write_text("*\n")
    interpreter = tmp_path / "external-python"
    interpreter.write_text("external interpreter\n")
    (runtime / "bin" / "python").symlink_to(interpreter)
    (repo / "runtime-alias").symlink_to("runtime-cache", target_is_directory=True)
    subprocess.run(["git", "-C", str(repo), "add", "runtime-alias"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "track runtime alias"], check=True)

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    copied_sources: list[pathlib.Path] = []
    codex_calls: list[list[str]] = []
    real_copytree = __import__("shutil").copytree
    real_run = subprocess.run

    def record_copytree(src, dst, *args, **kwargs):
        copied_sources.append(pathlib.Path(src))
        return real_copytree(src, dst, *args, **kwargs)

    def intercept_run(args, **kwargs):
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.shutil.copytree", record_copytree)
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "ignored" in result.output.lower()
    assert codex_calls == []
    assert copied_sources == [], "ignored contents must never enter a review snapshot"


def test_fresh_reviewer_rejects_absolute_alias_containing_ignored_descendant_before_copy(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    runtime = repo / "runtime-cache"
    ignored = runtime / "ignored"
    ignored.mkdir(parents=True)
    (runtime / ".gitignore").write_text("ignored/\n")
    external = tmp_path / "external-runtime"
    external.write_text("external runtime\n")
    (ignored / "nested-link").symlink_to(external)
    (repo / "runtime-alias").symlink_to(runtime.resolve(), target_is_directory=True)
    subprocess.run(
        ["git", "-C", str(repo), "add", "runtime-cache/.gitignore", "runtime-alias"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "track ignore and absolute alias"],
        check=True,
    )

    assert _git_ignored_snapshot_paths(repo) == {
        pathlib.Path("runtime-cache/ignored")
    }

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    copied_sources: list[pathlib.Path] = []
    codex_calls: list[list[str]] = []
    real_copytree = __import__("shutil").copytree
    real_run = subprocess.run

    def record_copytree(src, dst, *args, **kwargs):
        copied_sources.append(pathlib.Path(src))
        return real_copytree(src, dst, *args, **kwargs)

    def intercept_run(args, **kwargs):
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.shutil.copytree", record_copytree)
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "ignored" in result.output.lower()
    assert codex_calls == []
    assert copied_sources == [], "ignored contents must never enter a review snapshot"


def test_fresh_reviewer_git_worktree_target_isolation(tmp_path, monkeypatch):
    main_repo = _repo(tmp_path / "main")
    wt_dir = tmp_path / "wt"
    subprocess.run(["git", "-C", str(main_repo), "worktree", "add", "-b", "feature-wt", str(wt_dir)], check=True)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(wt_dir)},
    )
    calls: list[pathlib.Path] = []
    real_run = subprocess.run

    main_head_before = subprocess.run(["git", "-C", str(main_repo), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    wt_head_before = subprocess.run(["git", "-C", str(wt_dir), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()

    def mutate_wt_git(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append(cwd)
            # Verify no git metadata in review dir references original main_repo or wt_dir
            for f in (cwd / ".git").rglob("*"):
                if f.is_file():
                    content = f.read_bytes()
                    assert str(main_repo).encode() not in content
                    assert str(wt_dir).encode() not in content
            # Exercise a git write in review dir
            (cwd / "value.txt").write_text("reviewer corrupted wt\n")
            subprocess.run(["git", "-C", str(cwd), "add", "value.txt"], check=True)
            subprocess.run(["git", "-C", str(cwd), "commit", "-qm", "reviewer commit"], check=True)
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", mutate_wt_git)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert result.metadata["reviewer_mutated_tracked_files"] == "true"
    assert len(calls) == 1
    assert calls[0] != wt_dir
    # Linked worktree and main repo remain pristine in content and git refs
    assert (wt_dir / "value.txt").read_text() == "before\n"
    assert (main_repo / "value.txt").read_text() == "before\n"
    main_head_after = subprocess.run(["git", "-C", str(main_repo), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    wt_head_after = subprocess.run(["git", "-C", str(wt_dir), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    assert main_head_after == main_head_before
    assert wt_head_after == wt_head_before


def test_fresh_reviewer_rejects_symlinked_target_before_codex(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    alias = tmp_path / "alias"
    alias.symlink_to(repo, target_is_directory=True)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(alias)},
    )
    real_run = subprocess.run

    def unexpected_run(args, **kwargs):
        if args and args[0] == "codex":
            raise AssertionError("Codex launched for a symlinked target")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", unexpected_run)
    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "non-symlinked" in result.output


def test_fresh_reviewer_allows_real_target_beneath_symlinked_parent(
    tmp_path, monkeypatch
):
    real_parent = tmp_path / "real"
    real_parent.mkdir()
    repo = _repo(real_parent)
    alias_parent = tmp_path / "alias"
    alias_parent.symlink_to(real_parent, target_is_directory=True)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(alias_parent / repo.name)},
    )
    calls: list[pathlib.Path] = []
    real_run = subprocess.run

    def pass_review(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append(cwd)
            assert (cwd / "value.txt").read_text() == "before\n"
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
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
    assert calls[0] != repo


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


def test_fresh_reviewer_preserves_safe_relative_symlinks_and_isolates_absolute_links(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    (repo / "sub").mkdir()
    (repo / "sub" / "data.txt").write_text("sub data\n")
    # Relative in-tree symlink
    (repo / "rel_link.txt").symlink_to("value.txt")
    # In-tree symlink pointing to sub directory file
    (repo / "sub_link.txt").symlink_to(pathlib.Path("sub") / "data.txt")
    # Absolute in-tree symlink
    (repo / "abs_link.txt").symlink_to((repo / "value.txt").resolve())

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    calls: list[pathlib.Path] = []
    real_run = subprocess.run

    def check_and_mutate_symlink(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append(cwd)
            assert (cwd / "rel_link.txt").is_symlink()
            assert (cwd / "rel_link.txt").readlink() == pathlib.Path("value.txt")
            assert (cwd / "rel_link.txt").resolve(strict=True) == cwd.resolve(strict=True) / "value.txt"
            assert (cwd / "sub_link.txt").is_symlink()
            assert (cwd / "sub_link.txt").readlink() == pathlib.Path("sub/data.txt")
            assert (cwd / "sub_link.txt").resolve(strict=True) == cwd.resolve(strict=True) / "sub/data.txt"
            assert not (cwd / "abs_link.txt").is_symlink()
            assert (cwd / "rel_link.txt").read_text() == "before\n"
            assert (cwd / "sub_link.txt").read_text() == "sub data\n"
            assert (cwd / "abs_link.txt").read_text() == "before\n"
            (cwd / "rel_link.txt").write_text("mutated through relative link\n")
            assert (repo / "value.txt").read_text() == "before\n"
            (cwd / "rel_link.txt").write_text("before\n")
            (cwd / "abs_link.txt").write_text("mutated in review dir\n")
            assert (repo / "value.txt").read_text() == "before\n"
            (cwd / "abs_link.txt").write_text("before\n")
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", check_and_mutate_symlink)

    result = _codergen(_review_node(prompt), ctx)
    assert result.outcome == "success", result.output
    assert result.metadata["verdict"] == "pass"
    assert len(calls) == 1
    assert (repo / "value.txt").read_text() == "before\n"


def test_fresh_reviewer_rejects_relative_link_to_snapshot_root_before_copy(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    (repo / "sub").mkdir()
    (repo / "sub" / "back").symlink_to("..", target_is_directory=True)
    subprocess.run(["git", "-C", str(repo), "add", "sub/back"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "track root alias"], check=True
    )

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    copied_sources: list[pathlib.Path] = []
    codex_calls: list[list[str]] = []
    real_copytree = __import__("shutil").copytree
    real_run = subprocess.run

    def record_copytree(src, dst, *args, **kwargs):
        copied_sources.append(pathlib.Path(src))
        return real_copytree(src, dst, *args, **kwargs)

    def intercept_run(args, **kwargs):
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.shutil.copytree", record_copytree)
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "ancestor" in result.output.lower() or "root" in result.output.lower()
    assert codex_calls == []
    assert copied_sources == [], "ancestor links must be rejected before snapshot copy"


def test_fresh_reviewer_rejects_escaping_and_dangling_symlinks_without_launch(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    outside = tmp_path / "outside.txt"
    outside.write_text("outside data\n")

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    codex_calls: list[list[str]] = []
    real_run = subprocess.run

    def intercept_run(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    # 1. Untracked escaping symlink
    (repo / "escaping_link").symlink_to(outside)
    result_untracked = _codergen(_review_node(prompt), ctx)
    assert result_untracked.outcome == "error"
    assert len(codex_calls) == 0, "Codex must not be launched when source has untracked escaping symlink"
    assert "symlink" in result_untracked.output.lower() or "isolation" in result_untracked.output.lower()

    # 2. Tracked escaping symlink
    subprocess.run(["git", "-C", str(repo), "add", "escaping_link"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "track link"], check=True)
    codex_calls.clear()
    result_tracked = _codergen(_review_node(prompt), ctx)
    assert result_tracked.outcome == "error"
    assert len(codex_calls) == 0, "Codex must not be launched when source has tracked escaping symlink"
    assert "symlink" in result_tracked.output.lower() or "isolation" in result_tracked.output.lower()

    # 3. Dangling symlink
    (repo / "escaping_link").unlink()
    (repo / "dangling_link").symlink_to(repo / "nonexistent.txt")
    codex_calls.clear()
    result_dangling = _codergen(_review_node(prompt), ctx)
    assert result_dangling.outcome == "error"
    assert len(codex_calls) == 0, "Codex must not be launched when source has dangling symlink"
    assert "symlink" in result_dangling.output.lower() or "isolation" in result_dangling.output.lower()


def test_fresh_reviewer_injected_linked_worktree_conversion_failure_rejects(tmp_path, monkeypatch):
    import shutil
    main_repo = _repo(tmp_path / "main")
    wt_dir = tmp_path / "wt"
    subprocess.run(["git", "-C", str(main_repo), "worktree", "add", "-b", "feature-wt", str(wt_dir)], check=True)

    real_copytree = shutil.copytree

    def failing_copytree(src, dst, *args, **kwargs):
        src_path = pathlib.Path(src).resolve()
        if src_path == (main_repo / ".git").resolve():
            raise OSError("injected main gitdir copy failure")
        return real_copytree(src, dst, *args, **kwargs)

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(wt_dir)},
    )
    codex_calls: list[list[str]] = []
    real_run = subprocess.run

    def intercept_run(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.shutil.copytree", failing_copytree)
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)
    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert len(codex_calls) == 0, "Codex must not be launched when worktree conversion fails"
    assert result.metadata["verdict"] == "unknown"


def test_fresh_reviewer_detects_original_target_mutation_fails_closed(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    real_run = subprocess.run

    def mutate_original_target(args, **kwargs):
        if args and args[0] == "codex":
            # Simulate a bypass where original coder target is mutated directly during review
            (repo / "value.txt").write_text("corrupted original\n")
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", mutate_original_target)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert result.metadata["reviewer_mutated_tracked_files"] == "true"
    assert "reviewer changed tracked files" in result.output.lower()


def test_fresh_reviewer_blocks_absolute_writes_to_original_target(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    (repo / "artifact.json").write_text('{"tests": "passed"}\n')
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    calls: list[tuple[list[str], pathlib.Path]] = []
    real_run = subprocess.run

    def behavioral_boundary_runner(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append((list(args), cwd))

            args_list = list(args)
            codex_idx = next(i for i, a in enumerate(args_list) if "codex" in str(a))
            sandbox_wrapper = args_list[:codex_idx]

            probe_code = (
                "import pathlib, sys\n"
                "target = pathlib.Path(sys.argv[1])\n"
                "cwd = pathlib.Path.cwd()\n"
                "for name, content in [\n"
                "    ('value.txt', 'reviewer corrupted tracked\\n'),\n"
                "    ('artifact.json', '{\"tests\": \"tampered\"}\\n'),\n"
                "    ('untracked_leak.txt', 'reviewer created untracked\\n'),\n"
                "]:\n"
                "    try:\n"
                "        (target / name).write_text(content)\n"
                "    except OSError:\n"
                "        pass\n"
                "(cwd / 'snapshot_output.txt').write_text('snapshot output\\n')\n"
                "print('No blocking findings.\\nVerdict: PASS\\n')\n"
            )
            probe_cmd = sandbox_wrapper + [sys.executable, "-c", probe_code, str(repo)]
            probe_proc = real_run(
                probe_cmd,
                cwd=cwd,
                capture_output=True,
                text=True,
                env=kwargs.get("env"),
                pass_fds=kwargs.get("pass_fds", ()),
                check=False,
            )
            return subprocess.CompletedProcess(
                args, probe_proc.returncode, stdout=probe_proc.stdout, stderr=probe_proc.stderr
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", behavioral_boundary_runner)
    result = _codergen(node, ctx)

    assert result.outcome == "success", result.output
    assert result.metadata["verdict"] == "pass"
    assert len(calls) == 1
    assert (repo / "value.txt").read_text() == "before\n"
    assert (repo / "artifact.json").read_text() == '{"tests": "passed"}\n'
    assert not (repo / "untracked_leak.txt").exists()


def test_fresh_reviewer_detects_untracked_target_mutation_fails_closed(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    (repo / "artifact.json").write_text('{"tests": "passed"}\n')
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    real_run = subprocess.run

    def mutate_untracked_target(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            (repo / "artifact.json").write_text('{"tests": "tampered"}\n')
            (repo / "leak.txt").write_text("untracked leak\n")
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", mutate_untracked_target)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert result.metadata["reviewer_mutated_tracked_files"] == "true"
    assert "reviewer changed tracked files" in result.output.lower()


def test_fresh_reviewer_linux_landlock_boundary_and_pass_fds(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    calls: list[tuple[list[str], dict]] = []
    fake_fd = 42

    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr("shutil.which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr(
        "runner.handlers._linux_landlock_launcher_path",
        lambda: pathlib.Path("/fake/landlock-launcher"),
    )
    monkeypatch.setattr("runner.handlers._linux_landlock_abi", lambda: 3)
    monkeypatch.setattr(
        "runner.handlers._linux_codex_runtime_paths",
        lambda path: [pathlib.Path("/usr/bin")],
    )

    observed_landlock: dict[str, object] = {}

    def fake_landlock_prefix(**kwargs):
        observed_landlock.update(kwargs)
        from runner.handler_sandbox import _PinnedLauncherCommand
        cmd = _PinnedLauncherCommand([f"/proc/self/fd/{fake_fd}", "--read", "/usr/bin", "--write", str(kwargs["writable_paths"][0]), "--"], fake_fd)
        return cmd

    monkeypatch.setattr(
        "runner.handlers._linux_controller_sandbox_prefix", fake_landlock_prefix
    )

    real_run = subprocess.run

    def intercept_run(args, **kwargs):
        if args and any("codex" in str(a) or f"/proc/self/fd/{fake_fd}" in str(a) for a in args):
            calls.append((list(args), kwargs))
            return subprocess.CompletedProcess(
                args, 0, stdout="No blocking findings.\nVerdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    closed_fds: list[object] = []
    monkeypatch.setattr(
        "runner.handlers._close_pinned_launcher_command",
        lambda cmd: closed_fds.append(getattr(cmd, "launcher_fd", None)),
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(node, ctx)

    assert result.outcome == "success"
    assert result.metadata["verdict"] == "pass"
    assert len(calls) == 1
    args, kwargs = calls[0]
    assert kwargs.get("pass_fds") == (fake_fd,)
    assert closed_fds == [fake_fd]

    # Verify Landlock boundary paths
    denied = observed_landlock.get("denied_paths", [])
    assert repo.resolve() in denied
    writable = observed_landlock.get("writable_paths", [])
    assert repo.resolve() not in writable
    assert all(not repo.resolve().is_relative_to(w) for w in writable)

    # Verify full codex tool access preserved (no restrictive flags)
    codex_idx = args.index("codex") if "codex" in args else next(i for i, a in enumerate(args) if "codex" in a)
    codex_args = args[codex_idx:]
    assert codex_args[:5] == ["codex", "exec", "--ephemeral", "--yolo", "--skip-git-repo-check"]
    assert "--disable" not in codex_args
    assert "shell_tool" not in codex_args


def test_fresh_reviewer_linux_unavailable_landlock_fails_closed(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    codex_calls: list[list[str]] = []

    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr("runner.handlers._linux_landlock_launcher_path", lambda: None)
    monkeypatch.setattr("runner.handlers._linux_landlock_abi", lambda: None)

    real_run = subprocess.run

    def intercept_run(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(node, ctx)

    assert result.outcome == "error"
    assert len(codex_calls) == 0, "Codex must not be launched when Landlock is unavailable on Linux"
    assert result.metadata["verdict"] == "unknown"


@pytest.mark.parametrize("platform", ["darwin", "linux"])
def test_fresh_reviewer_rejects_disable_sandbox_before_snapshot_or_launch(tmp_path, monkeypatch, platform):
    from runner.handler_codergen import _isolated_review_workdir

    monkeypatch.setattr(sys, "platform", platform)
    monkeypatch.setenv("DISABLE_SANDBOX", "1")

    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="codex",
        state={"ao.worktree": str(repo)},
    )

    snapshot_calls: list[pathlib.Path] = []
    real_isolated = _isolated_review_workdir

    def spy_isolated_review_workdir(target_workdir):
        snapshot_calls.append(target_workdir)
        return real_isolated(target_workdir)

    monkeypatch.setattr("runner.handler_codergen._isolated_review_workdir", spy_isolated_review_workdir)

    codex_calls: list[list[str]] = []
    real_run = subprocess.run

    def intercept_run(args, **kwargs):
        if args and any(pathlib.Path(a).name == "codex" for a in args):
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(node, ctx)

    assert result.outcome == "error"
    assert result.metadata["verdict"] == "unknown"
    assert result.metadata["fresh_session"] == "true"
    assert "DISABLE_SANDBOX" in result.output
    assert "isolation" in result.output.lower()
    assert len(snapshot_calls) == 0, "Snapshot must not be created when DISABLE_SANDBOX is set"
    assert len(codex_calls) == 0, "Subprocess must not be launched when DISABLE_SANDBOX is set"


@pytest.mark.parametrize("platform", ["darwin", "linux"])
def test_fresh_reviewer_rejects_disable_sandbox_before_shadow_review_popen_or_snapshot(tmp_path, monkeypatch, platform):
    import tempfile
    from runner.handler_codergen import _isolated_review_workdir

    monkeypatch.setattr(sys, "platform", platform)
    monkeypatch.setenv("DISABLE_SANDBOX", "1")
    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/codex")

    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    assert str(node.attrs.get("verdict_gate", "")).lower() in {"true", "1"}
    assert str(node.attrs.get("class", "")).lower() == "review"

    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="codex",
        state={
            "ao.worktree": str(repo),
            "_df_shadow_codex_review": "true",
        },
    )

    def fail_popen(*args, **kwargs):
        raise AssertionError(f"Popen must not be called when DISABLE_SANDBOX is set on verdict review: {args}")

    def fail_run(*args, **kwargs):
        raise AssertionError(f"subprocess.run must not be called when DISABLE_SANDBOX is set on verdict review: {args}")

    def fail_isolated_workdir(*args, **kwargs):
        raise AssertionError(f"_isolated_review_workdir must not be called when DISABLE_SANDBOX is set on verdict review: {args}")

    def fail_temp_dir(*args, **kwargs):
        raise AssertionError(f"tempfile.TemporaryDirectory must not be called when DISABLE_SANDBOX is set on verdict review: {args}")

    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", fail_popen)
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fail_run)
    monkeypatch.setattr("runner.handler_codergen._isolated_review_workdir", fail_isolated_workdir)
    monkeypatch.setattr("runner.handler_codergen.tempfile.TemporaryDirectory", fail_temp_dir)
    monkeypatch.setattr(tempfile, "TemporaryDirectory", fail_temp_dir)

    result = _codergen(node, ctx)

    assert result.outcome == "error"
    assert result.metadata.get("verdict") == "unknown"
    assert result.metadata.get("fresh_session") == "true"
    assert "verdict-gated fresh review refuses DISABLE_SANDBOX; isolation is required" in result.output
    assert "shadow_codex_review" not in result.metadata



@pytest.mark.parametrize("platform", ["darwin", "linux"])
def test_sandboxed_args_for_fresh_review_rejects_disable_sandbox(tmp_path, monkeypatch, platform):
    from runner.handler_codergen import _sandboxed_args_for_fresh_review

    monkeypatch.setattr(sys, "platform", platform)
    monkeypatch.setenv("DISABLE_SANDBOX", "1")

    repo = _repo(tmp_path)
    codex_workdir = tmp_path / "review_ws"
    codex_workdir.mkdir()

    with pytest.raises(ValueError, match="DISABLE_SANDBOX"):
        _sandboxed_args_for_fresh_review(
            ["codex", "exec", "--ephemeral", "prompt"],
            codex_workdir=codex_workdir,
            target_workdir=repo,
        )


def test_non_verdict_codex_preserves_disable_sandbox(tmp_path, monkeypatch):
    monkeypatch.setenv("DISABLE_SANDBOX", "1")

    repo = _repo(tmp_path)
    prompt = tmp_path / "coder.md"
    prompt.write_text("Implement ${goal}\n")
    node = make_node(
        name="coder",
        type="codergen",
        backend="codex",
        prompt=f"@{prompt}",
        verdict_gate="false",
    )
    ctx = Context(
        goal="do task",
        workdir=repo,
        backend="codex",
    )

    codex_calls: list[list[str]] = []
    real_run = subprocess.run

    def fake_run(args, **kwargs):
        if args and any(pathlib.Path(a).name == "codex" for a in args):
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="done", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)

    result = _codergen(node, ctx)
    assert result.outcome == "success"
    assert len(codex_calls) == 1
    assert any(pathlib.Path(a).name == "codex" for a in codex_calls[0])


@pytest.mark.parametrize("verdict_gate_val", ["true", "1", "yes", "on"])
@pytest.mark.parametrize("shadow_source", ["state", "node_attr", "both"])
def test_verdict_gated_review_disables_and_ignores_shadow_codex_review(tmp_path, monkeypatch, verdict_gate_val, shadow_source):
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node_attrs = {
        "name": "cold_reviewer",
        "type": "codergen",
        "backend": "codex",
        "class_": "review",
        "prompt": f"@{prompt}",
        "verdict_gate": verdict_gate_val,
        "fresh_session": "true",
    }
    if shadow_source in ("node_attr", "both"):
        node_attrs["shadow_codex_review"] = "true"
    node = make_node(**node_attrs)
    node.attrs["class"] = "review"
    node.attrs.pop("class_", None)

    state = {"ao.worktree": str(repo)}
    if shadow_source in ("state", "both"):
        state["_df_shadow_codex_review"] = "true"

    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state=state,
    )

    real_popen = subprocess.Popen

    def fail_popen(*args, **kwargs):
        cmd = args[0] if args else kwargs.get("args", [])
        if cmd and any("codex" in str(arg) for arg in cmd):
            pytest.fail(f"subprocess.Popen must not be called for codex shadow review: {cmd}")
        return real_popen(*args, **kwargs)

    codex_calls: list[tuple[list[str], pathlib.Path]] = []
    real_run = subprocess.run

    def fake_run(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            codex_calls.append((list(args), cwd))
            assert (cwd / "value.txt").read_text() == "before\n"
            return subprocess.CompletedProcess(
                args,
                0,
                stdout="No blocking findings.\nVerdict: PASS\n",
                stderr="",
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", fail_popen)
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)

    result = _codergen(node, ctx)

    assert result.outcome == "success"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata.get("fresh_session") == "true"
    assert not any(k.startswith("shadow_codex_") for k in result.metadata)
    assert not any("shadow_codex" in k for k in result.context_updates)
    assert "## Parallel Codex Review" not in result.output
    assert "## Review Comparison" not in result.output
    assert len(codex_calls) == 1
    argv, cwd = codex_calls[0]
    assert argv[:5] == ["codex", "exec", "--ephemeral", "--yolo", "--skip-git-repo-check"]
    assert cwd != repo
    assert cwd != tmp_path
    assert not cwd.exists()


def test_sandboxed_args_for_fresh_review_macos_allows_snapshot_docs_and_denies_target_and_holdouts(tmp_path, monkeypatch):
    from runner.handler_codergen import _sandboxed_args_for_fresh_review

    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/sandbox-exec")
    monkeypatch.setattr("runner.handlers._verify_darwin_sandbox_exec", lambda: True)

    holdouts = tmp_path / "holdouts"
    holdouts.mkdir()
    (holdouts / "secret.txt").write_text("holdout secret\n")
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts))

    target_repo = _repo(tmp_path / "target")
    review_ws = tmp_path / "review_ws"
    bench_dir = review_ws / "benchmarks" / "feature"
    bench_dir.mkdir(parents=True)
    (bench_dir / "README.md").write_text("doc\n")
    (bench_dir / "DESIGN.md").write_text("design\n")

    args = _sandboxed_args_for_fresh_review(
        ["codex", "exec", "--ephemeral", "prompt"],
        codex_workdir=review_ws,
        target_workdir=target_repo,
    )
    assert args is not None
    assert args[0] == "/usr/bin/sandbox-exec"
    assert args[1] == "-p"
    profile = args[2]
    assert f'(deny file-read* (subpath "{target_repo.resolve()}"))' in profile
    assert f'(deny file-write* (subpath "{target_repo.resolve()}"))' in profile
    assert f'(deny file-read* (subpath "{holdouts.resolve()}"))' in profile
    assert f'(deny file-write* (subpath "{holdouts.resolve()}"))' in profile
    assert str(review_ws.resolve()) not in profile
    assert f'(deny file-read* (subpath "{review_ws.resolve() / "benchmarks" / "feature" / "README.md"}"))' not in profile


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS Seatbelt sandbox-exec regression")
def test_fresh_reviewer_macos_allows_snapshot_benchmark_docs_while_denying_target_and_holdouts(tmp_path, monkeypatch):
    if shutil.which("sandbox-exec") is None:
        pytest.skip("sandbox-exec not available")

    holdouts = tmp_path / "holdouts"
    holdouts.mkdir()
    (holdouts / "secret_holdout.txt").write_text("sealed test case\n")
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts))

    repo = _repo(tmp_path)
    bench_dir = repo / "benchmarks" / "task1"
    bench_dir.mkdir(parents=True, exist_ok=True)
    (bench_dir / "README.md").write_text("benchmark design notes\n")
    (bench_dir / "DESIGN.md").write_text("benchmark design\n")
    (bench_dir / "SCORING.md").write_text("benchmark scoring\n")
    (bench_dir / "SCENARIOS.md").write_text("benchmark scenarios\n")
    subprocess.run(["git", "-C", str(repo), "add", "benchmarks"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "add benchmarks"], check=True)

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="codex",
        state={"ao.worktree": str(repo)},
    )

    calls: list[tuple[list[str], pathlib.Path]] = []
    real_run = subprocess.run

    def behavioral_runner(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append((list(args), cwd))

            args_list = list(args)
            codex_idx = next(i for i, a in enumerate(args_list) if "codex" in str(a))
            sandbox_wrapper = args_list[:codex_idx]

            probe_code = (
                "import pathlib, sys\n"
                "repo = pathlib.Path(sys.argv[1])\n"
                "holdouts = pathlib.Path(sys.argv[2])\n"
                "cwd = pathlib.Path.cwd()\n"
                "# 1. Snapshot benchmark docs must be readable\n"
                "for doc in ['README.md', 'DESIGN.md', 'SCORING.md', 'SCENARIOS.md']:\n"
                "    p = cwd / 'benchmarks' / 'task1' / doc\n"
                "    content = p.read_text()\n"
                "    assert len(content) > 0, f'Empty snapshot doc {p}'\n"
                "# 2. Original target reads must be denied\n"
                "target_read_denied = False\n"
                "try:\n"
                "    (repo / 'value.txt').read_text()\n"
                "except OSError:\n"
                "    target_read_denied = True\n"
                "assert target_read_denied, 'Read from original target must be denied by sandbox'\n"
                "# 3. Holdout reads must be denied\n"
                "holdout_read_denied = False\n"
                "try:\n"
                "    (holdouts / 'secret_holdout.txt').read_text()\n"
                "except OSError:\n"
                "    holdout_read_denied = True\n"
                "assert holdout_read_denied, 'Read from holdouts must be denied by sandbox'\n"
                "# 4. Target writes must be denied\n"
                "target_write_denied = False\n"
                "try:\n"
                "    (repo / 'value.txt').write_text('tampered\\n')\n"
                "except OSError:\n"
                "    target_write_denied = True\n"
                "assert target_write_denied, 'Write to original target must be denied by sandbox'\n"
                "print('No blocking findings.\\nVerdict: PASS\\n')\n"
            )
            probe_cmd = sandbox_wrapper + [sys.executable, "-c", probe_code, str(repo), str(holdouts)]
            probe_proc = real_run(
                probe_cmd,
                cwd=cwd,
                capture_output=True,
                text=True,
                env=kwargs.get("env"),
                pass_fds=kwargs.get("pass_fds", ()),
                check=False,
            )
            return subprocess.CompletedProcess(
                args, probe_proc.returncode, stdout=probe_proc.stdout, stderr=probe_proc.stderr
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", behavioral_runner)
    result = _codergen(node, ctx)

    assert result.outcome == "success", f"Reviewer failed: {result.output}"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["reviewer_mutated_tracked_files"] == "false"
    assert len(calls) == 1
    assert (repo / "value.txt").read_text() == "before\n"
    assert (repo / "benchmarks" / "task1" / "README.md").read_text() == "benchmark design notes\n"
