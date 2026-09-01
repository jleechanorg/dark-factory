"""Resume must not fail-open on runner-minted review-target state.

The factory two-node redesign (docs/superpowers/specs/2026-09-01-factory-two-
node-redesign-design.md, D2/D3/D8a) mints `ctx.state["target"]`,
`ctx.state["intent"]`, `ctx.state["_target_pin_chain"]`, and
`ctx.state["_target_base_sha"]` on every successful worker visit
(handler_codergen._mint_post_worker_target). Those keys previously lived only
in `ctx.state` (memory), never in the checkpointed `StepRecord.metadata`, so a
process restart between a worker's success visit and the next reviewer visit
silently dropped them: the next mint would re-anchor `_target_base_sha` from
the CURRENT HEAD instead of the frozen pre-worker base, breaking pin-chain
continuity (D8a) and losing the task intent (D2) exactly when the whole point
of this contract is fail-closed integrity.

File-disjoint: new test file. Reuses the real `pipelines/slim/two_node.dot`
graph (worker + cold_reviewer, class="worker"/"review", verdict_gate) plus a
real git workdir so the worker visits mint through the REAL
`handler_codergen._mint_post_worker_target` path; the reviewer node's
`codergen` type is monkeypatched per-visit (same technique as
tests/test_two_node_dot.py) so it never shells out to a real Codex CLI.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.parser import parse  # noqa: E402
from runner.handler_core import Context, Result  # noqa: E402
from runner.handlers import TYPE_REGISTRY  # noqa: E402
from runner.engine_run import _run_single_node, _TARGET_MINT_STATE_KEYS  # noqa: E402
from runner import engine_persist  # noqa: E402

_PIPELINE = "pipelines/slim/two_node.dot"


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


def test_worker_success_record_metadata_carries_minted_target_state(git_repo) -> None:
    """A worker success visit's Result.metadata must carry the minted
    target/intent/pin-chain/base-sha so it survives checkpoint serialization."""
    graph = parse(ROOT / _PIPELINE)
    worker = graph.nodes["worker"]
    ctx = Context(
        goal="implement the feature",
        workdir=git_repo,
        backend="echo",
        state={"_df_mint_review_target": "true"},
    )
    (git_repo / "change.txt").write_text("edit\n")

    results, records = _run_single_node(worker, ctx, graph)

    assert results[0].outcome == "success"
    assert ctx.state["target"].startswith("git-range://")
    record = records[0]
    for key in _TARGET_MINT_STATE_KEYS:
        assert record.metadata[key] == ctx.state[key]


def _make_reviewer_dispatcher(worker_marker_prefix: str, verdicts: list[str]):
    """Route `codergen` type: worker visits run the REAL echo-backend handler
    (so target minting is genuine); cold_reviewer visits are faked to return
    the next scripted verdict without shelling out to a real reviewer CLI."""
    real_codergen = TYPE_REGISTRY["codergen"]
    review_visits = {"n": 0}

    def dispatcher(node, ctx):
        if node.name == "worker":
            marker = pathlib.Path(str(ctx.workdir)) / f"{worker_marker_prefix}_{review_visits['n']}.txt"
            marker.write_text("worker edit\n")
            return real_codergen(node, ctx)
        review_visits["n"] += 1
        verdict = verdicts[review_visits["n"] - 1]
        if verdict == "FAIL":
            return Result(
                outcome="failure",
                output="Blocking: fix the thing.\nVerdict: FAIL\n",
            )
        return Result(outcome="success", output="Verdict: PASS\n")

    return dispatcher


def test_checkpoint_resume_continues_pin_chain_after_worker_success(
    git_repo, tmp_path, monkeypatch
) -> None:
    """Restart between a worker's fix-visit success and the reviewer's next
    visit must keep the pin chain anchored at the ORIGINAL base — not
    re-anchor from whatever HEAD happens to be current when the run resumes."""
    dispatcher = _make_reviewer_dispatcher("fix", ["FAIL", "FAIL", "PASS"])
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", dispatcher)

    graph = parse(ROOT / _PIPELINE)
    checkpoint = tmp_path / "checkpoint.json"
    append_record = engine_persist._append_record
    worker_success_count = {"n": 0}

    def interrupt_after_second_worker_success(*args, **kwargs):
        seq = append_record(*args, **kwargs)
        record = args[5]
        if record.node == "worker" and record.outcome == "success":
            worker_success_count["n"] += 1
            if worker_success_count["n"] == 2:
                raise KeyboardInterrupt("simulated restart before 2nd reviewer visit")
        return seq

    monkeypatch.setattr(engine_persist, "_append_record", interrupt_after_second_worker_success)

    from runner.engine import run

    ctx1 = Context(
        goal="fix the thing",
        workdir=git_repo,
        backend="echo",
        state={"_df_mint_review_target": "true"},
    )
    with pytest.raises(KeyboardInterrupt, match="simulated restart"):
        run(graph, ctx1, checkpoint=checkpoint)

    checkpoint_records = json.loads(checkpoint.read_text(encoding="utf-8"))
    worker_records = [r for r in checkpoint_records if r["node"] == "worker"]
    assert len(worker_records) == 2
    original_base_sha = worker_records[0]["metadata"]["_target_base_sha"]
    # The second worker visit's re-mint must already show the SAME frozen
    # base (D8a) — proving the bug isn't merely "resume forgets it", but
    # that the in-process chain itself stays anchored before any restart.
    assert worker_records[1]["metadata"]["_target_base_sha"] == original_base_sha
    assert len(json.loads(worker_records[1]["metadata"]["_target_pin_chain"])) == 2

    monkeypatch.setattr(engine_persist, "_append_record", append_record)

    # A brand-new Context simulates an actual process restart: no leftover
    # in-memory ctx.state, only what the checkpoint + fresh CLI init provide.
    ctx2 = Context(
        goal="fix the thing",
        workdir=git_repo,
        backend="echo",
        state={"_df_mint_review_target": "true"},
    )
    history = run(graph, ctx2, checkpoint=checkpoint, resume=checkpoint)

    assert history[-1].outcome == "success"
    final_worker_records = [
        r for r in json.loads(checkpoint.read_text(encoding="utf-8")) if r["node"] == "worker"
    ]
    assert len(final_worker_records) == 3
    # The third worker visit's re-mint (post-resume) must still show the
    # ORIGINAL base — not a fresh base re-anchored from the resumed HEAD.
    assert final_worker_records[2]["metadata"]["_target_base_sha"] == original_base_sha
    assert len(json.loads(final_worker_records[2]["metadata"]["_target_pin_chain"])) == 3


def test_resume_into_review_visit_fails_closed_when_target_state_missing(
    git_repo, tmp_path, monkeypatch
) -> None:
    """Reviewer-feedback metadata present but target-mint metadata missing
    on the most recent worker-success record must raise the fail-closed
    ValueError, mirroring the existing `_review_feedback` resume contract."""
    dispatcher = _make_reviewer_dispatcher("fix", ["FAIL", "PASS"])
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", dispatcher)

    graph = parse(ROOT / _PIPELINE)
    checkpoint = tmp_path / "checkpoint.json"
    append_record = engine_persist._append_record
    worker_success_count = {"n": 0}

    def interrupt_after_second_worker_success(*args, **kwargs):
        seq = append_record(*args, **kwargs)
        record = args[5]
        if record.node == "worker" and record.outcome == "success":
            worker_success_count["n"] += 1
            if worker_success_count["n"] == 2:
                raise KeyboardInterrupt("simulated restart")
        return seq

    monkeypatch.setattr(engine_persist, "_append_record", interrupt_after_second_worker_success)

    from runner.engine import run

    ctx1 = Context(
        goal="fix the thing",
        workdir=git_repo,
        backend="echo",
        state={"_df_mint_review_target": "true"},
    )
    with pytest.raises(KeyboardInterrupt, match="simulated restart"):
        run(graph, ctx1, checkpoint=checkpoint)

    monkeypatch.setattr(engine_persist, "_append_record", append_record)

    # Simulate a checkpoint that has full reviewer feedback but is missing
    # the target-mint state on the worker's success record (the exact gap
    # this fix closes).
    payload = json.loads(checkpoint.read_text(encoding="utf-8"))
    stripped_any = False
    for record in payload:
        if record["node"] == "worker" and record["outcome"] == "success":
            for key in ("target", "intent", "_target_pin_chain", "_target_base_sha"):
                record["metadata"].pop(key, None)
            stripped_any = True
    assert stripped_any
    checkpoint.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    ctx2 = Context(
        goal="fix the thing",
        workdir=git_repo,
        backend="echo",
        state={"_df_mint_review_target": "true"},
    )
    with pytest.raises(ValueError, match="review-target mint state"):
        run(graph, ctx2, checkpoint=checkpoint, resume=checkpoint)


def test_resume_into_review_visit_without_mint_opt_in_is_unaffected(
    tmp_path, monkeypatch
) -> None:
    """Graphs that never opted into the mint contract (`_df_mint_review_target`
    unset) must resume exactly as before — no new requirement, no raise."""
    from runner.engine import run
    from runner.parser import parse as _parse

    review_visits = {"n": 0}

    def fake_codergen(node, ctx):
        if node.name == "worker":
            return Result(outcome="success", output="worker done")
        review_visits["n"] += 1
        if review_visits["n"] == 1:
            return Result(outcome="failure", output="Blocking.\nVerdict: FAIL\n")
        return Result(outcome="success", output="Verdict: PASS\n")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)

    graph = _parse(ROOT / _PIPELINE)
    checkpoint = tmp_path / "checkpoint.json"
    append_record = engine_persist._append_record
    worker_success_count = {"n": 0}

    def interrupt_after_second_worker_success(*args, **kwargs):
        seq = append_record(*args, **kwargs)
        record = args[5]
        if record.node == "worker" and record.outcome == "success":
            worker_success_count["n"] += 1
            if worker_success_count["n"] == 2:
                raise KeyboardInterrupt("simulated restart")
        return seq

    monkeypatch.setattr(engine_persist, "_append_record", interrupt_after_second_worker_success)

    ctx1 = Context(goal="fix the thing", workdir=ROOT, backend="echo")
    with pytest.raises(KeyboardInterrupt, match="simulated restart"):
        run(graph, ctx1, checkpoint=checkpoint)

    monkeypatch.setattr(engine_persist, "_append_record", append_record)

    ctx2 = Context(goal="fix the thing", workdir=ROOT, backend="echo")
    history = run(graph, ctx2, checkpoint=checkpoint, resume=checkpoint)
    assert history[-1].outcome == "success"
