"""Public cold-review benchmark manifest and paired-run contract tests."""

from __future__ import annotations

import hashlib
import json
import subprocess
import threading
from pathlib import Path

import pytest

from benchmarks.scripts.run_controller_cold_review import (
    BenchmarkError,
    DEFAULT_WORKERS,
    FREEFORM_PROMPTS,
    MANUAL_OBSERVATION_PROMPT,
    MANUAL_OBSERVATION_SOURCE,
    SCREEN_CASE_IDS,
    SCREEN_MODEL,
    SCREEN_REASONING_EFFORT,
    SCREEN_TIMEOUT_SECONDS,
    SCREEN_WORKERS,
    _run_raw_freeform_review,
    SCREEN_VARIANTS,
    build_blinded_plan,
    build_manual_observation_plan,
    build_screen_plan,
    commit_claim_snapshot,
    load_manifest,
    main,
    render_manual_observation_prompt,
    run_benchmark,
    run_screen,
    validate_case,
)
from runner.review_controller import ControllerReviewResult, ValidatedReview
from benchmarks.scripts.check_boundary import (
    check_cold_review_public_surfaces,
    public_files,
)


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "benchmarks/controller-cold-review/cases.json"


EXPECTED_TRACEABILITY_PROMPT = (
    "Review this PR independently. Cross-check its design docs, goals and tenets, "
    "and PR description against the actual code and production paths and the "
    "executed evidence. Find every actionable defect and keep reviewing the whole "
    "change after the first finding. Report each finding as a separate bullet with "
    "an exact `path/to/file:L123` reference and explain which design goal, tenet, "
    "description claim, code behavior, or evidence claim it violates."
)
EXPECTED_ADVERSARIAL_PROMPT = (
    "Try to prove this PR is wrong. Cross-check the design docs, goals and tenets, "
    "PR description, actual production code paths and consumers, and executed "
    "evidence for contradictions, omissions, false-green tests, and unverified "
    "claims. Keep attacking independent failure modes after every finding until "
    "the entire change has been examined. Report each actionable defect as a "
    "separate bullet with an exact `path/to/file:L123` reference and the "
    "contradicted claim or evidence."
)


def _git(repo: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return proc.stdout.rstrip("\n")


def _fixture_case(tmp_path: Path):
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "--initial-branch=main")
    _git(repo, "config", "user.email", "benchmark@example.invalid")
    _git(repo, "config", "user.name", "Benchmark")
    (repo / "value.txt").write_text("before\n", encoding="utf-8")
    _git(repo, "add", "value.txt")
    _git(repo, "commit", "-qm", "base")
    base = _git(repo, "rev-parse", "HEAD")
    (repo / "value.txt").write_text("after\n", encoding="utf-8")
    _git(repo, "commit", "-qam", "head")
    head = _git(repo, "rev-parse", "HEAD")
    tree = _git(repo, "rev-parse", "HEAD^{tree}")
    diff = _git(repo, "diff", "--no-ext-diff", "--binary", f"{base}..{head}")
    changed_json = json.dumps(
        ["value.txt"], ensure_ascii=False, sort_keys=True, separators=(",", ":")
    )
    task_text = f"COMMIT {head}\nSUBJECT: head\nBODY:\n(empty)\n"
    case = {
        "id": "fixture-r1",
        "pr": 1,
        "revision": 1,
        "base_sha": base,
        "head_sha": head,
        "tree_sha": tree,
        "diff_sha256": hashlib.sha256(diff.encode()).hexdigest(),
        "changed_files": ["value.txt"],
        "changed_files_sha256": hashlib.sha256(changed_json.encode()).hexdigest(),
        "task_snapshot_kind": "git-commit-claims-v1",
        "task_sha256": hashlib.sha256(task_text.encode()).hexdigest(),
        "evidence": [],
        "evidence_manifest_sha256": hashlib.sha256(b"[]").hexdigest(),
    }
    return repo, case


def _fixture_screen(tmp_path: Path):
    repo, template = _fixture_case(tmp_path)
    cases = []
    for case_id, pr in zip(SCREEN_CASE_IDS, (8603, 8612, 8613), strict=True):
        case = dict(template)
        case["id"] = case_id
        case["pr"] = pr
        cases.append(case)
    return repo, {
        "schema": 1,
        "repository": "https://github.com/jleechanorg/worldarchitect.ai",
        "input_order": list(("task", "diff", "changed_files", "evidence")),
        "cases": cases,
    }


def test_public_manifest_pins_five_prs_and_seven_review_revisions():
    manifest = load_manifest(MANIFEST)

    assert [case["pr"] for case in manifest["cases"]] == [
        8603,
        8611,
        8612,
        8612,
        8613,
        8613,
        8618,
    ]
    assert manifest["repository"] == "https://github.com/jleechanorg/worldarchitect.ai"
    assert manifest["cases"][0]["head_sha"] == "c983f3295f9869708048f0a262ae7e506ebe9460"
    assert manifest["cases"][-1]["diff_sha256"] == "7ce9f55577a833a34ebbca26bef31393d677a6eb2cf6e3e7abc3c143054fba86"
    serialized = json.dumps(manifest).lower()
    for forbidden in ("review_url", "expected_find", "severity", "rubric"):
        assert forbidden not in serialized


def test_boundary_scanner_covers_public_benchmark_json_and_python():
    scanned = {path.resolve() for path in public_files()}

    assert MANIFEST.resolve() in scanned
    assert (ROOT / "benchmarks/scripts/run_controller_cold_review.py").resolve() in scanned
    assert check_cold_review_public_surfaces() == []


