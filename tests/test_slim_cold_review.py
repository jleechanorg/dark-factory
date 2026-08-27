"""Focused contract tests for the compact native cold-review fallback."""

from __future__ import annotations

import hashlib
import json
import subprocess
from dataclasses import replace
from pathlib import Path

import pytest
from test_review_controller import _inputs

from runner.review_controller import (
    CHECK_IDS,
    EvidenceArtifact,
    ExecutionReceipt,
    ReviewContractError,
    create_review_request,
    run_controller_review,
    validate_execution_receipts,
    validate_review_response,
)


def _compact_response(*, verdict: str = "pass", **overrides: object) -> str:
    payload: dict[str, object] = {
        "verdict": verdict,
        "findings": [],
        "evidence_checked": ["bound source tree and focused tests"],
        "commands_executed": ["python -m pytest -q"],
        "caveats": [],
    }
    payload.update(overrides)
    return json.dumps(payload, separators=(",", ":"))


def test_compact_contract_has_no_checklist_or_controller_hash_echoes() -> None:
    request = create_review_request(_inputs())
    assert CHECK_IDS == ()
    assert len(request.prompt_payload) < 5000
    assert "C0" not in request.prompt
    assert "E14" not in request.prompt
    assert "PROMPT_SHA256:" not in request.prompt
    assert "ENVELOPE_SHA256:" not in request.prompt
    assert "HEAD_SHA:" not in request.prompt
    assert "TASK_SHA256:" not in request.prompt
    assert "CHANGED_FILES_SHA256:" not in request.prompt
    assert "EVIDENCE_MANIFEST_SHA256:" not in request.prompt
    for phrase in (
        "untrusted",
        "callers",
        "security",
        "state",
        "boundary",
        "evidence",
        "read-only",
        "uncertainty",
        "continue",
    ):
        assert phrase in request.prompt_payload.lower()
    assert "base64-decode" in request.prompt_payload.lower()
    assert "utf-8" in request.prompt_payload.lower()


def test_compact_response_is_strict_json_with_empty_checks_compatibility() -> None:
    request = create_review_request(_inputs())
    result = validate_review_response(_compact_response(), request)
    assert result.verdict == "pass"
    assert result.checks == ()
    assert len(result.response_sha256) == 64


def test_pass_requires_meaningful_evidence_and_commands() -> None:
    request = create_review_request(_inputs())
    response = _compact_response(evidence_checked=[], commands_executed=[])
    with pytest.raises(ReviewContractError, match="pass requires"):
        validate_review_response(response, request)


@pytest.mark.parametrize(
    "field",
    ("findings", "caveats"),
)
def test_pass_rejects_semantic_content_in_findings_or_caveats(field: str) -> None:
    request = create_review_request(_inputs())
    with pytest.raises(ReviewContractError, match=f"pass requires empty {field}"):
        validate_review_response(
            _compact_response(**{field: ["BLOCKER: unresolved defect"]}), request
        )


@pytest.mark.parametrize(
    "field",
    ("findings", "evidence_checked", "commands_executed", "caveats"),
)
def test_response_arrays_require_nonempty_strings(field: str) -> None:
    request = create_review_request(_inputs())
    with pytest.raises(ReviewContractError, match="array entries"):
        validate_review_response(_compact_response(**{field: [""]}), request)
    with pytest.raises(ReviewContractError, match="array entries"):
        validate_review_response(_compact_response(**{field: [None]}), request)


def test_pass_requires_nonempty_controller_evidence_manifest() -> None:
    request = create_review_request(replace(_inputs(), evidence=()))
    with pytest.raises(ReviewContractError, match="evidence manifest"):
        validate_review_response(_compact_response(), request)


def test_pass_requires_a_captured_successful_command_receipt() -> None:
    request = create_review_request(_inputs())
    validated = validate_review_response(_compact_response(), request)
    with pytest.raises(ReviewContractError, match="successful command receipt"):
        validate_execution_receipts((), validated)


