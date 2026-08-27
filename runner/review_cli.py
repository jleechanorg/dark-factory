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
from dataclasses import replace

from .handler_core import Context
from .handler_dispatch import (
    _controller_codex_args,
    _gate_subprocess_args,
    _gate_subprocess_env,
)
from .handler_sandbox import (
    _cleanup_controller_runtime,
    _create_controller_runtime,
)
from .review_controller import (
    EvidenceArtifact,
    ReviewContractError,
    ReviewInputs,
    build_controller_receipt,
    create_review_request,
    ensure_review_pass_allowed,
    parse_codex_jsonl,
    run_controller_review,
    validate_execution_receipts,
    validate_immutable_target,
    validate_review_response,
    validate_workspace_path,
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
    parser.add_argument("--timeout", type=int, default=1200)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    # Keep the lexical path until immutable-target validation. Resolving here
    # would erase a symlinked parent and bypass its containment check.
    lexical_workdir = args.workdir.expanduser()
    output_dir = args.output_dir.expanduser().resolve()
    claimed_output = False
    request = None
    runtime = None
    try:
        try:
            from .handler_sandbox import _holdout_denied_paths

            holdout_roots = tuple(str(path) for path in _holdout_denied_paths())
        except (OSError, RuntimeError):
            holdout_roots = ()
        # This must precede every git query and every read from the target.
        workdir = validate_workspace_path(
            str(lexical_workdir),
            holdout_roots=holdout_roots,
        )
        base_sha = _full_revision(workdir, args.base_sha)
        head_sha = _full_revision(workdir, args.head_sha)
        _require_review_range(workdir, base_sha, head_sha)
        tree_sha = _git(workdir, "rev-parse", f"{head_sha}^{{tree}}").lower()
        changed_files_text = _git(
            workdir,
            "diff",
            "--name-only",
            f"{base_sha}..{head_sha}",
            allow_empty=True,
        )
        task_path = args.task_file.expanduser().resolve(strict=True)
        task_text = task_path.read_text(encoding="utf-8")
        try:
            repository = _git(workdir, "config", "--get", "remote.origin.url")
        except ReviewContractError:
            repository = workdir.name

        # Validate the lexical path before reading any target-owned evidence.
        # Keep that raw spelling in the request so a later symlink-parent swap
        # is observable when the target is revalidated after review.
        inputs = ReviewInputs(
            repository=repository,
            workspace_path=str(lexical_workdir),
            base_sha=base_sha,
            head_sha=head_sha,
            tree_sha=tree_sha,
            task_text=task_text,
            changed_files=tuple(
                line for line in changed_files_text.splitlines() if line.strip()
            ),
            evidence=(),
            run_id=f"review-{int(time.time())}",
        )
        workdir = validate_immutable_target(inputs, holdout_roots=holdout_roots)
        evidence = _evidence_artifacts(workdir, args.evidence)
        inputs = replace(inputs, evidence=evidence)
        # Re-check after collecting evidence so the raw path and every
        # evidence path are bound before the request is emitted.
        workdir = validate_immutable_target(inputs, holdout_roots=holdout_roots)
        try:
            output_dir.relative_to(workdir)
        except ValueError:
            pass
        else:
            parser.error("--output-dir must be outside the reviewed workspace")
        before = _snapshot(workdir, base_sha, head_sha)
        request = create_review_request(inputs)
        output_dir.mkdir(parents=True, mode=0o700, exist_ok=False)
        claimed_output = True
        prompt_path = output_dir / "prompt.txt"
        envelope_path = output_dir / "envelope.json"
        response_path = output_dir / "reviewer.output.md"
        transport_path = output_dir / "transport.jsonl"
        receipt_path = output_dir / "controller-receipt.json"
        prompt_bytes = request.prompt.encode("utf-8")
        envelope_bytes = request.envelope_json.encode("utf-8")
        _write_atomic(prompt_path, prompt_bytes)
        _write_atomic(envelope_path, envelope_bytes)

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
        stdin_text = None
        transport_is_jsonl = args.backend == "codex"
        if transport_is_jsonl:
            try:
                runtime = _create_controller_runtime()
                command = _controller_codex_args(
                    command,
                    read_only_path=workdir,
                    writable_path=runtime.codex_home,
                )
            except ValueError as exc:
                raise ReviewContractError(
                    "codex review command did not contain the codex executable"
                ) from exc
            stdin_text = request.prompt
        proc = subprocess.run(
            command,
            cwd=output_dir,
            capture_output=True,
            text=True,
            input=stdin_text,
            timeout=args.timeout,
            check=False,
            env=runtime.env if runtime is not None else _gate_subprocess_env(args.backend),
        )
        raw_transport = proc.stdout
        command_receipts = ()
        response = proc.stdout.strip()
        parse_error = ""
        if transport_is_jsonl and proc.returncode == 0:
            try:
                response, command_receipts = parse_codex_jsonl(raw_transport)
            except ReviewContractError as exc:
                response = ""
                parse_error = str(exc)
        response_bytes = response.encode("utf-8")
        _write_atomic(response_path, response_bytes)
        if transport_is_jsonl:
            _write_atomic(transport_path, raw_transport.encode("utf-8"))

        contract_error = parse_error
        verdict = "invalid"
        response_sha256 = _sha256(response_bytes)
        if proc.returncode == 0 and not contract_error:
            try:
                validated = validate_review_response(response, request)
                validate_execution_receipts(command_receipts, validated)
                ensure_review_pass_allowed(
                    validated,
                    backend=args.backend,
                    execution_path="cli",
                )
                verdict = validated.verdict
                response_sha256 = validated.response_sha256
            except ReviewContractError as exc:
                contract_error = str(exc)
        elif proc.returncode != 0:
            contract_error = f"review backend exited with {proc.returncode}"

        # Revalidate the original lexical path before the post-review snapshot;
        # resolving only once would let a symlink-parent swap redirect the
        # final check to an unreviewed target.
        workdir = validate_immutable_target(inputs, holdout_roots=holdout_roots)
        after = _snapshot(workdir, base_sha, head_sha)
        _verify_evidence(workdir, evidence)
        # head + tree pin the whole reviewed state; with a fixed base_sha the
        # reviewed change cannot differ while both still match.
        if (
            before["head_sha"] != after["head_sha"]
            or before["tree_sha"] != after["tree_sha"]
        ):
            contract_error = "reviewed repository changed during cold review"
            verdict = "invalid"

        receipt = build_controller_receipt(
            request,
            lane="controller",
            backend=args.backend,
            neutral_cwd=output_dir,
            output_dir=output_dir,
            transport_argv=tuple(str(arg) for arg in command),
            transport_text=raw_transport if transport_is_jsonl else "",
            response_text=response,
            backend_returncode=proc.returncode,
            status="valid" if not contract_error else "invalid",
            verdict=verdict,
            contract_error=contract_error,
            receipts=command_receipts,
        )
        receipt.update(
            {
                "prompt_path": prompt_path.name,
                "envelope_path": envelope_path.name,
                "response_path": response_path.name,
                "transport_path": transport_path.name if transport_is_jsonl else "",
            }
        )
        _write_atomic(
            receipt_path,
            (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode("utf-8"),
        )
        if runtime is not None:
            _cleanup_controller_runtime(runtime.run_dir)
            runtime = None
        print(json.dumps(receipt, indent=2, sort_keys=True))
        if contract_error:
            return 1
        return 0 if verdict == "pass" else 2
    except (OSError, UnicodeError, subprocess.TimeoutExpired, ReviewContractError) as exc:
        if runtime is not None:
            try:
                _cleanup_controller_runtime(runtime.run_dir)
            except Exception:
                pass
        contract_error = f"{type(exc).__name__}: {exc}"
        if request is not None:
            process = locals().get("proc")
            returncode = getattr(process, "returncode", -1)
            payload = build_controller_receipt(
                request,
                lane="controller",
                backend=args.backend,
                neutral_cwd=output_dir,
                output_dir=output_dir,
                transport_argv=tuple(
                    str(arg) for arg in locals().get("command", ())
                ),
                transport_text=str(locals().get("raw_transport", "")),
                response_text=str(locals().get("response", "")),
                backend_returncode=returncode
                if isinstance(returncode, int)
                else -1,
                status="invalid",
                verdict="invalid",
                contract_error=contract_error,
                receipts=locals().get("command_receipts", ()),
            )
        else:
            payload = {
                "schema": 1,
                "status": "invalid",
                "verdict": "invalid",
                "contract_error": contract_error,
            }
        if claimed_output:
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


__all__ = ["main", "run_controller_review"]
