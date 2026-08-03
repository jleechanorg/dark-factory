"""Evidence audit gate + git/gh evidence helpers.

Owns:
  * `_git_config_origin_url` — ``git config --get remote.origin.url``.
  * `_git_merge_base` — try ``origin/main``/``main``/``master``/``origin/master``.
  * `_git_diff_stat` — ``git diff --stat <base_sha>``.
  * `_gh_pr_body` — ``gh pr view <pr> --json body -q .body``.
  * `_compute_evidence_sha` — sha256 over listed evidence files.
  * `_verify_evidence_freshness` — grep evidence files for HEAD SHA.
  * `_check_unresolved_review_state` — ``gh pr view`` for reviewDecision.
  * `_is_replacement_or_deletion_work` — diff_summary + pr_description keyword
    + net-LOC heuristic.
  * `_gate_audit` — evidence-audit gate: missing/stale/unresolved/replacement/
    non-replacement path.
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import re
import contextlib
import tempfile
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Optional

import runner.handlers as _handlers_shim

from .handler_core import Result, _gate_strict_flag
from .subprocess_control import BoundedProcessBytesResult, run_bounded_process_bytes

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context




# Vendor-neutral default evidence filenames probed when neither ``state["evidence_paths"]``
# nor the node ``evidence_paths`` attribute is set. Project-local aliases (e.g.
# ``gemini_http_request_responses.jsonl``) live in ``<workdir>/.dark-factory/evidence.yaml``
# under the ``aliases:`` key — see ``_load_evidence_aliases`` below.
DEFAULT_EVIDENCE_FILENAMES: tuple[str, ...] = (
    "llm_request_responses.jsonl",
    "llm_responses.jsonl",
    "evidence.jsonl",
)

_OPERATOR_MANIFEST_MAX_BYTES = 1 << 20
_OPERATOR_TIMEOUT_MIN_SECONDS = 5
_OPERATOR_TIMEOUT_MAX_SECONDS = 1800
_OPERATOR_LANES = frozenset({"worker_safe", "operator_unwrapped"})
_OPERATOR_CLASSIFICATIONS = frozenset({"required"})
_OPERATOR_EXECUTABLE_ALLOWLIST = frozenset({"/usr/bin/git"})
_OPERATOR_ID_RE = re.compile(r"[a-z][a-z0-9_-]{0,63}\Z")
_OPERATOR_RAW_STREAM_MAX_BYTES = 8 << 20
_OPERATOR_RECEIPT_MAX_BYTES = 1 << 20

_OPERATOR_BUILTINS: tuple[OperatorCommand, ...] = ()  # populated after dataclass declaration


def _target_provenance(workdir: pathlib.Path) -> tuple[str, str]:
    """Return trusted HEAD plus a deterministic target-workspace fingerprint."""
    root = workdir.resolve()
    head = subprocess.run(
        ["/usr/bin/git", "rev-parse", "HEAD"],
        cwd=root,
        capture_output=True,
        check=True,
        timeout=30,
    ).stdout.strip().decode("ascii", errors="strict").lower()
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", head):
        raise ValueError("target HEAD is malformed")
    pathspec = ("--", ".", ":(exclude)evidence/operator-verification.json")
    commands = (
        ("/usr/bin/git", "status", "--porcelain=v1", "-z", "--untracked-files=all", *pathspec),
        ("/usr/bin/git", "diff", "--binary", "HEAD", *pathspec),
        ("/usr/bin/git", "diff", "--binary", "--cached", "HEAD", *pathspec),
    )
    digest = hashlib.sha256()
    for argv in commands:
        result = subprocess.run(
            argv, cwd=root, capture_output=True, check=True, timeout=30
        )
        digest.update(len(result.stdout).to_bytes(8, "big"))
        digest.update(result.stdout)
    return head, digest.hexdigest()


def _reject_unknown_keys(value: dict, allowed: frozenset[str], label: str) -> None:
    unknown = set(value) - allowed
    if unknown:
        raise ValueError(f"{label} has unknown keys: {sorted(unknown)}")


def _operator_executable(workdir: pathlib.Path, raw: str) -> tuple[str, tuple[str, ...]]:
    if "\0" in raw:
        raise ValueError("operator executable contains NUL")
    candidate = pathlib.Path(raw)
    if candidate.is_absolute():
        if raw not in _OPERATOR_EXECUTABLE_ALLOWLIST:
            raise ValueError("operator executable is not an exact allowlist entry")
        if candidate.is_symlink() or not candidate.is_file() or not os.access(candidate, os.X_OK):
            raise ValueError("allowlisted operator executable is unavailable")
        return raw, ()
    if raw == "@runner-python":
        executable = pathlib.Path(sys.executable).resolve()
        if executable.is_symlink() or not executable.is_file() or not os.access(executable, os.X_OK):
            raise ValueError("runner Python executable is unavailable")
        return str(executable), ("@runner-python",)
    raise ValueError("operator executable must be a runner-owned token or exact allowlist entry")


@dataclass(frozen=True, slots=True)
class OperatorCommand:
    id: str
    requested_argv: tuple[str, ...]
    effective_argv: tuple[str, ...]
    lane: str
    timeout_seconds: int
    classification: str
    transform_chain: tuple[str, ...] = ()

    @property
    def argv(self) -> tuple[str, ...]:
        return self.effective_argv


@dataclass(frozen=True, slots=True)
class OperatorExclusion:
    id: str
    classification: str
    reason: str


@dataclass(frozen=True, slots=True)
class OperatorManifest:
    schema_version: int
    commands: tuple[OperatorCommand, ...]
    exclusions: tuple[OperatorExclusion, ...]
    path: pathlib.Path
    raw_bytes: bytes


@contextlib.contextmanager
def _trusted_operator_snapshot(
    workdir: pathlib.Path, trusted_head: str, parent: pathlib.Path
):
    snapshot = pathlib.Path(tempfile.mkdtemp(prefix="trusted-snapshot-", dir=parent))
    snapshot.rmdir()
    subprocess.run(
        ["/usr/bin/git", "worktree", "add", "--detach", str(snapshot), trusted_head],
        cwd=workdir,
        capture_output=True,
        check=True,
        timeout=60,
    )
    try:
        yield snapshot
    finally:
        subprocess.run(
            ["/usr/bin/git", "worktree", "remove", "--force", str(snapshot)],
            cwd=workdir,
            capture_output=True,
            check=False,
            timeout=60,
        )


_OPERATOR_BUILTINS = (
    OperatorCommand(
        "git-head",
        ("/usr/bin/git", "rev-parse", "HEAD"),
        ("/usr/bin/git", "rev-parse", "HEAD"),
        "operator_unwrapped",
        30,
        "required",
    ),
    OperatorCommand(
        "git-diff-names",
        ("/usr/bin/git", "diff", "--name-only", "origin/main..HEAD"),
        ("/usr/bin/git", "diff", "--name-only", "origin/main..HEAD"),
        "operator_unwrapped",
        30,
        "required",
    ),
    OperatorCommand(
        "git-diff-check",
        ("/usr/bin/git", "diff", "--check", "origin/main..HEAD"),
        ("/usr/bin/git", "diff", "--check", "origin/main..HEAD"),
        "operator_unwrapped",
        30,
        "required",
    ),
    OperatorCommand(
        "git-status",
        ("/usr/bin/git", "status", "--porcelain=v1"),
        ("/usr/bin/git", "status", "--porcelain=v1"),
        "operator_unwrapped",
        30,
        "required",
    ),
)
_OPERATOR_FINAL_HEAD = OperatorCommand(
    "git-head-final",
    ("/usr/bin/git", "rev-parse", "HEAD"),
    ("/usr/bin/git", "rev-parse", "HEAD"),
    "operator_unwrapped",
    30,
    "required",
)


def _operator_log_root() -> pathlib.Path:
    return pathlib.Path.home() / ".dark-factory" / "operator-verification"


def _safe_operator_component(raw: str, fallback: str) -> str:
    normalized = re.sub(r"[^A-Za-z0-9_.-]+", "_", raw).strip("._")
    return normalized[:80] or fallback


def _private_directory(path: pathlib.Path) -> None:
    if path.is_symlink():
        raise OSError(f"private operator path is a symlink: {path}")
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.chmod(0o700)


def _write_private_bytes(path: pathlib.Path, data: bytes) -> None:
    if path.is_symlink():
        raise OSError(f"private operator log is a symlink: {path}")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, 0o600)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except Exception:
        try:
            path.unlink()
        except OSError:
            pass
        raise


def _stream_reference(path: pathlib.Path, data: bytes) -> dict[str, object]:
    return {
        "path": str(path),
        "sha256": hashlib.sha256(data).hexdigest(),
        "size_bytes": len(data),
    }


def _command_receipt(
    command: OperatorCommand,
    result: BoundedProcessBytesResult,
    *,
    cwd: pathlib.Path,
    duration_ms: int,
    stdout_path: pathlib.Path,
    stderr_path: pathlib.Path,
) -> dict[str, object]:
    return {
        "id": command.id,
        "lane": command.lane,
        "classification": command.classification,
        "requested_argv": list(command.requested_argv),
        "effective_argv": list(command.effective_argv),
        "transform_chain": list(command.transform_chain),
        "cwd": str(cwd),
        "exit_code": result.returncode,
        "timed_out": result.timed_out,
        "output_overflowed": result.output_overflowed,
        "process_group_cleanup": result.process_group_cleanup,
        "duration_ms": duration_ms,
        "stdout": _stream_reference(stdout_path, result.stdout),
        "stderr": _stream_reference(stderr_path, result.stderr),
    }


def _write_operator_receipt(path: pathlib.Path, receipt: dict[str, object]) -> str:
    encoded = (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(encoded) > _OPERATOR_RECEIPT_MAX_BYTES:
        raise ValueError("operator verification receipt exceeds size bound")
    if path.is_symlink() or path.parent.is_symlink():
        raise OSError("operator verification receipt path is a symlink")
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    if temp.exists() or temp.is_symlink():
        temp.unlink()
    _write_private_bytes(temp, encoded)
    os.replace(temp, path)
    path.chmod(0o600)
    return hashlib.sha256(encoded).hexdigest()


def _validate_operator_receipt(
    path: pathlib.Path,
    expected_head: str,
    *,
    expected_receipt: dict[str, object] | None = None,
) -> str:
    if path.is_symlink() or not path.is_file():
        raise ValueError("operator verification receipt is missing or a symlink")
    encoded = path.read_bytes()
    if len(encoded) > _OPERATOR_RECEIPT_MAX_BYTES:
        raise ValueError("operator verification receipt exceeds size bound")
    receipt = json.loads(encoded)
    if receipt.get("schema_version") != 2 or receipt.get("target_head_sha") != expected_head:
        raise ValueError("operator verification receipt/head mismatch")
    if expected_receipt is not None and receipt != expected_receipt:
        raise ValueError("operator verification receipt was tampered after creation")
    private_root = _operator_log_root().resolve()
    for command in receipt.get("commands", []):
        requested = command.get("requested_argv")
        effective = command.get("effective_argv")
        transforms = command.get("transform_chain")
        if not isinstance(requested, list) or not requested:
            raise ValueError("operator verification requested argv is invalid")
        if not isinstance(effective, list) or not effective:
            raise ValueError("operator verification effective argv is invalid")
        if requested != effective and not transforms:
            raise ValueError("operator verification argv transform is missing")
        for stream_name in ("stdout", "stderr"):
            reference = command.get(stream_name, {})
            raw_path = pathlib.Path(str(reference.get("path", "")))
            try:
                raw_path.resolve().relative_to(private_root)
            except ValueError as exc:
                raise ValueError("operator verification raw log escapes private root") from exc
            if raw_path.is_symlink() or not raw_path.is_file():
                raise ValueError("operator verification raw log is missing or a symlink")
            data = raw_path.read_bytes()
            if len(data) != reference.get("size_bytes"):
                raise ValueError("operator verification raw-log size mismatch")
            if hashlib.sha256(data).hexdigest() != reference.get("sha256"):
                raise ValueError("operator verification raw-log digest mismatch")
    return hashlib.sha256(encoded).hexdigest()


def _load_operator_manifest(
    workdir: pathlib.Path, *, trusted_head: str | None = None
) -> OperatorManifest:
    manifest_path = workdir / ".dark-factory" / "evidence.yaml"
    if trusted_head is None:
        if manifest_path.is_symlink() or not manifest_path.is_file():
            raise ValueError("operator verification manifest is missing or a symlink")
        raw = manifest_path.read_bytes()
    else:
        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", trusted_head):
            raise ValueError("trusted manifest commit is malformed")
        raw = subprocess.run(
            ["/usr/bin/git", "show", f"{trusted_head}:.dark-factory/evidence.yaml"],
            cwd=workdir,
            capture_output=True,
            check=True,
            timeout=30,
        ).stdout
    if len(raw) > _OPERATOR_MANIFEST_MAX_BYTES:
        raise ValueError("operator verification manifest exceeds size bound")
    import yaml

    try:
        data = yaml.safe_load(raw) or {}
    except yaml.YAMLError as exc:
        raise ValueError("operator verification manifest is malformed YAML") from exc
    if not isinstance(data, dict):
        raise ValueError("operator verification manifest must be a mapping")
    _reject_unknown_keys(data, frozenset({"aliases", "operator_verification"}), "manifest")
    operator = data.get("operator_verification") if isinstance(data, dict) else None
    if not isinstance(operator, dict) or operator.get("schema_version") != 1:
        raise ValueError("operator_verification schema_version must be 1")
    _reject_unknown_keys(
        operator,
        frozenset({"schema_version", "commands", "exclusions"}),
        "operator_verification",
    )
    raw_commands = operator.get("commands")
    if not isinstance(raw_commands, list) or not raw_commands:
        raise ValueError("operator_verification commands must be a non-empty list")

    commands: list[OperatorCommand] = []
    seen_ids: set[str] = set()
    for entry in raw_commands:
        if not isinstance(entry, dict):
            raise ValueError("operator command must be a mapping")
        _reject_unknown_keys(
            entry,
            frozenset({"id", "argv", "lane", "timeout_seconds", "classification"}),
            "operator command",
        )
        command_id = entry.get("id")
        if not isinstance(command_id, str) or not _OPERATOR_ID_RE.fullmatch(command_id):
            raise ValueError("operator command id is invalid")
        if command_id in seen_ids:
            raise ValueError("operator verification ids must be unique")
        seen_ids.add(command_id)
        argv = entry.get("argv") if isinstance(entry, dict) else None
        if (
            not isinstance(argv, list)
            or not argv
            or not all(isinstance(v, str) and v and "\0" not in v for v in argv)
        ):
            raise ValueError("operator command argv must be a non-empty string array")
        lane = entry.get("lane")
        if lane not in _OPERATOR_LANES:
            raise ValueError("operator command lane is invalid")
        classification = entry.get("classification")
        if classification not in _OPERATOR_CLASSIFICATIONS:
            raise ValueError("operator command classification is invalid")
        timeout = entry.get("timeout_seconds")
        if (
            isinstance(timeout, bool)
            or not isinstance(timeout, int)
            or not _OPERATOR_TIMEOUT_MIN_SECONDS <= timeout <= _OPERATOR_TIMEOUT_MAX_SECONDS
        ):
            raise ValueError("operator command timeout is outside the bounded range")
        executable, transform_chain = _operator_executable(workdir, argv[0])
        commands.append(
            OperatorCommand(
                id=command_id,
                requested_argv=tuple(argv),
                effective_argv=(executable, *argv[1:]),
                lane=lane,
                timeout_seconds=timeout,
                classification=classification,
                transform_chain=transform_chain,
            )
        )

    raw_exclusions = operator.get("exclusions", [])
    if not isinstance(raw_exclusions, list):
        raise ValueError("operator exclusions must be a list")
    exclusions: list[OperatorExclusion] = []
    for entry in raw_exclusions:
        if not isinstance(entry, dict):
            raise ValueError("operator exclusion must be a mapping")
        _reject_unknown_keys(
            entry, frozenset({"id", "classification", "reason"}), "operator exclusion"
        )
        exclusion_id = entry.get("id")
        if not isinstance(exclusion_id, str) or not _OPERATOR_ID_RE.fullmatch(exclusion_id):
            raise ValueError("operator exclusion id is invalid")
        if exclusion_id in seen_ids:
            raise ValueError("operator verification ids must be unique")
        seen_ids.add(exclusion_id)
        if entry.get("classification") != "excluded":
            raise ValueError("operator exclusion classification must be excluded")
        reason = entry.get("reason")
        if not isinstance(reason, str) or not reason.strip():
            raise ValueError("operator exclusion reason is required")
        exclusions.append(OperatorExclusion(exclusion_id, "excluded", reason.strip()))
    return OperatorManifest(1, tuple(commands), exclusions, manifest_path, raw)


def _operator_verify(node: "Node", ctx: "Context") -> Result:
    """Run exact post-worker verification outside the worker sandbox."""
    workdir = pathlib.Path(ctx.workdir).resolve()
    if ctx.backend in {"echo", "mock_llm"}:
        target_head = str(ctx.state.get("target_head_sha") or "0" * 40).lower()
        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", target_head):
            target_head = "0" * 40
        receipt = {
            "schema_version": 2,
            "target_head_sha": target_head,
            "source_head_after": target_head,
            "manifest": {"path": None, "sha256": None, "schema_version": 1},
            "commands": [],
            "exclusions": [],
            "outcome": "success",
            "failure_type": None,
            "synthetic": True,
        }
        receipt_path = workdir / "evidence" / "operator-verification.json"
        try:
            _write_operator_receipt(receipt_path, receipt)
            receipt_sha = _validate_operator_receipt(
                receipt_path, target_head, expected_receipt=receipt
            )
        except Exception as exc:
            return Result(
                outcome="error",
                output=f"operator verification fixture failed closed: {type(exc).__name__}",
                metadata={"error_type": "receipt_validation"},
            )
        return Result(
            outcome="success",
            output="operator verification success; synthetic receipt fixture",
            metadata={
                "receipt_path": "evidence/operator-verification.json",
                "receipt_sha256": receipt_sha,
                "target_head_sha": target_head,
                "command_count": "0",
                "synthetic": "true",
            },
            context_updates={
                "operator_verification_receipt": "evidence/operator-verification.json"
            },
        )
    try:
        trusted_manifest_head = str(ctx.state.get("_df_run_initial_head") or "")
        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", trusted_manifest_head):
            raise ValueError("operator verification lacks trusted run-initial HEAD")
        entry_head, entry_workspace = _target_provenance(workdir)
        manifest = _load_operator_manifest(workdir, trusted_head=trusted_manifest_head)
        manifest_sha = hashlib.sha256(manifest.raw_bytes).hexdigest()
        visit = 1 + sum(
            1 for item in ctx.history if item.get("node") == node.name
        )
        raw_dir = (
            _operator_log_root()
            / _safe_operator_component(str(ctx.run_id or "adhoc"), "adhoc")
            / _safe_operator_component(node.name, "operator_verify")
            / str(visit)
        )
        _private_directory(raw_dir)
    except Exception as exc:
        return Result(
            outcome="error",
            output=f"operator verification setup failed: {type(exc).__name__}",
            metadata={"error_type": "operator_setup"},
        )

    receipt_commands: list[dict[str, object]] = []
    failure_outcome = "success"
    failure_type = ""
    target_head = ""
    final_head = ""
    commands = (*_OPERATOR_BUILTINS, *manifest.commands, _OPERATOR_FINAL_HEAD)
    env = _handlers_shim._sanitized_env()
    try:
        snapshot_context = (
            _trusted_operator_snapshot(workdir, trusted_manifest_head, raw_dir)
            if any(item.lane == "operator_unwrapped" for item in manifest.commands)
            else contextlib.nullcontext(workdir)
        )
        with snapshot_context as trusted_snapshot:
            for index, command in enumerate(commands, start=1):
                try:
                    execution = command
                    command_cwd = workdir
                    if command in manifest.commands and command.lane == "worker_safe":
                        wrapped = _handlers_shim._sandboxed_args_for_workdir(
                            list(command.effective_argv), workdir
                        )
                        if wrapped is None:
                            raise RuntimeError("operator worker-safe sandbox unavailable")
                        execution = OperatorCommand(
                            command.id, command.requested_argv, tuple(wrapped),
                            command.lane, command.timeout_seconds, command.classification,
                            (*command.transform_chain, "worker-holdout-sandbox"),
                        )
                    elif command in manifest.commands and command.lane == "operator_unwrapped":
                        command_cwd = trusted_snapshot
                        execution = OperatorCommand(
                            command.id, command.requested_argv, command.effective_argv,
                            command.lane, command.timeout_seconds, command.classification,
                            (*command.transform_chain, "trusted-run-initial-snapshot"),
                        )
                    started = time.monotonic_ns()
                    result = run_bounded_process_bytes(
                        execution.effective_argv,
                        cwd=command_cwd,
                        timeout=command.timeout_seconds,
                        env=env,
                        max_output_bytes=_OPERATOR_RAW_STREAM_MAX_BYTES,
                    )
                    duration_ms = max(0, (time.monotonic_ns() - started) // 1_000_000)
                    stdout_path = raw_dir / f"{index:02d}-{command.id}.stdout.bin"
                    stderr_path = raw_dir / f"{index:02d}-{command.id}.stderr.bin"
                    _write_private_bytes(stdout_path, result.stdout)
                    _write_private_bytes(stderr_path, result.stderr)
                    receipt_commands.append(
                        _command_receipt(
                            execution, result, cwd=command_cwd,
                            duration_ms=duration_ms, stdout_path=stdout_path,
                            stderr_path=stderr_path,
                        )
                    )
                    if command.id == "git-head":
                        target_head = result.stdout.decode("ascii", errors="strict").strip().lower()
                        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", target_head):
                            raise ValueError("operator verification target HEAD is malformed")
                    elif command.id == "git-head-final":
                        final_head = result.stdout.decode("ascii", errors="strict").strip().lower()
                    if result.output_overflowed:
                        failure_outcome, failure_type = "error", "output_overflow"
                        break
                    if result.process_group_cleanup == "failed":
                        failure_outcome, failure_type = "error", "process_group_cleanup"
                        break
                    if result.timed_out:
                        failure_outcome, failure_type = "error", "timeout"
                        break
                    if result.returncode != 0:
                        failure_outcome, failure_type = "failure", "nonzero_exit"
                        break
                except Exception as exc:
                    failure_outcome, failure_type = "error", type(exc).__name__
                    break
    except Exception as exc:
        failure_outcome, failure_type = "error", type(exc).__name__

    if failure_outcome == "success" and target_head != entry_head:
        failure_outcome = "error"
        failure_type = "source_head_drift"
    if failure_outcome == "success" and final_head != target_head:
        failure_outcome = "error"
        failure_type = "source_head_drift"
    if failure_outcome == "success":
        try:
            after_head, after_workspace = _target_provenance(workdir)
            if after_head != entry_head or after_workspace != entry_workspace:
                failure_outcome = "error"
                failure_type = "workspace_drift"
        except (OSError, subprocess.SubprocessError, UnicodeError, ValueError):
            failure_outcome = "error"
            failure_type = "workspace_provenance"

    receipt = {
        "schema_version": 2,
        "target_head_sha": target_head,
        "source_head_after": final_head,
        "manifest": {
            "path": str(manifest.path),
            "sha256": manifest_sha,
            "schema_version": manifest.schema_version,
        },
        "commands": receipt_commands,
        "exclusions": [
            {
                "id": item.id,
                "classification": item.classification,
                "reason": item.reason,
            }
            for item in manifest.exclusions
        ],
        "outcome": failure_outcome,
        "failure_type": failure_type or None,
    }
    receipt_path = workdir / "evidence" / "operator-verification.json"
    try:
        _write_operator_receipt(receipt_path, receipt)
        receipt_sha = _validate_operator_receipt(
            receipt_path, target_head, expected_receipt=receipt
        )
    except Exception as exc:
        return Result(
            outcome="error",
            output=f"operator verification receipt failed closed: {type(exc).__name__}",
            metadata={"error_type": "receipt_validation"},
        )

    return Result(
        outcome=failure_outcome,
        output=(
            f"operator verification {failure_outcome}; receipt="
            "evidence/operator-verification.json"
        ),
        metadata={
            "receipt_path": "evidence/operator-verification.json",
            "receipt_sha256": receipt_sha,
            "target_head_sha": target_head,
            "command_count": str(len(receipt_commands)),
            "error_type": failure_type,
        },
        context_updates={"operator_verification_receipt": "evidence/operator-verification.json"},
    )


def _load_evidence_aliases(workdir: pathlib.Path) -> list[str]:
    """Load vendor-specific filename aliases from ``<workdir>/.dark-factory/evidence.yaml``.

    The YAML file may contain a top-level ``aliases:`` key whose value is a list of
    strings (or a single comma-separated string). Files named here are probed in
    addition to ``DEFAULT_EVIDENCE_FILENAMES``. A missing or malformed file yields
    no aliases (silent fallback — the vendor-neutral defaults still apply).
    """
    manifest = workdir / ".dark-factory" / "evidence.yaml"
    if not manifest.is_file():
        return []
    try:
        import yaml  # local import: PyYAML is a runner dep but optional for callers
        data = yaml.safe_load(manifest.read_text(encoding="utf-8")) or {}
    except Exception:
        return []
    aliases = data.get("aliases") if isinstance(data, dict) else None
    if isinstance(aliases, list):
        return [str(a).strip() for a in aliases if str(a).strip()]
    if isinstance(aliases, str):
        return [a.strip() for a in aliases.split(",") if a.strip()]
    return []


def _git_config_origin_url(workdir: pathlib.Path) -> Optional[str]:
    try:
        res = subprocess.run(
            ["git", "config", "--get", "remote.origin.url"],
            cwd=workdir,
            capture_output=True,
            text=True,
            timeout=5,
        )
        if res.returncode == 0:
            return res.stdout.strip()
    except Exception:
        pass
    return None


def _git_merge_base(workdir: pathlib.Path) -> Optional[str]:
    try:
        for base in ("origin/main", "main", "master", "origin/master"):
            res = subprocess.run(
                ["git", "merge-base", base, "HEAD"],
                cwd=workdir,
                capture_output=True,
                text=True,
                timeout=5,
            )
            if res.returncode == 0:
                return res.stdout.strip()
    except Exception:
        pass
    return None


def _git_diff_stat(workdir: pathlib.Path, base_sha: str) -> Optional[str]:
    try:
        cmd = ["git", "diff", "--stat"]
        if base_sha:
            cmd.append(base_sha)
        res = subprocess.run(
            cmd,
            cwd=workdir,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if res.returncode == 0:
            return res.stdout.strip()
    except Exception:
        pass
    return None


def _gh_pr_body(workdir: pathlib.Path, target_pr: str) -> Optional[str]:
    if not target_pr or target_pr == "N/A":
        return None
    try:
        res = subprocess.run(
            ["gh", "pr", "view", target_pr, "--json", "body", "-q", ".body"],
            cwd=workdir,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if res.returncode == 0:
            return res.stdout.strip()
    except Exception:
        pass
    return None


def _compute_evidence_sha(workdir: pathlib.Path, paths: list[str]) -> Optional[str]:
    import hashlib
    h = hashlib.sha256()
    has_files = False
    for p in paths:
        fp = workdir / p
        if fp.is_file():
            try:
                h.update(fp.read_bytes())
                has_files = True
            except Exception:
                pass
    return h.hexdigest() if has_files else None


def _verify_evidence_freshness(workdir: pathlib.Path, paths: list[str], expected_sha: str) -> bool:
    if not expected_sha:
        return False
    expected_sha_lower = expected_sha.lower()
    for p in paths:
        fp = workdir / p
        if not fp.exists():
            return False
        try:
            content = fp.read_text(encoding="utf-8", errors="ignore")
            if expected_sha_lower in content.lower():
                return True
        except Exception:
            pass
    return False


def _check_unresolved_review_state(workdir: pathlib.Path, target_pr: str) -> bool:
    import json
    import subprocess
    if not target_pr or target_pr == "N/A":
        return True
    try:
        res = subprocess.run(
            ["gh", "pr", "view", target_pr, "--json", "reviewDecision,reviews"],
            cwd=workdir,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if res.returncode == 0:
            data = json.loads(res.stdout)
            decision = data.get("reviewDecision")
            if decision in ("CHANGES_REQUESTED", "REVIEW_REQUIRED"):
                return False
            reviews = data.get("reviews", [])
            reviewer_states = {}
            for r in reviews:
                author = r.get("author", {}).get("login")
                state = r.get("state")
                if author and state:
                    reviewer_states[author] = state
            if "CHANGES_REQUESTED" in reviewer_states.values():
                return False
    except Exception as exc:
        print(f"DEBUG EXCEPTION 2: {exc}", file=sys.stderr)
    return True


def _is_replacement_or_deletion_work(diff_summary: str, pr_description: str, is_replacement_attr: bool) -> bool:
    if is_replacement_attr:
        return True
    insertions = 0
    deletions = 0
    import re
    ins_match = re.search(r"(\d+)\s+insertion", diff_summary)
    del_match = re.search(r"(\d+)\s+deletion", diff_summary)
    if ins_match:
        insertions = int(ins_match.group(1))
    if del_match:
        deletions = int(del_match.group(1))
    if deletions > 0 and (insertions - deletions <= 0):
        return True
    keywords = ["delete", "remove", "refactor", "cleanup", "dead code", "replacement", "replace"]
    desc_lower = pr_description.lower()
    if any(k in desc_lower for k in keywords):
        return True
    return False


def _gate_audit(node: "Node", ctx: "Context") -> "Result":
    """automated, repository-agnostic Evidence Audit review gate handler."""
    target_repo = ctx.state.get("target_repo") or node.attrs.get("target_repo")
    if not target_repo:
        target_repo = _git_config_origin_url(ctx.workdir) or "N/A"

    target_pr = ctx.state.get("target_pr") or node.attrs.get("target_pr") or "N/A"

    target_head_sha = ctx.state.get("target_head_sha") or node.attrs.get("target_head_sha")
    if not target_head_sha:
        target_head_sha = _handlers_shim._worktree_head_sha(ctx.workdir)

    base_sha = ctx.state.get("base_sha") or node.attrs.get("base_sha")
    if not base_sha:
        base_sha = _git_merge_base(ctx.workdir) or ""

    diff_summary = ctx.state.get("diff_summary") or node.attrs.get("diff_summary")
    if not diff_summary:
        diff_summary = _git_diff_stat(ctx.workdir, base_sha) or "No changes found."

    pr_description = ctx.state.get("pr_description") or node.attrs.get("pr_description")
    if not pr_description:
        pr_description = _gh_pr_body(ctx.workdir, target_pr) or "N/A"

    evidence_paths_raw = ctx.state.get("evidence_paths") or node.attrs.get("evidence_paths")
    evidence_paths = []
    if evidence_paths_raw:
        if isinstance(evidence_paths_raw, str):
            if evidence_paths_raw.startswith("[") and evidence_paths_raw.endswith("]"):
                try:
                    evidence_paths = json.loads(evidence_paths_raw)
                except Exception:
                    evidence_paths = [p.strip() for p in evidence_paths_raw.split(",") if p.strip()]
            else:
                evidence_paths = [p.strip() for p in evidence_paths_raw.split(",") if p.strip()]
        elif isinstance(evidence_paths_raw, list):
            evidence_paths = evidence_paths_raw
    else:
        # Vendor-neutral defaults first, then any project-local aliases from
        # ``<workdir>/.dark-factory/evidence.yaml`` (preserves Gemini-shaped
        # filenames like ``gemini_http_request_responses.jsonl`` for existing
        # callers without forcing them on every project).
        probed = list(DEFAULT_EVIDENCE_FILENAMES)
        probed.extend(_load_evidence_aliases(ctx.workdir))
        # De-dupe while preserving order so missing-artifact errors list each
        # missing file exactly once.
        seen: set[str] = set()
        for standard_name in probed:
            if standard_name in seen:
                continue
            seen.add(standard_name)
            if (ctx.workdir / standard_name).is_file():
                evidence_paths.append(standard_name)

    missing_artifacts = []
    for p in evidence_paths:
        fp = ctx.workdir / p
        if not fp.is_file():
            missing_artifacts.append(p)

    verdict_artifact = {
        "target_repo": target_repo,
        "target_pr": target_pr,
        "target_head_sha": target_head_sha or "N/A",
        "base_sha": base_sha,
        "diff_summary": diff_summary,
        "pr_description": pr_description,
        "evidence_paths": evidence_paths,
        "evidence_sha": "N/A",
        "verdict": "unknown",
        "outcome": "error",
        "is_replacement": False,
        "audit_details": "",
        "timestamp": time.time(),
    }

    verdict_artifact_path = ctx.workdir / "gate_audit_verdict.json"

    def write_verdict_artifact(outcome: str, verdict: str, details: str, is_repl: bool, ev_sha: str):
        verdict_artifact["outcome"] = outcome
        verdict_artifact["verdict"] = verdict
        verdict_artifact["audit_details"] = details
        verdict_artifact["is_replacement"] = is_repl
        verdict_artifact["evidence_sha"] = ev_sha
        try:
            verdict_artifact_path.parent.mkdir(parents=True, exist_ok=True)
            verdict_artifact_path.write_text(json.dumps(verdict_artifact, indent=2), encoding="utf-8")
        except Exception:
            pass

    if not evidence_paths or missing_artifacts:
        err_msg = f"Evidence Audit: missing evidence artifacts: {missing_artifacts or 'no evidence paths specified'}"
        write_verdict_artifact("error", "unknown", err_msg, False, "N/A")
        return Result(
            outcome="error",
            output=err_msg,
            metadata={"verdict": "unknown", "error_type": "missing_artifact"},
        )

    evidence_sha = ctx.state.get("evidence_sha") or node.attrs.get("evidence_sha")
    if not evidence_sha:
        evidence_sha = _compute_evidence_sha(ctx.workdir, evidence_paths) or "N/A"

    if not target_head_sha or not _verify_evidence_freshness(ctx.workdir, evidence_paths, target_head_sha):
        err_msg = f"Evidence Audit: stale evidence, target HEAD SHA {target_head_sha} not found in evidence logs."
        write_verdict_artifact("failure", "fail", err_msg, False, evidence_sha)
        return Result(
            outcome="failure",
            output=err_msg,
            metadata={"verdict": "fail", "error_type": "stale_evidence", "evidence_sha": evidence_sha},
        )

    if not _check_unresolved_review_state(ctx.workdir, target_pr):
        err_msg = f"Evidence Audit: unresolved required review state (PR #{target_pr} changes requested or pending approval)."
        write_verdict_artifact("failure", "fail", err_msg, False, evidence_sha)
        return Result(
            outcome="failure",
            output=err_msg,
            metadata={"verdict": "fail", "error_type": "unresolved_reviews", "evidence_sha": evidence_sha},
        )

    is_replacement_attr = False
    raw_repl = node.attrs.get("is_replacement")
    if raw_repl is not None:
        if isinstance(raw_repl, bool):
            is_replacement_attr = raw_repl
        else:
            is_replacement_attr = str(raw_repl).strip().lower() in {"true", "1", "yes"}

    is_repl = _is_replacement_or_deletion_work(diff_summary, pr_description, is_replacement_attr)

    evidence_snapshots = []
    for p in evidence_paths:
        fp = ctx.workdir / p
        try:
            lines = fp.read_text(encoding="utf-8", errors="ignore").splitlines()[:50]
            snapshot = "\n".join(lines)
            evidence_snapshots.append(f"Evidence file: {p}\n---\n{snapshot}\n---")
        except Exception:
            pass
    evidence_snapshot_text = "\n\n".join(evidence_snapshots)

    audit_prompt = f"""\
