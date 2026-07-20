"""SHA-bound Skeptic gate for the dark-factory 7-green policy (issue #278).

This is the strict, fail-closed implementation. It is the gate-side
analogue of an Adjudicator — its job is to refuse everything that
does not meet the head-of-PR contract.

Public surface
--------------
- `parse_verdict(output)`            — strictly-anchored extractor.
                                       Returns `None` if any required
                                       field is missing, malformed,
                                       appears more than once, OR if
                                       the output contains extra prose
                                       outside the strict contract
                                       (no free-form text, no Markdown
                                       code blocks containing tokens).
- `bind_to_pr(parsed, ...)`          — validates the parsed verdict binds
                                       to the live PR head SHA. **Stale-
                                       SHA PASS must never satisfy a
                                       newer head** — the headline
                                       invariant.
- `verify_provenance(impl, reviewer)`— refuses PASS when the implementing
                                       model is the same model that
                                       reviewed.
- `extract_implementation_identity_from_commit(commit_subject)` —
                                       deterministic identity derivation
                                       from the commit-subject prefix
                                       (e.g. `claudem/minimax-M3: …`
                                       → `claude`). This is a pure
                                       string-prefix match — no ZFC
                                       keyword routing on free-form
                                       text. Unknown / un-prefixed
                                       subjects map to `unknown`, which
                                       the gate refuses PASS on.
- `bind_reviewer_identity(cli_name, declared_identity)` —
                                       refuses a verdict whose declared
                                       IDENTITY does not match the CLI
                                       that emitted it (codex CLI must
                                       declare `codex`, gemini must
                                       declare `gemini`).
- `format_comment(...)`              — idempotent upsert body — the
                                       `MARKER` HTML comment makes the
                                       GitHub comment replaceable in
                                       place rather than appended.
- `evaluate(output, error, ...)`     — deterministic verdict-binding for
                                       a single reviewer call.
- `aggregate_results(results, ...)`  — combines per-reviewer results; ALL
                                       must PASS for the gate to count
                                       as green. Rejects duplicate
                                       reviewer identities.
- `verify_published_comment(...)`    — full equality read-back. ALL six
                                       fields (HEAD_SHA, REPO,
                                       PR_NUMBER, VERDICT, REVIEWER,
                                       IMPLEMENTATION_PROVENANCE) must
                                       equal what we wrote; missing or
                                       mismatched values fail closed.
- `build_prompt(...)`                — pure string assembly. No judgment
                                       calls — the reviewer is the one
                                       that judges the diff.
- `BeadContract` / `AcceptanceItem` / `PriorFinding` — the durable
                                       input to the contract-echo step
                                       (issue #386).
- `load_bead_contract(source)`      — load a contract from a dict,
                                       JSON file path, or pass-through.
                                       Rejects empty acceptance items
                                       and duplicate IDs.
- `parse_contract_echo(output, contract)` — extract per-item verdicts
                                       (`ADDRESSED file:line` /
                                       `NOT-ADDRESSED` / `N-A` with
                                       reason) from the reviewer's
                                       `CONTRACT_ECHO:` block.
- `evaluate_contract_echo(report, contract)` — fail-closed check:
                                       every acceptance item must be
                                       `ADDRESSED` or `N-A`. Any
                                       `NOT-ADDRESSED` for an
                                       acceptance item surfaces the
                                       verbatim text in
                                       `unaddressed_items` and
                                       `constraint`.

ZFC compliance
--------------
The reviewer (a non-Claude CLI) emits a structured verdict. This module
is **only** allowed to validate the structured verdict, bind the SHA,
shape the comment, and aggregate per-reviewer results. There is no
`if text.contains("...")` routing, no scoring, no semantic classification
of the diff. Implementation-identity derivation is a deterministic
prefix match on the commit subject — not a keyword classification of
free-form prose. Failures flow from missing, malformed, duplicate, or
extra-prose structured fields — never from this code's opinion of the
diff.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from typing import List, Literal, Optional, Tuple, Union


# Unique HTML marker used by the GitHub comment upsert logic. Any prior
# bot comment with this marker is the one we replace; comments without
# it are not ours to touch.
MARKER = "<!-- skeptic-gate-verdict -->"

# Allowed model identities, normalized to a known set so provenance
# checks don't have to parse free-form author strings. Unknown model
# strings map to `unknown` (which is never allowed as a self-review).
ModelIdentity = Literal["claude", "codex", "gemini", "unknown"]

# Mapping from reviewer CLI name to the identity it MUST declare in
# its structured verdict. The CLI is bound to its identity — a codex
# process claiming `gemini` (or vice-versa) is rejected outright. This
# prevents a malicious PR-controlled reviewer invocation from
# impersonating another reviewer.
REVIEWER_CLI_TO_IDENTITY = {
    "codex": "codex",
    "gemini": "gemini",
}

# Commit-subject prefixes that map to model identities. The mapping is
# a deterministic prefix match — no ZFC keyword routing on free-form
# text. A subject that does not start with one of these prefixes
# resolves to `unknown`, which the gate refuses PASS on.
#
# Adding a new prefix here IS an explicit PR-time decision — the
# reviewer must review and merge the addition. Comment-only additions
# (sed/keyword substitutions in commit messages) cannot change the
# implementation identity.
COMMIT_PREFIX_TO_IDENTITY = {
    "claude/": "claude",
    "claudem/": "claude",
    "codex/": "codex",
    "codexm/": "codex",
    "gemini/": "gemini",
    "geminim/": "gemini",
    "human:": "human",
    "antig/": "unknown",  # Antigravity is the IDE; identity is whatever it spawned
}


@dataclass(frozen=True)
class ParsedVerdict:
    """A single, strictly-parsed reviewer verdict.

    Every required field is populated. If `reason`, `identities`, or any
    other than the headline four is missing in the reviewer's output,
    `parse_verdict` returns `None` rather than producing a partial
    `ParsedVerdict`.

    The execution-evidence fields (`test_run_evidence`,
    `lint_run_evidence`, `grep_cites`, `head_commit_verified`) are the
    headline enforcement of issue #384: a verdict that does not prove
    the reviewer actually executed the repo's tests/lint and grepped
    the call sites cited in the diff is rejected at parse time. They
    are stored as parsed structures (or `None` on absence — which the
    contract requires never happens for a valid verdict).
    """

    verdict: Literal["PASS", "FAIL"]
    head_sha: str
    repo: str
    pr_number: int
    reason: str
    reviewer_identity: str  # the model that emitted the verdict
    raw_excerpt: str
    # Execution-evidence fields (issue #384):
    test_run_evidence: Optional["ParsedTestRun"] = None
    lint_run_evidence: Optional["ParsedLintRun"] = None
    grep_cites: str = ""
    head_commit_verified: str = ""


@dataclass(frozen=True)
class ParsedTestRun:
    """Parsed `TEST_RUN_EVIDENCE` field from a reviewer verdict.

    The reviewer must report the result of actually running the
    repo's test suite on the PR HEAD. The form is:

        TEST_RUN_EVIDENCE: passed=<int> failed=<int> skipped=<int> exit=<int>

    `exit=0` AND `failed=0` is required for a PASS verdict —
    `parse_verdict` rejects internally inconsistent claims.
    """

    passed: int
    failed: int
    skipped: int
    exit: int


@dataclass(frozen=True)
class ParsedLintRun:
    """Parsed `LINT_RUN_EVIDENCE` field from a reviewer verdict.

    The reviewer must report the result of actually running the
    repo's primary linter on the PR HEAD. The form is:

        LINT_RUN_EVIDENCE: tool=<name> errors=<int> warnings=<int>

    `errors=0` is required for a PASS verdict — `parse_verdict`
    rejects the verdict if the reviewer claims green while the
    linter reports errors.
    """

    tool: str
    errors: int
    warnings: int


@dataclass(frozen=True)
class ValidationResult:
    """Outcome of `bind_to_pr`.

    `ok=True` means the parsed verdict is consistent with the current
    PR context and can be honored. `ok=False` means it must be rejected
    (stale SHA, wrong repo, wrong PR number) — `verdict` is left as
    None so the caller cannot accidentally propagate a stale PASS.
    """

    ok: bool
    reason: str
    verdict: Optional[Literal["PASS", "FAIL"]] = None
    parsed: Optional[ParsedVerdict] = None


@dataclass(frozen=True)
class SkepticResult:
    """What the deterministic evaluator hands back to the workflow shell."""

    check_state: Literal["success", "failure"]
    verdict: Optional[Literal["PASS", "FAIL"]]
    reason: str  # human-readable; lands in the comment and commit status
    comment_body: str
    parsed: Optional[ParsedVerdict] = None
    reviewer: Optional[str] = None


# ---------------------------------------------------------------------------
# Contract-echo types (issue #386)
# ---------------------------------------------------------------------------
#
# The skeptic gate currently evaluates the diff in isolation. The
# contract-echo step is the fix: the bead author writes a contract
# (description, prior findings, acceptance items) once, and the
# reviewer must emit per-item verdicts against it. Any acceptance
# item that is NOT-ADDRESSED in the diff fails the gate closed and
# surfaces the item's text verbatim — so the next roll's worker
# reads the exact problem, not a paraphrase.


ContractEchoVerdict = Literal["ADDRESSED", "NOT-ADDRESSED", "N-A"]


@dataclass(frozen=True)
class AcceptanceItem:
    """A single acceptance criterion from the bead's contract.

    `id` is the canonical identifier the reviewer echoes in the
    `CONTRACT_ECHO:` block (e.g. `A1`, `A2`). `text` is the
    verbatim wording the bead author wrote — it is the durable
    input the gate checks against and the constraint that flows
    into the next roll on NOT-ADDRESSED.
    """

    id: str
    text: str


@dataclass(frozen=True)
class PriorFinding:
    """A finding from a prior round that the bead author wants the
    reviewer to address.

    `source` is free-form (e.g. "r5 reviewer", "skeptic r3") and
    `text` is the verbatim finding. The prompt embeds both so the
    reviewer can address each prior item.
    """

    source: str
    text: str


@dataclass(frozen=True)
class BeadContract:
    """The bead's contract: the durable input to the contract-echo step.

    `id` is the bead identifier (`jleechan-pq08` for this work).
    `description` is the bead's body — the goal the worker is
    implementing. `prior_findings` lists findings from prior rounds
    that the bead author wants the reviewer to re-verify. Every
    `acceptance_items` entry is what the gate requires per-item
    verdicts for; if even one is `NOT-ADDRESSED` in the diff, the
    gate fails closed.
    """

    id: str
    description: str
    prior_findings: Tuple[PriorFinding, ...] = ()
    acceptance_items: Tuple[AcceptanceItem, ...] = ()


@dataclass(frozen=True)
class ContractEchoItem:
    """A single per-item verdict extracted from the reviewer's output.

    `cite` is the file:line where the reviewer says the item is
    addressed (required for `ADDRESSED`, may be empty for
    `NOT-ADDRESSED` or `N-A`). `reason` is the reviewer's
    justification — required for `N-A` and `NOT-ADDRESSED`.
    """

    id: str
    verdict: ContractEchoVerdict
    cite: str = ""
    reason: str = ""


@dataclass(frozen=True)
class ContractEchoReport:
    """The full set of per-item verdicts the reviewer emitted.

    `items` may be a strict subset of the contract's items when
    the reviewer cited unknown IDs or omitted items. `evaluate_contract_echo`
    cross-references this against the contract to determine which
    items are unaddressed.
    """

    items: Tuple[ContractEchoItem, ...]


@dataclass(frozen=True)
class ContractEchoVerdictResult:
    """Outcome of `evaluate_contract_echo`.

    `ok=True` means every acceptance item is `ADDRESSED` or
    `N-A` (with a reason). `ok=False` means at least one item
    is `NOT-ADDRESSED` or missing; `unaddressed_items` carries
    the full `AcceptanceItem` records (verbatim text) so the
    next roll's worker reads the exact problem, not a paraphrase.
    `constraint` is a human-readable constraint string suitable
    for embedding in the gate's failure comment or handing to
    the next roll.
    """

    ok: bool
    unaddressed_items: Tuple[AcceptanceItem, ...] = ()
    constraint: str = ""


# ---------------------------------------------------------------------------
# Anchored, case-insensitive, multi-match-detecting regexes
# ---------------------------------------------------------------------------
#
# Each field MUST appear EXACTLY ONCE on its own line. Anchored to start-
# of-line (`^` with re.MULTILINE) so the token cannot be smuggled inside
# a code block. re.IGNORECASE so a reviewer that emits `Verdict: Pass`
# (mixed case) is still accepted.
#
# `findall` returns ALL non-overlapping matches in the output. A
# reviewer that emits two VERDICT lines (one PASS inside a code block,
# one FAIL on its own) is rejected outright — anti-injection.

_VERDICT_RE = re.compile(
    r"^\s*VERDICT\s*:\s*(PASS|FAIL)\s*$", re.MULTILINE | re.IGNORECASE
)
_SHA_RE = re.compile(
    r"^\s*HEAD_SHA\s*:\s*([0-9a-f]{7,64})\s*$", re.MULTILINE | re.IGNORECASE
)
_FULL_SHA_RE = re.compile(
    r"^\s*HEAD_SHA\s*:\s*([0-9a-f]{40})\s*$", re.MULTILINE | re.IGNORECASE
)
_REPO_RE = re.compile(
    r"^\s*REPO\s*:\s*([\w.\-]+/[\w.\-]+)\s*$", re.MULTILINE | re.IGNORECASE
)
_PR_RE = re.compile(r"^\s*PR_NUMBER\s*:\s*(\d+)\s*$", re.MULTILINE | re.IGNORECASE)
_REASON_RE = re.compile(r"^\s*REASON\s*:\s*(.+?)\s*$", re.MULTILINE | re.IGNORECASE)
_IDENTITY_RE = re.compile(
    r"^\s*IDENTITY\s*:\s*(claude|codex|gemini|human|unknown)\s*$",
    re.MULTILINE | re.IGNORECASE,
)

# ---------------------------------------------------------------------------
# Execution-evidence regexes (issue #384)
# ---------------------------------------------------------------------------
#
# These four fields are MANDATORY for a valid verdict. They prove that
# the reviewer actually ran the repo's test suite + lint + grep on the
# PR HEAD, rather than pattern-matching the diff alone. Pattern-matched
# PASS verdicts are how prior PRs (#382: regression test never invokes
# code under test; #365 r5: fail-open paths) slipped past the gate.
#
# Each field MUST appear EXACTLY ONCE on its own line. Anchored to
# start-of-line + case-insensitive, same rule as the 6-field contract.

_TEST_RUN_EVIDENCE_RE = re.compile(
    r"^\s*TEST_RUN_EVIDENCE\s*:\s*"
    r"passed\s*=\s*(\d+)\s+"
    r"failed\s*=\s*(\d+)\s+"
    r"skipped\s*=\s*(\d+)\s+"
    r"exit\s*=\s*(-?\d+)\s*$",
    re.MULTILINE | re.IGNORECASE,
)
_LINT_RUN_EVIDENCE_RE = re.compile(
    r"^\s*LINT_RUN_EVIDENCE\s*:\s*"
    r"tool\s*=\s*([a-zA-Z0-9_.\-]+)\s+"
    r"errors\s*=\s*(\d+)\s+"
    r"warnings\s*=\s*(\d+)\s*$",
    re.MULTILINE | re.IGNORECASE,
)
_GREP_CITES_RE = re.compile(
    r"^\s*GREP_CITES\s*:\s*(\S[^\n]*?)[ \t]*$",
    re.MULTILINE | re.IGNORECASE,
)
_HEAD_COMMIT_VERIFIED_RE = re.compile(
    r"^\s*HEAD_COMMIT_VERIFIED\s*:\s*([0-9a-f]{40})\s*$",
    re.MULTILINE | re.IGNORECASE,
)

# Field-name regexes used by the no-prose check. A field line MUST
# consist only of "<FIELD>: <value>" with nothing else on the line.
# Case-insensitive to match the per-field regexes (Verdict: Pass is OK).
_FIELD_LINE_RE = re.compile(r"^[A-Z_]+\s*:.*$", re.MULTILINE | re.IGNORECASE)

# Required execution-evidence fields (issue #384). The deterministic
# gate refuses to honor a verdict that does not include all four —
# pattern-matched PASS verdicts slipped vacuous regression tests and
# fail-open paths past the gate in PRs #382 and #365 r5.
EXECUTION_EVIDENCE_FIELDS = (
    "TEST_RUN_EVIDENCE",
    "LINT_RUN_EVIDENCE",
    "GREP_CITES",
    "HEAD_COMMIT_VERIFIED",
)


# ---------------------------------------------------------------------------
# Contract-echo regex (issue #386)
# ---------------------------------------------------------------------------
#
# A per-item verdict line looks like:
#   ITEM: A1 VERDICT: ADDRESSED CITE: runner/skeptic_gate.py:42
#   ITEM: A2 VERDICT: NOT-ADDRESSED REASON: omitted from diff
#   ITEM: A3 VERDICT: N-A REASON: not applicable this round
#
# The line is anchored at start-of-line (re.MULTILINE) and the
# verdict token is restricted to the three known values. The cite
# is a file:line; the reason is free text. Each line is one
# item — no chained semicolon-separated items (parity with
# `GREP_CITES`).

CONTRACT_ECHO_HEADER_RE = re.compile(
    r"^\s*CONTRACT_ECHO\s*:\s*$",
    re.MULTILINE | re.IGNORECASE,
)
CONTRACT_ECHO_LINE_RE = re.compile(
    r"^\s*ITEM\s*:\s*(?P<id>[A-Za-z0-9._\-]+)\s+"
    r"VERDICT\s*:\s*(?P<verdict>ADDRESSED|NOT-ADDRESSED|N-A)\s+"
    r"(?:CITE\s*:\s*(?P<cite>[^\s\n][^\n]*?)|REASON\s*:\s*(?P<reason>[^\n]+?))"
    r"\s*$",
    re.MULTILINE | re.IGNORECASE,
)


# ---------------------------------------------------------------------------
# Contract-echo loader (issue #386)
# ---------------------------------------------------------------------------


def load_bead_contract(source: Union[str, os.PathLike, dict, BeadContract]) -> BeadContract:
    """Load a `BeadContract` from a dict, a JSON file path, or pass-through.

    Acceptable inputs:
      - `BeadContract`   — returned unchanged.
      - `dict`           — the in-memory shape callers wire into the gate.
      - `str` / Path     — path to a JSON file on disk; the file is read
                          and parsed. A non-existent file is a hard
                          error (we never silently fabricate a contract).

    Validation:
      - `id` is required and non-empty.
      - `description` defaults to empty string.
      - `prior_findings` is optional (defaults to empty).
      - `acceptance_items` MUST be non-empty — without items the
        contract-echo step has nothing to verify per-item against.
      - Duplicate `acceptance_items` IDs are rejected (per-item
        verdicts would not be uniquely addressable).

    Reject anything that isn't a dict / path / BeadContract — a
    stringified JSON literal in argv is a known injection surface.
    """
    if isinstance(source, BeadContract):
        return source
    if isinstance(source, dict):
        data = source
    elif isinstance(source, (str, os.PathLike)):
        path = os.fspath(source)
        with open(path, "r", encoding="utf-8") as fh:
            data = json.loads(fh.read())
    else:
        raise TypeError(
            f"load_bead_contract: source must be dict, path, or BeadContract; "
            f"got {type(source).__name__}"
        )

    if not isinstance(data, dict):
        raise TypeError(
            f"load_bead_contract: parsed contract must be a JSON object; "
            f"got {type(data).__name__}"
        )

    bead_id = str(data.get("id") or "").strip()
    if not bead_id:
        raise ValueError("load_bead_contract: 'id' is required and must be non-empty")

    description = str(data.get("description") or "")

    raw_prior = data.get("prior_findings") or []
    if not isinstance(raw_prior, list):
        raise TypeError("load_bead_contract: 'prior_findings' must be a list")
    prior_findings: List[PriorFinding] = []
    for pf in raw_prior:
        if not isinstance(pf, dict):
            raise TypeError(
                f"load_bead_contract: prior_finding must be a dict; got {type(pf).__name__}"
            )
        prior_findings.append(
            PriorFinding(
                source=str(pf.get("source") or "").strip(),
                text=str(pf.get("text") or "").strip(),
            )
        )

    raw_items = data.get("acceptance_items") or []
    if not isinstance(raw_items, list):
        raise TypeError("load_bead_contract: 'acceptance_items' must be a list")
    if not raw_items:
        raise ValueError(
            "load_bead_contract: 'acceptance_items' must be non-empty — the "
            "contract-echo step requires per-item verdicts and an empty list "
            "has nothing to verify against"
        )
    seen_ids = set()
    acceptance_items: List[AcceptanceItem] = []
    for it in raw_items:
        if not isinstance(it, dict):
            raise TypeError(
                f"load_bead_contract: acceptance_item must be a dict; got {type(it).__name__}"
            )
        item_id = str(it.get("id") or "").strip()
        if not item_id:
            raise ValueError("load_bead_contract: acceptance_item.id is required")
        if item_id in seen_ids:
            raise ValueError(
                f"load_bead_contract: duplicate acceptance_item.id={item_id!r}"
            )
        seen_ids.add(item_id)
        acceptance_items.append(
            AcceptanceItem(
                id=item_id,
                text=str(it.get("text") or "").strip(),
            )
        )

    return BeadContract(
        id=bead_id,
        description=description,
        prior_findings=tuple(prior_findings),
        acceptance_items=tuple(acceptance_items),
    )


# ---------------------------------------------------------------------------
# Contract-echo parser (issue #386)
# ---------------------------------------------------------------------------


def _strip_contract_echo_block(output: str) -> str:
    """Remove the `CONTRACT_ECHO:` block from a reviewer output so the
    10-field `parse_verdict` can run on the remainder.

    The block is at the END of the output (after the 10 structured
    fields). The header line (`CONTRACT_ECHO:`) is dropped, every
    `ITEM:` line is dropped, and any blank lines immediately around
    the block are normalized. If the block is not present, the
    output is returned unchanged (so the function is safe to call
    regardless of whether a contract was supplied).
    """
    if not isinstance(output, str):
        return output
    header_match = CONTRACT_ECHO_HEADER_RE.search(output)
    if not header_match:
        return output
    head_part = output[: header_match.start()]
    after = output[header_match.end():]
    kept_after: List[str] = []
    for raw_line in after.splitlines():
        stripped = raw_line.strip()
        if stripped.startswith("ITEM:"):
            continue
        kept_after.append(raw_line)
    out = head_part.rstrip("\n") + "\n" + "\n".join(kept_after).rstrip() + "\n"
    return out


def parse_contract_echo(
    output: object, contract: BeadContract
) -> Optional[ContractEchoReport]:
    """Extract a `ContractEchoReport` from a reviewer's free-form stdout.

    The expected block format is:

        CONTRACT_ECHO:
        ITEM: <id> VERDICT: <ADDRESSED|NOT-ADDRESSED|N-A> CITE: <file:line>
        ITEM: <id> VERDICT: <N-A> REASON: <free text>
        ...

    The block MUST be present (one or more `ITEM:` lines) for the
    output to be considered. A reviewer that omits the block has
    not addressed the contract — the caller should treat the
    resulting `None` as every item NOT-ADDRESSED.

    Per-item rules:
      - `ADDRESSED` requires a `CITE:` value matching the
        `file:line` pattern (a path followed by `:NUMBER`).
      - `N-A` and `NOT-ADDRESSED` require a `REASON:` value (no
        empty justification).
      - Items whose ID is not on the contract are kept in the
        report (caller decides what to do) but the unknown ID
        still counts as the contract item being unaddressed.
    """
    if not isinstance(output, str):
        return None
    if not isinstance(contract, BeadContract):
        return None

    # Locate the CONTRACT_ECHO: header line, then walk forward and
    # collect the immediately-following `ITEM:` lines. We stop at the
    # first non-blank, non-ITEM line (the strict no-prose contract
    # the gate enforces elsewhere — anything after the block is
    # considered out-of-block and ignored).
    header_match = CONTRACT_ECHO_HEADER_RE.search(output)
    if not header_match:
        return None
    after = output[header_match.end():]
    item_lines: List[str] = []
    for raw_line in after.splitlines():
        stripped = raw_line.strip()
        if not stripped:
            # Blank lines are allowed within the block.
            continue
        if not stripped.startswith("ITEM:"):
            # Out-of-block content; stop walking.
            break
        item_lines.append(raw_line)
    if not item_lines:
        return None

    items: List[ContractEchoItem] = []
    for raw_line in item_lines:
        line = raw_line.strip()
        if not line:
            continue
        m = CONTRACT_ECHO_LINE_RE.match(line)
        if m is None:
            # An unparseable line in the contract-echo block: the
            # reviewer emitted something we cannot interpret. Per
            # the strict no-prose contract the gate enforces for
            # the headline verdict, we reject the whole block
            # rather than guess.
            return None
        item_id = m.group("id")
        verdict_token = m.group("verdict").upper()
        cite = (m.group("cite") or "").strip()
        reason = (m.group("reason") or "").strip()

        if verdict_token == "ADDRESSED":
            if not cite:
                return None
            if not re.match(r"^[\w./\-]+:\d+$", cite):
                return None
        else:
            # N-A or NOT-ADDRESSED — reason is required.
            if not reason:
                return None

        items.append(
            ContractEchoItem(
                id=item_id,
                verdict=verdict_token,  # type: ignore[arg-type]
                cite=cite,
                reason=reason,
            )
        )

    if not items:
        return None
    return ContractEchoReport(items=tuple(items))


# ---------------------------------------------------------------------------
# Contract-echo evaluator (issue #386 — the headline invariant)
# ---------------------------------------------------------------------------


def evaluate_contract_echo(
    report: Optional[ContractEchoReport],
    contract: BeadContract,
) -> ContractEchoVerdictResult:
    """Check that every acceptance item is ADDRESSED or N-A.

    - `ADDRESSED` (with a `CITE:` file:line): the reviewer claims the
      diff addresses this item; we trust the reviewer on the cite
      (we don't second-guess the file:line — the executor evidence
      contract in `parse_verdict` is what backs the claim).
    - `N-A` (with a `REASON:`): the reviewer says the item is not
      applicable this round; this counts as a pass IF the reason is
      non-empty. An empty `N-A` reason is rejected at parse time
      (see `parse_contract_echo`), so by the time we get here, a
      `N-A` item is always with a reason.
    - `NOT-ADDRESSED` (with a `REASON:`): the reviewer flags the
      item as unaddressed; we surface it verbatim in
      `unaddressed_items` and `constraint`.
    - Missing item (the reviewer's report does not contain the
      item's ID): the item is unaddressed — the reviewer did not
      cover it.
    - `report is None` (no `CONTRACT_ECHO:` block at all): every
      contract item is unaddressed.

    The `constraint` string carries the unaddressed items VERBATIM
    (the bead author's text, not a paraphrase) so the next roll's
    worker reads the exact problem. This is the headline invariant
    of issue #386: constraint extraction MUST carry the
    unaddressed items verbatim.
    """
    if not isinstance(contract, BeadContract):
        return ContractEchoVerdictResult(ok=False, constraint="contract is not a BeadContract")

    # Build a lookup: item_id -> per-item verdict emitted by the reviewer.
    emitted: dict = {}
    if report is not None:
        for it in report.items:
            # First-write-wins on duplicate IDs; subsequent writes
            # for the same ID are ignored. The strict no-duplicate
            # invariant is on the contract side (load_bead_contract).
            emitted.setdefault(it.id, it)

    unaddressed: List[AcceptanceItem] = []
    for item in contract.acceptance_items:
        verdict_item = emitted.get(item.id)
        if verdict_item is None:
            unaddressed.append(item)
            continue
        if verdict_item.verdict == "ADDRESSED":
            continue
        if verdict_item.verdict == "N-A":
            # Reason is required at parse time; defensively re-check.
            if not verdict_item.reason:
                unaddressed.append(item)
            continue
        # NOT-ADDRESSED or anything else: unaddressed.
        unaddressed.append(item)

    if not unaddressed:
        return ContractEchoVerdictResult(
            ok=True,
            unaddressed_items=(),
            constraint="",
        )

    # Build the constraint string with the unaddressed items'
    # VERBATIM text. The worker's next-roll input MUST read the
    # exact problem the bead author wrote.
    lines = [
        f"Contract-echo gate: {len(unaddressed)} acceptance item(s) NOT-ADDRESSED.",
        "These items must be addressed in the next roll. The text below is "
        "verbatim from the bead author's contract — do not paraphrase:",
        "",
    ]
    for item in unaddressed:
        lines.append(f"- {item.id}: {item.text}")
    return ContractEchoVerdictResult(
        ok=False,
        unaddressed_items=tuple(unaddressed),
        constraint="\n".join(lines),
    )


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------


def parse_verdict(output: object) -> Optional[ParsedVerdict]:
    """Extract a structured verdict from a reviewer's free-form stdout.

    **Strict no-prose contract** (per post-audit comment 4953064910):

    - 10 required fields, each MUST appear EXACTLY ONCE on its own line:
      `VERDICT`, `HEAD_SHA`, `REPO`, `PR_NUMBER`, `REASON`, `IDENTITY`,
      `TEST_RUN_EVIDENCE`, `LINT_RUN_EVIDENCE`, `GREP_CITES`,
      `HEAD_COMMIT_VERIFIED`. The four execution-evidence fields
      (issue #384) prove the reviewer actually executed the repo's
      tests+lint+grep on the PR HEAD, rather than pattern-matching
      the diff alone.
    - The output MUST consist ONLY of:
        - up to one comment line (e.g. a leading `# reviewer: codex`)
        - the 10 contract fields, each on its own line
        - any number of blank lines
      No Markdown code blocks, no extra prose, no second VERDICT line
      smuggled inside a triple-backtick fence.
    - Any field appearing more than once → reject (anti-injection).
    - Any required field missing → reject (fail-closed).
    - The 10 lines themselves MUST be the ONLY non-blank lines (no
      trailing prose, no surrounding commentary).

    A reviewer that wraps the contract in a Markdown code block
    (```\\nVERDICT: PASS\\n…\\n```) is rejected. The deterministic
    parser enforces the bare-text contract, not "find a record
    somewhere inside prose."
    """
    if not isinstance(output, str):
        return None

    # ---- No-prose check --------------------------------------------------
    # Strip the leading comment line if present; reject anything that
    # still looks like Markdown prose or contains a code fence.
    if "```" in output:
        # A code-fence in reviewer output is a code-block injection
        # attempt. Reject unconditionally.
        return None

    # Count non-blank lines that start with a contract field token
    # ("<UPPER>:"). Anything else is prose and must be a leading
    # comment only — single line, starting with `#`.
    lines = output.splitlines()
    field_lines = 0
    leading_comment_lines = 0
    seen_field_names = set()
    for line in lines:
        stripped = line.strip()
        if not stripped:
            continue
        if re.match(r"^[A-Z_]+\s*:", stripped, re.IGNORECASE):
            field_lines += 1
            # Track the field name so we can enforce EXACTLY 10 distinct
            # contract fields. Per post-audit comment 4953116428, the
            # previous version accepted a 7th field. Now any 11th field
            # (or duplicate of any of the 10) → reject.
            field_name_match = re.match(r"^([A-Z_]+)\s*:", stripped, re.IGNORECASE)
            if field_name_match:
                fname = field_name_match.group(1).upper()
                if fname in seen_field_names:
                    # Duplicate field — anti-injection.
                    return None
                seen_field_names.add(fname)
            continue
        # Allow ONE leading comment line (e.g. "# reviewer: codex")
        # before any field line; reject any prose AFTER a field line.
        if field_lines == 0 and leading_comment_lines == 0 and stripped.startswith("#"):
            leading_comment_lines = 1
            continue
        # Prose outside the leading-comment window — reject.
        return None

    # ---- Field extraction (anchored, exactly-once) ----------------------
    verdicts = _VERDICT_RE.findall(output)
    shas = _FULL_SHA_RE.findall(output)
    short_shas = _SHA_RE.findall(output)
    repos = _REPO_RE.findall(output)
    prs = _PR_RE.findall(output)
    reasons = _REASON_RE.findall(output)
    identities = _IDENTITY_RE.findall(output)
    # Execution-evidence fields (issue #384):
    test_run_evidence = _TEST_RUN_EVIDENCE_RE.findall(output)
    lint_run_evidence = _LINT_RUN_EVIDENCE_RE.findall(output)
    grep_cites = _GREP_CITES_RE.findall(output)
    head_commit_verified = _HEAD_COMMIT_VERIFIED_RE.findall(output)

    # Full-length SHA is required (40 hex chars). A reviewer that emits
    # only a short SHA hasn't fully bound its verdict.
    if len(shas) != 1 or len(short_shas) != 1:
        return None
    sha = shas[0].lower()
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        return None

    # All ten required fields must be exactly one each. IDENTITY is
    # required for provenance (refuses self-review). The four
    # execution-evidence fields are required by issue #384 — a
    # verdict without them is a vacuous PASS, and the gate must
    # refuse to honor it.
    if (
        len(verdicts) != 1
        or len(repos) != 1
        or len(prs) != 1
        or len(reasons) != 1
        or len(identities) != 1
        or len(test_run_evidence) != 1
        or len(lint_run_evidence) != 1
        or len(grep_cites) != 1
        or len(head_commit_verified) != 1
    ):
        return None

    # Exact 10-field contract: no 11th field allowed. The
    # seen_field_names set already enforced no duplicates; here we
    # also enforce that EXACTLY 10 distinct contract fields are
    # present.
    expected_fields = {
        "VERDICT",
        "HEAD_SHA",
        "REPO",
        "PR_NUMBER",
        "REASON",
        "IDENTITY",
        "TEST_RUN_EVIDENCE",
        "LINT_RUN_EVIDENCE",
        "GREP_CITES",
        "HEAD_COMMIT_VERIFIED",
    }
    if seen_field_names != expected_fields:
        return None

    identity_token = identities[0].lower()
    if identity_token not in ("claude", "codex", "gemini", "human", "unknown"):
        return None

    verdict_token = verdicts[0].upper()
    if verdict_token not in ("PASS", "FAIL"):
        return None

    # `short_shas` is `shas` here (same regex anchored), so this list has
    # length 1; keep the dedupe explicit to guard against future drift.
    if len(set(short_shas)) != 1:
        return None

    # ---- Execution-evidence consistency checks (issue #384) ------------
    # A reviewer that claims PASS but reports failed>0, exit!=0, lint
    # errors>0, or an empty GREP_CITES is internally inconsistent and
    # must be rejected — the gate cannot accept a verdict where the
    # execution evidence contradicts the verdict.
    test_passed, test_failed, test_skipped, test_exit = (
        int(test_run_evidence[0][0]),
        int(test_run_evidence[0][1]),
        int(test_run_evidence[0][2]),
        int(test_run_evidence[0][3]),
    )
    if test_exit != 0 or test_failed != 0:
        # A non-zero exit OR any failed test is a hard fail signal.
        # The reviewer cannot claim a clean PASS.
        return None

    lint_tool, lint_errors, lint_warnings = (
        lint_run_evidence[0][0],
        int(lint_run_evidence[0][1]),
        int(lint_run_evidence[0][2]),
    )
    if lint_errors != 0:
        return None

    grep_cite_value = grep_cites[0].strip()
    if not grep_cite_value:
        # Empty GREP_CITES means the reviewer cited no enforcement call
        # sites — the gate cannot verify the reviewer's claims about
        # what code does or does not enforce. Reject.
        return None
    # Require at least one file:line cite (a "path:number" token).
    # The format is `path/to/file.py:LINE;path/to/file.py:LINE` —
    # semicolon-separated `path:number` pairs. A value like `;` or
    # `;;` contains separators but no real citation → reject.
    cite_tokens = [t.strip() for t in grep_cite_value.split(";") if t.strip()]
    if not any(re.match(r"^[\w./\-]+:\d+$", tok) for tok in cite_tokens):
        # No `file:line` cite pair — the reviewer did not cite any
        # enforcement call sites. Reject.
        return None

    head_verified_sha = head_commit_verified[0].lower()
    if not re.fullmatch(r"[0-9a-f]{40}", head_verified_sha):
        return None
    # HEAD_COMMIT_VERIFIED must equal HEAD_SHA byte-for-byte. If the
    # reviewer's "verified HEAD" differs from the gate SHA, the reviewer
    # was operating on a different tree (most likely the diff they read
    # is not what the gate sees). Reject.
    if head_verified_sha != sha:
        return None

    test_evidence_obj = ParsedTestRun(
        passed=test_passed,
        failed=test_failed,
        skipped=test_skipped,
        exit=test_exit,
    )
    lint_evidence_obj = ParsedLintRun(
        tool=lint_tool,
        errors=lint_errors,
        warnings=lint_warnings,
    )

    return ParsedVerdict(
        verdict=verdict_token,  # type: ignore[arg-type]
        head_sha=sha,
        repo=repos[0],
        pr_number=int(prs[0]),
        reason=reasons[0].strip(),
        reviewer_identity=identity_token,
        raw_excerpt=output[:500],
        test_run_evidence=test_evidence_obj,
        lint_run_evidence=lint_evidence_obj,
        grep_cites=grep_cite_value,
        head_commit_verified=head_verified_sha,
    )


# ---------------------------------------------------------------------------
# Binding
# ---------------------------------------------------------------------------


def bind_to_pr(
    parsed: ParsedVerdict,
    *,
    expected_repo: str,
    expected_pr_number: int,
    expected_head_sha: str,
) -> ValidationResult:
    """Validate that the parsed verdict matches the current PR head."""
    expected_head_sha_norm = expected_head_sha.lower()
    if parsed.head_sha != expected_head_sha_norm:
        return ValidationResult(
            ok=False,
            reason=(
                f"stale SHA: verdict binds to {parsed.head_sha[:12]}, "
                f"current head is {expected_head_sha_norm[:12]}"
            ),
        )
    if parsed.repo != expected_repo:
        return ValidationResult(
            ok=False,
            reason=f"repo mismatch: verdict says {parsed.repo}, expected {expected_repo}",
        )
    if parsed.pr_number != expected_pr_number:
        return ValidationResult(
            ok=False,
            reason=(
                f"PR number mismatch: verdict says {parsed.pr_number}, "
                f"expected {expected_pr_number}"
            ),
        )
    return ValidationResult(
        ok=True,
        reason="verdict binds to expected PR head",
        verdict=parsed.verdict,
        parsed=parsed,
    )


# ---------------------------------------------------------------------------
# Provenance
# ---------------------------------------------------------------------------


def extract_implementation_identity_from_commit(commit_subject: str) -> str:
    """Deterministically derive the implementer's identity from the
    commit subject prefix.

    The mapping is a pure prefix match — no ZFC keyword routing on
    free-form text. A subject that does not start with one of the
    prefixes in `COMMIT_PREFIX_TO_IDENTITY` returns `"unknown"`,
    which the gate refuses PASS on (conservative fail-closed).

    Examples:
        >>> extract_implementation_identity_from_commit("claudem/minimax-M3: feat(x): ...")
        'claude'
        >>> extract_implementation_identity_from_commit("codexm/o3: fix: ...")
        'codex'
        >>> extract_implementation_identity_from_commit("naked commit message")
        'unknown'
    """
    if not isinstance(commit_subject, str):
        return "unknown"
    subject = commit_subject.strip()
    if not subject:
        return "unknown"
    for prefix, identity in COMMIT_PREFIX_TO_IDENTITY.items():
        if subject.startswith(prefix):
            return identity
    return "unknown"


def bind_reviewer_identity(
    reviewer_cli: str, declared_identity: str
) -> Tuple[bool, str]:
    """Refuse a verdict whose declared IDENTITY does not match the CLI
    that emitted it.

    Codex CLI must declare `codex`; gemini CLI must declare `gemini`.
    This binds the reviewer process to its identity — a codex
    invocation cannot claim `gemini`, and vice-versa. A reviewer that
    declares `claude` or `unknown` is also refused (the CLI list is
    fixed; only the two pinned binaries can satisfy gate-7).
    """
    cli = (reviewer_cli or "").strip().lower()
    declared = (declared_identity or "").strip().lower()
    expected = REVIEWER_CLI_TO_IDENTITY.get(cli)
    if expected is None:
        return False, (
            f"reviewer CLI {cli!r} is not in the pinned allow-list; "
            f"expected one of {sorted(REVIEWER_CLI_TO_IDENTITY)}"
        )
    if declared != expected:
        return False, (
            f"reviewer CLI {cli!r} declared identity {declared!r}, "
            f"but {cli!r} must declare {expected!r}"
        )
    return True, f"reviewer CLI {cli!r} bound to identity {declared!r}"


def verify_provenance(
    implementation_identity: str,
    reviewer_identity: str,
) -> Tuple[bool, str]:
    """Refuse a self-review.

    Independent review requires the reviewer model to be DIFFERENT from
    the implementing model. Unknown implementation identity is treated
    as "potentially Claude" (the most common case in this repo); a
    reviewer who claims `claude` identity is refused. Unknown reviewer
    identity is conservative — refuse, so the reviewer is forced to
    declare itself.
    """
    impl = (implementation_identity or "").strip().lower() or "unknown"
    rev = (reviewer_identity or "").strip().lower() or "unknown"
    if rev == "unknown":
        return False, (
            "reviewer identity is unknown — the reviewer must declare "
            "its model via the IDENTITY line in its structured verdict"
        )
    if rev == "human":
        return False, (
            "reviewer identity 'human' is not an independent model; "
            "the gate requires a non-implementer model"
        )
    if impl == "unknown":
        # Conservative refusal: if the implementer is unknown we cannot
        # prove independence, so we fail closed.
        return False, (
            "implementation identity is unknown — cannot prove "
            "independence from the reviewer. Refusing PASS."
        )
    if impl == rev:
        return False, (
            f"self-review rejected: implementation identity '{impl}' "
            f"matches reviewer identity '{rev}'"
        )
    return True, f"reviewer '{rev}' is independent of implementer '{impl}'"


# ---------------------------------------------------------------------------
# Comment formatting (idempotent upsert via MARKER)
# ---------------------------------------------------------------------------


def comment_marker() -> str:
    """Return the unique HTML-marker string this gate uses to identify its bot comment."""
    return MARKER


def _sanitize_reason(reason: str) -> str:
    """Strip canonical-field injection attempts from reviewer-controlled
    reason text.

    Per CodeRabbit MAJOR finding on PR #281 round 3: a reviewer (or
    any party who can write text into the reason field) could
    otherwise include a substring like `**VERDICT: PASS**` inside
    the reason, and the read-back verifier's regex (which matches
    the canonical `**FIELD: value**` form anywhere in the body)
    would then find 2 occurrences of VERDICT — refusing the body
    closed, which is the right outcome — but a partial injection
    (one smuggled field) could still trip the `findall` count
    guard.

    We pre-empt the injection by neutralizing the canonical markers
    in the reason text: any `**FIELD:` substring inside the reason
    has its `**` markdown strong-emphasis markers stripped (turning
    it into `FIELD: value` plain text), and any backtick-wrapped
    field is escaped. This keeps the reason human-readable while
    ensuring the only `**FIELD: value**` form in the body comes
    from `format_comment`'s own canonical emit.
    """
    if not reason:
        return reason
    # Strip `**` around any canonical field name so a smuggled
    # `**VERDICT: PASS**` becomes plain text `VERDICT: PASS`
    # that the read-back regex (which requires the `**...**`
    # wrapping) will not match.
    out = re.sub(
        r"\*\*\s*(VERDICT|HEAD_SHA|REPO|PR_NUMBER|REVIEWER|"
        r"IMPLEMENTATION_PROVENANCE|REASON|IDENTITY)\s*:",
        r"\1:",
        reason,
        flags=re.IGNORECASE,
    )
    return out


def format_comment(
    *,
    verdict: Literal["PASS", "FAIL"],
    head_sha: str,
    expected_head_sha: str,
    repo: str,
    pr_number: int,
    reviewer: str,
    implementation_provenance: str = "unknown",
    reason: str = "",
    extra_reviewer_lines: Optional[List[str]] = None,
) -> str:
    """Render the bot comment body.

    The `MARKER` HTML comment is always present so the GitHub upsert
    logic can find the prior comment and replace it. A stale-SHA PASS is
    preserved verbatim in the body (so the audit trail shows what the
    reviewer said) but is visually marked STALE so the gate consumer
    knows not to honor it.

    The 6 required fields (VERDICT, HEAD_SHA, REPO, PR_NUMBER,
    REVIEWER, IMPLEMENTATION_PROVENANCE) are emitted in the canonical
    form the read-back verifier expects. `extra_reviewer_lines` lets
    the multi-reviewer aggregator append a per-reviewer breakdown
    WITHOUT breaking the upsert marker or the read-back field
    extraction.
    """
    head_sha_norm = head_sha.lower()
    expected_norm = expected_head_sha.lower()
    stale = head_sha_norm != expected_norm

    display_state = verdict
    if stale:
        display_state = "FAIL"  # type: ignore[assignment]

    stale_block = ""
    if stale:
        stale_block = (
            "\n> ⚠️ **STALE VERDICT** — this comment binds to "
            f"`{head_sha_norm[:12]}` but the current PR head is "
            f"`{expected_norm[:12]}`. The gate treats this as **FAIL**.\n"
        )

    # Sanitize reviewer-controlled text BEFORE emitting it into the
    # body. Without this, a smuggled `**VERDICT: PASS**` inside the
    # reason would be picked up by the read-back regex as a second
    # VERDICT occurrence, refusing the body closed (CodeRabbit MAJOR
    # finding on PR #281 round 3).
    safe_reason = _sanitize_reason(reason)
    safe_extras = (
        [_sanitize_reason(line) for line in extra_reviewer_lines]
        if extra_reviewer_lines
        else None
    )

    reason_block = f"\n**Reason:** {safe_reason}\n" if safe_reason else ""
    extras = ""
    if safe_extras:
        extras = "\n" + "\n".join(safe_extras) + "\n"

    return (
        f"{MARKER}\n"
        f"## Skeptic Gate — `{display_state}`\n\n"
        f"**VERDICT: {verdict}**\n"
        f"**HEAD_SHA: {head_sha_norm}**\n"
        f"**REPO: {repo}**\n"
        f"**PR_NUMBER: {pr_number}**\n"
        f"**REVIEWER: {reviewer}**\n"
        f"**IMPLEMENTATION_PROVENANCE: {implementation_provenance}**\n"
        f"{reason_block}"
        f"{extras}"
        f"{stale_block}\n"
        f"---\n"
        f"<sub>Skeptic gate verdict for PR #{pr_number} at head "
        f"`{head_sha_norm[:12]}`. Stale-SHA verdicts never satisfy a "
        f"newer head; the most recent run on the current head is the "
        f"only one that counts. Multi-reviewer independence: the gate "
        f"requires non-Claude reviewers and rejects self-review.</sub>\n"
    )


# ---------------------------------------------------------------------------
# evaluate — the deterministic verdict-binding step the workflow calls
# ---------------------------------------------------------------------------


def evaluate(
    *,
    review_output: Optional[str] = None,
    review_error: Optional[str] = None,
    repo: str,
    pr_number: int,
    head_sha: str,
    implementation_provenance: str = "unknown",
    base_sha: str = "",  # kept for future use
    diff: str = "",  # kept for future use
    reviewer: str = "reviewer",
    contract: Optional[BeadContract] = None,
) -> SkepticResult:
    """Decide a single reviewer's outcome from its output (or absence).

    - `review_output` is the reviewer's stdout if it succeeded.
    - `review_error` is the captured error/timeout/missing-binary
      message. At least one is meaningful; if both are None/empty, the
      reviewer did not run at all and the gate fails closed.
    - `implementation_provenance` is the deterministic implementer
      identity derived from the commit subject prefix; it is rendered
      into the comment body so the read-back verifier can equality-
      check it.
    """
    if review_error or not (review_output and review_output.strip()):
        reason = "reviewer unavailable"
        if review_error:
            reason = f"reviewer unavailable: {review_error.strip()[:200]}"
        elif not review_output:
            reason = "reviewer produced no output"
        else:
            reason = "reviewer produced empty output"
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=reviewer,
            implementation_provenance=implementation_provenance,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
            reviewer=reviewer,
        )

    # ---- Pre-parse: extract contract-echo block (issue #386) -----------
    # The `parse_verdict` function enforces a strict 10-field
    # no-extra-fields contract (issue #384). When a bead contract is
    # supplied, the reviewer's output additionally carries a
    # `CONTRACT_ECHO:` block. We extract that block FIRST (storing
    # the result for the post-parse contract-echo enforcement
    # below) and strip it from the output before handing the rest to
    # `parse_verdict`, so the 10-field contract is preserved
    # unchanged. The contract-echo block is preserved verbatim in
    # `raw_excerpt` if a parser-level pass is needed.
    echo_report: Optional[ContractEchoReport] = None
    output_for_parse = review_output
    if contract is not None:
        echo_report = parse_contract_echo(review_output, contract)
        output_for_parse = _strip_contract_echo_block(review_output)

    parsed = parse_verdict(output_for_parse)
    if parsed is None:
        # Diagnose the most likely failure mode for the reason. Issue
        # #384: distinguish "execution evidence missing" from generic
        # unparseable — evidence-free verdicts are invalid (fail-closed)
        # and the operator/Healer needs the specific reason to triage.
        missing_evidence = [
            f for f in EXECUTION_EVIDENCE_FIELDS
            if f not in review_output
        ]
        if missing_evidence:
            reason = (
                "reviewer output missing execution-evidence fields "
                f"({', '.join(missing_evidence)}) — verdict is invalid "
                "without proof the reviewer ran the repo's tests/lint/"
                f"grep on the PR HEAD (issue #384)"
            )
        else:
            reason = (
                "reviewer output was unparseable (one or more of "
                "VERDICT/HEAD_SHA/REPO/PR_NUMBER/REASON/IDENTITY/"
                "TEST_RUN_EVIDENCE/LINT_RUN_EVIDENCE/GREP_CITES/"
                "HEAD_COMMIT_VERIFIED missing, duplicated, "
                "inconsistent with VERDICT, or extra prose/code-block "
                "present — fail-closed)"
            )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=reviewer,
            implementation_provenance=implementation_provenance,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
            reviewer=reviewer,
        )

    binding = bind_to_pr(
        parsed,
        expected_repo=repo,
        expected_pr_number=pr_number,
        expected_head_sha=head_sha,
    )
    if not binding.ok:
        body = format_comment(
            verdict=parsed.verdict,
            head_sha=parsed.head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer=reviewer,
            implementation_provenance=implementation_provenance,
            reason=binding.reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=binding.reason,
            comment_body=body,
            parsed=parsed,
            reviewer=reviewer,
        )

    # ---- Contract-echo enforcement (issue #386) ------------------------
    # When the gate is invoked with a `contract`, the reviewer's
    # output MUST include a valid `CONTRACT_ECHO:` block that
    # addresses every acceptance item (or marks them `N-A` with a
    # reason). A reviewer that omits the block, marks items as
    # `NOT-ADDRESSED`, or fails to cite one of the contract's
    # acceptance items fails the gate closed. The failure reason
    # carries the unaddressed item's VERBATIM text — the next
    # roll's worker reads the exact problem, not a paraphrase.
    if contract is not None:
        echo_verdict = evaluate_contract_echo(echo_report, contract)
        if not echo_verdict.ok:
            reason = (
                f"contract-echo gate failed: {len(echo_verdict.unaddressed_items)} "
                f"acceptance item(s) not addressed in the diff. The constraint "
                f"below is verbatim from the bead author's contract and must "
                f"be addressed in the next roll.\n\n{echo_verdict.constraint}"
            )
            body = format_comment(
                verdict=parsed.verdict,
                head_sha=parsed.head_sha,
                expected_head_sha=head_sha,
                repo=repo,
                pr_number=pr_number,
                reviewer=reviewer,
                implementation_provenance=implementation_provenance,
                reason=reason,
            )
            return SkepticResult(
                check_state="failure",
                verdict=None,
                reason=reason,
                comment_body=body,
                parsed=parsed,
                reviewer=reviewer,
            )

    body = format_comment(
        verdict=parsed.verdict,
        head_sha=parsed.head_sha,
        expected_head_sha=head_sha,
        repo=repo,
        pr_number=pr_number,
        reviewer=reviewer,
        implementation_provenance=implementation_provenance,
        reason=parsed.reason or "",
    )
    return SkepticResult(
        check_state="success" if parsed.verdict == "PASS" else "failure",
        verdict=parsed.verdict,
        reason=parsed.reason or "verdict bound to current head",
        comment_body=body,
        parsed=parsed,
        reviewer=reviewer,
    )


# Mandatory reviewer identities. Per CodeRabbit CRITICAL finding on
# PR #281 round 2: the gate MUST aggregate over exactly the
# Codex-and-Gemini set on every PR. A subset (e.g. only a single
# successful codex run) is rejected even if it succeeded, because
# a partial review cannot satisfy the dual-independent-reviewer
# policy the gate is designed to enforce.
MANDATORY_REVIEWERS = ("codex", "gemini")


def aggregate_results(
    results: List[SkepticResult],
    *,
    repo: str,
    pr_number: int,
    head_sha: str,
    implementation_provenance: str = "unknown",
) -> SkepticResult:
    """Combine per-reviewer results; ALL must be success for the gate.

    Also rejects duplicate reviewer identities in the input list — if
    the workflow is invoked with `[["codex",""],["codex","gpt-5"]]`
    (two codex invocations), the gate fails closed. This is enforced
    even before any reviewer runs, because a single PR is not allowed
    to be reviewed twice by the same model.

    Per CodeRabbit CRITICAL finding on PR #281 round 2: this
    aggregator also refuses to produce a PASS unless BOTH mandatory
    reviewer identities (codex and gemini) are present in the input.
    A single successful reviewer's result (e.g. `[codex_pass]` or
    `[gemini_pass]`) yields `check_state="failure"` with a reason
    that names the missing reviewer.
    """
    if not results:
        reason = "no reviewers ran — gate cannot pass without any review"
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(none)",
            implementation_provenance=implementation_provenance,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
            reviewer="(aggregate)",
        )

    # Duplicate-reviewer guard: the input list must contain distinct
    # reviewer identities. Two codex invocations (or two gemini
    # invocations) on the same PR are rejected outright.
    seen_reviewers = []
    duplicates = []
    for r in results:
        rid = (r.reviewer or "").strip().lower()
        if rid in seen_reviewers and rid:
            duplicates.append(rid)
        elif rid:
            seen_reviewers.append(rid)
    if duplicates:
        reason = (
            f"duplicate reviewer identities in input list: {sorted(set(duplicates))}; "
            "the gate requires distinct reviewers per PR"
        )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(aggregate)",
            implementation_provenance=implementation_provenance,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
            reviewer="(aggregate)",
        )

    # Mandatory-set guard: BOTH codex AND gemini must be present.
    # Per CodeRabbit CRITICAL finding on PR #281 round 2: a single
    # successful reviewer cannot satisfy the policy.
    missing = [r for r in MANDATORY_REVIEWERS if r not in seen_reviewers]
    if missing:
        reason = (
            f"mandatory reviewer(s) missing from input list: {missing}; "
            "the gate requires BOTH codex and gemini on every PR"
        )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(aggregate)",
            implementation_provenance=implementation_provenance,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
            reviewer="(aggregate)",
        )

    all_success = all(r.check_state == "success" for r in results)
    bound = [r for r in results if r.parsed is not None]
    primary = bound[0] if bound else None
    primary_sha = primary.parsed.head_sha if primary and primary.parsed else head_sha

    # Execution-evidence guard (issue #384): even if a reviewer's
    # `check_state` is 'success', the verdict is invalid if the parsed
    # result is missing execution-evidence fields. A vacuous
    # `ParsedVerdict` (`test_run_evidence is None` or empty
    # `grep_cites`) means the reviewer submitted a pattern-matched PASS
    # rather than running the repo's tests/lint/grep — refuse the
    # aggregation as if the reviewer had failed. This prevents the
    # PR-#382 failure mode (regression test never invokes code under
    # test) from passing the gate.
    vacuous = [
        r.reviewer for r in results
        if r.parsed is None
        or r.parsed.test_run_evidence is None
        or r.parsed.lint_run_evidence is None
        or not r.parsed.grep_cites
        or not r.parsed.head_commit_verified
    ]
    if vacuous:
        reason = (
            f"mandatory reviewer(s) submitted a vacuous verdict without "
            f"execution evidence: {vacuous}; the gate requires every "
            f"reviewer to have actually run the repo's tests/lint/grep "
            f"on the PR HEAD (issue #384)"
        )
        body = format_comment(
            verdict="FAIL",
            head_sha=head_sha,
            expected_head_sha=head_sha,
            repo=repo,
            pr_number=pr_number,
            reviewer="(aggregate)",
            implementation_provenance=implementation_provenance,
            reason=reason,
        )
        return SkepticResult(
            check_state="failure",
            verdict=None,
            reason=reason,
            comment_body=body,
            parsed=None,
            reviewer="(aggregate)",
        )

    extras: List[str] = []
    for r in results:
        marker = "✅ PASS" if r.check_state == "success" else "❌ FAIL"
        extras.append(f"- **{r.reviewer}** — {marker} — {r.reason[:200]}")

    if all_success:
        agg_verdict = "PASS"
        agg_state = "success"
        agg_reason = (
            f"all {len(results)} reviewers passed with execution "
            f"evidence; primary reviewer: "
            f"{primary.reviewer if primary else '(unknown)'}"
        )
    else:
        agg_verdict = "FAIL"
        agg_state = "failure"
        failed = [r for r in results if r.check_state != "success"]
        agg_reason = (
            f"{len(failed)} of {len(results)} reviewers did not pass: "
            + "; ".join(f"{r.reviewer}={r.reason[:60]}" for r in failed)
        )

    body = format_comment(
        verdict=agg_verdict,  # type: ignore[arg-type]
        head_sha=primary_sha,
        expected_head_sha=head_sha,
        repo=repo,
        pr_number=pr_number,
        # The headline REVIEWER field is the aggregate identity (the
        # gate itself), not the primary reviewer's CLI. The per-
        # reviewer breakdown is in extra_reviewer_lines below.
        reviewer="(aggregate)",
        implementation_provenance=implementation_provenance,
        reason=agg_reason,
        extra_reviewer_lines=extras,
    )
    return SkepticResult(
        check_state=agg_state,
        verdict=agg_verdict if all_success else None,
        reason=agg_reason,
        comment_body=body,
        parsed=primary.parsed if primary else None,
        reviewer="(aggregate)",
    )


# ---------------------------------------------------------------------------
# Read-back verification — fail-closed if the published comment+status
# don't agree with what we tried to write
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ReadBackCheck:
    actor: str
    body_contains_marker: bool
    body_sha: Optional[str]
    body_repo: Optional[str]
    body_pr_number: Optional[int]
    body_verdict: Optional[str]
    body_reviewer: Optional[str]
    body_implementation_provenance: Optional[str]


def verify_published_comment(
    readback: ReadBackCheck,
    *,
    expected_actor: str,
    expected_sha: str,
    expected_repo: str,
    expected_pr_number: int,
    expected_verdict: str,
    expected_reviewer: str,
    expected_implementation_provenance: str,
) -> Tuple[bool, str]:
    """Verify what we just published — full equality read-back.

    Returns (ok, reason). ok=True ONLY when every published field
    equals the value we wrote, byte-for-byte (HEAD_SHA is the full
    40-hex form, lower-cased). Missing or mismatched fields fail
    closed. Per post-audit comment 4953064910, the previous version
    only checked non-empty; an attacker (or a stale publication
    API) could therefore post a comment that satisfied the read-
    back while publishing a different SHA/repo/PR/verdict.
    """
    if readback.actor != expected_actor:
        return False, (
            f"published comment actor is {readback.actor!r}, expected "
            f"{expected_actor!r}"
        )
    if not readback.body_contains_marker:
        return False, "published comment body is missing the upsert marker"
    if (readback.body_sha or "").lower() != expected_sha.lower():
        return False, (
            f"published comment HEAD_SHA is {readback.body_sha!r}, "
            f"expected {expected_sha!r}"
        )
    if (readback.body_repo or "") != expected_repo:
        return False, (
            f"published comment REPO is {readback.body_repo!r}, "
            f"expected {expected_repo!r}"
        )
    if readback.body_pr_number != expected_pr_number:
        return False, (
            f"published comment PR_NUMBER is {readback.body_pr_number!r}, "
            f"expected {expected_pr_number!r}"
        )
    if (readback.body_verdict or "").upper() != expected_verdict.upper():
        return False, (
            f"published comment VERDICT is {readback.body_verdict!r}, "
            f"expected {expected_verdict!r}"
        )
    if (readback.body_reviewer or "") != expected_reviewer:
        return False, (
            f"published comment REVIEWER is {readback.body_reviewer!r}, "
            f"expected {expected_reviewer!r}"
        )
    if (
        readback.body_implementation_provenance or ""
    ) != expected_implementation_provenance:
        return False, (
            f"published comment IMPLEMENTATION_PROVENANCE is "
            f"{readback.body_implementation_provenance!r}, expected "
            f"{expected_implementation_provenance!r}"
        )
    return True, "comment read-back agrees with the published verdict"


# ---------------------------------------------------------------------------
# Prompt building — pure string assembly, no judgment
# ---------------------------------------------------------------------------


_PROMPT_TEMPLATE = """You are an INDEPENDENT SKEPTIC reviewer for the dark-factory
PR-gate. You are NOT the implementing model. You did not write this
diff. Your only job is to emit a single structured verdict bound to the
exact commit SHA you were shown.