def test_validate_case_recomputes_git_and_commit_claim_task(tmp_path):
    repo, case = _fixture_case(tmp_path)

    validated = validate_case(repo, case)

    assert validated.head_sha == case["head_sha"]
    assert validated.task_text == (
        f"COMMIT {case['head_sha']}\nSUBJECT: head\nBODY:\n(empty)\n"
    )
    assert commit_claim_snapshot(repo, case["base_sha"], case["head_sha"]) == validated.task_text
    assert validated.evidence == ()


def test_validate_case_fails_closed_on_mismatched_commit_claim_task(tmp_path):
    repo, case = _fixture_case(tmp_path)
    case["task_sha256"] = "0" * 64
    with pytest.raises(BenchmarkError, match="task snapshot digest mismatch"):
        validate_case(repo, case)


def test_validate_case_fails_closed_on_changed_diff_binding(tmp_path):
    repo, case = _fixture_case(tmp_path)
    case["diff_sha256"] = "0" * 64

    with pytest.raises(BenchmarkError, match="diff_sha256 mismatch"):
        validate_case(repo, case)


def test_blinded_plan_pairs_identical_controls_and_hides_contract_identity():
    manifest = load_manifest(MANIFEST)

    public_plan, arm_map = build_blinded_plan(
        manifest["cases"],
        seed=86038618,
        model="gpt-5.6-terra",
        reasoning_effort="high",
        timeout_seconds=1200,
    )

    assert len(public_plan["runs"]) == 14
    assert len(arm_map["arms"]) == 14
    assert "cold-review-v" not in json.dumps(public_plan)
    for case_id in {run["case_id"] for run in public_plan["runs"]}:
        runs = [run for run in public_plan["runs"] if run["case_id"] == case_id]
        assert [run["arm"] for run in runs] == ["arm-1", "arm-2"]
        assert runs[0]["controls"] == runs[1]["controls"]
        assert runs[0]["input_order"] == runs[1]["input_order"]
        contracts = {
            row["review_contract"]
            for row in arm_map["arms"]
            if row["case_id"] == case_id
        }
        assert contracts == {"cold-review-v1", "cold-review-v2"}


def test_screen_plan_has_three_blinded_variants_with_identical_controls():
    manifest = load_manifest(MANIFEST)
    cases = [case for case in manifest["cases"] if case["id"] in SCREEN_CASE_IDS]

    public, private = build_screen_plan(
        cases,
        seed=20260802,
        model="gpt-5.6-luna",
        reasoning_effort="high",
        timeout_seconds=900,
    )

    assert SCREEN_VARIANTS == (
        "control-v2",
        "freeform-traceability",
        "freeform-adversarial",
    )
    assert FREEFORM_PROMPTS == {
        "freeform-traceability": EXPECTED_TRACEABILITY_PROMPT,
        "freeform-adversarial": EXPECTED_ADVERSARIAL_PROMPT,
    }
    assert len(public["runs"]) == 9
    assert len(private["arms"]) == 9
    assert "freeform" not in json.dumps(public).lower()
    assert "cold-review-v2" not in json.dumps(public).lower()
    for case in cases:
        runs = [row for row in public["runs"] if row["case_id"] == case["id"]]
        assert [row["arm"] for row in runs] == ["arm-1", "arm-2", "arm-3"]
        assert len({json.dumps(row["controls"], sort_keys=True) for row in runs}) == 1
        assert len({json.dumps(row["input_order"]) for row in runs}) == 1
        variants = {
            row["review_variant"]
            for row in private["arms"]
            if row["case_id"] == case["id"]
        }
        assert variants == set(SCREEN_VARIANTS)
    assert build_screen_plan(
        cases,
        seed=20260802,
        model="gpt-5.6-luna",
        reasoning_effort="high",
        timeout_seconds=900,
    ) == (public, private)


@pytest.mark.parametrize(
    ("case_ids", "model", "reasoning_effort", "timeout_seconds", "error"),
    (
        (SCREEN_CASE_IDS[:2], SCREEN_MODEL, SCREEN_REASONING_EFFORT, SCREEN_TIMEOUT_SECONDS, "case"),
        (SCREEN_CASE_IDS, "gpt-5.6-terra", SCREEN_REASONING_EFFORT, SCREEN_TIMEOUT_SECONDS, "model"),
        (SCREEN_CASE_IDS, SCREEN_MODEL, "medium", SCREEN_TIMEOUT_SECONDS, "reasoning"),
        (SCREEN_CASE_IDS, SCREEN_MODEL, SCREEN_REASONING_EFFORT, 1200, "timeout"),
    ),
)
def test_screen_plan_rejects_noncanonical_protocol(
    case_ids, model, reasoning_effort, timeout_seconds, error
):
    manifest = load_manifest(MANIFEST)
    cases = [case for case in manifest["cases"] if case["id"] in case_ids]

    with pytest.raises(BenchmarkError, match=error):
        build_screen_plan(
            cases,
            seed=20260802,
            model=model,
            reasoning_effort=reasoning_effort,
            timeout_seconds=timeout_seconds,
        )


def test_run_screen_rejects_noncanonical_worker_count(tmp_path):
    manifest = load_manifest(MANIFEST)

    with pytest.raises(BenchmarkError, match="workers"):
        run_screen(
            manifest,
            case_ids=list(SCREEN_CASE_IDS),
            repo=tmp_path,
            output_dir=tmp_path / "never-created",
            seed=20260802,
            model=SCREEN_MODEL,
            reasoning_effort=SCREEN_REASONING_EFFORT,
            timeout_seconds=SCREEN_TIMEOUT_SECONDS,
            workers=1,
        )
    assert not (tmp_path / "never-created").exists()


