"""Immutable / read-only target validation for the controller contract.

The controller must reject any reviewer request whose target workspace is not a
real, non-symlink directory contained inside the reviewed project (and outside
any sealed holdout root). Evidence files must be regular files inside the
target and within the 1 MiB input ceiling. Post-review reverification must
catch mutations between request creation and lane return.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from runner.review_controller import (
    EvidenceArtifact,
    ReviewContractError,
    ReviewInputs,
    create_review_request,
    validate_immutable_target,
)


def _git(cwd: Path, *args: str, allow_empty: bool = False) -> str:
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0 and not allow_empty:
        raise AssertionError(f"git {args} failed: {proc.stderr}")
    return proc.stdout


def _init_clean_repo(tmp: Path) -> Path:
    """Initialize a git repo with one commit so HEAD/tree SHAs exist."""
    repo = tmp / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.email", "jleechan2015@users.noreply.github.com")
    _git(repo, "config", "user.name", "ci")
    (repo / "README.md").write_text("hello\n")
    _git(repo, "add", "README.md")
    _git(repo, "commit", "-q", "-m", "init")
    return repo


def _make_holdout_roots(tmp: Path) -> tuple[str, ...]:
    holdout = tmp / "holdout"
    holdout.mkdir()
    return (str(holdout),)


def _base_inputs(repo: Path, holdouts: tuple[str, ...], task: str = "task") -> ReviewInputs:
    placeholder_sha = "0" * 40
    head = placeholder_sha
    try:
        head = _git(repo, "rev-parse", "HEAD").strip() or placeholder_sha
        tree = _git(repo, "rev-parse", "HEAD^{tree}").strip() or placeholder_sha
    except AssertionError:
        tree = placeholder_sha
    return ReviewInputs(
        repository="example/repo",
        workspace_path=str(repo),
        base_sha=head,
        head_sha=head,
        tree_sha=tree,
        task_text=task,
        diff_text="",
        changed_files=("README.md",),
        evidence=(),
        run_id="test",
    )


class WorkspacePathValidationTests(unittest.TestCase):
    """workspace_path must be a real, non-symlink directory outside holdouts."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.holdouts = _make_holdout_roots(self.tmp)
        self.repo = _init_clean_repo(self.tmp)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_workspace_is_regular_directory_passes(self) -> None:
        # Should not raise.
        validate_immutable_target(_base_inputs(self.repo, self.holdouts), holdout_roots=self.holdouts)
        self.assertTrue(self.repo.is_dir())

    def test_workspace_nonexistent_rejected(self) -> None:
        inputs = _base_inputs(self.tmp / "does-not-exist", self.holdouts)
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_workspace_is_regular_file_rejected(self) -> None:
        not_a_dir = self.tmp / "regular_file.txt"
        not_a_dir.write_text("not a dir")
        inputs = _base_inputs(not_a_dir, self.holdouts)
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_workspace_is_symlink_rejected(self) -> None:
        link = self.tmp / "link_to_repo"
        os.symlink(self.repo, link)
        inputs = _base_inputs(link, self.holdouts)
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_workspace_inside_holdout_rejected(self) -> None:
        inside = self.tmp / "holdout" / "scenarios"
        inside.mkdir()
        _git(inside, "init", "-q")
        _git(inside, "config", "user.email", "jleechan2015@users.noreply.github.com")
        _git(inside, "config", "user.name", "ci")
        (inside / "x").write_text("x")
        _git(inside, "add", "x")
        _git(inside, "commit", "-q", "-m", "i")
        inputs = _base_inputs(inside, self.holdouts)
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_workspace_symlink_into_holdout_rejected(self) -> None:
        inside = self.tmp / "holdout" / "scenarios"
        inside.mkdir()
        link = self.tmp / "link_to_holdout"
        os.symlink(inside, link)
        inputs = _base_inputs(link, self.holdouts)
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_base_inputs_helper_uses_holdouts(self) -> None:
        # Sanity: the helper builds a valid ReviewInputs and would pass.
        validate_immutable_target(_base_inputs(self.repo, self.holdouts), holdout_roots=self.holdouts)


