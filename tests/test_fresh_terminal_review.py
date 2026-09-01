from __future__ import annotations

import contextlib
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
import runner.handlers as _handlers_shim  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402
from runner.handler_codergen import (  # noqa: E402
    _git_ignored_snapshot_paths,
    _git_path_is_ignored,
    _normalize_and_validate_review_git,
)
from runner.handler_sandbox import _ControllerRuntime  # noqa: E402


def _dummy_controller_runtime(base_dir: pathlib.Path) -> _ControllerRuntime:
    parent = base_dir / ".dark-factory" / "controller-runtimes"
    parent.mkdir(parents=True)
    run_dir = pathlib.Path(tempfile.mkdtemp(prefix="review-", dir=parent))
    codex_home = run_dir / "codex-home"
    (codex_home / "tmp").mkdir(parents=True)
    (codex_home / "auth.json").write_text("{}\n")
    env = _handlers_shim._sanitized_env()
    env.update(
        {
            "CODEX_HOME": str(codex_home),
            "HOME": str(codex_home),
            "TMPDIR": str(codex_home / "tmp"),
        }
    )
    return _ControllerRuntime(run_dir, codex_home, env)


@pytest.fixture(autouse=True)
def _hermetic_controller_runtime(monkeypatch, tmp_path):
    monkeypatch.setattr(
        "runner.handlers._create_controller_runtime",
        lambda: _dummy_controller_runtime(tmp_path),
    )


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

    assert result.outcome == "success"
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


def test_fresh_reviewer_pass_with_only_failed_required_checks_fails_closed(
    tmp_path, monkeypatch
):
    """A reviewer cannot turn an unavailable test runtime into PASS."""
    review = (
        "I ran the required reviewer checks.\n"
        "$ python -m pytest\n"
        "/usr/bin/python: No module named pytest\n"
        "exit code: 126\n"
        "$ pytest tests/test_app.py\n"
        "pytest: command not found\n"
        "rc=126\n"
        "No blocking findings.\n"
        "Verdict: PASS\n"
    )

    result, _, _ = _run_review(tmp_path, monkeypatch, review)

    assert result.outcome == "error"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["reviewer_check_failed"] == "true"
    assert "required reviewer check" in result.output.lower()
    assert "126" in result.output


def test_linux_fresh_reviewer_allows_python_and_pytest_runtime_paths(
    tmp_path, monkeypatch
):
    from runner.handler_codergen import _sandboxed_args_for_fresh_review

    repo = _repo(tmp_path / "target")
    review_ws = tmp_path / "review"
    review_ws.mkdir()
    python_exe = tmp_path / "uv" / "bin" / "python3"
    pytest_exe = tmp_path / "venv" / "bin" / "pytest"
    python_exe.parent.mkdir(parents=True)
    pytest_exe.parent.mkdir(parents=True)
    (python_exe.parent.parent / "lib").mkdir()
    (pytest_exe.parent.parent / "lib").mkdir()
    python_exe.write_text("#!/bin/sh\n")
    pytest_exe.write_text(f"#!{python_exe}\n")
    python_exe.chmod(0o755)
    pytest_exe.chmod(0o755)

    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(sys, "executable", str(python_exe))
    monkeypatch.setattr(
        "shutil.which",
        lambda name: str(pytest_exe) if name == "pytest" else f"/usr/bin/{name}",
    )
    monkeypatch.setattr(
        "runner.handlers._linux_landlock_launcher_path",
        lambda: pathlib.Path("/fake/landlock-launcher"),
    )
    monkeypatch.setattr("runner.handlers._linux_landlock_abi", lambda: 3)
    monkeypatch.setattr(
        "runner.handlers._linux_codex_runtime_paths",
        lambda path: [pathlib.Path(path).parent],
    )
    monkeypatch.setattr(
        "runner.handlers._holdout_denied_paths", lambda: [tmp_path / "holdouts"]
    )
    observed: dict[str, object] = {}
    monkeypatch.setattr(
        "runner.handlers._linux_controller_sandbox_prefix",
        lambda **kwargs: observed.update(kwargs) or ["/fake/launcher", "--"],
    )

    args = _sandboxed_args_for_fresh_review(
        ["codex", "exec", "review"], review_ws, repo
    )

    assert args == ["/fake/launcher", "--", "codex", "exec", "review"]
    assert python_exe.parent in observed["read_paths"]
    assert pytest_exe.parent in observed["read_paths"]
    assert python_exe.parent.parent in observed["read_paths"]
    assert pytest_exe.parent.parent in observed["read_paths"]
    assert repo.resolve() not in observed["writable_paths"]