def test_manual_observation_is_separate_and_preserves_prompt_bytes():
    manifest = load_manifest(MANIFEST)
    cases = [case for case in manifest["cases"] if case["id"] in SCREEN_CASE_IDS]
    public, private = build_screen_plan(
        cases,
        seed=20260802,
        model="gpt-5.6-luna",
        reasoning_effort="high",
        timeout_seconds=900,
    )

    observation = build_manual_observation_plan(
        cases,
        model="gpt-5.6-luna",
        reasoning_effort="high",
        timeout_seconds=900,
    )

    assert len(observation["runs"]) == 3
    assert observation["observational"] is True
    assert observation["eligible_for_advancement"] is False
    assert observation["source"] == MANUAL_OBSERVATION_SOURCE
    assert all(row["observational"] is True for row in observation["runs"])
    assert all(row["eligible_for_advancement"] is False for row in observation["runs"])
    assert "manual" not in json.dumps(private["arms"]).lower()
    assert len(public["runs"]) == 9

    first_url = "https://github.com/jleechanorg/worldarchitect.ai/pull/8603"
    second_url = "https://github.com/jleechanorg/worldarchitect.ai/pull/8612"
    first = render_manual_observation_prompt(first_url)
    second = render_manual_observation_prompt(second_url)
    assert first == MANUAL_OBSERVATION_PROMPT.format(pr_url=first_url)
    assert second == MANUAL_OBSERVATION_PROMPT.format(pr_url=second_url)
    assert first.replace(first_url, "{pr_url}") == MANUAL_OBSERVATION_PROMPT
    assert second.replace(second_url, "{pr_url}") == MANUAL_OBSERVATION_PROMPT
    historical = render_manual_observation_prompt(
        "https://github.com/jleechanorg/worldarchitect.ai/pull/8328"
    )
    assert len(historical.encode("utf-8")) == 150
    assert hashlib.sha256(historical.encode("utf-8")).hexdigest() == (
        "74e09b7cebfcb3fed630f7a9085c984b04e04726cc29a89ed23029d9a2a6bcb3"
    )
    assert len(historical.encode("utf-8")) == MANUAL_OBSERVATION_SOURCE[
        "source_prompt_utf8_bytes"
    ]
    assert hashlib.sha256(historical.encode("utf-8")).hexdigest() == (
        MANUAL_OBSERVATION_SOURCE["source_prompt_sha256"]
    )


def _fake_validated_control_review(request, *, neutral_cwd, output_dir, **_kwargs):
    output_dir.mkdir(parents=True)
    response = (
        "PROMPT_ID: controller-cold-review-v2\n"
        "HEAD_SHA: bound-control-head\n"
        "## Findings\nControl narrative.\n"
        "## Commands Executed\nNone.\n"
        "## Evidence Checked\nDiff.\n## Caveats\nNone.\n"
    )
    paths = {}
    for name, filename in {
        "prompt": "prompt.txt",
        "envelope": "envelope.json",
        "response": "reviewer.output.md",
        "transport": "transport.jsonl",
        "receipt": "controller-receipt.json",
        "findings": "findings.json",
    }.items():
        paths[name] = output_dir / filename
    paths["prompt"].write_text(request.prompt)
    paths["envelope"].write_text(request.envelope_json)
    paths["response"].write_text(response)
    paths["transport"].write_text('{"type":"turn.completed"}\n')
    paths["findings"].write_text("{}\n")
    paths["receipt"].write_text(
        json.dumps(
            {
                "duration_seconds": 0.25,
                "usage": {"input_tokens": 10, "output_tokens": 5},
            }
        )
    )
    return ControllerReviewResult(
        review=ValidatedReview(
            verdict="fail",
            checks=(),
            response_sha256=hashlib.sha256(response.encode()).hexdigest(),
        ),
        receipts=(),
        response_text=response,
        transport_text=paths["transport"].read_text(),
        output_paths={key: str(path) for key, path in paths.items()},
    )


def test_run_screen_uses_raw_freeform_transport_and_transcript_bundles(
    tmp_path, monkeypatch
):
    repo, manifest = _fixture_screen(tmp_path)
    output = tmp_path / "screen-output"
    completed = []
    real_run = subprocess.run
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", "/sealed/do-not-leak")
    monkeypatch.setenv("CUSTOM_HOLDOUT_SENTINEL", "secret")
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        _fake_validated_control_review,
    )

    def fake_run(args, **kwargs):
        if args and args[0] == "git":
            return real_run(args, **kwargs)
        completed.append((args, kwargs))
        raw = "\n".join(
            (
                json.dumps(
                    {
                        "type": "item.completed",
                        "item": {"type": "agent_message", "text": "Raw finding."},
                    }
                ),
                json.dumps(
                    {
                        "type": "turn.completed",
                        "usage": {"input_tokens": 12, "output_tokens": 7},
                    }
                ),
            )
        ) + "\n"
        return subprocess.CompletedProcess(args, 0, stdout=raw, stderr="")

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.subprocess.run", fake_run
    )

    run_screen(
        manifest,
        case_ids=list(SCREEN_CASE_IDS),
        repo=repo,
        output_dir=output,
        seed=20260802,
        model="gpt-5.6-luna",
        reasoning_effort="high",
        timeout_seconds=900,
        workers=SCREEN_WORKERS,
    )

    assert len(completed) == 6
    for _args, kwargs in completed:
        assert Path(kwargs["cwd"]).parts[-3] == "neutral"
        assert kwargs["env"].get("DARK_FACTORY_HOLDOUTS") is None
        assert kwargs["env"].get("CUSTOM_HOLDOUT_SENTINEL") is None
        assert "PROMPT_ID:" not in kwargs["input"]
        assert "BEGIN_CONTROLLER_ENVELOPE_BASE64" in kwargs["input"]
    for arm in ("arm-1", "arm-2", "arm-3"):
        bundle = json.loads((output / f"blinded-{arm}-bundle.json").read_text())
        assert bundle["schema_version"] == "cold-review-transcript-run-v1"
        assert set(bundle) == {
            "schema_version", "run_id", "public_plan_sha256",
            "private_arm_map_sha256", "cases",
        }
        assert bundle["public_plan_sha256"] == hashlib.sha256(
            (output / "run-plan.json").read_bytes()
        ).hexdigest()
        assert bundle["private_arm_map_sha256"] == hashlib.sha256(
            (output / "private-arm-map.json").read_bytes()
        ).hexdigest()
        assert set(bundle["cases"][0]) == {
            "case_id", "base_sha", "head_sha", "diff", "diff_sha256",
            "case_sha256", "transcript", "transcript_sha256", "metrics",
        }
    transcripts = [
        case["transcript"]
        for arm in ("arm-1", "arm-2", "arm-3")
        for case in json.loads((output / f"blinded-{arm}-bundle.json").read_text())["cases"]
    ]
    control_transcripts = [text for text in transcripts if "Control narrative." in text]
    assert len(control_transcripts) == 3
    assert all("controller-cold-review-v2" not in text for text in control_transcripts)
    assert all(text.startswith("HEAD_SHA: bound-control-head\n") for text in control_transcripts)
    assert all("## Findings\nControl narrative." in text for text in control_transcripts)
    freeform_records = [
        json.loads(path.read_text())
        for path in (output / "raw").glob("*/*/run-record.json")
        if json.loads(path.read_text())["review_variant"].startswith("freeform-")
    ]
    assert len(freeform_records) == 6
    assert all(record["prompt_sha256"] for record in freeform_records)
    assert all(record["envelope_sha256"] for record in freeform_records)
    assert all(record["transport_sha256"] for record in freeform_records)
    assert all(
        record["public_plan_sha256"]
        == hashlib.sha256((output / "run-plan.json").read_bytes()).hexdigest()
        for record in freeform_records
    )
    assert all(
        record["private_arm_map_sha256"]
        == hashlib.sha256((output / "private-arm-map.json").read_bytes()).hexdigest()
        for record in freeform_records
    )