def test_pass_uses_authoritative_receipts_with_human_command_summary() -> None:
    request = create_review_request(_inputs())
    validated = validate_review_response(
        _compact_response(
            commands_executed=["probe command", "focused verification command"],
        ),
        request,
    )
    receipts = (
        ExecutionReceipt(
            command="probe command",
            exit_code=1,
            output_sha256="1" * 64,
        ),
        ExecutionReceipt(
            command="focused verification command",
            exit_code=0,
            output_sha256="2" * 64,
        ),
    )
    validate_execution_receipts(receipts, validated)


def test_pass_rejects_successful_receipt_with_empty_output_digest() -> None:
    request = create_review_request(_inputs())
    validated = validate_review_response(_compact_response(), request)
    receipts = (
        ExecutionReceipt(
            command="python -m pytest -q",
            exit_code=0,
            output_sha256="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
    )
    with pytest.raises(ReviewContractError, match="non-empty output"):
        validate_execution_receipts(receipts, validated)


def test_controller_acceptance_rejects_pass_without_captured_receipt(monkeypatch) -> None:
    from runner.handler_core import Context, Result
    from runner.handler_parallel_reviewer import _contract_adjusted_result

    request = create_review_request(_inputs())
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._verify_controller_workspace",
        lambda ctx, request: None,
    )
    result = _contract_adjusted_result(
        Result(outcome="success", output=_compact_response()),
        request,
        Context(goal="", workdir=Path(".")),
        lane="primary",
    )
    assert result.outcome == "failure"
    assert "successful command receipt" in result.metadata["review_contract_gap"]



@pytest.mark.parametrize(
    ("backend", "expected_outcome"),
    (("codex", "failure"), ("echo", "failure"), ("mock_llm", "failure")),
)
def test_graph_controller_stub_pass_is_rejected_for_every_backend(
    monkeypatch, backend, expected_outcome
) -> None:
    from runner.handler_core import Context, Result
    from runner.handler_parallel_reviewer import _contract_adjusted_result

    request = create_review_request(_inputs())
    monkeypatch.setenv("DARK_FACTORY_ITERATION_STUB", "1")
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._verify_controller_workspace",
        lambda ctx, request: None,
    )
    result = _contract_adjusted_result(
        Result(
            outcome="success",
            output=_compact_response(),
            metadata={
                "_controller_command_receipts": [
                    {
                        "command": "python -m pytest -q",
                        "exit_code": 0,
                        "output_sha256": "0" * 64,
                    }
                ]
            },
        ),
        request,
        Context(goal="", workdir=Path("."), backend=backend),
        lane="primary",
        backend=backend,
    )

    assert result.outcome == expected_outcome
    assert "stub-mode" in result.metadata["review_contract_gap"]


@pytest.mark.parametrize("backend", ("echo", "mock_llm"))
def test_parallel_reviewer_stub_backends_do_not_bypass_cold_review_contract(
    tmp_path, monkeypatch, backend
) -> None:
    from runner.handler_core import Context, Result
    from runner.handler_parallel_reviewer import _parallel_reviewer
    from runner.parser import Node

    request = create_review_request(_inputs())
    node = Node(
        name="cold_reviewer",
        attrs={"type": "parallel_reviewer", "review_contract": "cold-review-v1"},
    )
    ctx = Context(
        goal="review",
        workdir=tmp_path,
        backend=backend,
        state={"cold_reviewer.outcome": "success"},
    )
    monkeypatch.setattr("runner.handlers._target_worktree", lambda _: tmp_path)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda _: request.head_sha)
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._controller_review_request",
        lambda node, ctx, expected_sha: request,
    )
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._run_primary_review",
        lambda *args, **kwargs: Result(
            outcome="success",
            output=_compact_response(),
            metadata={
                "_controller_command_receipts": [
                    {
                        "command": "python -m pytest -q",
                        "exit_code": 0,
                        "output_sha256": "1" * 64,
                    }
                ]
            },
        ),
    )
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._record_primary_output",
        lambda node, attempt, result, seq, ctx: result,
    )
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._verify_controller_workspace",
        lambda ctx, request: None,
    )

    result = _parallel_reviewer(node, ctx)

    assert result.outcome == "success"
    assert result.metadata["review_contract_status"] == "valid"
    assert result.metadata.get("parallel_reviewer") != "echo"


