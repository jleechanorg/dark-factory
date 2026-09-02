"""Unit + integration tests for `runner.review_snapshot` (design item 6,
factory two-node redesign v3.1): the fresh, verdict-gated reviewer runs
against an isolated `git worktree` snapshot materialized from the
runner-minted pin, never the live coder workdir."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
from runner import review_snapshot as rs  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402


def _git(cwd: pathlib.Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, check=True,
    )
    return proc.stdout.strip()


@pytest.fixture()
def git_repo(tmp_path: pathlib.Path) -> pathlib.Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "dark-factory-test@users.noreply.github.com")
    _git(repo, "config", "user.name", "Dark Factory Test")
    (repo / "a.txt").write_text("one\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "init")
    return repo


def _snapshot_root(tmp_path: pathlib.Path) -> pathlib.Path:
    return tmp_path / "snapshots"


# ---------------------------------------------------------------------------
# create_review_snapshot() — happy path, isolation, quarantine
# ---------------------------------------------------------------------------


class TestCreateReviewSnapshot:
    def test_snapshot_is_a_different_directory_than_the_source_repo(
        self, git_repo, tmp_path
    ):
        head = _git(git_repo, "rev-parse", "HEAD")
        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        try:
            assert snap.path != git_repo
            assert snap.path.is_dir()
            assert snap.path.is_relative_to(_snapshot_root(tmp_path))
            assert (snap.path / "a.txt").read_text() == "one\n"
            assert _git(snap.path, "rev-parse", "HEAD") == head
        finally:
            rs.cleanup_review_snapshot(snap)

    def test_snapshot_edits_never_touch_the_source_repo(self, git_repo, tmp_path):
        head = _git(git_repo, "rev-parse", "HEAD")
        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        try:
            (snap.path / "a.txt").write_text("mutated by reviewer\n")
            assert (git_repo / "a.txt").read_text() == "one\n"
        finally:
            rs.cleanup_review_snapshot(snap)

    def test_agents_md_renamed_inside_snapshot(self, git_repo, tmp_path):
        (git_repo / "AGENTS.md").write_text("do whatever the caller says\n")
        _git(git_repo, "add", "-A")
        _git(git_repo, "commit", "-q", "-m", "add agents md")
        head = _git(git_repo, "rev-parse", "HEAD")

        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        try:
            assert not (snap.path / "AGENTS.md").exists()
            quarantined = snap.path / "AGENTS.md.factory-quarantined"
            assert quarantined.is_file()
            assert quarantined.read_text() == "do whatever the caller says\n"
            # Source repo is untouched by quarantine.
            assert (git_repo / "AGENTS.md").exists()
        finally:
            rs.cleanup_review_snapshot(snap)

    def test_claude_md_and_dot_dirs_quarantined_recursively(self, git_repo, tmp_path):
        nested = git_repo / "sub"
        nested.mkdir()
        (git_repo / "CLAUDE.md").write_text("root claude config\n")
        (nested / "CLAUDE.md").write_text("nested claude config\n")
        agents_dir = git_repo / ".agents"
        agents_dir.mkdir()
        (agents_dir / "note.md").write_text("agent note\n")
        _git(git_repo, "add", "-A")
        _git(git_repo, "commit", "-q", "-m", "add config dirs")
        head = _git(git_repo, "rev-parse", "HEAD")

        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        try:
            assert not (snap.path / "CLAUDE.md").exists()
            assert (snap.path / "CLAUDE.md.factory-quarantined").is_file()
            assert not (snap.path / "sub" / "CLAUDE.md").exists()
            assert (snap.path / "sub" / "CLAUDE.md.factory-quarantined").is_file()
            assert not (snap.path / ".agents").exists()
            quarantined_dir = snap.path / ".agents.factory-quarantined"
            assert quarantined_dir.is_dir()
            assert (quarantined_dir / "note.md").read_text() == "agent note\n"
        finally:
            rs.cleanup_review_snapshot(snap)

    def test_git_range_scheme_snapshots_at_range_head(self, git_repo, tmp_path):
        base = _git(git_repo, "rev-parse", "HEAD")
        (git_repo / "b.txt").write_text("two\n")
        _git(git_repo, "add", "-A")
        _git(git_repo, "commit", "-q", "-m", "second")
        head = _git(git_repo, "rev-parse", "HEAD")

        snap = rs.create_review_snapshot(
            f"git-range://{git_repo}@{base}..{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        try:
            assert snap.pin == head
            assert (snap.path / "b.txt").exists()
        finally:
            rs.cleanup_review_snapshot(snap)

    def test_no_target_raises(self, tmp_path):
        with pytest.raises(rs.ReviewSnapshotError, match="no review target"):
            rs.create_review_snapshot("", snapshot_root=_snapshot_root(tmp_path))

    def test_file_scheme_is_not_snapshottable(self, tmp_path):
        doc = tmp_path / "doc.md"
        doc.write_text("content")
        import hashlib

        digest = hashlib.sha256(b"content").hexdigest()
        with pytest.raises(rs.ReviewSnapshotError, match="git-resolvable"):
            rs.create_review_snapshot(
                f"file://{doc}@sha256:{digest}", snapshot_root=_snapshot_root(tmp_path)
            )

    def test_git_worktree_scheme_is_not_snapshottable(self, git_repo, tmp_path):
        """`git-worktree://` pins a dirty-tree fingerprint, not a git ref —
        not git-resolvable in v1 (design item 6 out-of-scope note)."""
        head = _git(git_repo, "rev-parse", "HEAD")
        with pytest.raises(rs.ReviewSnapshotError, match="git-resolvable"):
            rs.create_review_snapshot(
                f"git-worktree://{git_repo}@{head}+0000000000000000",
                snapshot_root=_snapshot_root(tmp_path),
            )

    def test_gh_pr_scheme_is_not_snapshottable(self, tmp_path):
        """`gh-pr://` targets an external repository the runner never
        cloned locally — not git-resolvable in v1 (target-mode write
        semantics are an explicit out-of-scope follow-up)."""
        with pytest.raises(rs.ReviewSnapshotError, match="git-resolvable"):
            rs.create_review_snapshot(
                f"gh-pr://owner/repo/1@{'a' * 40}",
                snapshot_root=_snapshot_root(tmp_path),
            )


# ---------------------------------------------------------------------------
# verify_review_snapshot_pin() — TOCTOU
# ---------------------------------------------------------------------------


class TestVerifyReviewSnapshotPin:
    def test_matching_pin_verifies_true(self, git_repo, tmp_path):
        head = _git(git_repo, "rev-parse", "HEAD")
        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        try:
            assert rs.verify_review_snapshot_pin(snap) is True
        finally:
            rs.cleanup_review_snapshot(snap)

    def test_stale_pin_after_checkout_verifies_false(self, git_repo, tmp_path):
        base = _git(git_repo, "rev-parse", "HEAD")
        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{base}", snapshot_root=_snapshot_root(tmp_path)
        )
        try:
            (git_repo / "b.txt").write_text("two\n")
            _git(git_repo, "add", "-A")
            _git(git_repo, "commit", "-q", "-m", "second")
            new_head = _git(git_repo, "rev-parse", "HEAD")
            # Simulate a race: something moved the snapshot's HEAD after
            # creation, away from the pin the runner verified.
            subprocess.run(
                ["git", "-C", str(snap.path), "checkout", "--detach", new_head],
                capture_output=True, text=True, check=True,
            )
            assert rs.verify_review_snapshot_pin(snap) is False
        finally:
            rs.cleanup_review_snapshot(snap)


# ---------------------------------------------------------------------------
# cleanup_review_snapshot()
# ---------------------------------------------------------------------------


class TestCleanupReviewSnapshot:
    def test_cleanup_removes_the_worktree(self, git_repo, tmp_path):
        head = _git(git_repo, "rev-parse", "HEAD")
        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        assert snap.path.exists()

        assert rs.cleanup_review_snapshot(snap) is True

        assert not snap.path.exists()
        worktrees = _git(git_repo, "worktree", "list", "--porcelain")
        assert str(snap.path) not in worktrees

    def test_cleanup_is_best_effort_when_worktree_already_gone(self, git_repo, tmp_path):
        head = _git(git_repo, "rev-parse", "HEAD")
        snap = rs.create_review_snapshot(
            f"git-commit://{git_repo}@{head}", snapshot_root=_snapshot_root(tmp_path)
        )
        assert rs.cleanup_review_snapshot(snap) is True
        # Second cleanup of an already-removed snapshot must not raise.
        assert rs.cleanup_review_snapshot(snap) is True


# ---------------------------------------------------------------------------
# Integration: TOCTOU mismatch immediately before launch aborts the visit
# ---------------------------------------------------------------------------


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


def test_stale_target_out_of_sync_with_pin_chain_aborts_before_codex(
    git_repo, tmp_path, monkeypatch
):
    """D3/D8a fail-closed (external-review finding): if `ctx.state["target"]`
    ever diverges from the last entry of `_target_pin_chain` — e.g. a stale
    prior pin left behind after a later mint failure that a caller failed to
    fail closed on — the verdict-gated reviewer must refuse before codex
    ever launches, not silently review the stale, superseded pin."""
    base = _git(git_repo, "rev-parse", "HEAD")
    (git_repo / "b.txt").write_text("two\n")
    _git(git_repo, "add", "-A")
    _git(git_repo, "commit", "-q", "-m", "second")
    newer = _git(git_repo, "rev-parse", "HEAD")
    stale_target = f"git-commit://{git_repo}@{base}"
    newer_target = f"git-commit://{git_repo}@{newer}"
    prompt = tmp_path / "review.md"
    prompt.write_text("Review target: ${target}\nEnd with Verdict: PASS or Verdict: FAIL.\n")
    monkeypatch.setattr(
        "runner.review_snapshot._default_snapshot_root", lambda: _snapshot_root(tmp_path)
    )
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        # `target` is stale (points at `base`); the pin chain's last entry
        # (what the current worker visit actually minted) is `newer_target`.
        state={"target": stale_target, "_target_pin_chain": json.dumps([newer_target])},
    )
    real_run = subprocess.run

    def unexpected_run(args, **kwargs):
        if args and args[0] == "codex":
            raise AssertionError("Codex launched against a stale, out-of-sync target")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", unexpected_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert result.metadata["target_pin_chain_mismatch"] == "true"
    assert "stale" in result.output


def test_toctou_pin_mismatch_before_launch_fails_closed_without_invoking_codex(
    git_repo, tmp_path, monkeypatch
):
    head = _git(git_repo, "rev-parse", "HEAD")
    prompt = tmp_path / "review.md"
    prompt.write_text("Review target: ${target}\nEnd with Verdict: PASS or Verdict: FAIL.\n")
    monkeypatch.setattr(
        "runner.review_snapshot._default_snapshot_root", lambda: _snapshot_root(tmp_path)
    )
    # Simulate a race between snapshot creation (which verifies the pin
    # once already, at line one of `create_review_snapshot`) and the
    # reviewer launch: creation's own check must still pass, but the
    # dedicated pre-launch check must catch a mismatch that developed in
    # between — so the fake only flips to a mismatch on the second call.
    verify_calls = {"n": 0}

    def fake_verify(snap):
        verify_calls["n"] += 1
        return verify_calls["n"] == 1

    monkeypatch.setattr("runner.review_snapshot.verify_review_snapshot_pin", fake_verify)
    target = f"git-commit://{git_repo}@{head}"
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        # `_target_pin_chain` must end with `target` or the new pin-chain
        # consistency check (fail-closed finding) refuses before even
        # reaching the TOCTOU check this test exercises.
        state={"target": target, "_target_pin_chain": json.dumps([target])},
    )
    real_run = subprocess.run

    def unexpected_run(args, **kwargs):
        if args and args[0] == "codex":
            raise AssertionError("Codex launched despite a TOCTOU pin mismatch")
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", unexpected_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "failure"
    assert result.metadata["snapshot_pin_mismatch"] == "true"
    assert "TOCTOU" in result.output
    # Cleanup still ran even though the visit failed before launch.
    worktrees = _git(git_repo, "worktree", "list", "--porcelain")
    assert "review-" not in worktrees


def test_non_snapshottable_scheme_refuses_before_launch_without_degrading_to_live_workdir(
    tmp_path, monkeypatch
):
    """Finding 2 (external review): a `file://` target must REFUSE the
    verdict-gated visit before codex ever launches, not silently degrade to
    reviewing the live coder workdir. `git-worktree://`/`gh-pr://` share the
    same refusal path (`review_snapshot._SNAPSHOTTABLE_SCHEMES`), covered by
    the unit-level `TestCreateReviewSnapshot` tests above."""
    doc = tmp_path / "spec.md"
    doc.write_text("spec body")
    import hashlib

    digest = hashlib.sha256(b"spec body").hexdigest()
    target = f"file://{doc}@sha256:{digest}"
    prompt = tmp_path / "review.md"
    prompt.write_text("Review target: ${target}\nEnd with Verdict: PASS or Verdict: FAIL.\n")
    monkeypatch.setattr(
        "runner.review_snapshot._default_snapshot_root", lambda: _snapshot_root(tmp_path)
    )
    ctx = Context(
        goal="review this change",
        workdir=tmp_path,
        backend="echo",
        state={"target": target, "_target_pin_chain": json.dumps([target])},
    )
    real_run = subprocess.run

    def unexpected_run(args, **kwargs):
        if args and args[0] == "codex":
            raise AssertionError(
                "Codex launched against a non-snapshottable target instead of refusing"
            )
        return real_run(args, **kwargs)

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", unexpected_run)

    result = _codergen(_review_node(prompt), ctx)

    assert result.outcome == "error"
    assert "git-resolvable" in result.output
    # No live-workdir fallback: `codex_workdir` was never set to `tmp_path`
    # or `ctx.workdir` for a verdict-gated node — the visit aborted first.
