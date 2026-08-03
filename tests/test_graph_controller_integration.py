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
    CHECK_IDS,
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
    evidence = repo / "evidence" / "worker-verification.json"
    evidence.parent.mkdir()
    evidence.write_text('{"schema_version": 1}\n')
    _git(repo, "add", "README.md", "evidence/worker-verification.json")
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


def _valid_response(request: object, *, verdict: str = "pass") -> str:
    return "\n".join(
        [
            f"PROMPT_ID: {request.prompt_id}",
            f"PROMPT_SHA256: {request.prompt_sha256}",
            f"ENVELOPE_SHA256: {request.envelope_sha256}",
            f"HEAD_SHA: {request.head_sha}",
            f"TASK_SHA256: {request.task_sha256}",
            f"DIFF_SHA256: {request.diff_sha256}",
            f"CHANGED_FILES_SHA256: {request.changed_files_sha256}",
            f"EVIDENCE_MANIFEST_SHA256: {request.evidence_manifest_sha256}",
            f"VERDICT: {verdict}",
            *(
                f"{check_id}: {'fail' if verdict == 'fail' and check_id == 'C0' else 'pass'}"
                for check_id in CHECK_IDS
            ),
            "",
            "## Findings",
            "None.",
            "## Commands Executed",
            "No commands required for this bounded transport test.",
            "## Evidence Checked",
            "The controller-bound request envelope.",
            "## Caveats",
            "None.",
        ]
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

    def test_graph_handler_runs_canonical_executor_and_emits_artifacts(self) -> None:
        from unittest.mock import patch

        from runner.handler_core import Context
        from runner.handler_parallel_reviewer import _parallel_reviewer
        from runner.parser import Node
        from runner.review_controller import run_controller_review as canonical_run

        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            repo = _init_clean_repo(tmp)
            node = Node(
                name="cold_reviewer",
                attrs={
                    "review_contract": "cold-review-v1",
                    "backend_priority": "codex",
                    "timeout": 1200,
                },
            )
            ctx = Context(
                goal="review the exact target",
                workdir=repo,
                backend="codex",
                run_id=f"graph-pass-{tmp.name}",
            )
            calls: list[dict[str, object]] = []

            def _canonical_spy(request, **kwargs):
                response = _valid_response(request)
                transport = json.dumps(
                    {
                        "type": "item.completed",
                        "item": {"type": "agent_message", "text": response},
                    }
                )

                def _fake_transport(command, **run_kwargs):
                    calls.append({"command": command, **run_kwargs})
                    return subprocess.CompletedProcess(command, 0, transport, "")

                with patch("runner.review_controller.subprocess.run", _fake_transport):
                    return canonical_run(request, **kwargs)

            with patch.dict(
                os.environ,
                {
                    "DARK_FACTORY_HOLDOUTS": "/sealed/holdouts",
                    "MY_HOLDOUT_SECRET": "must-not-leak",
                },
            ), patch(
                "runner.handler_parallel_reviewer._resolve_gate_backend",
                return_value=("codex", {"reviewer_backend_resolution": "test"}),
            ), patch(
                "runner.handler_parallel_reviewer._gate_subprocess_args",
                return_value=["codex", "exec", "--yolo", "unused-prompt"],
            ), patch(
                "runner.review_controller.run_controller_review",
                side_effect=_canonical_spy,
            ), patch(
                "runner.handler_parallel_reviewer._run_primary_review",
                side_effect=AssertionError("legacy gate executor must not run"),
            ):
                result = _parallel_reviewer(node, ctx)
                first_lane_dir = Path(
                    ctx.state["_df_controller_review_lane_dirs"]["primary"]
                )
                ctx._df_current_seq = 2
                second_result = _parallel_reviewer(node, ctx)
                second_lane_dir = Path(
                    ctx.state["_df_controller_review_lane_dirs"]["primary"]
                )

            self.assertEqual(result.outcome, "success", result)
            self.assertEqual(second_result.outcome, "success", second_result)
            self.assertEqual(result.metadata["review_contract_status"], "valid")
            self.assertEqual(len(calls), 2)
            self.assertEqual(calls[0]["timeout"], 1200)
            transport_env = calls[0]["env"]
            self.assertNotIn("DARK_FACTORY_HOLDOUTS", transport_env)
            self.assertNotIn("MY_HOLDOUT_SECRET", transport_env)
            self.assertNotEqual(first_lane_dir, second_lane_dir)
            self.assertTrue(first_lane_dir.is_dir())
            self.assertTrue(second_lane_dir.is_dir())
            for name in (
                "controller-receipt.json",
                "reviewer.output.md",
                "findings.json",
            ):
                self.assertTrue((first_lane_dir / name).is_file(), name)
                self.assertTrue((second_lane_dir / name).is_file(), name)

    def test_controller_timeout_is_terminal_error_without_worker_rerun(self) -> None:
        from unittest.mock import patch

        from runner.engine import run
        from runner.handler_core import Context, Result
        from runner.handlers import TYPE_REGISTRY
        from runner.parser import parse

        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            repo = _init_clean_repo(tmp)
            graph = parse(Path(__file__).parent.parent / "pipelines/slim/two_node.dot")
            worker_calls: list[str] = []

            def _worker(node, ctx):
                worker_calls.append(node.name)
                return Result(outcome="success", output="worker complete")

            ctx = Context(
                goal="review the exact target",
                workdir=repo,
                backend="codex",
                run_id=f"graph-timeout-{tmp.name}",
            )
            with patch.dict(TYPE_REGISTRY, {"codergen": _worker}), patch(
                "runner.handler_parallel_reviewer._resolve_gate_backend",
                return_value=("codex", {"reviewer_backend_resolution": "test"}),
            ), patch(
                "runner.handler_parallel_reviewer._gate_subprocess_args",
                return_value=["codex", "exec", "--yolo", "unused-prompt"],
            ), patch(
                "runner.review_controller.run_controller_review",
                side_effect=subprocess.TimeoutExpired(["codex"], 1200),
            ), patch(
                "runner.handler_parallel_reviewer._run_primary_review",
                side_effect=AssertionError("legacy gate executor must not run"),
            ):
                history = run(graph, ctx, checkpoint=tmp / "checkpoint.json", max_steps=6)

            self.assertEqual(worker_calls, ["worker"])
            reviewer = next(record for record in history if record.node == "cold_reviewer")
            self.assertEqual(reviewer.outcome, "error")
            self.assertEqual(reviewer.metadata.get("timed_out"), "true", reviewer)
            self.assertEqual(history[-1].node, "exit")
            self.assertNotEqual(history[-1].outcome, "success")

    def test_valid_controller_failure_retries_worker_then_uses_fresh_lane_dir(self) -> None:
        from unittest.mock import patch

        from runner.engine import run
        from runner.handler_core import Context, Result
        from runner.handlers import TYPE_REGISTRY
        from runner.parser import parse
        from runner.review_controller import run_controller_review as canonical_run

        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            repo = _init_clean_repo(tmp)
            graph = parse(Path(__file__).parent.parent / "pipelines/slim/two_node.dot")
            worker_calls: list[str] = []
            output_dirs: list[Path] = []

            def _worker(node, ctx):
                worker_calls.append(node.name)
                return Result(outcome="success", output="worker complete")

            def _canonical_sequence(request, **kwargs):
                output_dirs.append(Path(kwargs["output_dir"]))
                verdict = "fail" if len(output_dirs) == 1 else "pass"
                transport = json.dumps(
                    {
                        "type": "item.completed",
                        "item": {
                            "type": "agent_message",
                            "text": _valid_response(request, verdict=verdict),
                        },
                    }
                )

                def _fake_transport(command, **run_kwargs):
                    return subprocess.CompletedProcess(command, 0, transport, "")

                with patch("runner.review_controller.subprocess.run", _fake_transport):
                    return canonical_run(request, **kwargs)

            ctx = Context(
                goal="review the exact target",
                workdir=repo,
                backend="codex",
                run_id=f"graph-retry-{tmp.name}",
            )
            with patch.dict(TYPE_REGISTRY, {"codergen": _worker}), patch(
                "runner.handler_parallel_reviewer._resolve_gate_backend",
                return_value=("codex", {"reviewer_backend_resolution": "test"}),
            ), patch(
                "runner.handler_parallel_reviewer._gate_subprocess_args",
                return_value=["codex", "exec", "--yolo", "unused-prompt"],
            ), patch(
                "runner.review_controller.run_controller_review",
                side_effect=_canonical_sequence,
            ):
                history = run(graph, ctx, checkpoint=tmp / "checkpoint.json", max_steps=8)

            self.assertEqual(worker_calls, ["worker", "worker"])
            self.assertEqual(
                [record.outcome for record in history if record.node == "cold_reviewer"],
                ["failure", "success"],
            )
            self.assertEqual(history[-1].node, "exit")
            self.assertEqual(history[-1].outcome, "success")
            self.assertEqual(len(output_dirs), 2)
            self.assertNotEqual(output_dirs[0], output_dirs[1])
            for directory in output_dirs:
                self.assertTrue((directory / "controller-receipt.json").is_file())


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