# PR context

- Repository: {repo}
- PR number: {pr_number}
- Head SHA (full, 40 hex chars): {head_sha}
- Base SHA: {base_sha}
- Implementation identity: {implementation_identity}

# Diff under review

```
{diff}
```

{contract_block}

# Output contract — REQUIRED, no extra prose

Emit EXACTLY this format, on its own lines, with the values substituted.
Each line MUST appear EXACTLY ONCE in your output, on its own line,
with no extra commentary or code blocks:

    VERDICT: <PASS|FAIL>
    HEAD_SHA: {head_sha}
    REPO: {repo}
    PR_NUMBER: {pr_number}
    REASON: <one-sentence justification>
    IDENTITY: <codex|gemini|claude|unknown>
    TEST_RUN_EVIDENCE: passed=<N> failed=<N> skipped=<N> exit=<N>
    LINT_RUN_EVIDENCE: tool=<name> errors=<N> warnings=<N>
    GREP_CITES: <file:line;file:line;...>
    HEAD_COMMIT_VERIFIED: <full 40-hex SHA of the local HEAD you actually exercised>

Rules:
- The `HEAD_SHA` MUST be the FULL 40-character hex SHA shown above. Not
  a short prefix.
- Use `VERDICT: PASS` only if the diff is small, well-scoped, and free
  of destructive operations, secret leakage, out-of-scope changes, or
  anything else a reviewer should refuse. Otherwise use `VERDICT: FAIL`.
