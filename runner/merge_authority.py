"""Fail-closed exact-head 7-green merge authority (jleechan-goal-unattended-e2e-2026-07-17-bze8.1).

The merge authority is the single decision point for whether a factory
PR may be merged. It replaces the `latest_assessment_no_red` predicate
in `daemon/scripts/auto-merge-guard.sh` with a SHA-bound assessment of
every one of the seven named gates.

Headline invariants
-------------------

1. Every gate's evidence must be SHA-bound to the EXACT current PR head.
   A stale-SHA Green is unconditionally rejected — a gate that
   satisfied yesterday's head does not satisfy today's.

2. A disposition note or operator assertion can NEVER bypass a missing
   or Red gate. Disposition is metadata carried in the telemetry;
   the verdict comes from the per-gate evidence.

3. The seven gates are exactly the same set `daemon/src/verifier.rs`
   already publishes (`ci_green`, `no_conflicts`, `coderabbit`,
   `bugbot`, `comments_resolved`, `evidence_review`, `skeptic`).

4. CodeRabbit requires a formal APPROVED review record at the exact
   head. A green CI status check from the `coderabbitai` account, or
   the absence of any review record (rate-limited / unknown), is
   NEVER approval.

5. Bugbot requires zero error-severity findings. Warn / info findings
   are non-blocking.

6. The github-actions Skeptic workflow must produce a PASS verdict
   SHA-bound to the current head. The same fail-closed contract the
   skeptic gate workflow applies to its OWN review of the diff
   applies to it from outside.

7. The `/er` evidence-review verdict must be PASS at the current head.
   Stale PASSes (from before the current head commit) are rejected.

Per-gate telemetry
------------------

Each `GateEvidence` carries `source_actor`, `source_url`, `source_id`,
`head_sha`, and `observed_at`. The full per-gate map is emitted as
JSON on every `assess_merge_authority` call so the audit trail is
reconstructable from the daemon telemetry log alone.

ZFC compliance
--------------

No keyword routing, no scoring, no semantic classification of free-form
text. A gate's verdict is exactly one of `GREEN` / `RED` / `UNKNOWN`,
each a closed enum; SHA binding is a literal-string equality check;
the CodeRabbit "is this an APPROVED review" test is a structural
prefix match on `source_id`. Nothing in this module inspects the
reviewer's prose.
"""

from __future__ import annotations

import enum
from dataclasses import asdict, dataclass, field
from typing import Any, Dict, Optional


class GateName(str, enum.Enum):
    """The seven named gates. Order is fixed and matches the verifier.rs vocabulary."""

    CI = "ci_green"
    NO_CONFLICTS = "no_conflicts"
    CODERABBIT_APPROVED = "coderabbit"
    BUGBOT_CLEAN = "bugbot"
    COMMENTS_RESOLVED = "comments_resolved"
    EVIDENCE_REVIEW = "evidence_review"
    SKEPTIC = "skeptic"


ALL_GATE_NAMES = (
    GateName.CI,
    GateName.NO_CONFLICTS,
    GateName.CODERABBIT_APPROVED,
    GateName.BUGBOT_CLEAN,
    GateName.COMMENTS_RESOLVED,
    GateName.EVIDENCE_REVIEW,
    GateName.SKEPTIC,
)


class GateStatus(str, enum.Enum):
    """Closed verdict vocabulary — mirrors verifier.rs GateResult semantics.

    `UNKNOWN` is deliberately distinct from `RED`: a `RED` gate is real
    evidence of a defect (e.g. CHANGES_REQUESTED), while `UNKNOWN` is
    evidence of infra unavailability (rate limit, fetch error). Both
    block merge — only `GREEN` clears a gate.
    """

    GREEN = "GREEN"
    RED = "RED"
    UNKNOWN = "UNKNOWN"


class MergeVerdict(str, enum.Enum):
    """Closed merge verdict vocabulary.

    `MERGE` — every gate is `GREEN` at the exact head SHA.
    `BLOCK` — at least one gate is missing, unknown, stale-SHA, or red.
    """

    MERGE = "MERGE"
    BLOCK = "BLOCK"


# Source-id prefix that establishes "this is a formal GitHub PR review
# with state=APPROVED" rather than a CI status context. The merge
# authority refuses to recognize a coderabbit[bot] CI check as approval.
CODERABBIT_APPROVED_REVIEW_PREFIX = "review:APPROVED"

# Source-id prefix for the Bugbot error-severity counter. Zero of these
# means the gate is GREEN. The token must be parseable as a non-negative
# integer so a malformed payload is treated as UNKNOWN, not GREEN.
BUGBOT_ERROR_COUNT_PREFIX = "bugbot_error_count:"


@dataclass(frozen=True)
class GateEvidence:
    """Per-gate evidence captured from the live PR + SCM/GraphQL."""

    gate: GateName
    status: GateStatus
    head_sha: str
    source_actor: str
    source_url: str
    source_id: str
    observed_at: str