def test_parallel_reviewer_fixture_requires_explicit_marker_and_state(tmp_path) -> None:
    from runner.handler_core import Context
    from runner.handler_parallel_reviewer import (
        _controller_fixture_enabled,
        _parallel_reviewer,
    )
    from runner.parser import Node

    node = Node(
        name="cold_reviewer",
        attrs={
            "type": "parallel_reviewer",
            "review_contract": "cold-review-v1",
            "test_fixture": "true",
        },
    )
    assert not _controller_fixture_enabled(
        node,
        Context(goal="", workdir=tmp_path, backend="echo"),
    )
    fixture_ctx = Context(
        goal="",
        workdir=tmp_path,
        backend="echo",
        state={
            "_df_controller_fixture": "cold-review-v1",
            "cold_reviewer.outcome": "success",
        },
    )
    assert _controller_fixture_enabled(node, fixture_ctx)
    result = _parallel_reviewer(node, fixture_ctx)
    assert result.outcome == "success"
    assert result.metadata["review_contract_status"] == "fixture"


def test_clean_worker_uses_a_detached_controller_snapshot(tmp_path, monkeypatch):
    from test_review_cli import _repo

    from runner.handler_parallel_reviewer import _controller_snapshot

    repo, _, head = _repo(tmp_path)
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer.pathlib.Path.home",
        lambda: tmp_path,
    )
    snapshot, snapshot_head, origin = _controller_snapshot(repo, head, ())

    try:
        assert snapshot != repo
        assert snapshot_head == head
        assert origin is None
        assert subprocess.run(
            ["git", "-C", str(snapshot), "symbolic-ref", "-q", "HEAD"],
            check=False,
        ).returncode == 1
        assert subprocess.run(
            ["git", "-C", str(snapshot), "status", "--porcelain=v1"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout == ""
    finally:
        subprocess.run(
            ["git", "-C", str(repo), "worktree", "remove", "--force", str(snapshot)],
            check=True,
        )


def test_controller_acceptance_binds_detached_target_and_exit_repins(
    tmp_path,
) -> None:
    from test_review_cli import _repo

    from runner.handler_core import Context, Result, _exit
    from runner.handler_parallel_reviewer import _contract_adjusted_result
    from runner.parser import Node

    repo, base, head = _repo(tmp_path)
    snapshot = tmp_path / "detached-review-snapshot"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "--detach", str(snapshot), head],
        check=True,
        capture_output=True,
        text=True,
    )
    tree = subprocess.check_output(
        ["git", "-C", str(snapshot), "rev-parse", "HEAD^{tree}"],
        text=True,
    ).strip()
    request = create_review_request(
        replace(
            _inputs(),
            workspace_path=str(snapshot),
            base_sha=base,
            head_sha=head,
            tree_sha=tree,
            evidence=(
                EvidenceArtifact(
                    path="value.txt",
                    size_bytes=6,
                    sha256=hashlib.sha256(b"after\n").hexdigest(),
                ),
            ),
        )
    )
    result = _contract_adjusted_result(
        Result(
            outcome="success",
            output=_compact_response(),
            metadata={
                "_controller_command_receipts": [
                    {
                        "command": "python -m pytest -q",
                        "exit_code": 0,
                        "output_sha256": "0" * 64,
                    }
                ]
            },
        ),
        request,
        Context(goal="", workdir=repo),
        lane="primary",
        backend="codex",
    )

    assert result.outcome == "success", result.metadata
    binding = json.loads(result.context_updates["_verified_review_target"])
    assert binding == {
        "workspace_path": str(snapshot),
        "head_sha": head,
        "tree_sha": tree,
    }
    ctx = Context(goal="", workdir=repo, state=dict(result.context_updates))
    assert _exit(Node(name="exit", attrs={}), ctx).outcome == "success"

    subprocess.run(
        ["git", "-C", str(snapshot), "checkout", "--quiet", "--detach", base],
        check=True,
    )
    changed = _exit(Node(name="exit", attrs={}), ctx)
    assert changed.outcome == "error"
    assert changed.metadata["exit_sha_status"] == "mismatched"