- `IDENTITY` MUST be one of `codex`, `gemini`, `claude`, `unknown`.
  The gate rejects self-review: a verdict whose IDENTITY matches the
  implementing model will fail closed regardless of the verdict.
- `REPO` MUST equal the repository above.
- `PR_NUMBER` MUST equal the PR number above.
- Do not include any other text — no extra VERDICT lines, no code
  blocks containing verdict tokens, no commentary. The deterministic
  gate will reject anything that does not match this contract exactly,
  including outputs where any of the ten lines appears more than once.

# Execution-evidence requirement (issue #384) — NOT optional

The four execution-evidence lines (`TEST_RUN_EVIDENCE`,
`LINT_RUN_EVIDENCE`, `GREP_CITES`, `HEAD_COMMIT_VERIFIED`) are
MANDATORY. Verdicts without them are evidence-free and the gate
will reject the verdict with `verdict=null` even if the rest of the
contract is well-formed.

Before emitting your verdict you MUST actually execute the repo's
test suite and primary linter on the PR HEAD and cite the call
sites for any enforcement claim you make:

- `TEST_RUN_EVIDENCE` — run the repo's primary test command (e.g.
  `pytest`, `cargo test`, `go test ./...`, `npm test`) on the PR HEAD
  and report real counts: `passed=N failed=N skipped=N exit=N`. A
  PASS verdict with `failed>0` or `exit!=0` is internally inconsistent
  and will be rejected. **Do not pattern-match** the diff to guess test
  results — a vacuous regression test (`def test_placeholder(): pass`)
  has passed the gate in the past (PR #382). Run the suite.
- `LINT_RUN_EVIDENCE` — run the repo's primary linter (ruff, clippy,
  eslint, etc.) on the PR HEAD and report real counts:
  `tool=<name> errors=N warnings=N`. A PASS verdict with `errors>0`
  is internally inconsistent and will be rejected.