class EvidenceValidationTests(unittest.TestCase):
    """Evidence paths must be regular files inside the workspace, not symlinks."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.holdouts = _make_holdout_roots(self.tmp)
        self.repo = _init_clean_repo(self.tmp)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def _evidence_for(self, path: Path) -> EvidenceArtifact:
        data = path.read_bytes()
        rel = path.relative_to(self.repo).as_posix()
        return EvidenceArtifact(path=rel, size_bytes=len(data), sha256=_sha256(data))

    def test_regular_file_evidence_passes(self) -> None:
        # README.md already in repo.
        evidence = (self._evidence_for(self.repo / "README.md"),)
        inputs = replace_evidence(_base_inputs(self.repo, self.holdouts), evidence)
        validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_directory_evidence_rejected(self) -> None:
        # .git is a directory.
        bad = EvidenceArtifact(
            path=".git",
            size_bytes=0,
            sha256="0" * 64,
        )
        inputs = replace_evidence(_base_inputs(self.repo, self.holdouts), (bad,))
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_symlink_evidence_rejected(self) -> None:
        link = self.repo / "link_to_readme"
        os.symlink(self.repo / "README.md", link)
        bad = EvidenceArtifact(
            path="link_to_readme",
            size_bytes=6,
            sha256=_sha256(b"hello\n"),
        )
        inputs = replace_evidence(_base_inputs(self.repo, self.holdouts), (bad,))
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_evidence_outside_workspace_rejected(self) -> None:
        outside = self.tmp / "outside.txt"
        outside.write_text("x")
        rel = "../outside.txt"
        bad = EvidenceArtifact(path=rel, size_bytes=1, sha256=_sha256(b"x"))
        inputs = replace_evidence(_base_inputs(self.repo, self.holdouts), (bad,))
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_evidence_over_1mib_rejected(self) -> None:
        big = self.repo / "big.bin"
        big.write_bytes(b"a" * (1024 * 1024 + 1))
        bad = EvidenceArtifact(
            path="big.bin",
            size_bytes=1024 * 1024 + 1,
            sha256=_sha256(b"a" * (1024 * 1024 + 1)),
        )
        inputs = replace_evidence(_base_inputs(self.repo, self.holdouts), (bad,))
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)

    def test_evidence_symlink_swap_to_outside_rejected(self) -> None:
        """Regression: a symlink that initially pointed inside must be rejected."""
        # Create a file outside the workspace, symlink it into workspace, then
        # try to use the symlink as evidence. validate_immutable_target must
        # refuse the symlink at request-creation time.
        outside = self.tmp / "outside_secret.txt"
        outside.write_text("supersecret")
        link = self.repo / "innocent_looking"
        os.symlink(outside, link)
        # Provide the size/sha of the underlying file (the easy mistake a
        # permissive validator would let through).
        data = outside.read_bytes()
        bad = EvidenceArtifact(
            path="innocent_looking",
            size_bytes=len(data),
            sha256=_sha256(data),
        )
        inputs = replace_evidence(_base_inputs(self.repo, self.holdouts), (bad,))
        with self.assertRaises(ReviewContractError):
            validate_immutable_target(inputs, holdout_roots=self.holdouts)


class PostReviewReverifyTests(unittest.TestCase):
    """The post-review reverifier must catch workspace mutation."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.holdouts = _make_holdout_roots(self.tmp)
        self.repo = _init_clean_repo(self.tmp)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_post_review_workspace_mutation_fails(self) -> None:
        # Build a real request from the clean repo.
        inputs = _base_inputs(self.repo, self.holdouts)
        # Wire validate_immutable_target into the request builder.
        from runner.review_controller import validate_immutable_target as vit

        vit(inputs, holdout_roots=self.holdouts)
        request = create_review_request(inputs)

        # Now simulate the reviewer mutating a tracked file in workspace.
        # We need a fake "context" object that _verify_controller_workspace
        # can use; the function expects ctx.workdir.
        class _FakeCtx:
            def __init__(self, workdir: Path) -> None:
                self.workdir = workdir

        ctx: object = _FakeCtx(self.repo)  # type: ignore[assignment]
        (self.repo / "README.md").write_text("mutated by reviewer\n")
        _git(self.repo, "add", "README.md")
        _git(self.repo, "commit", "-q", "-m", "reviewer wrote here")

        from runner.handler_parallel_reviewer import _verify_controller_workspace

        with self.assertRaises(ReviewContractError):
            _verify_controller_workspace(ctx, request)  # type: ignore[arg-type]

    def test_post_review_clean_workspace_passes(self) -> None:
        """A clean, unchanged controller target remains valid after review."""
        request = create_review_request(_base_inputs(self.repo, self.holdouts))

        class _FakeCtx:
            def __init__(self, workdir: Path) -> None:
                self.workdir = workdir

        from runner.handler_parallel_reviewer import _verify_controller_workspace

        _verify_controller_workspace(_FakeCtx(self.repo), request)  # type: ignore[arg-type]


