#!/usr/bin/env python3
"""Validate and run the public, blinded controller cold-review A/B."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import pathlib
import random
import subprocess
import sys
import tempfile
import threading
from contextlib import ExitStack, contextmanager
from dataclasses import replace

ROOT = pathlib.Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from runner.handler_core import Context
from runner.handler_dispatch import (
    _build_controller_codex_transport,
    _gate_subprocess_args,
)
from runner.review_cli import _git as _review_git
from runner.review_cli import _snapshot
from runner.review_controller import (
    EvidenceArtifact,
    ReviewInputs,
    create_review_request,
    run_controller_review,
    validate_immutable_target,
)


CONTRACTS = ("cold-review-v1", "cold-review-v2")
INPUT_ORDER = ("task", "diff", "changed_files", "evidence")
TASK_SNAPSHOT_KIND = "git-commit-claims-v1"
DEFAULT_WORKERS = max(1, int(os.environ.get("DARK_FACTORY_REVIEW_CASE_WORKERS", "2")))
_SHA40 = frozenset("0123456789abcdef")
_CASE_KEYS = {
    "id", "pr", "revision", "base_sha", "head_sha", "tree_sha",
    "diff_sha256", "changed_files", "changed_files_sha256",
    "task_snapshot_kind", "task_sha256", "evidence", "evidence_manifest_sha256",
}


class BenchmarkError(ValueError):
    """The public benchmark input or execution violated its binding contract."""


def _canonical(value: object) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _is_digest(value: object, length: int) -> bool:
    return (
        isinstance(value, str)
        and len(value) == length
        and set(value) <= _SHA40
    )


def _read_json(path: pathlib.Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise BenchmarkError(f"cannot read JSON {path}: {exc}") from exc


def load_manifest(path: str | pathlib.Path) -> dict:
    """Load only the public immutable-input schema; reject extra fields."""
    path = pathlib.Path(path).resolve()
    payload = _read_json(path)
    if not isinstance(payload, dict) or set(payload) != {
        "schema", "repository", "input_order", "cases"
    }:
        raise BenchmarkError("benchmark manifest schema is invalid")
    if payload["schema"] != 1 or payload["input_order"] != list(INPUT_ORDER):
        raise BenchmarkError("benchmark manifest version or input order is invalid")
    cases = payload.get("cases")
    if not isinstance(cases, list) or not cases:
        raise BenchmarkError("benchmark manifest requires cases")
    ids: set[str] = set()
    for case in cases:
        if not isinstance(case, dict) or set(case) != _CASE_KEYS:
            raise BenchmarkError("benchmark case schema is invalid")
        if not isinstance(case["id"], str) or not case["id"] or case["id"] in ids:
            raise BenchmarkError("benchmark case IDs must be unique and non-empty")
        ids.add(case["id"])
        for key in ("base_sha", "head_sha", "tree_sha"):
            if not _is_digest(case[key], 40):
                raise BenchmarkError(f"{case['id']} {key} is invalid")
        for key in (
            "diff_sha256", "changed_files_sha256", "task_sha256",
            "evidence_manifest_sha256",
        ):
            if not _is_digest(case[key], 64):
                raise BenchmarkError(f"{case['id']} {key} is invalid")
        changed = case["changed_files"]
        if not isinstance(changed, list) or any(
            not isinstance(item, str) or not item for item in changed
        ):
            raise BenchmarkError(f"{case['id']} changed_files is invalid")
        if changed != sorted(changed) or len(changed) != len(set(changed)):
            raise BenchmarkError(f"{case['id']} changed_files must be sorted and unique")
        if _sha256(_canonical(changed).encode()) != case["changed_files_sha256"]:
            raise BenchmarkError(f"{case['id']} changed_files_sha256 mismatch")
        evidence = case["evidence"]
        if not isinstance(evidence, list):
            raise BenchmarkError(f"{case['id']} evidence must be a list")
        if _sha256(_canonical(evidence).encode()) != case["evidence_manifest_sha256"]:
            raise BenchmarkError(f"{case['id']} evidence_manifest_sha256 mismatch")
        if case["task_snapshot_kind"] != TASK_SNAPSHOT_KIND:
            raise BenchmarkError(f"{case['id']} task_snapshot_kind is invalid")
    return payload


def commit_claim_snapshot(
    worktree: str | pathlib.Path,
    base_sha: str,
    head_sha: str,
) -> str:
    """Derive immutable task claims from pinned commit subjects and bodies."""
    worktree = pathlib.Path(worktree).resolve()
    shas = _review_git(
        worktree,
        "rev-list",
        "--reverse",
        "--topo-order",
        f"{base_sha}..{head_sha}",
    ).splitlines()
    if not shas:
        raise BenchmarkError("commit-claim snapshot has no commits")
    blocks: list[str] = []
    for sha in shas:
        subject = _review_git(worktree, "show", "-s", "--format=%s", sha)
        body = _review_git(
            worktree,
            "show",
            "-s",
            "--format=%b",
            sha,
            allow_empty=True,
        )
        blocks.append(
            f"COMMIT {sha}\nSUBJECT: {subject}\nBODY:\n{body or '(empty)'}"
        )
    return "\n\n".join(blocks) + "\n"


def validate_case(
    worktree: str | pathlib.Path,
    case: dict,
) -> ReviewInputs:
    """Recompute every public binding against one clean detached worktree."""
    worktree = pathlib.Path(worktree).resolve()
    try:
        snapshot = _snapshot(worktree, case["base_sha"], case["head_sha"])
    except Exception as exc:
        raise BenchmarkError(f"{case['id']} snapshot failed: {exc}") from exc
    observed = {
        "tree_sha": snapshot["tree_sha"],
        "diff_sha256": snapshot["diff_sha256"],
        "changed_files": list(snapshot["changed_files"]),
    }
    for key in ("tree_sha", "diff_sha256", "changed_files"):
        if observed[key] != case[key]:
            raise BenchmarkError(f"{case['id']} {key} mismatch")
    if _sha256(_canonical(observed["changed_files"]).encode()) != case["changed_files_sha256"]:
        raise BenchmarkError(f"{case['id']} changed_files_sha256 mismatch")
    artifacts = tuple(
        EvidenceArtifact(
            path=item["path"],
            size_bytes=item["size_bytes"],
            sha256=item["sha256"],
        )
        for item in case["evidence"]
    )
    task_text = commit_claim_snapshot(worktree, case["base_sha"], case["head_sha"])
    if _sha256(task_text.encode()) != case["task_sha256"]:
        raise BenchmarkError(f"{case['id']} task snapshot digest mismatch")
    inputs = ReviewInputs(
        repository="https://github.com/jleechanorg/worldarchitect.ai",
        workspace_path=str(worktree),
        base_sha=case["base_sha"],
        head_sha=case["head_sha"],
        tree_sha=case["tree_sha"],
        task_text=task_text,
        diff_text=str(snapshot["diff_text"]),
        changed_files=tuple(snapshot["changed_files"]),
        evidence=artifacts,
        run_id="",
    )
    try:
        validate_immutable_target(inputs)
    except Exception as exc:
        raise BenchmarkError(f"{case['id']} immutable input rejected: {exc}") from exc
    return inputs


def build_blinded_plan(
    cases: list[dict],
    *,
    seed: int,
    model: str,
    reasoning_effort: str,
    timeout_seconds: int,
) -> tuple[dict, dict]:
    """Create public arm order plus a separate reveal map."""
    if not model.strip():
        raise BenchmarkError("model is required")
    if reasoning_effort not in ("minimal", "low", "medium", "high", "xhigh"):
        raise BenchmarkError("reasoning_effort is invalid")
    if timeout_seconds <= 0:
        raise BenchmarkError("timeout_seconds must be positive")
    rng = random.Random(seed)
    runs: list[dict] = []
    mappings: list[dict] = []
    controls = {
        "model": model,
        "reasoning_effort": reasoning_effort,
        "tools": "codex-read-only",
        "timeout_seconds": timeout_seconds,
    }
    for case in cases:
        contracts = list(CONTRACTS)
        rng.shuffle(contracts)
        for index, contract in enumerate(contracts, start=1):
            arm = f"arm-{index}"
            runs.append(
                {
                    "case_id": case["id"],
                    "arm": arm,
                    "controls": dict(controls),
                    "input_order": list(INPUT_ORDER),
                }
            )
            mappings.append(
                {
                    "case_id": case["id"],
                    "arm": arm,
                    "review_contract": contract,
                }
            )
    return (
        {"schema": 1, "seed": seed, "runs": runs},
        {"schema": 1, "seed": seed, "arms": mappings},
    )


def _git(repo: pathlib.Path, *args: str) -> None:
    proc = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise BenchmarkError(proc.stderr.strip() or f"git {' '.join(args)} failed")


@contextmanager
def _detached_worktree(repo: pathlib.Path, path: pathlib.Path, head_sha: str):
    if path.exists():
        raise BenchmarkError(f"worktree path already exists: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    _git(repo, "worktree", "add", "--detach", str(path), head_sha)
    try:
        yield path
    finally:
        _git(repo, "worktree", "remove", "--force", str(path))


def _findings_only(response: str) -> str:
    try:
        return response.split("## Findings\n", 1)[1].split("\n## Commands Executed", 1)[0].strip()
    except IndexError as exc:
        raise BenchmarkError("validated response has no findings section") from exc


def _narrative_transcript(response: str) -> str:
    marker = "## Findings\n"
    if marker not in response:
        raise BenchmarkError("validated response has no narrative transcript")
    transcript = marker + response.split(marker, 1)[1]
    if any(identity in transcript for identity in CONTRACTS):
        raise BenchmarkError("review transcript exposes arm identity")
    return transcript


def _file_digest(path: str | pathlib.Path) -> str:
    return _sha256(pathlib.Path(path).read_bytes())


def _token_metrics(receipt: dict) -> dict[str, int | float | None]:
    usage = receipt.get("usage")
    if not isinstance(usage, dict):
        usage = {}
    input_tokens = usage.get("input_tokens")
    output_tokens = usage.get("output_tokens")
    total_tokens = usage.get("total_tokens")
    if total_tokens is None and isinstance(input_tokens, (int, float)) and isinstance(
        output_tokens, (int, float)
    ):
        total_tokens = input_tokens + output_tokens
    return {
        "latency_ms": round(float(receipt["duration_seconds"]) * 1000),
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": total_tokens,
    }


def run_benchmark(
    manifest: dict,
    *,
    repo: pathlib.Path,
    output_dir: pathlib.Path,
    seed: int,
    model: str,
    reasoning_effort: str,
    timeout_seconds: int,
    workers: int,
) -> None:
    """Run each pair serially while independent cases use bounded concurrency."""
    if workers <= 0:
        raise BenchmarkError("workers must be positive")
    output_dir = output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=False)
    public_plan, arm_map = build_blinded_plan(
        manifest["cases"],
        seed=seed,
        model=model,
        reasoning_effort=reasoning_effort,
        timeout_seconds=timeout_seconds,
    )
    (output_dir / "run-plan.json").write_text(
        json.dumps(public_plan, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output_dir / "private-arm-map.json").write_text(
        json.dumps(arm_map, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    cases = {case["id"]: case for case in manifest["cases"]}
    mapping = {
        (row["case_id"], row["arm"]): row["review_contract"]
        for row in arm_map["arms"]
    }
    active_cases = 0
    observed_max_cases = 0
    active_lock = threading.Lock()

    def run_case(case_id: str, worktree_path: pathlib.Path) -> dict[str, dict]:
        nonlocal active_cases, observed_max_cases
        with active_lock:
            active_cases += 1
            observed_max_cases = max(observed_max_cases, active_cases)
        try:
            case = cases[case_id]
            inputs = validate_case(worktree_path, case)
            bundle_entries: dict[str, dict] = {}
            case_runs = [
                run for run in public_plan["runs"] if run["case_id"] == case_id
            ]
            for run in case_runs:
                contract = mapping[(case_id, run["arm"])]
                request = create_review_request(
                    replace(inputs, run_id=f"{case_id}-{run['arm']}"),
                    review_contract=contract,
                )
                ctx = Context(
                    goal="controller cold-review benchmark",
                    workdir=worktree_path,
                    backend="codex",
                )
                base_argv = _gate_subprocess_args(
                    "codex", request.prompt, ctx, timeout_seconds
                )
                if base_argv is None:
                    raise BenchmarkError(
                        "codex read-only sandbox transport is unavailable"
                    )
                argv = _build_controller_codex_transport(
                    base_argv,
                    model=model,
                    reasoning_effort=reasoning_effort,
                )
                neutral = output_dir / "neutral" / case_id / run["arm"]
                neutral.mkdir(parents=True)
                raw_dir = output_dir / "raw" / case_id / run["arm"]
                result = run_controller_review(
                    request,
                    neutral_cwd=neutral,
                    output_dir=raw_dir,
                    transport_argv=tuple(argv),
                    timeout=timeout_seconds,
                )
                receipt = json.loads(
                    pathlib.Path(result.output_paths["receipt"]).read_text()
                )
                transcript = _narrative_transcript(result.response_text)
                findings_text = _findings_only(result.response_text)
                findings = [] if not findings_text else [{
                    "observed_id": f"obs-{_sha256(findings_text.encode())[:16]}",
                    "text": findings_text,
                }]
                case_digest_input = {
                    "case_id": case_id,
                    "base_sha": case["base_sha"],
                    "head_sha": case["head_sha"],
                    "diff_sha256": case["diff_sha256"],
                }
                record = {
                    "schema": "cold-review-raw-v1",
                    "case_id": case_id,
                    "arm": run["arm"],
                    "review_contract": contract,
                    "case_sha256": _sha256(_canonical(case_digest_input).encode()),
                    "task_sha256": case["task_sha256"],
                    "controls": run["controls"],
                    "input_order": run["input_order"],
                }
                for artifact in (
                    "prompt", "envelope", "response", "transport", "receipt", "findings"
                ):
                    record[f"{artifact}_sha256"] = _file_digest(
                        result.output_paths[artifact]
                    )
                (raw_dir / "run-record.json").write_text(
                    json.dumps(record, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )
                blinded = {
                    "schema": 1,
                    "case_id": case_id,
                    "arm": run["arm"],
                    "verdict": result.review.verdict,
                    "findings": findings_text,
                    "response_sha256": result.review.response_sha256,
                    "duration_seconds": receipt["duration_seconds"],
                    "usage": receipt.get("usage", {}),
                    "head_sha": case["head_sha"],
                    "diff_sha256": case["diff_sha256"],
                }
                blinded_dir = output_dir / "blinded" / case_id
                blinded_dir.mkdir(parents=True, exist_ok=True)
                (blinded_dir / f"{run['arm']}.json").write_text(
                    json.dumps(blinded, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )
                bundle_entries[run["arm"]] = {
                    "case_id": case_id,
                    "base_sha": case["base_sha"],
                    "head_sha": case["head_sha"],
                    "diff_sha256": case["diff_sha256"],
                    "case_sha256": record["case_sha256"],
                    "diff": inputs.diff_text,
                    "transcript": transcript,
                    "transcript_sha256": _sha256(transcript.encode()),
                    "review_verdict": result.review.verdict.upper(),
                    "findings": [
                        {"id": item["observed_id"], "text": item["text"]}
                        for item in findings
                    ],
                    "metrics": _token_metrics(receipt),
                }
                validate_case(worktree_path, case)
            return bundle_entries
        finally:
            with active_lock:
                active_cases -= 1

    entries_by_case: dict[str, dict[str, dict]] = {}
    with ExitStack() as stack:
        worktrees = {
            case_id: stack.enter_context(
                _detached_worktree(
                    repo,
                    output_dir / "worktrees" / case_id,
                    case["head_sha"],
                )
            )
            for case_id, case in cases.items()
        }
        max_workers = min(workers, len(cases))
        with concurrent.futures.ThreadPoolExecutor(max_workers=max_workers) as pool:
            futures = {
                case_id: pool.submit(run_case, case_id, worktrees[case_id])
                for case_id in cases
            }
            for case_id in cases:
                entries_by_case[case_id] = futures[case_id].result()

    for arm in ("arm-1", "arm-2"):
        bundle = {
            "schema_version": "cold-review-run-v1",
            "run_id": f"seed-{seed}-{arm}",
            "cases": [entries_by_case[case_id][arm] for case_id in cases],
        }
        (output_dir / f"blinded-{arm}-bundle.json").write_text(
            json.dumps(bundle, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    (output_dir / "concurrency.json").write_text(
        json.dumps(
            {
                "observed_max_cases": observed_max_cases,
                "requested_workers": workers,
            },
            indent=2,
            sort_keys=True,
        ) + "\n",
        encoding="utf-8",
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="run_controller_cold_review.py")
    parser.add_argument("command", choices=("validate", "run"))
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--repo", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--seed", type=int, default=86038618)
    parser.add_argument("--model")
    parser.add_argument("--reasoning-effort", default="high")
    parser.add_argument("--timeout", type=int, default=1200)
    parser.add_argument("--workers", type=int, default=DEFAULT_WORKERS)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        manifest = load_manifest(args.manifest)
        if args.command == "run":
            if args.output is None or not args.model:
                raise BenchmarkError("run requires --output and explicit --model")
            run_benchmark(
                manifest,
                repo=args.repo.resolve(),
                output_dir=args.output,
                seed=args.seed,
                model=args.model,
                reasoning_effort=args.reasoning_effort,
                timeout_seconds=args.timeout,
                workers=args.workers,
            )
        else:
            with tempfile.TemporaryDirectory(prefix="cold-review-validate-") as temp:
                for case in manifest["cases"]:
                    path = pathlib.Path(temp) / case["id"]
                    with _detached_worktree(args.repo.resolve(), path, case["head_sha"]):
                        validate_case(path, case)
        print(json.dumps({"status": "valid", "cases": len(manifest["cases"])}))
        return 0
    except (BenchmarkError, OSError, UnicodeError) as exc:
        print(json.dumps({"status": "invalid", "error": str(exc)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
