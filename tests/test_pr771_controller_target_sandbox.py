"""Controller transport must sandbox the envelope target, not the source checkout."""

from __future__ import annotations

import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest

from runner.handler_core import Context, Result
from runner.handler_dispatch import (
    _build_controller_codex_transport,
    _launch_shadow_gate_review,
    _ShadowGateReview,
)
from runner.handler_parallel_reviewer import _parallel_reviewer
from runner.handler_sandbox import (
    _cleanup_controller_runtime,
    _controller_output_schema,
    _create_controller_runtime,
    _macos_read_only_profile,
)
from runner.parser import Node
from runner.review_controller import (
    ReviewContractError,
    ReviewInputs,
    create_review_request,
    validate_workspace_path,
)


def _request(snapshot: Path):
    sha = "a" * 40
    return create_review_request(
        ReviewInputs(
            repository="example/repo",
            workspace_path=str(snapshot),
            base_sha=sha,
            head_sha=sha,
            tree_sha="b" * 40,
            task_text="Review the change.",
        )
    )


def _sandboxed_codex_args() -> list[str]:
    return [
        "/usr/bin/sandbox-exec",
        "-p",
        (
            '(version 1)\n(allow default)\n'
            '(deny file-read* (subpath "/sealed/holdouts"))\n'
        ),
        "/usr/local/bin/codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        "ignored prompt",
    ]


def _auth_home(tmp_path: Path) -> Path:
    home = tmp_path / "home"
    auth = home / ".codex"
    auth.mkdir(parents=True, mode=0o700)
    os.chmod(home, 0o700)
    (auth / "auth.json").write_text('{"token":"test"}\n')
    os.chmod(auth / "auth.json", 0o600)
    return home


@pytest.fixture
def private_tmp_path():
    """Provide a temporary root whose full ancestry passes private-dir checks."""
    with tempfile.TemporaryDirectory(prefix="df-controller-test-", dir=Path.home()) as root:
        yield Path(root)


def test_controller_runtime_is_private_and_cleans_only_its_run_dir(
    private_tmp_path, monkeypatch
):
    home = _auth_home(private_tmp_path)
    outside = private_tmp_path / "outside-sentinel"
    outside.write_text("preserve\n")
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.delenv("CODEX_HOME", raising=False)

    runtime = _create_controller_runtime()
    auth = runtime.codex_home / "auth.json"
    assert runtime.run_dir.parent == home / ".dark-factory" / "controller-runtimes"
    assert runtime.env["CODEX_HOME"] == str(runtime.codex_home)
    assert runtime.env["HOME"] == str(runtime.codex_home)
    assert runtime.env["TMPDIR"] == str(runtime.codex_home / "tmp")
    assert not runtime.codex_home.is_symlink()
    assert stat.S_IMODE(auth.stat().st_mode) == 0o600
    assert auth.stat().st_nlink == 1
    assert auth.read_text() == '{"token":"test"}\n'

    _cleanup_controller_runtime(runtime.run_dir)
    assert not runtime.run_dir.exists()
    assert outside.read_text() == "preserve\n"


def test_controller_runtime_repairs_current_owned_writable_factory_dir(
    private_tmp_path, monkeypatch
):
    home = _auth_home(private_tmp_path)
    factory_dir = home / ".dark-factory"
    factory_dir.mkdir(mode=0o775)
    os.chmod(factory_dir, 0o775)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.delenv("CODEX_HOME", raising=False)

    runtime = _create_controller_runtime()
    try:
        assert stat.S_IMODE(factory_dir.stat().st_mode) == 0o700
        assert runtime.run_dir.parent == factory_dir / "controller-runtimes"
    finally:
        _cleanup_controller_runtime(runtime.run_dir)


def test_controller_output_schema_is_exactly_readable_under_restrictive_umask(
    private_tmp_path,
):
    run_dir = private_tmp_path / "run"
    run_dir.mkdir(mode=0o700)
    previous_umask = os.umask(0o077)
    try:
        schema_path = _controller_output_schema(run_dir)
    finally:
        os.umask(previous_umask)

    assert stat.S_IMODE(schema_path.lstat().st_mode) == 0o444


