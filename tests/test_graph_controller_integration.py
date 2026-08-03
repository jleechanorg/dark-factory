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
    ControllerTransportError,
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
                real_run = subprocess.run

                def _fake_transport(command, **run_kwargs):
                    if command and command[0] == "git":
                        return real_run(command, **run_kwargs)
                    calls.append({"command": command, **run_kwargs})
                    return subprocess.CompletedProcess(command, 0, transport, "")

                with patch("runner.review_controller.run_bounded_process", _fake_transport):
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
                side_effect=ControllerTransportError(
                    "controller timed out", timed_out=True
                ),
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
                real_run = subprocess.run

                def _fake_transport(command, **run_kwargs):
                    if command and command[0] == "git":
                        return real_run(command, **run_kwargs)
                    return subprocess.CompletedProcess(command, 0, transport, "")

                with patch("runner.review_controller.run_bounded_process", _fake_transport):
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

    def test_controller_mutation_leaves_no_accepted_artifacts(self) -> None:
        """Post-review mutation must fail before receipt/findings acceptance."""
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
                goal="reject a reviewer-mutated target",
                workdir=repo,
                backend="codex",
                run_id=f"graph-mutation-{tmp.name}",
            )

            def _canonical_mutating_transport(request, **kwargs):
                response = _valid_response(request)
                transport = json.dumps(
                    {
                        "type": "item.completed",
                        "item": {"type": "agent_message", "text": response},
                    }
                )
                target = Path(
                    json.loads(request.envelope_json)["target"]["workspace_path"]
                )
                real_run = subprocess.run

                def _fake_transport(command, **run_kwargs):
                    if command and command[0] == "git":
                        return real_run(command, **run_kwargs)
                    (target / "README.md").write_text(
                        "mutated during review\n", encoding="utf-8"
                    )
                    return subprocess.CompletedProcess(command, 0, transport, "")

                with patch("runner.review_controller.run_bounded_process", _fake_transport):
                    return canonical_run(request, **kwargs)

            with patch(
                "runner.handler_parallel_reviewer._resolve_gate_backend",
                return_value=("codex", {"reviewer_backend_resolution": "test"}),
            ), patch(
                "runner.handler_parallel_reviewer._gate_subprocess_args",
                return_value=["codex", "exec", "--yolo", "unused-prompt"],
            ), patch(
                "runner.review_controller.run_controller_review",
                side_effect=_canonical_mutating_transport,
            ):
                result = _parallel_reviewer(node, ctx)

            lane_dir = Path(ctx.state["_df_controller_review_lane_dirs"]["primary"])
            self.assertEqual(result.outcome, "error", result)
            self.assertEqual(result.metadata["review_contract_status"], "invalid")
            self.assertEqual(result.metadata["verdict"], "unknown")
            self.assertIn("reviewed workspace is not clean", result.output)
            self.assertFalse((lane_dir / "controller-receipt.json").exists())
            self.assertFalse((lane_dir / "findings.json").exists())

    def test_controller_contract_suppresses_ambient_shadows(self) -> None:
        """Controller-owned review is exactly one lane despite ambient flags."""
        from unittest.mock import patch

        from runner.handler_core import Context, Result
        from runner.handler_parallel_reviewer import _parallel_reviewer
        from runner.parser import Node

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
                goal="run exactly one controller lane",
                workdir=repo,
                backend="codex",
                run_id=f"graph-no-shadows-{tmp.name}",
            )
            ctx.state["_df_shadow_backends"] = "codex,minimax"
            ctx.state["_df_shadow_codex_review"] = "true"

            with patch(
                "runner.handler_parallel_reviewer._resolve_gate_backend",
                return_value=("codex", {"reviewer_backend_resolution": "test"}),
            ), patch(
                "runner.handler_parallel_reviewer._run_controller_primary",
                return_value=Result(
                    outcome="success",
                    output="controller pass",
                    metadata={"verdict": "pass", "fallback_used": "false"},
                ),
            ), patch(
                "runner.handler_parallel_reviewer._launch_shadow_gate_review",
                side_effect=AssertionError("controller must suppress shadow backends"),
            ), patch(
                "runner.handler_parallel_reviewer._start_shadow_gate_review",
                side_effect=AssertionError("controller must suppress legacy shadow"),
            ):
                result = _parallel_reviewer(node, ctx)

            self.assertEqual(result.outcome, "success", result)
            self.assertNotIn("parallel_reviewer_shadow_backends", result.metadata)
            self.assertEqual(ctx.state["_df_shadow_backends"], "codex,minimax")
            self.assertEqual(ctx.state["_df_shadow_codex_review"], "true")

    def test_every_controller_primary_result_declares_no_fallback(self) -> None:
        """Early, error, invalid, PASS, and FAIL controller results are explicit."""
        from types import SimpleNamespace
        from unittest.mock import patch

        from runner.handler_core import Context
        from runner.handler_parallel_reviewer import _run_controller_primary
        from runner.review_controller import ControllerTransportError

        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            repo = _init_clean_repo(tmp)
            request = _build_request(repo)
            ctx = Context(goal="controller branches", workdir=repo, backend="codex")
            ctx.state["_df_controller_review_cwd"] = str(tmp / "neutral")
            ctx.state["_df_controller_review_lane_dirs"] = {
                "primary": str(tmp / "primary")
            }

            results = [
                _run_controller_primary(request, 1200, ctx, "cold_reviewer", "agy")
            ]
            with patch(
                "runner.handler_parallel_reviewer._gate_subprocess_args",
                return_value=None,
            ):
                results.append(
                    _run_controller_primary(
                        request, 1200, ctx, "cold_reviewer", "codex"
                    )
                )
            with patch(
                "runner.handler_parallel_reviewer._gate_subprocess_args",
                return_value=["codex", "exec", "prompt"],
            ), patch(
                "runner.handler_parallel_reviewer._controller_codex_args",
                side_effect=ValueError("bad controller argv"),
            ):
                results.append(
                    _run_controller_primary(
                        request, 1200, ctx, "cold_reviewer", "codex"
                    )
                )

            branch_results = (
                ControllerTransportError("transport timed out", timed_out=True),
                ControllerTransportError("transport failed"),
                ReviewContractError("invalid review"),
                SimpleNamespace(
                    review=SimpleNamespace(verdict="pass", response_sha256="a" * 64),
                    response_text="pass",
                    output_paths={
                        "receipt": "receipt",
                        "response": "response",
                        "findings": "findings",
                        "transport": "transport",
                    },
                ),
                SimpleNamespace(
                    review=SimpleNamespace(verdict="fail", response_sha256="b" * 64),
                    response_text="fail",
                    output_paths={
                        "receipt": "receipt",
                        "response": "response",
                        "findings": "findings",
                        "transport": "transport",
                    },
                ),
            )
            with patch(
                "runner.handler_parallel_reviewer._gate_subprocess_args",
                return_value=["codex", "exec", "prompt"],
            ), patch(
                "runner.handler_parallel_reviewer._controller_codex_args",
                return_value=["codex", "exec", "-"],
            ), patch(
                "runner.handler_parallel_reviewer._verify_controller_workspace"
            ):
                for controller_result in branch_results:
                    with patch(
                        "runner.review_controller.run_controller_review",
                        side_effect=(
                            controller_result
                            if isinstance(controller_result, BaseException)
                            else None
                        ),
                        return_value=(
                            None
                            if isinstance(controller_result, BaseException)
                            else controller_result
                        ),
                    ):
                        results.append(
                            _run_controller_primary(
                                request, 1200, ctx, "cold_reviewer", "codex"
                            )
                        )

            self.assertEqual(
                [result.outcome for result in results],
                ["error", "error", "error", "error", "error", "error", "success", "failure"],
            )
            for result in results:
                self.assertEqual(result.metadata.get("fallback_used"), "false", result)

    def test_controller_contract_errors_are_terminal_errors(self) -> None:
        """Invalid controller output never masquerades as reviewer-authored FAIL."""
        from unittest.mock import patch

        from runner.handler_core import Context
        from runner.handler_parallel_reviewer import _run_controller_primary

        contract_gaps = (
            "review response must be non-empty text",
            "controller refuses PASS verdict under stub-mode env vars",
            "command receipt has invalid output digest",
            "reviewed workspace tree changed",
        )
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            repo = _init_clean_repo(tmp)
            request = _build_request(repo)
            ctx = Context(goal="controller errors", workdir=repo, backend="codex")
            ctx.state["_df_controller_review_cwd"] = str(tmp / "neutral")
            ctx.state["_df_controller_review_lane_dirs"] = {
                "primary": str(tmp / "primary")
            }
            for gap in contract_gaps:
                with self.subTest(gap=gap), patch(
                    "runner.handler_parallel_reviewer._gate_subprocess_args",
                    return_value=["codex", "exec", "prompt"],
                ), patch(
                    "runner.handler_parallel_reviewer._controller_codex_args",
                    return_value=["codex", "exec", "-"],
                ), patch(
                    "runner.review_controller.run_controller_review",
                    side_effect=ReviewContractError(gap),
                ):
                    result = _run_controller_primary(
                        request, 1200, ctx, "cold_reviewer", "codex"
                    )

                self.assertEqual(result.outcome, "error", result)
                self.assertEqual(result.metadata["review_contract_status"], "invalid")
                self.assertEqual(result.metadata["verdict"], "unknown")
                self.assertEqual(result.metadata["fallback_used"], "false")

    def test_controller_early_results_declare_no_fallback(self) -> None:
        """Echo fixture, unknown contract, and build errors are explicit."""
        from unittest.mock import patch

        from runner.handler_core import Context
        from runner.handler_parallel_reviewer import _parallel_reviewer
        from runner.parser import Node

        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            repo = _init_clean_repo(tmp)
            echo_ctx = Context(goal="echo", workdir=repo, backend="echo")
            echo_ctx.state["cold_reviewer.outcome"] = "success"
            with patch(
                "runner.handler_parallel_reviewer._controller_review_request",
                side_effect=AssertionError("echo must not build a controller request"),
            ), patch(
                "runner.handler_parallel_reviewer._run_controller_primary",
                side_effect=AssertionError("echo must not launch controller transport"),
            ):
                echo_result = _parallel_reviewer(
                    Node(
                        name="cold_reviewer",
                        attrs={"review_contract": "cold-review-v1"},
                    ),
                    echo_ctx,
                )

            self.assertEqual(echo_result.outcome, "success", echo_result)
            self.assertEqual(echo_result.metadata["parallel_reviewer"], "echo")

            codex_ctx = Context(goal="controller", workdir=repo, backend="codex")
            unknown_result = _parallel_reviewer(
                Node(
                    name="cold_reviewer",
                    attrs={"review_contract": "unknown-contract"},
                ),
                codex_ctx,
            )
            with patch(
                "runner.handler_parallel_reviewer._controller_review_request",
                side_effect=ValueError("cannot build"),
            ):
                build_result = _parallel_reviewer(
                    Node(
                        name="cold_reviewer",
                        attrs={"review_contract": "cold-review-v1"},
                    ),
                    codex_ctx,
                )

            for result in (echo_result, unknown_result, build_result):
                self.assertEqual(result.metadata.get("fallback_used"), "false", result)


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
