"""Graph-integration tests for the shared controller review path.

Both the standalone CLI (`runner/review_cli.py`) and the graph lane
(`runner/handler_parallel_reviewer.py`) must share one controller
request/execution/artifact implementation. Every parallel lane (primary +
every shadow) must use a distinct neutral cwd and output directory.
"""

from __future__ import annotations

import base64
import json
import os
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path
from typing import Iterable

from runner.review_controller import (
    EvidenceArtifact,
    ReviewContractError,
    ReviewInputs,
    create_review_request,
)
from runner.handler_core import Context
from runner.parser import Node


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
    repo = tmp / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.email", "jleechan2015@users.noreply.github.com")
    _git(repo, "config", "user.name", "ci")
    (repo / "README.md").write_text("hello\n")
    _git(repo, "add", "README.md")
    _git(repo, "commit", "-q", "-m", "init")
    _git(repo, "update-ref", "refs/remotes/origin/main", "HEAD")
    return repo


def _sha256(data: bytes) -> str:
    import hashlib

    return hashlib.sha256(data).hexdigest()


def _build_request(repo: Path) -> object:
    head = _git(repo, "rev-parse", "HEAD").strip()
    return create_review_request(
        ReviewInputs(
            repository="example/repo",
            workspace_path=str(repo),
            base_sha=head,
            head_sha=head,
            tree_sha=_git(repo, "rev-parse", "HEAD^{tree}").strip(),
            task_text="task",
            diff_text="",
            changed_files=("README.md",),
            evidence=(
                EvidenceArtifact(
                    path="README.md",
                    size_bytes=6,
                    sha256=_sha256(b"hello\n"),
                ),
            ),
            run_id="test",
        )
    )


class SharedHelperTests(unittest.TestCase):
    """The shared helper must exist and be importable from both call sites."""

    def test_helper_importable_from_controller(self) -> None:
        from runner.review_controller import run_controller_review

        self.assertTrue(callable(run_controller_review))

    def test_helper_is_used_by_cli(self) -> None:
        """The CLI module imports the shared helper rather than building its own."""
        from runner import review_cli

        src = Path(review_cli.__file__).read_text()
        self.assertIn("run_controller_review", src)