@pytest.mark.skipif(
    not sys.platform.startswith("linux"), reason="requires Linux Landlock"
)
def test_linux_landlock_fresh_reviewer_runs_pytest_and_denies_target_write(
    tmp_path, monkeypatch
):
    from runner.handler_codergen import _sandboxed_args_for_fresh_review

    main_repo = _repo(tmp_path / "main")
    target = tmp_path / "target"
    subprocess.run(
        ["git", "-C", str(main_repo), "worktree", "add", "-b", "target-branch", str(target)],
        check=True,
    )
    review_ws = tmp_path / "review"
    review_ws.mkdir()
    (review_ws / "test_runtime.py").write_text(
        "import os\n"
        "from pathlib import Path\n\n"
        "def test_runtime_and_boundary():\n"
        "    assert 2 + 3 == 5\n"
        "    with __import__('pytest').raises(OSError):\n"
        "        Path(os.environ['DF_DENIED_TARGET']).write_text('blocked')\n"
        "    with __import__('pytest').raises(OSError):\n"
        "        Path(os.environ['DF_SEALED_HOLDOUT']).read_text()\n"
    )
    holdouts = tmp_path / "holdouts"
    holdouts.mkdir()
    sealed_holdout = holdouts / "sealed.txt"
    sealed_holdout.write_text("sealed\n")
    pytest_exe = pathlib.Path(sys.executable).parent / "pytest"
    if not pytest_exe.is_file():
        pytest.skip("pytest executable unavailable")

    real_which = shutil.which

    def runtime_which(name: str):
        if name == "pytest":
            return str(pytest_exe)
        return real_which(name)

    monkeypatch.setattr("runner.handler_codergen.shutil.which", runtime_which)
    monkeypatch.setattr(
        "runner.handlers._holdout_denied_paths", lambda: [holdouts.resolve()]
    )

    args = _sandboxed_args_for_fresh_review(
        [sys.executable, "-m", "pytest", "-q", "test_runtime.py"], review_ws, target
    )

    assert args is not None
    env = dict(os.environ)
    env["DF_DENIED_TARGET"] = str(target)
    env["DF_SEALED_HOLDOUT"] = str(sealed_holdout)
    proc = subprocess.run(
        args,
        cwd=review_ws,
        capture_output=True,
        text=True,
        check=False,
        env=env,
        pass_fds=getattr(args, "pass_fds", ()),
    )
    _handlers_shim._close_pinned_launcher_command(args)

    assert proc.returncode == 0, proc.stderr + proc.stdout
    assert "1 passed" in proc.stdout
    assert (target / "value.txt").read_text() == "before\n"


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

    assert result.outcome == "success", result.output
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
    assert "absolute" in result.output.lower()
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


def test_fresh_reviewer_rejects_linked_worktree_metadata_symlink_before_copy(
    tmp_path, monkeypatch
):
    main_repo = _repo(tmp_path / "main")
    wt_dir = tmp_path / "wt"
    subprocess.run(
        ["git", "-C", str(main_repo), "worktree", "add", "-b", "feature-wt", str(wt_dir)],
        check=True,
    )
    gitdir_raw = (wt_dir / ".git").read_text(encoding="utf-8").strip()
    gitdir_path = pathlib.Path(gitdir_raw.removeprefix("gitdir:").strip())
    external = tmp_path / "external-metadata"
    external.write_text("external metadata must not enter snapshot\n")
    metadata_link = gitdir_path / "malicious-metadata"
    metadata_link.symlink_to(external)

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(wt_dir)},
    )
    copied_external_bytes: list[str] = []
    codex_calls: list[list[str]] = []
    real_copy2 = shutil.copy2
    real_run = subprocess.run

    def record_copy2(source, destination, *args, **kwargs):
        result = real_copy2(source, destination, *args, **kwargs)
        if pathlib.Path(source) == metadata_link:
            copied_external_bytes.append(pathlib.Path(destination).read_text())
        return result

    def intercept_run(args, **kwargs):
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.shutil.copy2", record_copy2)
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "metadata" in result.output.lower()
    assert codex_calls == []
    assert copied_external_bytes == []