@dataclass(frozen=True)
class MergeDecision:
    """The merge authority's verdict plus the full per-gate telemetry.

    `verdict == MERGE` ⇔ every gate is `GREEN` AND SHA-bound to
    `expected_head_sha` AND every required telemetry field is populated.

    `verdict == BLOCK` carries `failing_gate` (the first gate that
    violated the contract — diagnostic, not exhaustive) and `reason`
    (human-readable explanation; lands in the daemon log line).
    """

    verdict: MergeVerdict
    pr_number: int
    expected_head_sha: str
    gate_telemetry: Dict[GateName, GateEvidence] = field(default_factory=dict)
    failing_gate: Optional[GateName] = None
    reason: str = ""
    disposition_note: str = ""

    def to_dict(self) -> Dict[str, Any]:
        """JSON-serializable shape for telemetry emission.

        The auto-merge-guard consumer reads `verdict`/`failing_gate`/
        `reason` and writes the full payload to the daemon log so the
        audit trail is reconstructable from one line per assessment.
        """
        out: Dict[str, Any] = {
            "verdict": self.verdict.value,
            "pr_number": self.pr_number,
            "expected_head_sha": self.expected_head_sha,
            "failing_gate": self.failing_gate.value if self.failing_gate else None,
            "reason": self.reason,
            "disposition_note": self.disposition_note,
            "gate_telemetry": {
                g.value: {
                    "status": ev.status.value,
                    "head_sha": ev.head_sha,
                    "source_actor": ev.source_actor,
                    "source_url": ev.source_url,
                    "source_id": ev.source_id,
                    "observed_at": ev.observed_at,
                }
                for g, ev in self.gate_telemetry.items()
            },
        }
        return out


def _parse_bugbot_error_count(source_id: str) -> Optional[int]:
    """Parse a Bugbot error-count payload from `source_id`.

    Returns `None` on malformed input — the gate is then UNKNOWN, not
    silently GREEN. This is the fail-closed discipline that prevents
    garbage-in / garbage-out from a malformed Bugbot payload.
    """
    if not source_id.startswith(BUGBOT_ERROR_COUNT_PREFIX):
        return None
    raw = source_id[len(BUGBOT_ERROR_COUNT_PREFIX):].strip()
    if not raw.isdigit():
        return None
    return int(raw)


def _check_coderabbit_approval(evidence: GateEvidence) -> Optional[str]:
    """Validate that a CodeRabbit evidence record is a formal APPROVED review.

    Returns `None` if the record is a valid APPROVED review at the
    expected head, or a human-readable failure reason otherwise.

    A green CI status check from the `coderabbitai` account is NOT
    approval — the source_id must begin with `review:APPROVED` to be
    honored as a formal GitHub PR review with state=APPROVED.
    """
    if not evidence.source_id.startswith(CODERABBIT_APPROVED_REVIEW_PREFIX):
        return (
            "CodeRabbit gate requires a formal review record "
            f"(source_id must begin with '{CODERABBIT_APPROVED_REVIEW_PREFIX}'); "
            f"got source_id='{evidence.source_id}' (a green CI status from "
            "coderabbitai is not approval)"
        )
    return None


def _check_bugbot_clean(evidence: GateEvidence) -> Optional[str]:
    """Validate that Bugbot evidence carries zero error-severity findings."""
    count = _parse_bugbot_error_count(evidence.source_id)
    if count is None:
        return (
            f"Bugbot gate source_id must be '{BUGBOT_ERROR_COUNT_PREFIX}<n>' "
            f"with an integer count; got '{evidence.source_id}'"
        )
    if count > 0:
        return f"Bugbot reports {count} error-severity finding(s) (must be zero)"
    return None


def _check_telemetry_complete(evidence: GateEvidence) -> Optional[str]:
    """Every GateEvidence must carry all five provenance fields populated.

    A blank source_actor/source_url/source_id/head_sha/observed_at means
    the upstream capture failed — treat it as UNKNOWN, not GREEN.
    """
    missing = [
        name
        for name, value in (
            ("source_actor", evidence.source_actor),
            ("source_url", evidence.source_url),
            ("source_id", evidence.source_id),
            ("head_sha", evidence.head_sha),
            ("observed_at", evidence.observed_at),
        )
        if not value
    ]
    if missing:
        return f"telemetry incomplete (missing: {', '.join(missing)})"
    return None


def _check_sha_bound(evidence: GateEvidence, expected_head_sha: str) -> Optional[str]:
    """A green gate's `head_sha` must equal the current PR head SHA.

    This is the headline invariant — stale-SHA PASSes are unconditionally
    rejected. The skeptic gate workflow enforces the same contract
    internally; this is the merge-authority-level enforcement for every
    other gate.
    """
    if evidence.head_sha.lower() != expected_head_sha.lower():
        return (
            f"stale SHA: gate binds to {evidence.head_sha[:12]}, "
            f"current head is {expected_head_sha[:12]}"
        )
    return None


