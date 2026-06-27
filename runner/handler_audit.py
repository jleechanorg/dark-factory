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

import json
import pathlib
import subprocess
import sys
import time
from typing import TYPE_CHECKING, Optional

import runner.handlers as _handlers_shim

from .handler_core import Result, _gate_strict_flag

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
        for standard_name in ("gemini_http_request_responses.jsonl", "gemini_http_responses.jsonl", "evidence.jsonl"):
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