@pytest.mark.parametrize(
    ("returncode", "usage", "error"),
    ((7, {"input_tokens": 1}, "exited with 7"), (0, None, "usage")),
)
def test_run_screen_fails_closed_and_preserves_receipt(
    tmp_path, monkeypatch, returncode, usage, error
):
    repo, manifest = _fixture_screen(tmp_path)
    output = tmp_path / "invalid-screen"
    real_run = subprocess.run
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        _fake_validated_control_review,
    )

    def fake_run(args, **kwargs):
        if args and args[0] == "git":
            return real_run(args, **kwargs)
        events = [
            {
                "type": "item.completed",
                "item": {"type": "agent_message", "text": "Raw finding."},
            }
        ]
        if usage is not None:
            events.append({"type": "turn.completed", "usage": usage})
        raw = "\n".join(json.dumps(event) for event in events) + "\n"
        return subprocess.CompletedProcess(args, returncode, stdout=raw, stderr="boom")

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.subprocess.run", fake_run
    )

    with pytest.raises(BenchmarkError, match=error):
        run_screen(
            manifest,
            case_ids=list(SCREEN_CASE_IDS),
            repo=repo,
            output_dir=output,
            seed=20260802,
            model="gpt-5.6-luna",
            reasoning_effort="high",
            timeout_seconds=900,
            workers=SCREEN_WORKERS,
        )

    receipts = list((output / "raw").glob("*/*/controller-receipt.json"))
    assert receipts
    assert "exit_code" in json.loads(receipts[-1].read_text())


@pytest.mark.parametrize("failure", ("timeout", "launch"))
def test_run_screen_preserves_timeout_and_launch_failure_receipts(
    tmp_path, monkeypatch, failure
):
    repo, manifest = _fixture_screen(tmp_path)
    output = tmp_path / f"{failure}-screen"
    real_run = subprocess.run
    partial = '{"type":"item.completed","item":{"type":"agent_message","text":"partial"}}\n'
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        _fake_validated_control_review,
    )

    def fake_run(args, **kwargs):
        if args and args[0] == "git":
            return real_run(args, **kwargs)
        if failure == "timeout":
            raise subprocess.TimeoutExpired(
                args,
                SCREEN_TIMEOUT_SECONDS,
                output=partial,
                stderr="timed out with partial stderr",
            )
        raise OSError("codex launch failed")

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.subprocess.run", fake_run
    )

    with pytest.raises(BenchmarkError, match= failure):
        run_screen(
            manifest,
            case_ids=list(SCREEN_CASE_IDS),
            repo=repo,
            output_dir=output,
            seed=20260802,
            model=SCREEN_MODEL,
            reasoning_effort=SCREEN_REASONING_EFFORT,
            timeout_seconds=SCREEN_TIMEOUT_SECONDS,
            workers=SCREEN_WORKERS,
        )

    receipts = [
        json.loads(path.read_text())
        for path in (output / "raw").glob("*/*/controller-receipt.json")
        if json.loads(path.read_text()).get("failure_class") == failure
    ]
    assert receipts
    assert all(receipt["attempt_id"] for receipt in receipts)
    transport_paths = list((output / "raw").glob("*/*/transport.jsonl"))
    assert transport_paths
    if failure == "timeout":
        assert any(path.read_text() == partial for path in transport_paths)