def test_fresh_reviewer_rejects_foreign_linked_worktree_gitdir_before_copy_or_codex(
    tmp_path, monkeypatch
):
    """A regular .git pointer must name metadata that identifies this worktree."""
    target_main = _repo(tmp_path / "target-main")
    target_worktree = tmp_path / "target-worktree"
    subprocess.run(
        ["git", "-C", str(target_main), "worktree", "add", "-b", "target-branch", str(target_worktree)],
        check=True,
    )
    foreign_main = _repo(tmp_path / "foreign-main")
    foreign_worktree = tmp_path / "foreign-worktree"
    subprocess.run(
        ["git", "-C", str(foreign_main), "worktree", "add", "-b", "foreign-branch", str(foreign_worktree)],
        check=True,
    )
    foreign_pointer = (foreign_worktree / ".git").read_text(encoding="utf-8")
    (target_worktree / ".git").write_text(foreign_pointer, encoding="utf-8")

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(target_worktree)},
    )
    copied_sources: list[pathlib.Path] = []
    codex_calls: list[list[str]] = []
    real_copytree = shutil.copytree
    real_run = subprocess.run

    def record_copytree(source, destination, *args, **kwargs):
        copied_sources.append(pathlib.Path(source).resolve())
        return real_copytree(source, destination, *args, **kwargs)

    def intercept_run(args, **kwargs):
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    foreign_gitdir = pathlib.Path(
        foreign_pointer.removeprefix("gitdir:").strip()
    )
    foreign_common = (foreign_gitdir / "commondir").read_text(encoding="utf-8").strip()
    foreign_main_git = (foreign_gitdir / foreign_common).resolve()
    monkeypatch.setattr("runner.handler_codergen.shutil.copytree", record_copytree)
    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "gitdir" in result.output.lower() or "metadata" in result.output.lower()
    assert codex_calls == []
    assert foreign_main_git not in copied_sources


def test_fresh_reviewer_rejects_foreign_linked_worktree_commondir_before_copy_or_codex(
    tmp_path, monkeypatch
):
    """A valid target admin dir cannot be rebound to a foreign common Git dir."""
    target_main = _repo(tmp_path / "target-main")
    target_worktree = tmp_path / "target-worktree"
    subprocess.run(
        ["git", "-C", str(target_main), "worktree", "add", "-b", "target-branch", str(target_worktree)],
        check=True,
    )
    foreign_main = _repo(tmp_path / "foreign-main")
    foreign_marker = foreign_main / ".git" / "FOREIGN_REVIEW_METADATA"
    foreign_marker.write_text("must not enter the review snapshot\n")

    target_pointer = (target_worktree / ".git").read_text(encoding="utf-8").strip()
    target_gitdir = pathlib.Path(target_pointer.removeprefix("gitdir:").strip())
    foreign_common = pathlib.Path(
        os.path.relpath(foreign_main / ".git", target_gitdir)
    )
    (target_gitdir / "commondir").write_text(f"{foreign_common}\n", encoding="utf-8")

    review_dir = tmp_path / "review"
    shutil.copytree(target_worktree, review_dir, symlinks=True)

    with pytest.raises(RuntimeError, match="common|registration"):
        _normalize_and_validate_review_git(target_worktree, review_dir)
    assert not (review_dir / ".git" / "FOREIGN_REVIEW_METADATA").exists()