def test_controller_runtime_rejects_writable_home_without_repair(
    private_tmp_path, monkeypatch
):
    home = _auth_home(private_tmp_path)
    os.chmod(home, 0o775)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.delenv("CODEX_HOME", raising=False)

    with pytest.raises(ValueError, match="private"):
        _create_controller_runtime()
    assert stat.S_IMODE(home.stat().st_mode) == 0o775
    assert not (home / ".dark-factory").exists()


def test_controller_runtime_rejects_symlinked_root_and_auth(private_tmp_path, monkeypatch):
    home = _auth_home(private_tmp_path)
    outside = private_tmp_path / "outside"
    outside.mkdir()
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.delenv("CODEX_HOME", raising=False)

    runtime_parent = home / ".dark-factory" / "controller-runtimes"
    runtime_parent.parent.mkdir(parents=True, mode=0o700)
    runtime_parent.symlink_to(outside, target_is_directory=True)
    with pytest.raises(ValueError, match="private"):
        _create_controller_runtime()

    runtime_parent.unlink()
    runtime_parent.mkdir(mode=0o700)
    auth = home / ".codex" / "auth.json"
    auth.unlink()
    auth.symlink_to(outside / "auth.json")
    with pytest.raises(ValueError, match="regular file"):
        _create_controller_runtime()
    assert not list(runtime_parent.iterdir())


def test_workspace_rejects_untrusted_top_level_symlink(tmp_path):
    target = tmp_path / "target"
    target.mkdir()
    alias = tmp_path / "alias"
    alias.symlink_to(target, target_is_directory=True)
    with pytest.raises(ReviewContractError, match="symlink"):
        validate_workspace_path(str(alias))


def test_controller_runtime_uses_configured_codex_home(private_tmp_path, monkeypatch):
    home = private_tmp_path / "home"
    home.mkdir(mode=0o700)
    configured = private_tmp_path / "configured-codex-home"
    configured.mkdir(mode=0o700)
    auth = configured / "auth.json"
    auth.write_text('{"token":"configured-source"}\n')
    os.chmod(auth, 0o600)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.setenv("CODEX_HOME", str(configured))

    runtime = _create_controller_runtime()
    try:
        assert (runtime.codex_home / "auth.json").read_text() == (
            '{"token":"configured-source"}\n'
        )
        assert runtime.env["CODEX_HOME"] == str(runtime.codex_home)
    finally:
        _cleanup_controller_runtime(runtime.run_dir)


def test_controller_runtime_rejects_invalid_configured_codex_home(private_tmp_path, monkeypatch):
    home = private_tmp_path / "home"
    home.mkdir(mode=0o700)
    outside = private_tmp_path / "outside"
    outside.mkdir(mode=0o700)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.setenv("CODEX_HOME", str(private_tmp_path / "missing-codex-home"))
    with pytest.raises((ValueError, FileNotFoundError)):
        _create_controller_runtime()

    configured = private_tmp_path / "configured-codex-home"
    configured.mkdir(mode=0o700)
    (configured / "auth.json").symlink_to(outside / "auth.json")
    monkeypatch.setenv("CODEX_HOME", str(configured))
    with pytest.raises(ValueError, match="regular file"):
        _create_controller_runtime()
    assert not list((home / ".dark-factory" / "controller-runtimes").iterdir())