@pytest.mark.parametrize("failure", ("timeout", "launch"))
def test_control_arm_preserves_timeout_and_launch_failure_receipts(
    tmp_path, monkeypatch, failure
):
    repo, manifest = _fixture_screen(tmp_path)
    output = tmp_path / f"control-{failure}-screen"
    real_run = subprocess.run
    partial = "partial validated-control transport\n"
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )

    def failed_control(*_args, **_kwargs):
        if failure == "timeout":
            raise subprocess.TimeoutExpired(
                ["codex"],
                SCREEN_TIMEOUT_SECONDS,
                output=partial,
                stderr="partial control stderr",
            )
        raise OSError("control launch failed")

    def successful_raw(args, **kwargs):
        if args and args[0] == "git":
            return real_run(args, **kwargs)
        raw = "\n".join(
            (
                json.dumps(
                    {
                        "type": "item.completed",
                        "item": {"type": "agent_message", "text": "Raw finding."},
                    }
                ),
                json.dumps(
                    {
                        "type": "turn.completed",
                        "usage": {"input_tokens": 4, "output_tokens": 2},
                    }
                ),
            )
        ) + "\n"
        return subprocess.CompletedProcess(args, 0, stdout=raw, stderr="")

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        failed_control,
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.subprocess.run",
        successful_raw,
    )

    with pytest.raises(BenchmarkError, match=f"control review transport {failure}"):
        run_screen(
            manifest,
            case_ids=list(SCREEN_CASE_IDS),
            repo=repo,
            output_dir=output,
            seed=20260802,
            model=SCREEN_MODEL,
            reasoning_effort=SCREEN_REASONING_EFFORT,
            timeout_seconds=SCREEN_TIMEOUT_SECONDS,
            workers=SCREEN_WORKERS,
        )

    plan_sha = hashlib.sha256((output / "run-plan.json").read_bytes()).hexdigest()
    map_sha = hashlib.sha256(
        (output / "private-arm-map.json").read_bytes()
    ).hexdigest()
    receipts = [
        json.loads(path.read_text())
        for path in (output / "raw").glob("*/*/controller-receipt.json")
        if json.loads(path.read_text()).get("failure_class") == failure
    ]
    assert receipts
    assert all(receipt["review_contract"] == "cold-review-v2" for receipt in receipts)
    assert all(receipt["public_plan_sha256"] == plan_sha for receipt in receipts)
    assert all(receipt["private_arm_map_sha256"] == map_sha for receipt in receipts)
    if failure == "timeout":
        assert any(
            path.read_text() == partial
            for path in (output / "raw").glob("*/*/transport.jsonl")
        )


@pytest.mark.parametrize(
    ("prompt", "identity"),
    (
        (FREEFORM_PROMPTS["freeform-traceability"], "cold-review-v2"),
        (FREEFORM_PROMPTS["freeform-adversarial"], "controller-cold-review-v1"),
        (
            MANUAL_OBSERVATION_PROMPT.format(
                pr_url="https://github.com/jleechanorg/worldarchitect.ai/pull/8328"
            ),
            "cold-review-v1",
        ),
    ),
)
def test_raw_evaluator_transcripts_fail_closed_on_prompt_identity_echo(
    tmp_path, monkeypatch, prompt, identity
):
    repo, case = _fixture_case(tmp_path)
    inputs = validate_case(repo, case)
    neutral = tmp_path / "neutral"
    neutral.mkdir()
    output = tmp_path / "raw-review"
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )

    def echoed_identity(args, **_kwargs):
        transcript = f"Finding text echoes {identity} in ordinary prose."
        raw = "\n".join(
            (
                json.dumps(
                    {
                        "type": "item.completed",
                        "item": {"type": "agent_message", "text": transcript},
                    }
                ),
                json.dumps(
                    {
                        "type": "turn.completed",
                        "usage": {"input_tokens": 4, "output_tokens": 2},
                    }
                ),
            )
        ) + "\n"
        return subprocess.CompletedProcess(args, 0, stdout=raw, stderr="")

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.subprocess.run",
        echoed_identity,
    )

    with pytest.raises(BenchmarkError, match="prompt identity"):
        _run_raw_freeform_review(
            inputs,
            prompt=prompt,
            neutral_cwd=neutral,
            output_dir=output,
            model=SCREEN_MODEL,
            reasoning_effort=SCREEN_REASONING_EFFORT,
            timeout_seconds=SCREEN_TIMEOUT_SECONDS,
        )

    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["failure_class"] == "prompt_identity_echo"
    assert receipt["echoed_prompt_identity"] == identity
    assert identity in (output / "reviewer.output.md").read_text()
    assert identity in (output / "transport.jsonl").read_text()


def test_control_narrative_identity_echo_fails_closed_with_raw_receipt(
    tmp_path, monkeypatch
):
    repo, manifest = _fixture_screen(tmp_path)
    output = tmp_path / "control-identity-screen"
    real_run = subprocess.run
    identity = "cold-review-v2"
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )

    def echoing_control(request, *, neutral_cwd, output_dir, **kwargs):
        result = _fake_validated_control_review(
            request, neutral_cwd=neutral_cwd, output_dir=output_dir, **kwargs
        )
        response = result.response_text.replace(
            "Control narrative.", f"Control narrative echoes {identity}."
        )
        Path(result.output_paths["response"]).write_text(response)
        return ControllerReviewResult(
            review=result.review,
            receipts=result.receipts,
            response_text=response,
            transport_text=result.transport_text,
            output_paths=result.output_paths,
        )

    def clean_raw(args, **kwargs):
        if args and args[0] == "git":
            return real_run(args, **kwargs)
        raw = "\n".join(
            (
                json.dumps(
                    {
                        "type": "item.completed",
                        "item": {"type": "agent_message", "text": "Clean raw finding."},
                    }
                ),
                json.dumps(
                    {
                        "type": "turn.completed",
                        "usage": {"input_tokens": 4, "output_tokens": 2},
                    }
                ),
            )
        ) + "\n"
        return subprocess.CompletedProcess(args, 0, stdout=raw, stderr="")

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        echoing_control,
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.subprocess.run", clean_raw,
    )

    with pytest.raises(BenchmarkError, match="prompt identity"):
        run_screen(
            manifest,
            case_ids=list(SCREEN_CASE_IDS),
            repo=repo,
            output_dir=output,
            seed=20260802,
            model=SCREEN_MODEL,
            reasoning_effort=SCREEN_REASONING_EFFORT,
            timeout_seconds=SCREEN_TIMEOUT_SECONDS,
            workers=SCREEN_WORKERS,
        )

    receipts = [
        json.loads(path.read_text())
        for path in (output / "raw").glob("*/*/controller-receipt.json")
        if json.loads(path.read_text()).get("failure_class")
        == "prompt_identity_echo"
    ]
    assert receipts
    assert all(receipt["echoed_prompt_identity"] == identity for receipt in receipts)
    assert any(
        identity in path.read_text()
        for path in (output / "raw").glob("*/*/reviewer.output.md")
    )