def test_fresh_reviewer_rejects_symlinked_linked_worktree_gitdir_parent(
    tmp_path,
):
    """The admin-directory path itself may not traverse a symlinked parent."""
    main_repo = _repo(tmp_path / "main")
    target_worktree = tmp_path / "target-worktree"
    subprocess.run(
        ["git", "-C", str(main_repo), "worktree", "add", "-b", "target-branch", str(target_worktree)],
        check=True,
    )
    target_pointer = (target_worktree / ".git").read_text(encoding="utf-8").strip()
    target_gitdir = pathlib.Path(target_pointer.removeprefix("gitdir:").strip())
    aliased_parent = tmp_path / "admin-parent-alias"
    aliased_parent.symlink_to(target_gitdir.parent, target_is_directory=True)
    (target_worktree / ".git").write_text(
        f"gitdir: {aliased_parent / target_gitdir.name}\n", encoding="utf-8"
    )

    review_dir = tmp_path / "review"
    shutil.copytree(target_worktree, review_dir, symlinks=True)
    with pytest.raises(RuntimeError, match="[Ss]ymlink"):
        _normalize_and_validate_review_git(target_worktree, review_dir)


def test_fresh_reviewer_rejects_symlinked_target_git_pointer(
    tmp_path,
):
    """The target .git pointer itself must be a regular file, never a link."""
    main_repo = _repo(tmp_path / "main")
    target = tmp_path / "target"
    subprocess.run(
        ["git", "-C", str(main_repo), "worktree", "add", "-b", "target-branch", str(target)],
        check=True,
    )
    external_pointer = tmp_path / "external-git-pointer"
    external_pointer.write_text("gitdir: /not-a-real-admin-directory\n", encoding="utf-8")
    (target / ".git").unlink()
    (target / ".git").symlink_to(external_pointer)

    review_dir = tmp_path / "review"
    shutil.copytree(target, review_dir, symlinks=True)
    with pytest.raises(RuntimeError, match="[Ss]ymlink"):
        _normalize_and_validate_review_git(target, review_dir)


@pytest.mark.parametrize("target_kind", ["external", "dangling"])
def test_fresh_reviewer_rejects_symlinked_common_git_metadata_before_codex(
    tmp_path, monkeypatch, target_kind
):
    main_repo = _repo(tmp_path / "main")
    wt_dir = tmp_path / "wt"
    subprocess.run(
        ["git", "-C", str(main_repo), "worktree", "add", "-b", "feature-wt", str(wt_dir)],
        check=True,
    )
    if target_kind == "external":
        target = tmp_path / "external-metadata"
        target.write_text("external metadata\n")
    else:
        target = tmp_path / "missing-metadata"
    (main_repo / ".git" / "malicious-metadata").symlink_to(target)

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
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "metadata" in result.output.lower()
    assert codex_calls == []


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
        Context(goal="review the default", workdir=_repo(tmp_path), backend="echo"),
    )

    assert history[-1].outcome == "success"


