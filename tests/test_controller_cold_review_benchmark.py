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
    build_blinded_plan,
    commit_claim_snapshot,
    load_manifest,
    run_benchmark,
    validate_case,
)
from runner.review_controller import ControllerReviewResult, ValidatedReview
from benchmarks.scripts.check_boundary import (
    check_cold_review_public_surfaces,
    public_files,
)


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "benchmarks/controller-cold-review/cases.json"


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