@pytest.mark.parametrize(
    "binding",
    (
        "not-json",
        "[]",
        json.dumps({"workspace_path": "/tmp", "head_sha": "a" * 40}),
        json.dumps(
            {"workspace_path": 7, "head_sha": "a" * 40, "tree_sha": "b" * 40}
        ),
    ),
)
def test_contract_exit_fails_closed_on_malformed_binding(binding) -> None:
    from runner.handler_core import Context, _exit
    from runner.parser import Node

    result = _exit(
        Node(name="exit", attrs={}),
        Context(
            goal="",
            workdir=Path("."),
            state={
                "_verified_review_target_required": "true",
                "_verified_review_target": binding,
            },
        ),
    )

    assert result.outcome == "error"
    assert result.metadata["exit_sha_status"] == "invalid"


def test_contract_exit_requires_binding_when_controller_marked_it_required() -> None:
    from runner.handler_core import Context, _exit
    from runner.parser import Node

    result = _exit(
        Node(name="exit", attrs={}),
        Context(
            goal="",
            workdir=Path("."),
            state={"_verified_review_target_required": "true"},
        ),
    )

    assert result.outcome == "error"
    assert "binding is missing" in result.output


def test_contract_exit_rejects_branch_target(tmp_path) -> None:
    from test_review_cli import _repo

    from runner.handler_core import Context, _exit
    from runner.parser import Node

    repo, _, head = _repo(tmp_path)
    tree = subprocess.check_output(
        ["git", "-C", str(repo), "rev-parse", "HEAD^{tree}"],
        text=True,
    ).strip()
    binding = json.dumps(
        {"workspace_path": str(repo), "head_sha": head, "tree_sha": tree},
        sort_keys=True,
        separators=(",", ":"),
    )
    result = _exit(
        Node(name="exit", attrs={}),
        Context(
            goal="",
            workdir=repo,
            state={
                "_verified_review_target_required": "true",
                "_verified_review_target": binding,
            },
        ),
    )

    assert result.outcome == "error"
    assert "detached snapshot" in result.output


def test_fail_response_remains_valid_without_evidence_or_receipts() -> None:
    request = create_review_request(_inputs())
    validated = validate_review_response(
        _compact_response(
            verdict="fail",
            findings=["material uncertainty"],
            evidence_checked=[],
            commands_executed=[],
        ),
        request,
    )
    validate_execution_receipts((), validated)


def test_two_node_controller_requires_receipt() -> None:
    from runner.parser import parse

    graph = parse(Path(__file__).parents[1] / "pipelines/slim/two_node.dot")
    assert graph.nodes["cold_reviewer"].attrs.get("receipt_required") == "true"


@pytest.mark.parametrize(
    "terminal_case", ("success", "review_failure", "exit_repin_failure", "exhausted")
)
def test_engine_finalization_removes_controller_snapshot(
    tmp_path, monkeypatch, terminal_case
) -> None:
    from test_review_cli import _repo

    from runner import engine_run

    run = engine_run.run
    from runner.handler_core import Context, Result
    from runner.handler_parallel_reviewer import _controller_snapshot
    from runner.parser import Edge, Graph, Node

    repo, base, head = _repo(tmp_path)
    monkeypatch.setattr("pathlib.Path.home", lambda: tmp_path)
    snapshots = [
        _controller_snapshot(repo, head, ())[0],
        _controller_snapshot(repo, head, ())[0],
    ]
    snapshot = snapshots[-1]
    tree = subprocess.check_output(
        ["git", "-C", str(snapshot), "rev-parse", "HEAD^{tree}"],
        text=True,
    ).strip()
    binding = json.dumps(
        {"workspace_path": str(snapshot), "head_sha": head, "tree_sha": tree},
        sort_keys=True,
        separators=(",", ":"),
    )
    state = {
        "_controller_review_snapshots": json.dumps(
            [
                {"snapshot_path": str(path), "source_worktree": str(repo)}
                for path in snapshots
            ],
            sort_keys=True,
            separators=(",", ":"),
        ),
        "_verified_review_target_required": "true",
        "_verified_review_target": binding,
    }
    nodes = {
        "start": Node(name="start", attrs={"type": "start"}),
        "exit": Node(name="exit", attrs={"type": "exit"}),
    }
    edges = [Edge(src="start", dst="exit", attrs={})]
    if terminal_case == "review_failure":
        nodes["review"] = Node(name="review", attrs={"type": "codergen"})
        edges = [
            Edge(src="start", dst="review", attrs={}),
            Edge(src="review", dst="exit", attrs={"condition": "outcome=failure"}),
        ]
        real_resolve = engine_run.resolve

        def resolve(node):
            if node.name == "review":
                return lambda _node, _ctx: Result(
                    outcome="failure", output="review failure"
                )
            return real_resolve(node)

        monkeypatch.setattr("runner.engine_run.resolve", resolve)
    elif terminal_case == "exit_repin_failure":
        subprocess.run(
            ["git", "-C", str(snapshot), "checkout", "--quiet", "--detach", base],
            check=True,
        )
    elif terminal_case == "exhausted":
        edges = [Edge(src="start", dst="start", attrs={})]

    graph = Graph(name="snapshot-cleanup", goal="", nodes=nodes, edges=edges)
    history = run(
        graph,
        Context(goal="", workdir=repo, state=state),
        max_steps=1 if terminal_case == "exhausted" else 10,
    )

    assert history
    assert all(not path.exists() for path in snapshots)
    worktrees = subprocess.check_output(
        ["git", "-C", str(repo), "worktree", "list", "--porcelain"],
        text=True,
    )
    assert all(str(path) not in worktrees for path in snapshots)


