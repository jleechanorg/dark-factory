"""Binary-owned cold-review command.

This command freezes repository inputs, builds the source-owned review
contract, runs one existing reviewer backend, validates the response, and
writes a digest-bound receipt. The caller selects inputs and backend; it
cannot supply or replace the review authority.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import time

from .handler_core import Context
from .handler_dispatch import (
    _controller_codex_args,
    _gate_subprocess_args,
    _gate_subprocess_env,
)
from .review_controller import (
    EvidenceArtifact,
    ReviewContractError,
    ReviewInputs,
    create_review_request,
    run_controller_review,
    validate_immutable_target,
)


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _write_atomic(path: pathlib.Path, data: bytes) -> None:
    """Write one controller artifact without following a target symlink."""
    fd, temp_name = tempfile.mkstemp(prefix=".review-", dir=path.parent)
    temp_path = pathlib.Path(temp_name)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temp_path, path)
    except Exception:
        try:
            temp_path.unlink()
        except OSError:
            pass
        raise


def _git(workdir: pathlib.Path, *args: str, allow_empty: bool = False) -> str:
    proc = subprocess.run(
        ["git", "-C", str(workdir), *args],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if proc.returncode != 0 or (not allow_empty and not proc.stdout.strip()):
        detail = proc.stderr.strip() or proc.stdout.strip() or "no output"
        raise ReviewContractError(f"git {' '.join(args)} failed: {detail}")
    return proc.stdout.rstrip("\n")


def _full_revision(workdir: pathlib.Path, value: str) -> str:
    return _git(workdir, "rev-parse", f"{value}^{{commit}}").lower()


def _evidence_artifacts(
    workdir: pathlib.Path,
    paths: list[pathlib.Path],
) -> tuple[EvidenceArtifact, ...]:
    root = workdir.resolve()
    artifacts: list[EvidenceArtifact] = []
    for supplied in paths:
        path = supplied if supplied.is_absolute() else root / supplied
        resolved = path.resolve(strict=True)
        try:
            relative = resolved.relative_to(root).as_posix()
        except ValueError as exc:
            raise ReviewContractError(
                f"evidence must be inside the reviewed workspace: {supplied}"
            ) from exc
        if not resolved.is_file():
            raise ReviewContractError(f"evidence is not a file: {supplied}")
        data = resolved.read_bytes()
        artifacts.append(
            EvidenceArtifact(
                path=relative,
                size_bytes=len(data),
                sha256=_sha256(data),
            )
        )
    return tuple(artifacts)


def _verify_evidence(
    workdir: pathlib.Path,
    artifacts: tuple[EvidenceArtifact, ...],
) -> None:
    root = workdir.resolve()
    for artifact in artifacts:
        path = (root / artifact.path).resolve()
        try:
            path.relative_to(root)
        except ValueError as exc:
            raise ReviewContractError("evidence escaped the reviewed workspace") from exc
        if not path.is_file():
            raise ReviewContractError(f"evidence disappeared: {artifact.path}")
        data = path.read_bytes()
        if len(data) != artifact.size_bytes:
            raise ReviewContractError(f"evidence size changed: {artifact.path}")
        if _sha256(data) != artifact.sha256:
            raise ReviewContractError(f"evidence digest changed: {artifact.path}")


def _snapshot(workdir: pathlib.Path, base_sha: str, head_sha: str) -> dict[str, object]:
    status = _git(
        workdir,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        allow_empty=True,
    )
    if status:
        raise ReviewContractError("reviewed workspace must be clean")
    actual_head = _full_revision(workdir, "HEAD")
    if actual_head != head_sha:
        raise ReviewContractError(
            f"workspace HEAD mismatch: expected {head_sha}, observed {actual_head}"
        )
    tree_sha = _git(workdir, "rev-parse", f"{head_sha}^{{tree}}").lower()
    diff_text = _git(
        workdir,
        "diff",
        "--no-ext-diff",
        "--binary",
        f"{base_sha}..{head_sha}",
        allow_empty=True,
    )
    changed_files_text = _git(
        workdir,
        "diff",
        "--name-only",
        f"{base_sha}..{head_sha}",
        allow_empty=True,
    )
    return {
        "head_sha": actual_head,
        "tree_sha": tree_sha,
        "diff_text": diff_text,
        "diff_sha256": _sha256(diff_text.encode("utf-8")),
        "changed_files": tuple(
            line for line in changed_files_text.splitlines() if line.strip()
        ),
    }


def _require_review_range(
    workdir: pathlib.Path,
    base_sha: str,
    head_sha: str,
) -> None:
    if base_sha == head_sha:
        raise ReviewContractError("base_sha and head_sha must be different")
    proc = subprocess.run(
        ["git", "-C", str(workdir), "merge-base", "--is-ancestor", base_sha, head_sha],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if proc.returncode != 0:
        raise ReviewContractError("base_sha must be an ancestor of head_sha")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="dark-factory review")
    parser.add_argument("--workdir", type=pathlib.Path, default=pathlib.Path.cwd())
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--head-sha", required=True)
    parser.add_argument("--task-file", type=pathlib.Path, required=True)
    parser.add_argument("--evidence", type=pathlib.Path, action="append", default=[])
    parser.add_argument("--output-dir", type=pathlib.Path, required=True)
    parser.add_argument(
        "--backend",
        choices=["codex", "claude", "agy", "minimax", "claude-sonnet"],
        default="codex",
    )
    parser.add_argument(
        "--review-contract",
        choices=["cold-review-v1", "cold-review-v2"],
        default="cold-review-v1",
    )
    parser.add_argument("--timeout", type=int, default=1200)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    workdir = args.workdir.expanduser().resolve()
    output_dir = args.output_dir.expanduser().resolve()
    try:
        output_dir.relative_to(workdir)
    except ValueError:
        pass
    else:
        parser.error("--output-dir must be outside the reviewed workspace")
    claimed_output = False
    try:
        base_sha = _full_revision(workdir, args.base_sha)
        head_sha = _full_revision(workdir, args.head_sha)
        _require_review_range(workdir, base_sha, head_sha)
        before = _snapshot(workdir, base_sha, head_sha)
        task_path = args.task_file.expanduser().resolve(strict=True)
        task_text = task_path.read_text(encoding="utf-8")
        evidence = _evidence_artifacts(workdir, args.evidence)
        try:
            repository = _git(workdir, "config", "--get", "remote.origin.url")
        except ReviewContractError:
            repository = workdir.name

        inputs = ReviewInputs(
            repository=repository,
            workspace_path=str(workdir),
            base_sha=base_sha,
            head_sha=head_sha,
            tree_sha=str(before["tree_sha"]),
            task_text=task_text,
            diff_text=str(before["diff_text"]),
            changed_files=tuple(before["changed_files"]),
            evidence=evidence,
            run_id=f"review-{int(time.time())}",
        )
        try:
            from .handler_sandbox import _holdout_denied_paths

            holdout_roots = tuple(
                str(path) for path in _holdout_denied_paths()
            )
        except Exception:
            holdout_roots = ()
        validate_immutable_target(inputs, holdout_roots=holdout_roots)
        request = create_review_request(
            inputs, review_contract=args.review_contract
        )

        ctx = Context(
            goal="controller-owned cold review",
            workdir=workdir,
            backend=args.backend,
        )
        command = _gate_subprocess_args(
            args.backend,
            request.prompt,
            ctx,
            args.timeout,
        )
        if command is None:
            raise ReviewContractError(
                f"review backend could not be launched: {args.backend}"
            )
        if args.backend == "codex":
            try:
                command = _controller_codex_args(command)
            except ValueError as exc:
                raise ReviewContractError(
                    "codex review command did not contain the codex executable"
                ) from exc

        # The controller owns transport execution, response/receipt validation,
        # and the canonical artifact set for both CLI and graph review lanes.
        claimed_output = True
        result = run_controller_review(
            request,
            neutral_cwd=output_dir,
            output_dir=output_dir,
            transport_argv=tuple(command),
            transport_env=_gate_subprocess_env(args.backend),
            transport_is_jsonl=args.backend == "codex",
            timeout=args.timeout,
        )

        after = _snapshot(workdir, base_sha, head_sha)
        _verify_evidence(workdir, evidence)
        contract_error = ""
        verdict = result.review.verdict
        if (
            before["head_sha"] != after["head_sha"]
            or before["tree_sha"] != after["tree_sha"]
            or before["diff_sha256"] != after["diff_sha256"]
        ):
            contract_error = "reviewed repository changed during cold review"
            verdict = "invalid"

        receipt_path = output_dir / "controller-receipt.json"
        shared_receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        backend_returncode = shared_receipt["exit_code"]
        if backend_returncode != 0:
            contract_error = f"review backend exited with {backend_returncode}"
            verdict = "invalid"
        prompt_bytes = request.prompt.encode("utf-8")
        receipt = {
            "schema": 1,
            "status": "valid" if not contract_error else "invalid",
            "verdict": verdict,
            "contract_error": contract_error,
            "backend": args.backend,
            "backend_returncode": backend_returncode,
            "base_sha": base_sha,
            "head_sha": head_sha,
            "tree_sha": before["tree_sha"],
            "diff_sha256": before["diff_sha256"],
            "review_contract": request.review_contract,
            "prompt_id": request.prompt_id,
            "prompt_sha256": _sha256(prompt_bytes),
            "prompt_payload_sha256": request.prompt_sha256,
            "envelope_sha256": request.envelope_sha256,
            "response_sha256": result.review.response_sha256,
            "transport_sha256": _sha256(result.transport_text.encode("utf-8")),
            "command_receipts": [
                {
                    "command": item.command,
                    "exit_code": item.exit_code,
                    "output_sha256": item.output_sha256,
                }
                for item in result.receipts
            ],
            "prompt_path": "prompt.txt",
            "envelope_path": "envelope.json",
            "response_path": "reviewer.output.md",
            "transport_path": "transport.jsonl",
        }
        _write_atomic(
            receipt_path,
            (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode("utf-8"),
        )
        print(json.dumps(receipt, indent=2, sort_keys=True))
        if contract_error:
            return 1
        return 0 if verdict == "pass" else 2
    except (OSError, UnicodeError, subprocess.TimeoutExpired, ReviewContractError) as exc:
        payload = {
            "schema": 1,
            "status": "invalid",
            "verdict": "invalid",
            "contract_error": f"{type(exc).__name__}: {exc}",
        }
        if claimed_output:
            try:
                existing_receipt = json.loads(
                    (output_dir / "controller-receipt.json").read_text(
                        encoding="utf-8"
                    )
                )
                backend_returncode = existing_receipt.get("exit_code")
                if isinstance(backend_returncode, int) and not isinstance(
                    backend_returncode, bool
                ):
                    payload["backend_returncode"] = backend_returncode
            except (OSError, UnicodeError, json.JSONDecodeError):
                pass
            try:
                _write_atomic(
                    output_dir / "controller-receipt.json",
                    (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode(
                        "utf-8"
                    ),
                )
            except OSError:
                pass
        print(json.dumps(payload, indent=2, sort_keys=True))
        return 1


__all__ = ["main"]