def test_fresh_reviewer_preserves_safe_tracked_relative_symlinks(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    (repo / "sub").mkdir()
    (repo / "sub" / "data.txt").write_text("sub data\n")
    # Relative in-tree symlink
    (repo / "rel_link.txt").symlink_to("value.txt")
    # In-tree symlink pointing to sub directory file
    (repo / "sub_link.txt").symlink_to(pathlib.Path("sub") / "data.txt")
    subprocess.run(
        ["git", "-C", str(repo), "add", "rel_link.txt", "sub_link.txt"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "track relative links"],
        check=True,
    )

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
            assert (cwd / "rel_link.txt").read_text() == "before\n"
            assert (cwd / "sub_link.txt").read_text() == "sub data\n"
            (cwd / "rel_link.txt").write_text("mutated through relative link\n")
            assert (repo / "value.txt").read_text() == "before\n"
            (cwd / "rel_link.txt").write_text("before\n")
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


def test_fresh_reviewer_rejects_absolute_in_tree_symlink_before_codex(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    absolute_link = repo / "abs_link.txt"
    absolute_link.symlink_to((repo / "value.txt").resolve())
    subprocess.run(["git", "-C", str(repo), "add", "abs_link.txt"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "track absolute link"],
        check=True,
    )

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
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "absolute" in result.output.lower()
    assert codex_calls == []


def test_fresh_reviewer_allows_tracked_relative_metadata_symlink_without_target(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    metadata_link = repo / ".codex" / "skills" / "goal-define"
    metadata_link.parent.mkdir(parents=True)
    metadata_link.symlink_to("../../.claude/skills/goal-define")
    subprocess.run(["git", "-C", str(repo), "add", ".codex"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "add metadata link"], check=True
    )

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

    def check_metadata_link(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            copied_link = cwd / ".codex" / "skills" / "goal-define"
            calls.append(copied_link)
            assert copied_link.is_symlink()
            assert copied_link.readlink() == pathlib.Path(
                "../../.claude/skills/goal-define"
            )
            assert not copied_link.resolve(strict=False).exists()
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", check_metadata_link)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "success", result.output
    assert result.metadata["verdict"] == "pass"
    assert len(calls) == 1


def test_fresh_reviewer_rejects_tracked_dangling_link_to_ignored_path(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    (repo / ".gitignore").write_text("ignored/\n")
    metadata_link = repo / ".codex" / "skills" / "goal-define"
    metadata_link.parent.mkdir(parents=True)
    metadata_link.symlink_to("../../ignored/missing.txt")
    subprocess.run(
        ["git", "-C", str(repo), "add", ".gitignore", ".codex"], check=True
    )
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "add ignored metadata link"],
        check=True,
    )

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
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "ignored" in result.output.lower()
    assert codex_calls == []


def test_fresh_reviewer_rejects_tracked_existing_link_to_ignored_path(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    (repo / ".gitignore").write_text("ignored.txt\n")
    (repo / "ignored.txt").write_text("force-added but ignored\n")
    metadata_link = repo / ".codex" / "skills" / "goal-define"
    metadata_link.parent.mkdir(parents=True)
    metadata_link.symlink_to("../../ignored.txt")
    subprocess.run(
        ["git", "-C", str(repo), "add", ".gitignore", ".codex"], check=True
    )
    subprocess.run(
        ["git", "-C", str(repo), "add", "-f", "ignored.txt"], check=True
    )
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "track ignored target"],
        check=True,
    )

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
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "ignored" in result.output.lower()
    assert codex_calls == []


def test_fresh_reviewer_rejects_source_link_swap_before_copy(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    link = repo / "link.txt"
    link.symlink_to("value.txt")
    subprocess.run(["git", "-C", str(repo), "add", "link.txt"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "track source link"],
        check=True,
    )
    external = tmp_path / "external.txt"
    external.write_text("external bytes must not reach reviewer\n")

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    real_copytree = shutil.copytree
    real_run = subprocess.run
    codex_calls: list[pathlib.Path] = []

    def swap_source_link_then_copy(source, destination, *args, **kwargs):
        if pathlib.Path(source) == repo:
            link.unlink()
            link.symlink_to(external)
        return real_copytree(source, destination, *args, **kwargs)

    def reject_codex(args, **kwargs):
        if args and args[0] == "codex":
            cwd = pathlib.Path(kwargs["cwd"])
            codex_calls.append(cwd)
            raise AssertionError("Codex launched after a source symlink swap")
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr(
        "runner.handler_codergen.shutil.copytree", swap_source_link_then_copy
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", reject_codex)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "absolute" in result.output.lower()
    assert codex_calls == []


def test_git_path_is_ignored_fails_closed_on_git_error(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    real_run = subprocess.run

    def fail_check_ignore(args, **kwargs):
        if args[3:5] == ["check-ignore", "--quiet"]:
            return subprocess.CompletedProcess(args, 2, stdout=b"", stderr=b"broken")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fail_check_ignore)

    with pytest.raises(RuntimeError, match="Cannot determine whether review path"):
        _git_path_is_ignored(repo, pathlib.Path("ignored/missing.txt"))


def test_fresh_reviewer_rejects_ignored_file_added_during_snapshot(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path)
    (repo / ".gitignore").write_text("ignored/\n")
    subprocess.run(["git", "-C", str(repo), "add", ".gitignore"], check=True)
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-qm", "ignore local artifacts"],
        check=True,
    )

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"ao.worktree": str(repo)},
    )
    codex_calls: list[list[str]] = []
    real_copytree = shutil.copytree
    real_run = subprocess.run

    def add_ignored_file_after_validation(source, destination, *args, **kwargs):
        if pathlib.Path(source) == repo:
            ignored_file = repo / "ignored" / "late.txt"
            ignored_file.parent.mkdir()
            ignored_file.write_text("must not reach reviewer\n")
        return real_copytree(source, destination, *args, **kwargs)

    def intercept_run(args, **kwargs):
        if args and args[0] == "codex":
            codex_calls.append(list(args))
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr(
        "runner.handler_codergen.shutil.copytree", add_ignored_file_after_validation
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", intercept_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "ignored" in result.output.lower()
    assert codex_calls == []


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
    assert "looping" in result.output.lower()
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
    wrapper = tmp_path / "behavioral_sandbox.py"
    wrapper.write_text(
        "import pathlib\n"
        "import subprocess\n"
        "import sys\n"
        "target = pathlib.Path(sys.argv[1]).resolve()\n"
        "separator = sys.argv.index('--', 2)\n"
        "blocked = target / '.blocked-by-test-sandbox'\n"
        "command = [str(blocked) if arg == str(target) else arg "
        "for arg in sys.argv[separator + 1:]]\n"
        "raise SystemExit(subprocess.run(command, check=False).returncode)\n"
    )

    def deterministic_sandbox(command, codex_workdir, target_workdir, **_kwargs):
        assert codex_workdir != target_workdir
        return [
            sys.executable,
            str(wrapper),
            str(target_workdir.resolve()),
            "--",
            *command,
        ]

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        deterministic_sandbox,
    )

    def behavioral_boundary_runner(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append((list(args), cwd))

            args_list = list(args)
            if "--" in args_list:
                sandbox_wrapper = args_list[: args_list.index("--") + 1]
            else:
                codex_idx = next(
                    i for i, arg in enumerate(args_list) if "codex" in str(arg)
                )
                sandbox_wrapper = args_list[:codex_idx]

            probe = (
                "target=$1\n"
                "printf 'reviewer corrupted tracked\\n' > \"$target/value.txt\" 2>/dev/null || :\n"
                "printf '{\"tests\": \"tampered\"}\\n' > \"$target/artifact.json\" 2>/dev/null || :\n"
                "printf 'reviewer created untracked\\n' > \"$target/untracked_leak.txt\" 2>/dev/null || :\n"
                "printf 'snapshot output\\n' > snapshot_output.txt\n"
                "printf 'No blocking findings.\\nVerdict: PASS\\n'\n"
            )
            probe_cmd = sandbox_wrapper + ["/bin/sh", "-c", probe, "probe", str(repo)]
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
    benchmark = repo / "benchmarks" / "suite" / "README.md"
    benchmark.parent.mkdir(parents=True)
    benchmark.write_text("copied reviewer documentation\n")
    subprocess.run(["git", "-C", str(repo), "add", "benchmarks"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "add docs"], check=True)
    holdouts = tmp_path / "holdouts"
    holdouts.mkdir()
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
    monkeypatch.setattr(
        "runner.handlers._holdout_denied_paths", lambda: [holdouts.resolve()]
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
            copied_doc = pathlib.Path(kwargs["cwd"]) / benchmark.relative_to(repo)
            assert copied_doc.read_text() == "copied reviewer documentation\n"
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

    assert result.outcome == "success", result.output
    assert result.metadata["verdict"] == "pass"
    assert len(calls) == 1
    args, kwargs = calls[0]
    assert kwargs.get("pass_fds") == (fake_fd,)
    assert closed_fds == [fake_fd]

    # Verify Landlock boundary paths
    denied = observed_landlock.get("denied_paths", [])
    assert repo.resolve() in denied
    assert holdouts.resolve() in denied
    assert not any("benchmarks" in str(path) for path in denied)
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


def test_fresh_review_macos_profile_allows_copied_docs_and_denies_sealed_roots(
    tmp_path, monkeypatch
):
    from runner.handler_codergen import _sandboxed_args_for_fresh_review

    target = _repo(tmp_path)
    review = tmp_path / "review"
    copied_doc = review / "benchmarks" / "suite" / "README.md"
    copied_doc.parent.mkdir(parents=True)
    copied_doc.write_text("review me\n")
    holdouts = tmp_path / "holdouts"
    holdouts.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts))
    monkeypatch.setattr(sys, "platform", "darwin")
    monkeypatch.setattr("runner.handlers._verify_darwin_sandbox_exec", lambda: True)
    monkeypatch.setattr(
        "runner.handler_codergen.shutil.which", lambda name: "/usr/bin/sandbox-exec"
    )

    args = _sandboxed_args_for_fresh_review(["codex"], review, target)

    assert args is not None
    profile = args[2]
    assert str(copied_doc.resolve()) not in profile
    assert f'(deny file-read* (subpath "{target.resolve()}"))' in profile
    assert f'(deny file-read* (subpath "{holdouts.resolve()}"))' in profile


def test_fresh_reviewer_fails_closed_when_real_controller_auth_is_missing(
    tmp_path, monkeypatch
):
    from runner.handler_sandbox import _create_controller_runtime

    home = tmp_path / "private-home"
    home.mkdir(mode=0o700)
    monkeypatch.setattr(sys, "platform", "linux")
    monkeypatch.setattr(pathlib.Path, "home", lambda: home)
    monkeypatch.delenv("CODEX_HOME", raising=False)
    monkeypatch.setattr(
        "runner.handlers._create_controller_runtime", _create_controller_runtime
    )
    monkeypatch.setattr(
        "runner.handler_codergen._isolated_review_workdir",
        lambda target: contextlib.nullcontext(target),
    )
    launched: list[list[str]] = []
    real_run = subprocess.run

    def fail_if_launched(args, **kwargs):
        if args and any("codex" in str(arg) for arg in args):
            launched.append(list(args))
            return subprocess.CompletedProcess(
                args, 0, stdout="Verdict: PASS\n", stderr=""
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fail_if_launched)
    repo = _repo(tmp_path)
    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="codex",
        state={"ao.worktree": str(repo)},
    )

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "failed to initialize private codex runtime" in result.output.lower()
    assert launched == []