def test_screen_concurrency_is_measured_at_live_reviewer_call_seam(
    tmp_path, monkeypatch
):
    repo, manifest = _fixture_screen(tmp_path)
    output = tmp_path / "concurrent-screen"
    real_run = subprocess.run
    barrier = threading.Barrier(SCREEN_WORKERS)
    seen_cases = set()
    seen_lock = threading.Lock()
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )

    def rendezvous(neutral_cwd):
        case_id = Path(neutral_cwd).parts[-2]
        with seen_lock:
            first = case_id not in seen_cases
            seen_cases.add(case_id)
        if first:
            barrier.wait(timeout=5)

    def fake_control(request, *, neutral_cwd, output_dir, **kwargs):
        rendezvous(neutral_cwd)
        return _fake_validated_control_review(
            request, neutral_cwd=neutral_cwd, output_dir=output_dir, **kwargs
        )

    def fake_run(args, **kwargs):
        if args and args[0] == "git":
            return real_run(args, **kwargs)
        rendezvous(kwargs["cwd"])
        raw = "\n".join(
            (
                json.dumps(
                    {
                        "type": "item.completed",
                        "item": {"type": "agent_message", "text": "Raw finding."},
                    }
                ),
                json.dumps(
                    {
                        "type": "turn.completed",
                        "usage": {"input_tokens": 4, "output_tokens": 2},
                    }
                ),
            )
        ) + "\n"
        return subprocess.CompletedProcess(args, 0, stdout=raw, stderr="")

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        fake_control,
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.subprocess.run", fake_run,
    )

    run_screen(
        manifest,
        case_ids=list(SCREEN_CASE_IDS),
        repo=repo,
        output_dir=output,
        seed=20260802,
        model=SCREEN_MODEL,
        reasoning_effort=SCREEN_REASONING_EFFORT,
        timeout_seconds=SCREEN_TIMEOUT_SECONDS,
        workers=SCREEN_WORKERS,
    )

    concurrency = json.loads((output / "concurrency.json").read_text())
    assert concurrency == {
        "measurement": "live-reviewer-calls",
        "observed_max_calls": SCREEN_WORKERS,
        "observed_max_cases": SCREEN_WORKERS,
        "requested_workers": SCREEN_WORKERS,
    }


def test_manual_observation_runs_after_screen_and_stays_outside_arm_map(
    tmp_path, monkeypatch
):
    repo, manifest = _fixture_screen(tmp_path)
    output = tmp_path / "observational-screen"
    prompts = []
    observation_barrier = threading.Barrier(SCREEN_WORKERS)
    tracker_ids = set()
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        _fake_validated_control_review,
    )

    def fake_raw(
        _inputs,
        *,
        prompt,
        output_dir,
        receipt_bindings=None,
        call_tracker=None,
        tracker_phase="screen",
        **_kwargs,
    ):
        prompts.append(prompt)
        assert call_tracker is not None
        tracker_ids.add(id(call_tracker))
        with call_tracker.track(tracker_phase):
            if tracker_phase == "manual-observation":
                observation_barrier.wait(timeout=5)
            output_dir.mkdir(parents=True)
            paths = {}
            for name, filename in {
                "prompt": "prompt.txt",
                "envelope": "envelope.json",
                "transport": "transport.jsonl",
                "response": "reviewer.output.md",
                "receipt": "controller-receipt.json",
            }.items():
                path = output_dir / filename
                path.write_text(prompt if name in ("prompt", "response") else "{}\n")
                paths[name] = path
            receipt = {
                "duration_seconds": 0.1,
                "usage": {"input_tokens": 3, "output_tokens": 2},
                **(receipt_bindings or {}),
            }
            paths["receipt"].write_text(json.dumps(receipt))
        return {"transcript": prompt, "receipt": receipt, "paths": paths}

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._run_raw_freeform_review",
        fake_raw,
    )

    run_screen(
        manifest,
        case_ids=list(SCREEN_CASE_IDS),
        repo=repo,
        output_dir=output,
        seed=20260802,
        model="gpt-5.6-luna",
        reasoning_effort="high",
        timeout_seconds=900,
        workers=SCREEN_WORKERS,
        include_manual_observation=True,
    )

    assert len(prompts) == 9
    assert len(tracker_ids) == 1
    assert all(prompt in FREEFORM_PROMPTS.values() for prompt in prompts[:6])
    assert set(prompts[6:]) == {
        render_manual_observation_prompt(
            f"https://github.com/jleechanorg/worldarchitect.ai/pull/{pr}"
        )
        for pr in (8603, 8612, 8613)
    }
    private = json.loads((output / "private-arm-map.json").read_text())
    assert "manual" not in json.dumps(private).lower()
    observation = json.loads((output / "manual-observation-bundle.json").read_text())
    assert observation["schema_version"] == "cold-review-transcript-run-v1"
    assert observation["observational"] is True
    assert observation["eligible_for_advancement"] is False
    assert observation["source"] == MANUAL_OBSERVATION_SOURCE
    receipt_path = next(
        (output / "manual-observation/raw").glob("*/controller-receipt.json")
    )
    receipt = json.loads(receipt_path.read_text())
    assert receipt["observational"] is True
    assert receipt["eligible_for_advancement"] is False
    assert receipt["source"] == MANUAL_OBSERVATION_SOURCE
    observation_plan_sha = hashlib.sha256(
        (output / "manual-observation-plan.json").read_bytes()
    ).hexdigest()
    assert receipt["manual_observation_plan_sha256"] == observation_plan_sha
    record_path = next((output / "manual-observation/raw").glob("*/run-record.json"))
    record = json.loads(record_path.read_text())
    assert record["manual_observation_plan_sha256"] == observation_plan_sha
    assert observation["manual_observation_plan_sha256"] == observation_plan_sha
    observation_concurrency = json.loads(
        (output / "manual-observation-concurrency.json").read_text()
    )
    assert observation_concurrency == {
        "measurement": "live-reviewer-calls",
        "observed_max_calls": SCREEN_WORKERS,
        "observed_max_cases": SCREEN_WORKERS,
        "requested_workers": SCREEN_WORKERS,
    }