class DirtyWorkerSnapshotTests(unittest.TestCase):
    """A normal worker handoff contains uncommitted edits, not a clean commit."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.repo = _init_clean_repo(self.tmp)
        self.ctx = Context(
            goal="change the greeting",
            workdir=self.repo,
            backend="codex",
            run_id=f"dirty-worker-test-{self.tmp.name}",
        )
        self.node = Node(
            name="cold_reviewer",
            attrs={"review_contract": "cold-review-v1"},
        )

    def tearDown(self) -> None:
        from runner.handler_parallel_reviewer import _cleanup_controller_snapshot

        _cleanup_controller_snapshot(self.ctx)
        self._tmp.cleanup()

    def test_dirty_worker_tree_is_reviewed_from_a_clean_committed_snapshot(self) -> None:
        from runner.handler_parallel_reviewer import (
            _controller_review_request,
            _verify_controller_workspace,
        )

        (self.repo / "README.md").write_text("worker edit\n", encoding="utf-8")
        (self.repo / "new.py").write_text("VALUE = 1\n", encoding="utf-8")
        source_head = _git(self.repo, "rev-parse", "HEAD").strip()

        request = _controller_review_request(self.node, self.ctx, source_head)
        envelope = json.loads(request.envelope_json)
        snapshot = Path(envelope["target"]["workspace_path"])

        self.assertNotEqual(snapshot, self.repo)
        self.assertEqual(_git(snapshot, "status", "--porcelain"), "")
        self.assertEqual((snapshot / "README.md").read_text(), "worker edit\n")
        self.assertEqual((snapshot / "new.py").read_text(), "VALUE = 1\n")
        self.assertNotEqual(request.head_sha, source_head)
        self.assertIn("README.md", envelope["snapshots"]["changed_files"])
        self.assertIn("new.py", envelope["snapshots"]["changed_files"])
        _verify_controller_workspace(self.ctx, request)
        self.assertTrue(_git(self.repo, "status", "--porcelain"))

    def test_source_mutation_during_review_invalidates_snapshot_verdict(self) -> None:
        from runner.handler_parallel_reviewer import (
            _controller_review_request,
            _verify_controller_workspace,
        )

        (self.repo / "README.md").write_text("worker edit\n", encoding="utf-8")
        source_head = _git(self.repo, "rev-parse", "HEAD").strip()
        request = _controller_review_request(self.node, self.ctx, source_head)
        (self.repo / "README.md").write_text("changed during review\n", encoding="utf-8")

        with self.assertRaisesRegex(ReviewContractError, "source workspace changed"):
            _verify_controller_workspace(self.ctx, request)

    def test_stale_unregistered_snapshot_directory_is_replaced(self) -> None:
        from runner.handler_parallel_reviewer import _controller_review_request

        (self.repo / "README.md").write_text("worker edit\n", encoding="utf-8")
        source_head = _git(self.repo, "rev-parse", "HEAD").strip()
        snapshot = (
            Path.home()
            / ".dark-factory"
            / "controller-reviews"
            / str(self.ctx.run_id)
            / self.node.name
            / "1"
            / "review-worktree"
        )
        snapshot.mkdir(parents=True)
        (snapshot / "interrupted.txt").write_text("stale\n", encoding="utf-8")

        request = _controller_review_request(self.node, self.ctx, source_head)
        envelope = json.loads(request.envelope_json)

        self.assertEqual(
            Path(envelope["target"]["workspace_path"]), snapshot.resolve()
        )
        self.assertFalse((snapshot / "interrupted.txt").exists())
        self.assertEqual(_git(snapshot, "status", "--porcelain"), "")
        self.assertEqual((snapshot / "README.md").read_text(), "worker edit\n")

    def test_source_mutation_during_snapshot_capture_fails_closed(self) -> None:
        from runner.handler_parallel_reviewer import _controller_review_request, _git_bytes

        (self.repo / "README.md").write_text("worker edit\n", encoding="utf-8")
        source_head = _git(self.repo, "rev-parse", "HEAD").strip()
        self.node.attrs.update({"backend": "codex", "timeout": 600})
        original_git_bytes = _git_bytes
        mutated = False
        diff_calls = 0

        def mutate_after_initial_diff(workdir: Path, *args: str) -> bytes:
            nonlocal diff_calls, mutated
            output = original_git_bytes(workdir, *args)
            if (
                args == ("diff", "--binary", "HEAD")
            ):
                diff_calls += 1
            if not mutated and args == ("diff", "--binary", "HEAD"):
                (self.repo / "README.md").write_text(
                    "changed during snapshot capture\n", encoding="utf-8"
                )
                mutated = True
            return output

        with patch(
            "runner.handler_parallel_reviewer._git_bytes",
            side_effect=mutate_after_initial_diff,
        ):
            with self.assertRaisesRegex(
                ValueError, "source workspace changed during snapshot capture"
            ):
                _controller_review_request(self.node, self.ctx, source_head)

        self.assertTrue(mutated)
        self.assertEqual(diff_calls, 2)
        self.assertNotIn("_last_validated_head_sha", self.ctx.state)
        self.assertNotIn("_df_controller_review_snapshot_path", self.ctx.state)
        self.assertEqual(_git(self.repo, "rev-parse", "HEAD").strip(), source_head)
        self.assertEqual(
            (self.repo / "README.md").read_text(encoding="utf-8"),
            "changed during snapshot capture\n",
        )
        snapshot = (
            Path.home()
            / ".dark-factory"
            / "controller-reviews"
            / str(self.ctx.run_id)
            / self.node.name
            / "1"
            / "review-worktree"
        )
        self.assertFalse(snapshot.exists())

    def test_external_tracked_symlink_is_rejected_from_snapshot(self) -> None:
        from runner.handler_parallel_reviewer import _snapshot_dirty_worktree

        outside = self.tmp / "outside-tracked.txt"
        outside.write_text("secret\n", encoding="utf-8")
        link = self.repo / "tracked-link"
        link.symlink_to(outside)
        _git(self.repo, "add", "tracked-link")
        _git(self.repo, "commit", "-q", "-m", "tracked link")
        (self.repo / "README.md").write_text("worker edit\n", encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "symlink escapes"):
            _snapshot_dirty_worktree(
                self.node,
                self.ctx,
                self.repo,
                _git(self.repo, "rev-parse", "HEAD").strip(),
            )

    def test_external_untracked_symlink_is_rejected_from_snapshot(self) -> None:
        from runner.handler_parallel_reviewer import _snapshot_dirty_worktree

        outside = self.tmp / "outside-untracked.txt"
        outside.write_text("secret\n", encoding="utf-8")
        (self.repo / "untracked-link").symlink_to(outside)

        with self.assertRaisesRegex(ValueError, "symlink escapes"):
            _snapshot_dirty_worktree(
                self.node,
                self.ctx,
                self.repo,
                _git(self.repo, "rev-parse", "HEAD").strip(),
            )

    def test_cleanup_failure_downgrades_successful_snapshot_verdict(self) -> None:
        from runner.handler_core import Result
        from runner.handler_parallel_reviewer import _finish_controller_snapshot_result

        self.ctx.state.update(
            {
                "_df_controller_review_snapshot_path": str(
                    Path.home() / ".dark-factory" / "controller-reviews" / "cleanup-failure"
                ),
                "_df_controller_review_source_worktree": str(self.repo),
            }
        )
        result = Result(outcome="success", output="pass", metadata={"verdict": "pass"})
        with patch(
            "runner.handler_parallel_reviewer._cleanup_controller_snapshot",
            return_value="unable to remove snapshot",
        ):
            adjusted = _finish_controller_snapshot_result(result, self.ctx, "head")

        self.assertNotEqual(adjusted.outcome, "success")
        self.assertEqual(adjusted.metadata["controller_review_snapshot"], "cleanup_failed")
        self.assertEqual(adjusted.metadata["verdict"], "fail")

    def test_cleanup_failure_retains_bindings_for_successful_retry(self) -> None:
        from runner.handler_parallel_reviewer import _cleanup_controller_snapshot

        snapshot = (
            Path.home()
            / ".dark-factory"
            / "controller-reviews"
            / "retry-cleanup"
            / "review-worktree"
        )
        fingerprint = "f" * 64
        self.ctx.state.update(
            {
                "_df_controller_review_snapshot_path": str(snapshot),
                "_df_controller_review_source_worktree": str(self.repo),
                "_df_controller_review_source_fingerprint": fingerprint,
            }
        )
        calls: list[list[str]] = []

        def fake_run(cmd, **kwargs):
            calls.append(list(cmd))
            if len(calls) == 1:
                return subprocess.CompletedProcess(
                    cmd, 1, stdout="", stderr="snapshot is busy",
                )
            return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

        with patch("runner.handler_parallel_reviewer.subprocess.run", side_effect=fake_run):
            first_error = _cleanup_controller_snapshot(self.ctx)
            self.assertEqual(first_error, "snapshot is busy")
            self.assertEqual(
                self.ctx.state.get("_df_controller_review_snapshot_path"),
                str(snapshot),
            )
            self.assertEqual(
                self.ctx.state.get("_df_controller_review_source_worktree"),
                str(self.repo),
            )
            self.assertEqual(
                self.ctx.state.get("_df_controller_review_source_fingerprint"),
                fingerprint,
            )

            self.assertEqual(_cleanup_controller_snapshot(self.ctx), "")

        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0][-1], str(snapshot))
        self.assertEqual(calls[1][-1], str(snapshot))
        self.assertNotIn("_df_controller_review_snapshot_path", self.ctx.state)
        self.assertNotIn("_df_controller_review_source_worktree", self.ctx.state)
        self.assertNotIn("_df_controller_review_source_fingerprint", self.ctx.state)

    def test_snapshot_refuses_pending_cleanup_failure(self) -> None:
        from runner.handler_parallel_reviewer import _snapshot_dirty_worktree

        self.ctx.state.update(
            {
                "_df_controller_review_snapshot_path": str(
                    Path.home()
                    / ".dark-factory"
                    / "controller-reviews"
                    / "pending"
                    / "review-worktree"
                ),
                "_df_controller_review_source_worktree": str(self.repo),
            }
        )
        with patch(
            "runner.handler_parallel_reviewer._cleanup_controller_snapshot",
            return_value="snapshot is busy",
        ) as cleanup:
            with self.assertRaisesRegex(ValueError, "pending controller snapshot cleanup failed"):
                _snapshot_dirty_worktree(
                    self.node,
                    self.ctx,
                    self.repo,
                    _git(self.repo, "rev-parse", "HEAD").strip(),
                )
        cleanup.assert_called_once_with(self.ctx)

    def test_final_fingerprint_rejects_deterministic_aba_mutation(self) -> None:
        from runner.handler_core import Result
        from runner.handler_parallel_reviewer import (
            _finish_controller_snapshot_result,
            _source_worktree_fingerprint,
        )

        target = self.repo / "README.md"
        target.write_text("worker edit\n", encoding="utf-8")
        original_fingerprint = _source_worktree_fingerprint(self.repo)
        self.ctx.state.update(
            {
                "_df_controller_review_snapshot_path": str(
                    Path.home() / ".dark-factory" / "controller-reviews" / "aba"
                ),
                "_df_controller_review_source_worktree": str(self.repo),
                "_df_controller_review_source_fingerprint": original_fingerprint,
            }
        )
        source_fingerprint = _source_worktree_fingerprint

        def mutate_aba_and_measure(path: Path) -> str:
            target = path / "README.md"
            target.write_text("changed\n", encoding="utf-8")
            target.write_text("worker edit\n", encoding="utf-8")
            return source_fingerprint(path)

        result = Result(outcome="success", output="pass", metadata={"verdict": "pass"})
        with patch(
            "runner.handler_parallel_reviewer._cleanup_controller_snapshot",
            return_value="",
        ), patch(
            "runner.handler_parallel_reviewer._source_worktree_fingerprint",
            side_effect=mutate_aba_and_measure,
        ):
            adjusted = _finish_controller_snapshot_result(result, self.ctx, "head")

        self.assertNotEqual(adjusted.outcome, "success")
        self.assertEqual(
            adjusted.metadata["controller_review_source_fingerprint_status"],
            "changed",
        )
        self.assertEqual((self.repo / "README.md").read_text(encoding="utf-8"), "worker edit\n")

    def test_parallel_reviewer_accepts_dirty_worker_handoff_and_cleans_snapshot(self) -> None:
        from runner.handler_core import Result
        from runner.handler_parallel_reviewer import _parallel_reviewer

        (self.repo / "README.md").write_text("worker edit\n", encoding="utf-8")
        source_head = _git(self.repo, "rev-parse", "HEAD").strip()
        self.node.attrs.update({"backend": "codex", "timeout": 600})

        def fake_review(prompt, expected_sha, *_args, **_kwargs):
            encoded = prompt.split("BEGIN_CONTROLLER_ENVELOPE_BASE64\n", 1)[1].split(
                "\nEND_CONTROLLER_ENVELOPE_BASE64", 1
            )[0]
            target = json.loads(base64.b64decode(encoded).decode("utf-8"))["target"]
            inspection_command = (
                f"git -C {target['workspace_path']} diff --no-ext-diff --binary "
                f"{target['base_sha']}..{target['head_sha']}"
            )
            keys = (
                "PROMPT_ID",
                "PROMPT_SHA256",
                "ENVELOPE_SHA256",
                "HEAD_SHA",
                "TASK_SHA256",
                "DIFF_SHA256",
                "CHANGED_FILES_SHA256",
                "EVIDENCE_MANIFEST_SHA256",
            )
            bindings = {}
            for key in keys:
                match = __import__("re").search(rf"(?m)^{key}: (\S+)$", prompt)
                self.assertIsNotNone(match, key)
                bindings[key] = match.group(1)
            response = "\n".join(
                [
                    f"PROMPT_ID: {bindings['PROMPT_ID']}",
                    f"PROMPT_SHA256: {bindings['PROMPT_SHA256']}",
                    f"ENVELOPE_SHA256: {bindings['ENVELOPE_SHA256']}",
                    f"BASE_SHA: {target['base_sha']}",
                    f"HEAD_SHA: {bindings['HEAD_SHA']}",
                    f"TREE_SHA: {target['tree_sha']}",
                    f"TASK_SHA256: {bindings['TASK_SHA256']}",
                    f"DIFF_SHA256: {bindings['DIFF_SHA256']}",
                    f"CHANGED_FILES_SHA256: {bindings['CHANGED_FILES_SHA256']}",
                    "EVIDENCE_MANIFEST_SHA256: "
                    f"{bindings['EVIDENCE_MANIFEST_SHA256']}",
                    "VERDICT: pass",
                    "",
                    "## Findings",
                    "None.",
                    "## Commands Executed",
                    f"`{inspection_command}` — exit code 0.",
                    "## Evidence Checked",
                    "Worker snapshot and tests.",
                    "## Caveats",
                    "None.",
                ]
            )
            return Result(
                outcome="success",
                output=response,
                metadata={
                    "verdict": "pass",
                    "_controller_command_receipts": [
                        {
                            "command": inspection_command,
                            "exit_code": 0,
                            "output_sha256": "a" * 64,
                        }
                    ],
                },
                context_updates={"_last_validated_head_sha": expected_sha},
            )

        with patch(
            "runner.handler_parallel_reviewer._run_primary_review",
            side_effect=fake_review,
        ), patch(
            "runner.handler_parallel_reviewer._record_primary_output",
            side_effect=lambda _name, _attempt, result, _seq, _ctx: result,
        ):
            result = _parallel_reviewer(self.node, self.ctx)

        self.assertEqual(result.outcome, "success")
        self.assertEqual(result.context_updates["_last_validated_head_sha"], source_head)
        self.assertEqual(result.metadata["controller_review_snapshot"], "cleaned")
        self.assertNotIn("_df_controller_review_snapshot_path", self.ctx.state)
        self.assertTrue(_git(self.repo, "status", "--porcelain"))


class PerLaneIsolationTests(unittest.TestCase):
    """Every parallel lane must use a distinct cwd + output directory."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_per_lane_output_dirs_are_distinct(self) -> None:
        from runner.handler_parallel_reviewer import lane_output_dir

        neutral = self.tmp / "neutral"
        a = lane_output_dir(neutral, "primary")
        b = lane_output_dir(neutral, "shadow_codex")
        c = lane_output_dir(neutral, "shadow_agy")
        self.assertNotEqual(a, b)
        self.assertNotEqual(b, c)
        self.assertNotEqual(a, c)
        # All under the neutral cwd.
        for d in (a, b, c):
            self.assertTrue(str(d).startswith(str(neutral)))