You are performing an automated, repository-agnostic Evidence Audit review.
You MUST audit the active repository changes, diff, and evidence files.

You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.

RECORDS:
Target Repo: {target_repo}
Target PR: {target_pr}
Target HEAD SHA: {target_head_sha}
Base SHA: {base_sha}
Diff Summary: {diff_summary}
PR Description Snapshot: {pr_description}
Evidence Paths: {", ".join(evidence_paths)}
Evidence SHA: {evidence_sha}

EVIDENCE SNAPSHOTS:
{evidence_snapshot_text}

Verify the following:
1. Stale evidence: Confirm that the evidence SHA matches target_head_sha and reflects the current changes.
2. PR alignment: Verify that the implementation in the diff matches the PR description's intent.
3. Deletion/integrity review: Since this task is {"" if is_repl else "NOT "}replacement/deletion/refactor/dead-code work:
   - Confirm that the deletion proof/evidence is present and verified.
   - Confirm that the dead code/deleted logic is completely removed and there are no stray/unused remnants.
   - Confirm that the new implementation is not a simple additive overlay without cleaning up the replaced code.

CRITICAL FORMATTING INSTRUCTIONS:
1. You MUST include a binding verification line:
   head_sha: {target_head_sha}
2. You MUST conclude your review with:
   verdict: <pass|fail|warn|inconclusive>