def test_controller_runtime_cleanup_rejects_symlink_and_profile_allows_only_home(
    private_tmp_path, monkeypatch
):
    home = _auth_home(private_tmp_path)
    outside = private_tmp_path / "outside"
    outside.mkdir()
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.delenv("CODEX_HOME", raising=False)
    runtime = _create_controller_runtime()
    linked = private_tmp_path / "linked-review"
    linked.symlink_to(runtime.run_dir, target_is_directory=True)

    with pytest.raises(ValueError, match="private"):
        _cleanup_controller_runtime(linked)
    assert runtime.run_dir.exists()
    profile = _macos_read_only_profile(
        '(version 1)\n(allow default)\n(deny file-read* (subpath "/sealed"))',
        read_only_path=runtime.run_dir,
        writable_path=runtime.codex_home,
    )
    assert "(deny file-write*)" in profile
    assert '(allow file-write* (literal "/dev/null"))' in profile
    assert f'(allow file-write* (subpath "{runtime.codex_home}"))' in profile
    assert f'(deny file-read* (subpath "{runtime.run_dir}"))' in profile
    assert str(outside) not in profile
    _cleanup_controller_runtime(runtime.run_dir)


def _assert_snapshot_profile(transport: list[str], source: Path, snapshot: Path) -> None:
    profile = transport[2]
    assert "(deny file-write*)" in profile
    assert f'(deny file-read* (subpath "{snapshot}"))' in profile
    assert str(source) not in profile
    assert "--dangerously-bypass-approvals-and-sandbox" not in transport
    assert "--disable" in transport
    assert "shell_tool" in transport
    assert "--sandbox" not in transport


@pytest.mark.skipif(
    sys.platform != "darwin" or shutil.which("sandbox-exec") is None,
    reason="macOS sandbox-exec is required",
)
def test_macos_controller_denies_target_read_but_allows_neutral_cwd(tmp_path):
    target = tmp_path / "target"
    neutral = tmp_path / "neutral"
    target.mkdir(mode=0o700)
    neutral.mkdir(mode=0o700)
    (target / "secret.txt").write_text("TARGET-SECRET\n", encoding="utf-8")
    (neutral / "marker.txt").write_text("NEUTRAL-OK\n", encoding="utf-8")
    profile = _macos_read_only_profile(
        '(version 1)\n(allow default)\n(deny file-read* (subpath "/sealed"))',
        read_only_path=target,
    )
    denied = subprocess.run(
        ["sandbox-exec", "-p", profile, "/bin/cat", str(target / "secret.txt")],
        capture_output=True,
        text=True,
        check=False,
    )
    allowed = subprocess.run(
        ["sandbox-exec", "-p", profile, "/bin/cat", str(neutral / "marker.txt")],
        capture_output=True,
        text=True,
        check=False,
    )
    assert denied.returncode != 0
    assert "TARGET-SECRET" not in denied.stdout
    assert allowed.returncode == 0
    assert allowed.stdout == "NEUTRAL-OK\n"


@pytest.mark.skipif(
    sys.platform != "darwin", reason="graph primary write-denial profile is macOS-specific"
)
def test_graph_primary_uses_validated_envelope_snapshot_for_write_denial(
    tmp_path, monkeypatch
):
    source = tmp_path / "source"
    snapshot = tmp_path / "snapshot"
    source.mkdir()
    snapshot.mkdir()
    request = _request(snapshot)
    seen: dict[str, Path] = {}

    def fake_primary(*args, **kwargs):
        seen["read_only_path"] = Path(kwargs["read_only_path"])
        return Result(outcome="success", output="controller response")

    node = Node(
        name="cold_reviewer",
        attrs={"review_contract": "cold-review-v1", "backend": "codex"},
    )
    ctx = Context(goal="review", workdir=source, backend="codex", run_id="target")
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._controller_review_request",
        lambda node, ctx, expected_sha: request,
    )
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda path: "a" * 40)
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._run_primary_review", fake_primary
    )
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._record_primary_output",
        lambda node, attempt, result, seq, ctx: result,
    )
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._contract_adjusted_result",
        lambda result, request, ctx, **kwargs: result,
    )

    result = _parallel_reviewer(node, ctx)

    assert result.outcome == "success"
    assert seen["read_only_path"] == snapshot.resolve()
    assert seen["read_only_path"] != source.resolve()
    transport = _build_controller_codex_transport(
        _sandboxed_codex_args(), read_only_path=seen["read_only_path"]
    )
    _assert_snapshot_profile(transport, source, snapshot.resolve())