class PipelineOptInTests(unittest.TestCase):
    """The three hard-tier factory pipelines must opt into cold-review-v1."""

    def _reviewer_nodes(self, dot_text: str) -> Iterable[str]:
        # Each reviewer line in pydot-parsed text starts with the node name
        # then attrs. We grab any node with `review_contract=` or any node
        # whose shape is Msquare/box (reviewer).
        for line in dot_text.splitlines():
            if "review_contract=" in line:
                yield line.strip()

    def test_gates_dot_has_review_contract(self) -> None:
        text = Path("pipelines/factory/gates.dot").read_text()
        self.assertTrue(any('review_contract="cold-review-v1"' in line for line in text.splitlines()))

    def test_level5_feature_dot_has_review_contract(self) -> None:
        text = Path("pipelines/factory/level5_feature.dot").read_text()
        self.assertTrue(any('review_contract="cold-review-v1"' in line for line in text.splitlines()))

    def test_pr_gates_dot_has_review_contract(self) -> None:
        text = Path("pipelines/factory/pr_gates.dot").read_text()
        self.assertTrue(any('review_contract="cold-review-v1"' in line for line in text.splitlines()))


def test_controller_contract_rejects_echo_backend_shortcut(tmp_path: Path) -> None:
    from runner.handler_parallel_reviewer import _parallel_reviewer

    node = Node(
        name="cold_reviewer",
        attrs={"review_contract": "cold-review-v1", "backend": "codex"},
    )
    ctx = Context(goal="review exact work", workdir=tmp_path, backend="echo")

    result = _parallel_reviewer(node, ctx)

    assert result.outcome == "error"
    assert result.metadata["review_contract_status"] == "backend_rejected"
    assert result.metadata["reviewer_backend"] == "echo"
    assert result.metadata["verdict"] == "infra_failure"


if __name__ == "__main__":
    unittest.main()
