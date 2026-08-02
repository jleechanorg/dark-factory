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
from types import SimpleNamespace
from unittest.mock import patch

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

    def test_parallel_handler_passes_explicit_v2_contract_to_request_builder(self) -> None:
        """Graph selection is explicit and reaches the shared request seam."""
        from conftest import make_node
        from runner.handler_core import Context, Result
        from runner.handler_parallel_reviewer import _parallel_reviewer

        node = make_node(
            name="review",
            type="parallel_reviewer",
            backend="codex",
            review_contract="cold-review-v2",
        )
        ctx = Context(goal="review", workdir=Path.cwd(), backend="codex", run_id="test")
        captured: list[str] = []

        def build_request(_node, _ctx, _sha, review_contract):
            captured.append(review_contract)
            return SimpleNamespace(prompt="bound prompt", prompt_id="controller-cold-review-v2")

        with (
            patch("runner.handlers._worktree_head_sha", return_value="a" * 40),
            patch("runner.handler_parallel_reviewer._controller_review_request", side_effect=build_request),
            patch("runner.handler_parallel_reviewer._resolve_gate_backend", return_value=("codex", {})),
            patch("runner.handler_parallel_reviewer._run_primary_review", return_value=Result()),
            patch("runner.handler_parallel_reviewer._record_primary_output", side_effect=lambda *args: args[2]),
            patch("runner.handler_parallel_reviewer._contract_adjusted_result", side_effect=lambda result, *args, **kwargs: result),
        ):
            result = _parallel_reviewer(node, ctx)

        self.assertEqual(captured, ["cold-review-v2"])
        self.assertEqual(result.outcome, "success")

    def test_handler_records_v2_gate_metadata_without_prose_interpretation(self) -> None:
        from runner.handler_core import Context, Result
        from runner.handler_parallel_reviewer import _contract_adjusted_result

        request = SimpleNamespace(
            review_contract="cold-review-v2",
            prompt_id="controller-cold-review-v2",
            prompt_sha256="a" * 64,
            envelope_sha256="b" * 64,
        )
        validated = SimpleNamespace(
            response_sha256="c" * 64,
            verdict="pass",
            checks=(
                ("CLAIMS", "pass"),
                ("RUNTIME", "pass"),
                ("EVIDENCE", "pass"),
                ("ADVERSARIAL", "pass"),
            ),
        )
        ctx = Context(goal="review", workdir=Path.cwd(), backend="codex")

        with (
            patch("runner.handler_parallel_reviewer._verify_controller_workspace"),
            patch("runner.review_controller.validate_review_response", return_value=validated),
            patch("runner.review_controller.validate_execution_receipts"),
        ):
            result = _contract_adjusted_result(Result(output="untrusted prose"), request, ctx, lane="primary")

        self.assertEqual(result.metadata["review_contract"], "cold-review-v2")
        self.assertEqual(result.metadata["verdict"], "pass")
        for gate_id in ("claims", "runtime", "evidence", "adversarial"):
            self.assertEqual(result.metadata[f"review_gate_{gate_id}"], "pass")


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