class ControllerSnapshotTests(unittest.TestCase):
    """Worker output must be reviewed through a clean frozen Git target."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.repo = _init_clean_repo(self.tmp)
        _git(self.repo, "update-ref", "refs/remotes/origin/main", "HEAD")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_worker_task_artifact_uses_clean_frozen_review_snapshot(self) -> None:
        """A worker's task file cannot block or enter the controller review target."""
        from runner.handler_core import Context
        from runner.handler_parallel_reviewer import _controller_review_request
        from runner.parser import Node

        (self.repo / "README.md").write_text("worker change\n")
        task_dir = self.repo / ".dark-factory"
        task_dir.mkdir()
        (task_dir / "agy-task-worker.md").write_text("runner task artifact\n")

        request = _controller_review_request(
            Node(name="cold_reviewer", attrs={}),
            Context(goal="review worker output", workdir=self.repo),
            _git(self.repo, "rev-parse", "HEAD").strip(),
        )
        envelope = json.loads(request.envelope_json)
        snapshot = Path(envelope["target"]["workspace_path"])

        self.assertNotEqual(snapshot, self.repo)
        self.assertTrue((self.repo / ".dark-factory" / "agy-task-worker.md").exists())
        self.assertNotEqual(_git(self.repo, "status", "--porcelain=v1"), "")
        self.assertEqual(_git(snapshot, "status", "--porcelain=v1"), "")
        self.assertEqual((snapshot / "README.md").read_text(), "worker change\n")
        self.assertFalse((snapshot / ".dark-factory" / "agy-task-worker.md").exists())
        self.assertEqual(_git(snapshot, "rev-parse", "HEAD").strip(), request.head_sha)

    def test_ignored_declared_evidence_is_bound_into_snapshot(self) -> None:
        """Ignored evidence declared by the graph remains reviewable and digest-bound."""
        from runner.handler_core import Context
        from runner.handler_parallel_reviewer import _controller_review_request
        from runner.parser import Node

        (self.repo / ".gitignore").write_text("evidence/\n")
        _git(self.repo, "add", ".gitignore")
        _git(self.repo, "commit", "-q", "-m", "ignore evidence")
        _git(self.repo, "update-ref", "refs/remotes/origin/main", "HEAD")
        (self.repo / "README.md").write_text("worker change\n")
        evidence_dir = self.repo / "evidence"
        evidence_dir.mkdir()
        evidence = evidence_dir / "controller.json"
        evidence.write_text('{"status":"pass"}\n')
        task_dir = self.repo / ".dark-factory"
        task_dir.mkdir()
        (task_dir / "agy-task-worker.md").write_text("runner task artifact\n")

        request = _controller_review_request(
            Node(name="cold_reviewer", attrs={"evidence_paths": "evidence/controller.json"}),
            Context(goal="review worker evidence", workdir=self.repo),
            _git(self.repo, "rev-parse", "HEAD").strip(),
        )
        envelope = json.loads(request.envelope_json)
        snapshot = Path(envelope["target"]["workspace_path"])
        bound = envelope["evidence"]

        self.assertEqual((snapshot / "evidence" / "controller.json").read_text(), evidence.read_text())
        self.assertFalse((snapshot / ".dark-factory" / "agy-task-worker.md").exists())
        self.assertEqual(bound, [{
            "path": "evidence/controller.json",
            "size_bytes": len(evidence.read_bytes()),
            "sha256": _sha256(evidence.read_bytes()),
        }])


def replace_evidence(
    inputs: ReviewInputs, evidence: tuple[EvidenceArtifact, ...]
) -> ReviewInputs:
    """Return a copy of `inputs` with `evidence` swapped in."""
    return ReviewInputs(
        repository=inputs.repository,
        workspace_path=inputs.workspace_path,
        base_sha=inputs.base_sha,
        head_sha=inputs.head_sha,
        tree_sha=inputs.tree_sha,
        task_text=inputs.task_text,
        diff_text=inputs.diff_text,
        changed_files=inputs.changed_files,
        evidence=evidence,
        run_id=inputs.run_id,
    )


def _sha256(data: bytes) -> str:
    import hashlib

    return hashlib.sha256(data).hexdigest()


if __name__ == "__main__":
    unittest.main()