def test_fresh_reviewer_linked_worktree_with_dirty_calc_passes_and_not_mutated(
    tmp_path, monkeypatch
):
    main_repo = _repo(tmp_path / "main")
    (main_repo / "calc.py").write_text("def add(a, b):\n    return 0\n")
    subprocess.run(["git", "-C", str(main_repo), "add", "calc.py"], check=True)
    subprocess.run(
        ["git", "-C", str(main_repo), "commit", "-qm", "add calc.py"], check=True
    )

    wt_dir = tmp_path / "worker_wt"
    subprocess.run(
        [
            "git",
            "-C",
            str(main_repo),
            "worktree",
            "add",
            "-b",
            "worker-feature",
            str(wt_dir),
        ],
        check=True,
    )
    (wt_dir / "calc.py").write_text("def add(a, b):\n    return a + b\n")

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="codex",
        state={"ao.worktree": str(wt_dir)},
    )

    calls: list[tuple[list[str], pathlib.Path]] = []
    real_run = subprocess.run

    def fake_run(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            cwd = pathlib.Path(kwargs["cwd"])
            calls.append((list(args), cwd))
            assert (cwd / "calc.py").read_text() == "def add(a, b):\n    return a + b\n"
            return subprocess.CompletedProcess(
                args,
                0,
                stdout="No blocking findings.\nVerdict: PASS\n",
                stderr="",
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)

    result = _codergen(node, ctx)

    assert result.outcome == "success", f"Reviewer failed: {result.output}"
    assert result.metadata["verdict"] == "pass"
    assert result.metadata["reviewer_mutated_tracked_files"] == "false"
    assert len(calls) == 1
    assert calls[0][1] != wt_dir
    assert (wt_dir / "calc.py").read_text() == "def add(a, b):\n    return a + b\n"


def test_fresh_reviewer_fingerprint_component_attribution_on_unstaged_mutation(
    tmp_path, monkeypatch
):
    main_repo = _repo(tmp_path / "main")
    (main_repo / "calc.py").write_text("def add(a, b):\n    return 0\n")
    subprocess.run(["git", "-C", str(main_repo), "add", "calc.py"], check=True)
    subprocess.run(
        ["git", "-C", str(main_repo), "commit", "-qm", "add calc.py"], check=True
    )

    wt_dir = tmp_path / "worker_wt"
    subprocess.run(
        [
            "git",
            "-C",
            str(main_repo),
            "worktree",
            "add",
            "-b",
            "worker-feature",
            str(wt_dir),
        ],
        check=True,
    )
    (wt_dir / "calc.py").write_text("def add(a, b):\n    return a + b\n")

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="codex",
        state={"ao.worktree": str(wt_dir)},
    )

    real_run = subprocess.run

    def fake_run(args, **kwargs):
        if args and any("codex" in str(a) for a in args):
            cwd = pathlib.Path(kwargs["cwd"])
            # Reviewer mutates unstaged calc.py in snapshot
            (cwd / "calc.py").write_text("def add(a, b):\n    return a + b + 1\n")
            return subprocess.CompletedProcess(
                args,
                0,
                stdout="No blocking findings.\nVerdict: PASS\n",
                stderr="",
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)

    result = _codergen(node, ctx)

    assert result.outcome == "error"
    assert result.metadata["reviewer_mutated_tracked_files"] == "true"
    assert result.metadata["fingerprint_unstaged_changed"] == "true"
    assert result.metadata["fingerprint_head_changed"] == "false"
    assert result.metadata["fingerprint_staged_changed"] == "false"
    assert result.metadata["fingerprint_untracked_changed"] == "false"
    assert result.metadata["fingerprint_git_error"] == ""