"""

    if ctx.backend in ("echo", "mock_llm"):
        hint = ctx.state.get(f"{node.name}.outcome", "success")
        verdict = "pass" if hint == "success" else ("warn" if hint == "warn" else "fail")
        outcome = hint
        if is_repl:
            if verdict not in ("pass", "approved", "approve"):
                outcome = "failure"
        output_text = f"echo gate_audit: pre-seeded {hint}\nhead_sha: {target_head_sha}\nverdict: {verdict}"
        write_verdict_artifact(outcome, verdict, output_text, is_repl, evidence_sha)
        return Result(
            outcome=outcome,
            output=output_text,
            metadata={"verdict": verdict, "evidence_sha": evidence_sha, "is_replacement": str(is_repl)},
        )

    # Rationale (jleechan-arr): the 1200s default exceeds the roadmap's
    # 300s "review/deep auditing" proposal in
    # ``docs/plans/factory_improvement_analysis.md``. Observed gate_audit
    # p99 = 206s in production logs (max observed = 206s); the 1200s
    # headroom is intentional because audit prompts can review very large
    # diffs in one shot. See ``TIMEOUT_DEFAULTS_RATIONALE`` in
    # ``docs/plans/factory_improvement_analysis.implementation.md``
    # Pillar 4 for the empirical distribution.
    timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1200"), 1200)
    backend, gate_meta = _handlers_shim._resolve_gate_backend(node, ctx)
    result = _handlers_shim._execute_gate(
        audit_prompt, target_head_sha, timeout, ctx, "gate_audit", backend,
        gate_strict=_gate_strict_flag(node),
    )

    if gate_meta:
        for k, v in gate_meta.items():
            result.metadata.setdefault(k, v)

    verdict, normalized = _handlers_shim._parse_verdict(result.output)

    if result.outcome == "error":
        write_verdict_artifact("error", verdict, result.output, is_repl, evidence_sha)
        return result

    outcome = normalized
    if is_repl:
        if verdict not in ("pass", "approved", "approve"):
            outcome = "failure"

    write_verdict_artifact(outcome, verdict, result.output, is_repl, evidence_sha)
    result.outcome = outcome
    result.metadata.update({
        "verdict": verdict,
        "evidence_sha": evidence_sha,
        "is_replacement": str(is_repl),
    })
    return result
