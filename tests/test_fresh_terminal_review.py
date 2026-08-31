from __future__ import annotations

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


def test_fresh_reviewer_materializes_in_tree_symlinks_without_write_through(tmp_path, monkeypatch):
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
            # Verify symlinks are materialized as real files, not symlinks
            assert not (cwd / "rel_link.txt").is_symlink()
            assert not (cwd / "sub_link.txt").is_symlink()
            assert not (cwd / "abs_link.txt").is_symlink()
            assert (cwd / "rel_link.txt").read_text() == "before\n"
            assert (cwd / "sub_link.txt").read_text() == "sub data\n"
            assert (cwd / "abs_link.txt").read_text() == "before\n"
            # Mutating a materialized file in review dir must not touch target
            (cwd / "abs_link.txt").write_text("mutated in review dir\n")
            assert (repo / "value.txt").read_text() == "before\n"
            (cwd / "abs_link.txt").write_text("before\n")
            return subprocess.CompletedProcess(args, 0, stdout="Verdict: PASS\n", stderr="")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", check_and_mutate_symlink)

    result = _codergen(_review_node(prompt), ctx)
    assert result.outcome == "success"
    assert result.metadata["verdict"] == "pass"
    assert len(calls) == 1
    assert (repo / "value.txt").read_text() == "before\n"


def test_fresh_reviewer_rejects_escaping_and_dangling_symlinks_without_launch(tmp_path, monkeypatch):
    repo = _repo(tmp_path)
    outside = tmp_path / "outside.txt"
    outside.write_text("outside data\n")
    (repo / "escaping_link").symlink_to(outside)

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
    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert len(codex_calls) == 0, "Codex must not be launched when source has escaping symlink"
    assert "symlink" in result.output.lower() or "isolation" in result.output.lower()

    # Now test dangling symlink
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

    assert result.outcome == "success"
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