def test_screen_cli_reports_selected_case_count(tmp_path, monkeypatch, capsys):
    captured = {}
    manifest = load_manifest(MANIFEST)
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.load_manifest",
        lambda _path: manifest,
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_screen",
        lambda *args, **kwargs: captured.update(kwargs),
    )

    rc = main(
        [
            "screen",
            "--manifest", str(MANIFEST),
            "--repo", str(tmp_path),
            "--output", str(tmp_path / "screen"),
            "--model", SCREEN_MODEL,
            "--reasoning-effort", SCREEN_REASONING_EFFORT,
            *[
                value
                for case_id in SCREEN_CASE_IDS
                for value in ("--case-id", case_id)
            ],
        ]
    )

    assert rc == 0
    assert captured["case_ids"] == list(SCREEN_CASE_IDS)
    assert captured["timeout_seconds"] == SCREEN_TIMEOUT_SECONDS
    assert captured["workers"] == SCREEN_WORKERS
    assert json.loads(capsys.readouterr().out) == {
        "status": "valid",
        "cases": len(SCREEN_CASE_IDS),
    }


@pytest.mark.parametrize(
    ("command", "extra_flag", "error"),
    (
        ("validate", ("--case-id", SCREEN_CASE_IDS[0]), "--case-id"),
        ("validate", ("--include-manual-observation",), "--include-manual-observation"),
        ("run", ("--case-id", SCREEN_CASE_IDS[0]), "--case-id"),
        ("run", ("--include-manual-observation",), "--include-manual-observation"),
    ),
)
def test_non_screen_commands_reject_screen_only_flags(
    tmp_path, capsys, command, extra_flag, error
):
    args = [
        command,
        "--manifest", str(MANIFEST),
        "--repo", str(tmp_path),
        *extra_flag,
    ]
    if command == "run":
        args.extend(("--output", str(tmp_path / "run"), "--model", "gpt-5.6-terra"))

    assert main(args) == 1
    assert error in json.loads(capsys.readouterr().out)["error"]


def test_run_cli_retains_legacy_timeout_and_worker_defaults(
    tmp_path, monkeypatch, capsys
):
    captured = {}
    manifest = load_manifest(MANIFEST)
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.load_manifest",
        lambda _path: manifest,
    )
    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_benchmark",
        lambda *args, **kwargs: captured.update(kwargs),
    )

    assert main(
        [
            "run",
            "--manifest", str(MANIFEST),
            "--repo", str(tmp_path),
            "--output", str(tmp_path / "run"),
            "--model", "gpt-5.6-terra",
        ]
    ) == 0
    assert captured["timeout_seconds"] == 1200
    assert captured["workers"] == DEFAULT_WORKERS
    assert json.loads(capsys.readouterr().out)["cases"] == len(manifest["cases"])