def test_complete_prompt_shadow_uses_envelope_snapshot_for_write_denial(
    tmp_path, private_tmp_path, monkeypatch
):
    # CI may expose HOME as a writable staging path (including /tmp).  The
    # controller runtime validator must see a private ancestry, so construct
    # the auth/runtime home under the existing private fixture before changing
    # Path.home for this test.
    home = _auth_home(private_tmp_path)
    monkeypatch.setattr("pathlib.Path.home", lambda: home)
    monkeypatch.delenv("CODEX_HOME", raising=False)
    source = tmp_path / "source"
    snapshot = tmp_path / "snapshot"
    source.mkdir()
    snapshot.mkdir()
    seen: dict[str, object] = {}

    class FakePopen:
        pid = 123

        def __init__(self, command, **kwargs):
            seen["command"] = command

    monkeypatch.setattr("runner.handler_dispatch.sys.platform", "darwin")
    monkeypatch.setattr(
        "runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex"
    )
    monkeypatch.setattr(
        "runner.handler_dispatch._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: _sandboxed_codex_args(),
    )
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", FakePopen)
    neutral = tmp_path / "neutral"
    neutral.mkdir(mode=0o700)
    ctx = Context(
        goal="review",
        workdir=source,
        backend="codex",
        state={"_df_controller_review_cwd": str(neutral)},
    )

    review = _launch_shadow_gate_review(
        "cold_reviewer",
        "COMPLETE CONTROLLER PROMPT",
        "a" * 40,
        300,
        ctx,
        prompt_is_complete=True,
        read_only_path=snapshot,
    )

    assert isinstance(review, _ShadowGateReview)
    transport = seen["command"]
    assert isinstance(transport, list)
    _assert_snapshot_profile(transport, source, snapshot.resolve())


def test_post_review_rejects_symlinked_parent_before_canonicalization(tmp_path):
    from runner.handler_parallel_reviewer import _verify_controller_workspace
    from runner.review_controller import ReviewContractError

    real_parent = tmp_path / "real-parent"
    real_parent.mkdir()
    (real_parent / "repo").mkdir()
    alias_parent = tmp_path / "alias"
    alias_parent.symlink_to(real_parent, target_is_directory=True)
    request = _request(alias_parent / "repo")

    with pytest.raises(ReviewContractError, match="symlink"):
        _verify_controller_workspace(None, request)


def test_controller_request_survives_shadow_finish_then_clears_on_failure(monkeypatch):
    """The authenticated request must outlive shadow validation and cleanup."""
    import runner.handler_parallel_reviewer as reviewer

    request = object()
    observed: list[object] = []

    def fake_impl(node, ctx):
        ctx.state["_df_controller_review_request"] = request
        observed.append(ctx.state["_df_controller_review_request"])
        return Result(outcome="failure", output="shadow contract rejected")

    monkeypatch.setattr(reviewer, "_parallel_reviewer_impl", fake_impl)
    ctx = Context(goal="review", workdir=Path.cwd(), backend="codex")
    result = reviewer._parallel_reviewer(Node(name="cold_reviewer"), ctx)

    assert result.outcome == "failure"
    assert observed == [request]
    assert "_df_controller_review_request" not in ctx.state


def test_controller_request_clears_after_shadow_finish_exception(monkeypatch):
    """Exceptional lane completion must not leak the authenticated request."""
    import runner.handler_parallel_reviewer as reviewer

    request = object()

    def fake_impl(node, ctx):
        ctx.state["_df_controller_review_request"] = request
        raise RuntimeError("shadow finish failed")

    monkeypatch.setattr(reviewer, "_parallel_reviewer_impl", fake_impl)
    ctx = Context(goal="review", workdir=Path.cwd(), backend="codex")

    with pytest.raises(RuntimeError, match="shadow finish failed"):
        reviewer._parallel_reviewer(Node(name="cold_reviewer"), ctx)
    assert "_df_controller_review_request" not in ctx.state
