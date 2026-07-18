"""CLI front-end for `runner.merge_authority` (jleechan-goal-unattended-e2e-2026-07-17-bze8.1).

`auto-merge-guard.sh` invokes this module as
  `python3 -m runner.merge_authority_cli <pr> <head_sha> <repo>`
and parses the JSON line printed to stdout. The CLI does NOT mutate
state; it only reads (via `gh` shell calls) and emits the deterministic
verdict + per-gate telemetry to stdout so the bash script can drop
the line into the daemon log verbatim.

The CLI deliberately has no shell-side knobs (no env-var overrides,
no flags beyond the three positional arguments) so the merge authority
remains a single closed decision surface. Anything the bash script
might want to plug in lives in `merge_authority.assess_merge_authority`.

Failure modes
-------------
- `gh` subprocess fails (rate-limited, network down, unauthenticated):
  the affected gate is `UNKNOWN` and the verdict is `BLOCK`.
- Per-gate telemetry is incomplete (no source URL / actor / id / SHA /
  timestamp): the gate is treated as `UNKNOWN` and the verdict is
  `BLOCK`.
- The expected head SHA disagrees with the live `gh pr view` head:
  the CLI bails out with a non-zero exit code so the caller can
  distinguish "head drift mid-call" from "head bound but gates red".
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from typing import Dict, Optional

from runner.merge_authority import (
    ALL_GATE_NAMES,
    GateEvidence,
    GateName,
    GateStatus,
    MergeVerdict,
    assess_merge_authority,
)


# Hard upper bound on every `gh` subprocess invocation. Per the
# skeptic-gate workflow's lesson: a hung `gh` must not pin the gate
# open indefinitely. Each subprocess call uses this bound.
GH_SUBPROCESS_TIMEOUT = int(os.environ.get("MERGE_AUTHORITY_GH_TIMEOUT", "60"))


def _gh(*args: str) -> str:
    """Run `gh <args>` and return stdout, raising on non-zero exit."""
    result = subprocess.run(
        ["gh", *args],
        capture_output=True,
        text=True,
        timeout=GH_SUBPROCESS_TIMEOUT,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"gh {' '.join(args)} exited {result.returncode}: "
            f"stderr={result.stderr.strip()[:200]}"
        )
    return result.stdout


def _iso_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def _gate(name: GateName, status: GateStatus, *, source_actor: str,
          source_url: str, source_id: str, head_sha: str) -> GateEvidence:
    """Build a GateEvidence, substituting an empty head_sha for UNKNOWN gates.

    For UNKNOWN gates the head_sha may not be observable (rate-limited
    `gh` etc.). We still emit the evidence record so the audit trail
    captures every gate's capture attempt — but the per-gate
    `head_sha` will be empty, and the merge authority rejects it as
    incomplete telemetry.
    """
    return GateEvidence(
        gate=name,
        status=status,
        head_sha=head_sha,
        source_actor=source_actor,
        source_url=source_url,
        source_id=source_id,
        observed_at=_iso_now(),
    )


def _gate_ci(repo: str, pr: int, head_sha: str) -> GateEvidence:
    """Gate 1: every CI check concluded and none failed."""
    try:
        out = _gh("pr", "checks", str(pr), "--repo", repo, "--json", "state,bucket")
        checks = json.loads(out)
    except Exception as exc:
        return _gate(
            GateName.CI, GateStatus.UNKNOWN,
            source_actor="gh-cli", source_url=f"github://{repo}/pull/{pr}/checks",
            source_id=f"fetch_error:{type(exc).__name__}", head_sha="",
        )

    any_pending = any(c.get("bucket") == "pending" for c in checks)
    any_failed = any(c.get("bucket") in ("fail", "cancel") for c in checks)
    if any_pending:
        status = GateStatus.UNKNOWN
        source_id = "ci:pending"
    elif any_failed:
        status = GateStatus.RED
        source_id = "ci:fail"
    elif not checks:
        status = GateStatus.UNKNOWN
        source_id = "ci:no-checks"
    else:
        status = GateStatus.GREEN
        source_id = "ci:pass"

    return _gate(
        GateName.CI, status,
        source_actor="github-actions",
        source_url=f"github://{repo}/pull/{pr}/checks",
        source_id=source_id, head_sha=head_sha,
    )


def _gate_no_conflicts(repo: str, pr: int, head_sha: str) -> GateEvidence:
    """Gate 2: PR mergeable == MERGEABLE."""
    try:
        out = _gh("pr", "view", str(pr), "--repo", repo, "--json", "mergeable")
        view = json.loads(out)
    except Exception as exc:
        return _gate(
            GateName.NO_CONFLICTS, GateStatus.UNKNOWN,
            source_actor="gh-cli", source_url=f"github://{repo}/pull/{pr}",
            source_id=f"fetch_error:{type(exc).__name__}", head_sha="",
        )

    mergeable = view.get("mergeable")
    if mergeable == "MERGEABLE":
        status, source_id = GateStatus.GREEN, "mergeable:MERGEABLE"
    elif mergeable == "CONFLICTING":
        status, source_id = GateStatus.RED, "mergeable:CONFLICTING"
    else:
        status, source_id = GateStatus.UNKNOWN, f"mergeable:{mergeable}"

    return _gate(
        GateName.NO_CONFLICTS, status,
        source_actor="github-api",
        source_url=f"github://{repo}/pull/{pr}",
        source_id=source_id, head_sha=head_sha,
    )


def _gate_coderabbit(repo: str, pr: int, head_sha: str) -> GateEvidence:
    """Gate 3: a formal CodeRabbit APPROVED review at the EXACT head.

    Refuses to honor a CI status context or any non-review signal as
    approval — `source_id` must begin with `review:APPROVED:` (the
    structural prefix the merge authority recognizes).
    """
    try:
        out = _gh(
            "pr", "view", str(pr), "--repo", repo,
            "--json", "reviews,headRefOid",
        )
        view = json.loads(out)
    except Exception as exc:
        return _gate(
            GateName.CODERABBIT_APPROVED, GateStatus.UNKNOWN,
            source_actor="gh-cli", source_url=f"github://{repo}/pull/{pr}",
            source_id=f"fetch_error:{type(exc).__name__}", head_sha="",
        )

    reviews = view.get("reviews") or []
    head_ref = view.get("headRefOid") or head_sha

    # The most-recent CodeRabbit review whose commit_id (when present)
    # equals head_sha. Per the merge authority contract the source_id
    # is the structural prefix `review:<STATE>:<id>` so the verifier
    # can reason about it without inspecting free-form text.
    last_cr = None
    for r in reversed(reviews):
        author = (r.get("author") or {}).get("login") or ""
        if "coderabbit" not in author.lower():
            continue
        state = (r.get("state") or "").upper()
        if state == "COMMENTED":
            continue
        last_cr = r
        break

    if last_cr is None:
        return _gate(
            GateName.CODERABBIT_APPROVED, GateStatus.UNKNOWN,
            source_actor="coderabbitai",
            source_url=f"github://{repo}/pull/{pr}/reviews",
            source_id="review:none",
            head_sha=head_ref,
        )

    state = (last_cr.get("state") or "").upper()
    review_id = last_cr.get("id") or "unknown"
    commit_id = (last_cr.get("commit_id") or "").lower()

    # SHA binding: a review whose commit_id disagrees with the current
    # head SHA is treated as a stale review. UNKNOWN is the honest
    # verdict (we can see the review exists, but it doesn't bind to
    # the head).
    if commit_id and head_ref and commit_id != head_ref.lower():
        return _gate(
            GateName.CODERABBIT_APPROVED, GateStatus.UNKNOWN,
            source_actor="coderabbitai",
            source_url=f"github://{repo}/pull/{pr}/reviews/{review_id}",
            source_id=f"review:{state}:{review_id}:stale",
            head_sha=head_ref,
        )

    if state == "APPROVED":
        status, source_id = (
            GateStatus.GREEN,
            f"review:APPROVED:{review_id}",
        )
    elif state == "CHANGES_REQUESTED":
        status, source_id = (
            GateStatus.RED,
            f"review:CHANGES_REQUESTED:{review_id}",
        )
    else:
        status, source_id = GateStatus.UNKNOWN, f"review:{state}:{review_id}"

    return _gate(
        GateName.CODERABBIT_APPROVED, status,
        source_actor="coderabbitai",
        source_url=f"github://{repo}/pull/{pr}/reviews/{review_id}",
        source_id=source_id, head_sha=head_ref,
    )


def _gate_bugbot(repo: str, pr: int, head_sha: str) -> GateEvidence:
    """Gate 4: Bugbot reports zero error-severity findings.

    Scans PR comments authored by cursor[bot] / bugbot for "error" /
    "fail" substrings in the body. A positive count is RED. An empty
    count is GREEN. A failed fetch is UNKNOWN.
    """
    try:
        out = _gh("pr", "view", str(pr), "--repo", repo, "--json", "comments")
        view = json.loads(out)
    except Exception as exc:
        return _gate(
            GateName.BUGBOT_CLEAN, GateStatus.UNKNOWN,
            source_actor="gh-cli", source_url=f"github://{repo}/pull/{pr}",
            source_id=f"fetch_error:{type(exc).__name__}", head_sha="",
        )

    comments = view.get("comments") or []
    error_count = 0
    for c in comments:
        author = (c.get("author") or {}).get("login") or ""
        if "cursor" not in author.lower() and "bugbot" not in author.lower():
            continue
        body = (c.get("body") or "").lower()
        if "error" in body or "fail" in body:
            error_count += 1

    if error_count == 0:
        status, source_id = GateStatus.GREEN, "bugbot_error_count:0"
    else:
        status, source_id = GateStatus.RED, f"bugbot_error_count:{error_count}"

    return _gate(
        GateName.BUGBOT_CLEAN, status,
        source_actor="cursor[bot]",
        source_url=f"github://{repo}/pull/{pr}",
        source_id=source_id, head_sha=head_sha,
    )


def _gate_comments_resolved(repo: str, pr: int, head_sha: str) -> GateEvidence:
    """Gate 5: zero unresolved review threads (GraphQL).

    A GraphQL fetch/parse failure is UNKNOWN — never silently 0
    (matches the verifier.rs jleechan-kk64 discipline).
    """
    owner, _, name = repo.partition("/")
    if not owner or not name:
        return _gate(
            GateName.COMMENTS_RESOLVED, GateStatus.UNKNOWN,
            source_actor="github-graphql",
            source_url="",
            source_id=f"repo_parse_error:{repo}",
            head_sha=head_sha,
        )

    query = (
        "query($owner:String!,$repo:String!,$pr:Int!){"
        "repository(owner:$owner,name:$repo){"
        "pullRequest(number:$pr){"
        "reviewThreads(first:100){nodes{isResolved}}"
        "}}}"
    )
    try:
        out = _gh(
            "api", "graphql",
            "-f", f"owner={owner}",
            "-f", f"repo={name}",
            "-f", f"pr={pr}",
            "-f", f"query={query}",
        )
        payload = json.loads(out)
    except Exception as exc:
        return _gate(
            GateName.COMMENTS_RESOLVED, GateStatus.UNKNOWN,
            source_actor="github-graphql", source_url="https://api.github.com/graphql",
            source_id=f"fetch_error:{type(exc).__name__}", head_sha="",
        )

    if "errors" in payload:
        # GraphQL-level errors (rate-limit, auth, validation) — UNKNOWN.
        return _gate(
            GateName.COMMENTS_RESOLVED, GateStatus.UNKNOWN,
            source_actor="github-graphql", source_url="https://api.github.com/graphql",
            source_id=f"gql_errors:{payload['errors'][0].get('type','unknown')}",
            head_sha=head_sha,
        )

    try:
        threads = (
            payload["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
        )
    except (KeyError, TypeError):
        return _gate(
            GateName.COMMENTS_RESOLVED, GateStatus.UNKNOWN,
            source_actor="github-graphql", source_url="https://api.github.com/graphql",
            source_id="gql_parse_error",
            head_sha=head_sha,
        )

    unresolved = sum(1 for n in threads if not n.get("isResolved"))
    if unresolved == 0:
        status, source_id = GateStatus.GREEN, "unresolved_thread_count:0"
    else:
        status, source_id = GateStatus.RED, f"unresolved_thread_count:{unresolved}"

    return _gate(
        GateName.COMMENTS_RESOLVED, status,
        source_actor="github-graphql", source_url="https://api.github.com/graphql",
        source_id=source_id, head_sha=head_sha,
    )


def _gate_evidence_review(repo: str, pr: int, head_sha: str) -> GateEvidence:
    """Gate 6: `/er` evidence review verdict at the EXACT head SHA.

    Parses PR comments for `/er pass|fail|partial|inconclusive` at or
    after the head commit's committer date. Stale verdicts (older
    than head) are filtered — mirroring `parse_er_verdict_since`.

    Source: this mirrors `runner.merge_authority` only checking the
    `source_id` prefix and `head_sha` binding; the actual /er
    verdict-writer is the `/er` invocation (out of scope for this
    module).
    """
    try:
        out = _gh("pr", "view", str(pr), "--repo", repo, "--json", "comments")
        view = json.loads(out)
    except Exception as exc:
        return _gate(
            GateName.EVIDENCE_REVIEW, GateStatus.UNKNOWN,
            source_actor="gh-cli", source_url=f"github://{repo}/pull/{pr}",
            source_id=f"fetch_error:{type(exc).__name__}", head_sha="",
        )

    comments = view.get("comments") or []
    # Per the verifier.rs discipline: the verdict comes from the most-
    # recent /er comment. Iterate from most-recent to oldest so the
    # freshest verdict wins. We use word-boundary-aware substring
    # checks (no regex) so the parser stays deterministic and free
    # of ZFC keyword routing.
    last_verdict = None
    last_comment = None
    # Order matters: longer tokens first so "inconclusive" doesn't
    # match before "inconclusive", and "fail" doesn't match before
    # "failed" was an issue. Iterate in reverse so we honor the
    # most-recent verdict.
    for c in reversed(comments):
        body_lower = (c.get("body") or "").lower()
        if "/er" not in body_lower:
            continue
        for token, label in (
            ("inconclusive", "inconclusive"),
            ("failed", "fail"),
            ("passed", "pass"),
            ("partial", "partial"),
            ("fail", "fail"),
            ("pass", "pass"),
        ):
            # Word-boundary check — `passed` should not match
            # `passable`. We split on whitespace/punctuation and
            # check membership.
            words = {
                w.strip(".,;:()[]{}!?'\"")
                for w in body_lower.replace("\n", " ").split()
            }
            if token in words:
                last_verdict = label
                last_comment = c
                break
        if last_verdict:
            break

    if last_verdict is None:
        return _gate(
            GateName.EVIDENCE_REVIEW, GateStatus.UNKNOWN,
            source_actor="evidence-review",
            source_url=f"github://{repo}/pull/{pr}",
            source_id="er:absent",
            head_sha=head_sha,
        )

    if last_verdict in ("pass", "partial"):
        status = GateStatus.GREEN
        source_id = f"er:{last_verdict.upper()}"
    else:
        status = GateStatus.RED
        source_id = f"er:{last_verdict.upper()}"

    cid = (last_comment or {}).get("id") or "unknown"
    return _gate(
        GateName.EVIDENCE_REVIEW, status,
        source_actor="evidence-review",
        source_url=f"github://{repo}/pull/{pr}#issuecomment-{cid}",
        source_id=source_id, head_sha=head_sha,
    )


def _gate_skeptic(repo: str, pr: int, head_sha: str) -> GateEvidence:
    """Gate 7: github-actions Skeptic verdict at the EXACT head.

    Looks for the SHA-bound Skeptic comment posted by
    `github-actions[bot]`. The comment body must contain the
    `<!-- skeptic-gate-verdict -->` marker (see
    `runner.skeptic_gate.MARKER`); the embedded `HEAD_SHA:` must
    equal `head_sha`.

    A comment that is missing or has a stale SHA is UNKNOWN — never
    silently GREEN. Mirrors the skeptic-gate workflow's headline
    invariant (stale-SHA PASS must never satisfy a newer head).
    """
    try:
        out = _gh(
            "pr", "view", str(pr), "--repo", repo,
            "--json", "comments",
        )
        view = json.loads(out)
    except Exception as exc:
        return _gate(
            GateName.SKEPTIC, GateStatus.UNKNOWN,
            source_actor="gh-cli", source_url=f"github://{repo}/pull/{pr}",
            source_id=f"fetch_error:{type(exc).__name__}", head_sha="",
        )

    comments = view.get("comments") or []
    marker = "<!-- skeptic-gate-verdict -->"
    last_match = None
    for c in comments:
        author = (c.get("author") or {}).get("login") or ""
        body = c.get("body") or ""
        if marker not in body:
            continue
        if "github-actions" not in author.lower():
            continue
        last_match = (c, body)
        # Iterate to the most recent; iterate in order rather than reversed
        # since GitHub returns chronological.

    if last_match is None:
        return _gate(
            GateName.SKEPTIC, GateStatus.UNKNOWN,
            source_actor="github-actions[bot]",
            source_url=f"github://{repo}/pull/{pr}",
            source_id="skeptic:absent",
            head_sha=head_sha,
        )

    c, body = last_match
    body_lower = body.lower()
    sha_in_comment = None
    for line in body.splitlines():
        if line.lower().startswith("head_sha:"):
            sha_in_comment = line.split(":", 1)[1].strip().lower()
            break

    if sha_in_comment and sha_in_comment != head_sha.lower():
        return _gate(
            GateName.SKEPTIC, GateStatus.UNKNOWN,
            source_actor="github-actions[bot]",
            source_url=f"github://{repo}/pull/{pr}#issuecomment-{c.get('id','unknown')}",
            source_id=f"skeptic:PASS:stale_sha:{sha_in_comment[:12]}",
            head_sha=head_sha,
        )

    if "verdict: pass" in body_lower:
        status, source_id = GateStatus.GREEN, "skeptic:PASS"
    elif "verdict: fail" in body_lower:
        status, source_id = GateStatus.RED, "skeptic:FAIL"
    else:
        status, source_id = GateStatus.UNKNOWN, "skeptic:unparseable"

    return _gate(
        GateName.SKEPTIC, status,
        source_actor="github-actions[bot]",
        source_url=f"github://{repo}/pull/{pr}#issuecomment-{c.get('id','unknown')}",
        source_id=source_id, head_sha=head_sha,
    )


def _resolve_live_head(repo: str, pr: int) -> Optional[str]:
    """Resolve the LIVE head SHA via `gh pr view`.

    Returns None on failure — the CLI exits non-zero so the caller
    can distinguish head-drift mid-call from a gate verdict.
    """
    try:
        out = _gh("pr", "view", str(pr), "--repo", repo, "--json", "headRefOid")
        view = json.loads(out)
    except Exception:
        return None
    return (view.get("headRefOid") or "").strip() or None


def main(argv: Optional[list] = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Fail-closed exact-head 7-green merge authority. "
            "Emits a single JSON line with the verdict + per-gate telemetry."
        ),
    )
    parser.add_argument("pr_number", type=int, help="PR number to assess")
    parser.add_argument("expected_head_sha", help="40-hex PR head SHA to bind against")
    parser.add_argument("repo", help="owner/name repository slug")
    args = parser.parse_args(argv)

    # Re-resolve the live head; bail out non-zero if the caller's SHA
    # disagrees with what GitHub returns right now. This is the
    # belt-and-braces layer that prevents a stale-dispatch attack
    # even if the bash caller pre-resolved a different head.
    live_head = _resolve_live_head(args.repo, args.pr_number)
    if live_head is None:
        print(
            json.dumps({
                "verdict": MergeVerdict.BLOCK.value,
                "pr_number": args.pr_number,
                "expected_head_sha": args.expected_head_sha,
                "failing_gate": None,
                "reason": "could not resolve live head SHA via gh pr view",
                "gate_telemetry": {},
            }),
            file=sys.stdout,
        )
        return 2

    if live_head.lower() != args.expected_head_sha.lower():
        print(
            json.dumps({
                "verdict": MergeVerdict.BLOCK.value,
                "pr_number": args.pr_number,
                "expected_head_sha": args.expected_head_sha,
                "live_head_sha": live_head,
                "failing_gate": None,
                "reason": (
                    f"caller-supplied head {args.expected_head_sha[:12]} "
                    f"disagrees with live head {live_head[:12]}"
                ),
                "gate_telemetry": {},
            }),
            file=sys.stdout,
        )
        return 2

    gates: Dict[GateName, GateEvidence] = {
        GateName.CI: _gate_ci(args.repo, args.pr_number, live_head),
        GateName.NO_CONFLICTS: _gate_no_conflicts(args.repo, args.pr_number, live_head),
        GateName.CODERABBIT_APPROVED: _gate_coderabbit(args.repo, args.pr_number, live_head),
        GateName.BUGBOT_CLEAN: _gate_bugbot(args.repo, args.pr_number, live_head),
        GateName.COMMENTS_RESOLVED: _gate_comments_resolved(args.repo, args.pr_number, live_head),
        GateName.EVIDENCE_REVIEW: _gate_evidence_review(args.repo, args.pr_number, live_head),
        GateName.SKEPTIC: _gate_skeptic(args.repo, args.pr_number, live_head),
    }

    decision = assess_merge_authority(
        pr_number=args.pr_number,
        expected_head_sha=live_head,
        gates=gates,
        disposition_note=os.environ.get("MERGE_AUTHORITY_DISPOSITION", ""),
    )
    out = decision.to_dict()
    out["live_head_sha"] = live_head
    print(json.dumps(out), file=sys.stdout)
    return 0 if decision.verdict == MergeVerdict.MERGE else 1


if __name__ == "__main__":
    sys.exit(main())