def test_run_preserves_raw_artifacts_and_writes_digest_bound_arm_records(
    tmp_path, monkeypatch
):
    repo, case = _fixture_case(tmp_path)
    manifest = {
        "schema": 1,
        "repository": "https://github.com/jleechanorg/worldarchitect.ai",
        "input_order": list(("task", "diff", "changed_files", "evidence")),
        "cases": [case],
    }
    output = tmp_path / "output"

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )

    def fake_review(request, *, neutral_cwd, output_dir, transport_argv, timeout):
        output_dir.mkdir(parents=True)
        response = (
            "## Findings\nOne actionable finding.\n"
            "## Commands Executed\n`pytest` - exit code 0.\n"
            "## Evidence Checked\nDiff.\n## Caveats\nNone.\n"
        )
        response_sha = hashlib.sha256(response.encode()).hexdigest()
        paths = {
            "prompt": output_dir / "prompt.txt",
            "envelope": output_dir / "envelope.json",
            "response": output_dir / "reviewer.output.md",
            "transport": output_dir / "transport.jsonl",
            "receipt": output_dir / "controller-receipt.json",
            "findings": output_dir / "findings.json",
        }
        paths["prompt"].write_text(request.prompt)
        paths["envelope"].write_text(request.envelope_json)
        paths["response"].write_text(response)
        paths["transport"].write_text('{"type":"turn.completed"}\n')
        paths["findings"].write_text("{}\n")
        paths["receipt"].write_text(
            json.dumps(
                {
                    "duration_seconds": 1.25,
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                }
            )
        )
        return ControllerReviewResult(
            review=ValidatedReview(
                verdict="fail", checks=(), response_sha256=response_sha
            ),
            receipts=(),
            response_text=response,
            transport_text=paths["transport"].read_text(),
            output_paths={key: str(path) for key, path in paths.items()},
        )

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        fake_review,
    )

    run_benchmark(
        manifest,
        repo=repo,
        output_dir=output,
        seed=7,
        model="gpt-5.6-terra",
        reasoning_effort="high",
        timeout_seconds=1200,
        workers=1,
    )

    for arm in ("arm-1", "arm-2"):
        raw_dir = output / "raw" / case["id"] / arm
        record = json.loads((raw_dir / "run-record.json").read_text())
        blinded = json.loads(
            (output / "blinded" / case["id"] / f"{arm}.json").read_text()
        )
        assert (raw_dir / "transport.jsonl").is_file()
        assert record["transport_sha256"] == hashlib.sha256(
            (raw_dir / "transport.jsonl").read_bytes()
        ).hexdigest()
        assert record["task_sha256"] == case["task_sha256"]
        assert record["controls"]["model"] == "gpt-5.6-terra"
        assert blinded["duration_seconds"] == 1.25
        assert blinded["usage"]["input_tokens"] == 10
        assert "cold-review-v" not in json.dumps(blinded)

    bundles = [
        json.loads((output / f"blinded-{arm}-bundle.json").read_text())
        for arm in ("arm-1", "arm-2")
    ]
    assert all(set(bundle) == {"schema_version", "run_id", "cases"} for bundle in bundles)
    assert all(bundle["schema_version"] == "cold-review-run-v1" for bundle in bundles)
    assert all(len(bundle["cases"]) == 1 for bundle in bundles)
    expected_fields = {
        "case_id", "base_sha", "head_sha", "diff_sha256", "case_sha256",
        "diff", "transcript", "transcript_sha256", "review_verdict",
        "findings", "metrics",
    }
    entries = [bundle["cases"][0] for bundle in bundles]
    assert all(set(item) == expected_fields for item in entries)
    assert all(item["case_id"] == case["id"] for item in entries)
    case_digest_input = {
        "case_id": case["id"],
        "base_sha": case["base_sha"],
        "head_sha": case["head_sha"],
        "diff_sha256": case["diff_sha256"],
    }
    canonical = json.dumps(
        case_digest_input, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    )
    assert all(
        item["case_sha256"] == hashlib.sha256(canonical.encode()).hexdigest()
        for item in entries
    )
    assert all(item["diff"] for item in entries)
    assert all(set(item["findings"][0]) == {"id", "text"} for item in entries)
    assert all(item["findings"][0]["id"].startswith("obs-") for item in entries)
    assert all(item["review_verdict"] == "FAIL" for item in entries)
    assert all(
        set(item["metrics"])
        == {"latency_ms", "input_tokens", "output_tokens", "total_tokens"}
        for item in entries
    )
    assert all(item["metrics"]["latency_ms"] == 1250 for item in entries)
    assert "cold-review-v" not in json.dumps(bundles)


def test_cross_case_workers_run_cases_concurrently_but_keep_arms_serial(
    tmp_path, monkeypatch
):
    repo, first = _fixture_case(tmp_path)
    second = dict(first)
    second["id"] = "fixture-r2"
    second["pr"] = 2
    manifest = {
        "schema": 1,
        "repository": "https://github.com/jleechanorg/worldarchitect.ai",
        "input_order": list(("task", "diff", "changed_files", "evidence")),
        "cases": [first, second],
    }
    barrier = threading.Barrier(2)
    seen_first_arm: set[str] = set()
    seen_lock = threading.Lock()

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review._gate_subprocess_args",
        lambda *args, **kwargs: ["codex", "exec", "PROMPT"],
    )

    def fake_review(request, *, neutral_cwd, output_dir, transport_argv, timeout):
        case_id = Path(neutral_cwd).parts[-2]
        with seen_lock:
            first_for_case = case_id not in seen_first_arm
            seen_first_arm.add(case_id)
        if first_for_case:
            barrier.wait(timeout=5)
        output_dir.mkdir(parents=True)
        response = (
            "## Findings\nObserved.\n## Commands Executed\nNone.\n"
            "## Evidence Checked\nDiff.\n## Caveats\nNone.\n"
        )
        response_sha = hashlib.sha256(response.encode()).hexdigest()
        paths = {}
        for name, filename in {
            "prompt": "prompt.txt",
            "envelope": "envelope.json",
            "response": "reviewer.output.md",
            "transport": "transport.jsonl",
            "receipt": "controller-receipt.json",
            "findings": "findings.json",
        }.items():
            paths[name] = output_dir / filename
        paths["prompt"].write_text(request.prompt)
        paths["envelope"].write_text(request.envelope_json)
        paths["response"].write_text(response)
        paths["transport"].write_text("{}\n")
        paths["findings"].write_text("{}\n")
        paths["receipt"].write_text(
            json.dumps({"duration_seconds": 0.1, "usage": {}})
        )
        return ControllerReviewResult(
            review=ValidatedReview(
                verdict="fail", checks=(), response_sha256=response_sha
            ),
            receipts=(),
            response_text=response,
            transport_text="{}\n",
            output_paths={key: str(path) for key, path in paths.items()},
        )

    monkeypatch.setattr(
        "benchmarks.scripts.run_controller_cold_review.run_controller_review",
        fake_review,
    )

    run_benchmark(
        manifest,
        repo=repo,
        output_dir=tmp_path / "parallel-output",
        seed=3,
        model="gpt-5.6-terra",
        reasoning_effort="high",
        timeout_seconds=1200,
        workers=2,
    )

    concurrency = json.loads(
        (tmp_path / "parallel-output/concurrency.json").read_text()
    )
    assert concurrency == {"observed_max_cases": 2, "requested_workers": 2}