def test_fresh_reviewer_fingerprint_component_attribution_on_git_diff_failure(
    tmp_path, monkeypatch
):
    main_repo = _repo(tmp_path / "main")
    (main_repo / "calc.py").write_text("def add(a, b):\n    return 0\n")
    subprocess.run(["git", "-C", str(main_repo), "add", "calc.py"], check=True)
    subprocess.run(
        ["git", "-C", str(main_repo), "commit", "-qm", "add calc.py"], check=True
    )

    wt_dir = tmp_path / "worker_wt"
    subprocess.run(
        [
            "git",
            "-C",
            str(main_repo),
            "worktree",
            "add",
            "-b",
            "worker-feature",
            str(wt_dir),
        ],
        check=True,
    )
    (wt_dir / "calc.py").write_text("def add(a, b):\n    return a + b\n")

    prompt = tmp_path / "review.md"
    prompt.write_text("Review ${goal}. End with Verdict: PASS or Verdict: FAIL.\n")
    node = _review_node(prompt)
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="codex",
        state={"ao.worktree": str(wt_dir)},
    )

    real_run = subprocess.run
    codex_executed = False

    def fake_run(args, **kwargs):
        nonlocal codex_executed
        if args and any("codex" in str(a) for a in args):
            codex_executed = True
            return subprocess.CompletedProcess(
                args,
                0,
                stdout="No blocking findings.\nVerdict: PASS\n",
                stderr="",
            )
        if (
            codex_executed
            and args
            and "diff" in args
            and "--binary" in args
            and "--cached" not in args
        ):
            return subprocess.CompletedProcess(
                args, 1, stdout="", stderr="simulated git diff error\n"
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr(
        "runner.handler_codergen._sandboxed_args_for_fresh_review",
        lambda command, *args, **kwargs: command,
    )
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)

    result = _codergen(node, ctx)

    assert result.outcome == "error"
    assert result.metadata["reviewer_mutated_tracked_files"] == "true"
    assert "simulated git diff error" in result.metadata["fingerprint_git_error"]