def assess_merge_authority(
    *,
    pr_number: int,
    expected_head_sha: str,
    gates: Dict[GateName, GateEvidence],
    disposition_note: str = "",
) -> MergeDecision:
    """Decide whether `pr_number` may merge at `expected_head_sha`.

    All seven gates are required. Any missing gate blocks. Any gate in
    `UNKNOWN` blocks (cannot-verify is not pass). Any gate in `RED`
    blocks (real defect). Any GREEN gate whose `head_sha` does not equal
    `expected_head_sha` blocks (stale-SHA). Any GREEN gate whose
    per-gate telemetry contract is incomplete blocks (garbage in).

    A disposition note is recorded verbatim in the audit telemetry; it
    does NOT alter the verdict. Operator assertions cannot substitute
    for per-gate evidence.

    Unknown keys in `gates` (e.g. typos) are silently ignored — the
    authority never substitutes a missing required gate for an unknown
    one.
    """
    # Only retain the seven named gates — anything else is filtered out so
    # a typo can't accidentally replace a required gate's evidence.
    recognized: Dict[GateName, GateEvidence] = {
        g: gates[g] for g in ALL_GATE_NAMES if g in gates
    }

    for gate in ALL_GATE_NAMES:
        evidence = recognized.get(gate)
        if evidence is None:
            return MergeDecision(
                verdict=MergeVerdict.BLOCK,
                pr_number=pr_number,
                expected_head_sha=expected_head_sha,
                gate_telemetry=recognized,
                failing_gate=gate,
                reason=f"gate '{gate.value}' has no evidence captured",
                disposition_note=disposition_note,
            )

        # Per-gate telemetry completeness — applies even to RED/UNKNOWN
        # gates so the audit trail always carries provenance metadata.
        telemetry_error = _check_telemetry_complete(evidence)
        if telemetry_error is not None:
            return MergeDecision(
                verdict=MergeVerdict.BLOCK,
                pr_number=pr_number,
                expected_head_sha=expected_head_sha,
                gate_telemetry=recognized,
                failing_gate=gate,
                reason=(
                    f"gate '{gate.value}' evidence is unparseable / incomplete: "
                    f"{telemetry_error}"
                ),
                disposition_note=disposition_note,
            )

        if evidence.status == GateStatus.RED:
            return MergeDecision(
                verdict=MergeVerdict.BLOCK,
                pr_number=pr_number,
                expected_head_sha=expected_head_sha,
                gate_telemetry=recognized,
                failing_gate=gate,
                reason=f"gate '{gate.value}' is RED (evidence: {evidence.source_id})",
                disposition_note=disposition_note,
            )

        if evidence.status == GateStatus.UNKNOWN:
            return MergeDecision(
                verdict=MergeVerdict.BLOCK,
                pr_number=pr_number,
                expected_head_sha=expected_head_sha,
                gate_telemetry=recognized,
                failing_gate=gate,
                reason=(
                    f"gate '{gate.value}' is UNKNOWN (cannot verify; "
                    f"unknown is not pass; source: {evidence.source_id})"
                ),
                disposition_note=disposition_note,
            )

        # status == GREEN — enforce SHA binding + per-gate payload shape.
        sha_error = _check_sha_bound(evidence, expected_head_sha)
        if sha_error is not None:
            return MergeDecision(
                verdict=MergeVerdict.BLOCK,
                pr_number=pr_number,
                expected_head_sha=expected_head_sha,
                gate_telemetry=recognized,
                failing_gate=gate,
                reason=f"gate '{gate.value}': {sha_error}",
                disposition_note=disposition_note,
            )

        if gate == GateName.CODERABBIT_APPROVED:
            approval_error = _check_coderabbit_approval(evidence)
            if approval_error is not None:
                return MergeDecision(
                    verdict=MergeVerdict.BLOCK,
                    pr_number=pr_number,
                    expected_head_sha=expected_head_sha,
                    gate_telemetry=recognized,
                    failing_gate=gate,
                    reason=f"gate '{gate.value}': {approval_error}",
                    disposition_note=disposition_note,
                )

        if gate == GateName.BUGBOT_CLEAN:
            bugbot_error = _check_bugbot_clean(evidence)
            if bugbot_error is not None:
                return MergeDecision(
                    verdict=MergeVerdict.BLOCK,
                    pr_number=pr_number,
                    expected_head_sha=expected_head_sha,
                    gate_telemetry=recognized,
                    failing_gate=gate,
                    reason=f"gate '{gate.value}': {bugbot_error}",
                    disposition_note=disposition_note,
                )

    return MergeDecision(
        verdict=MergeVerdict.MERGE,
        pr_number=pr_number,
        expected_head_sha=expected_head_sha,
        gate_telemetry=recognized,
        failing_gate=None,
        reason="all 7 gates GREEN at exact head SHA",
        disposition_note=disposition_note,
    )