def test_engine_snapshot_cleanup_skips_malformed_entries_and_deduplicates(
    tmp_path, monkeypatch
) -> None:
    from test_review_cli import _repo

    from runner.engine_run import _cleanup_controller_snapshot
    from runner.handler_core import Context
    from runner.handler_parallel_reviewer import _controller_snapshot

    repo, _, head = _repo(tmp_path)
    monkeypatch.setattr("pathlib.Path.home", lambda: tmp_path)
    snapshot = _controller_snapshot(repo, head, ())[0]
    state = {
        "_controller_review_snapshots": json.dumps(
            [
                {"snapshot_path": str(snapshot), "source_worktree": str(repo)},
                {"snapshot_path": str(snapshot), "source_worktree": str(repo)},
                {"snapshot_path": str(tmp_path / "outside"), "source_worktree": str(repo)},
                {"snapshot_path": str(snapshot)},
            ],
            separators=(",", ":"),
        )
    }

    _cleanup_controller_snapshot(Context(goal="", workdir=repo, state=state))

    assert not snapshot.exists()
    worktrees = subprocess.check_output(
        ["git", "-C", str(repo), "worktree", "list", "--porcelain"],
        text=True,
    )
    assert str(snapshot) not in worktrees


@pytest.mark.parametrize(
    "payload",
    (
        {"verdict": "pass", "findings": [], "evidence_checked": [], "commands_executed": []},
        {
            "verdict": "pass",
            "findings": [],
            "evidence_checked": [],
            "commands_executed": [],
            "caveats": [],
            "extra": "reject",
        },
        {
            "verdict": "PASS",
            "findings": [],
            "evidence_checked": [],
            "commands_executed": [],
            "caveats": [],
        },
        {
            "verdict": "pass",
            "findings": "not a list",
            "evidence_checked": [],
            "commands_executed": [],
            "caveats": [],
        },
        '{"verdict":"pass","verdict":"fail","findings":[],"evidence_checked":[],"commands_executed":[],"caveats":[]}',
    ),
)
def test_compact_response_rejects_non_exact_schema(payload: dict[str, object] | str) -> None:
    request = create_review_request(_inputs())
    with pytest.raises(ReviewContractError):
        validate_review_response(json.dumps(payload), request)


def test_nonzero_reviewer_exit_is_rejected_before_transport_parsing(monkeypatch, tmp_path) -> None:
    request = create_review_request(_inputs())

    class _FailedProcess:
        returncode = 23
        stdout = "not JSONL at all"
        stderr = "review backend exploded"

    calls: list[object] = []

    def fake_run(*args, **kwargs):
        calls.append((args, kwargs))
        return _FailedProcess()

    monkeypatch.setattr("runner.review_controller.subprocess.run", fake_run)
    with pytest.raises(ReviewContractError, match="exited with 23"):
        run_controller_review(
            request,
            neutral_cwd=tmp_path,
            output_dir=tmp_path / "review-output",
            transport_argv=("fake-reviewer",),
            timeout=10,
        )
    assert calls