- `GREP_CITES` — for each enforcement claim in the diff (e.g. "the
  gate now rejects evidence-free verdicts"), cite the production
  call site AND the test that exercises it as
  `path/to/file.py:LINE;path/to/test_X.py:LINE`. Empty `GREP_CITES`
  are rejected — citing zero call sites means the reviewer has not
  verified that the enforcement actually exists where claimed.
- `HEAD_COMMIT_VERIFIED` — the full 40-hex SHA of the local HEAD you
  actually exercised. It MUST equal `HEAD_SHA` byte-for-byte. If you
  ran your tests on a different tree than the gate, the gate will
  reject the verdict (you read a different diff than the one being
  gated).

If the repo's test command is slow or unavailable, report the real
result (including the failure) — do not fabricate numbers to satisfy
the contract. A FAIL verdict with honest execution evidence is
acceptable; a PASS verdict without evidence is not.

# Contract-echo requirement (issue #386) — REQUIRED when a contract is provided

When the prompt includes a `# Bead contract` section, the gate also
requires per-item verdicts against the bead's acceptance items.
Emit a `CONTRACT_ECHO:` block AFTER the ten structured fields,
with one line per acceptance item, in the form:

    CONTRACT_ECHO:
    ITEM: <id> VERDICT: <ADDRESSED|NOT-ADDRESSED|N-A> CITE: <file:line>
    ITEM: <id> VERDICT: <N-A> REASON: <free text>
    ...

Rules:
- `ADDRESSED` requires `CITE: <file:line>` — the file path and line
  number where the diff addresses the item. The file must exist
  in the diff; the line must be a real enforcement site.
- `NOT-ADDRESSED` requires `REASON: <text>` — the reviewer's
  explanation of why the diff does not address the item. The gate
  fails closed with this reason as the verbatim constraint for the
  next roll.
- `N-A` requires `REASON: <text>` — when the item does not apply
  to this round. A bare `N-A` is rejected (we never accept an
  unjustified classification).
- Every acceptance item MUST appear exactly once in the block —
  the gate cross-references the report against the contract and
  treats missing items as `NOT-ADDRESSED`.
- Items in prior rounds' findings (`# Prior findings` below) MUST
  also be addressed — either as `ADDRESSED` (with the file:line
  where the prior finding was closed) or as `N-A` (with the
  reason it is no longer applicable). A prior finding that is
  still open is `NOT-ADDRESSED` in the same block.
"""


_CONTRACT_BLOCK_TEMPLATE = """# Bead contract (issue #386)

The bead author wrote this contract. The gate verifies that every
acceptance item is addressed in the diff — see the `CONTRACT_ECHO:`
output requirement below.

- Bead id: {bead_id}
- Description: {bead_description}

## Prior findings

The following findings from prior rounds are open unless the
reviewer marks them `ADDRESSED` (with a file:line cite) or `N-A`
(with a reason). Per the bead author, these MUST be addressed:

{prior_findings_block}

## Acceptance items

The bead author has set the following acceptance items. Every item
MUST appear in the `CONTRACT_ECHO:` block below with one of
`ADDRESSED` (with `CITE: <file:line>`), `NOT-ADDRESSED` (with
`REASON: <text>`), or `N-A` (with `REASON: <text>`). Missing items
are treated as `NOT-ADDRESSED` and the gate fails closed:

{acceptance_items_block}
"""


def _format_prior_findings_block(prior_findings: Tuple[PriorFinding, ...]) -> str:
    if not prior_findings:
        return "(no prior findings)"
    lines = []
    for i, pf in enumerate(prior_findings, 1):
        lines.append(f"{i}. ({pf.source}) {pf.text}")
    return "\n".join(lines)


def _format_acceptance_items_block(items: Tuple[AcceptanceItem, ...]) -> str:
    if not items:
        return "(no acceptance items)"
    lines = []
    for it in items:
        lines.append(f"- {it.id}: {it.text}")
    return "\n".join(lines)


def _build_contract_block(contract: BeadContract) -> str:
    return _CONTRACT_BLOCK_TEMPLATE.format(
        bead_id=contract.id,
        bead_description=contract.description or "(no description)",
        prior_findings_block=_format_prior_findings_block(contract.prior_findings),
        acceptance_items_block=_format_acceptance_items_block(contract.acceptance_items),
    )


def build_prompt(
    *,
    repo: str,
    pr_number: int,
    head_sha: str,
    base_sha: str,
    diff: str,
    implementation_identity: str = "unknown",
    contract: Optional[BeadContract] = None,
) -> str:
    """Assemble the prompt sent to the independent reviewer CLI.

    Pure string assembly — no model call, no judgment. The reviewer
    model is the one that emits the structured verdict.

    When `contract` is supplied (issue #386), a `# Bead contract`
    section is interpolated into the prompt, documenting the bead's
    description, prior findings, and acceptance items. The reviewer
    is then required to emit a `CONTRACT_ECHO:` block in its
    verdict (see `_PROMPT_TEMPLATE`). The deterministic gate parses
    the block via `parse_contract_echo` and checks every acceptance
    item is `ADDRESSED` (or `N-A` with a reason) via
    `evaluate_contract_echo`. Without a contract, the prompt is the
    legacy 10-field form (issue #384).
    """
    if contract is not None:
        contract_block = _build_contract_block(contract)
    else:
        contract_block = ""
    return _PROMPT_TEMPLATE.format(
        repo=repo,
        pr_number=pr_number,
        head_sha=head_sha,
        base_sha=base_sha,
        diff=diff,
        implementation_identity=implementation_identity,
        contract_block=contract_block,
    )


__all__ = [
    "AcceptanceItem",
    "BeadContract",
    "COMMIT_PREFIX_TO_IDENTITY",
    "CONTRACT_ECHO_LINE_RE",
    "ContractEchoItem",
    "ContractEchoReport",
    "ContractEchoVerdict",
    "ContractEchoVerdictResult",
    "EXECUTION_EVIDENCE_FIELDS",
    "MARKER",
    "ModelIdentity",
    "ParsedLintRun",
    "ParsedTestRun",
    "ParsedVerdict",
    "PriorFinding",
    "REVIEWER_CLI_TO_IDENTITY",
    "ReadBackCheck",
    "SkepticResult",
    "ValidationResult",
    "aggregate_results",
    "bind_reviewer_identity",
    "bind_to_pr",
    "build_prompt",
    "comment_marker",
    "evaluate",
    "evaluate_contract_echo",
    "extract_implementation_identity_from_commit",
    "format_comment",
    "load_bead_contract",
    "parse_contract_echo",
    "parse_verdict",
    "verify_published_comment",
    "verify_provenance",
]
