"""Graph-integration tests for the shared controller review path.

Both the standalone CLI (`runner/review_cli.py`) and the graph lane
(`runner/handler_parallel_reviewer.py`) must share one controller
request/execution/artifact implementation. Every parallel lane (primary +
every shadow) must use a distinct neutral cwd and output directory.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Iterable

from runner.review_controller import (
    EvidenceArtifact,
    ReviewContractError,
    ReviewInputs,
    create_review_request,
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
    repo = tmp / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.email", "jleechan2015@users.noreply.github.com")
    _git(repo, "config", "user.name", "ci")
    (repo / "README.md").write_text("hello\n")
    _git(repo, "add", "README.md")
    _git(repo, "commit", "-q", "-m", "init")
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


if __name__ == "__main__":
    unittest.main()