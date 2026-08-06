"""Contract-echo subsystem for the skeptic gate (issue #386).

Extracted from ``runner/skeptic_gate`` so that the contract-echo
contract dataclasses, regexes, loaders, parser, and evaluator live
together. The headline invariant — the reviewer's per-item verdict
block must cover every acceptance item plus every prior finding —
is the contract-echo subsystem's only job; the rest of the skeptic
gate (10-field ``parse_verdict``, SHA binding, provenance, comment
formatting) stays in ``runner/skeptic_gate``.

Public surface
--------------
- ``AcceptanceItem`` / ``PriorFinding`` / ``BeadContract`` — the
  durable contract payload the bead author writes.
- ``ContractEchoItem`` / ``ContractEchoReport`` —
  ``ContractEchoVerdictResult`` / ``PriorFindingEcho`` — the parsed
  per-item verdicts and the gate's verdict.
- ``ContractEchoVerdict`` — the verdict literal vocabulary.
- ``CONTRACT_ECHO_HEADER_RE`` / ``CONTRACT_ECHO_LINE_RE`` /
  ``CONTRACT_ECHO_PRIOR_LINE_RE`` — anchored regexes used by the
  parser and the no-prose gates.
- ``load_bead_contract(source)`` — load a contract from a dict,
  a JSON file path, or pass through.
- ``load_bead_contract_from_bead(bead_id)`` — load the contract
  directly from ``br show --json`` (closes r3 gap 2).
- ``parse_contract_echo(output, contract)`` — extract a
  ``ContractEchoReport`` from the reviewer's stdout.
- ``evaluate_contract_echo(report, contract,
  report_prior_findings=...)`` — the headline fail-closed check
  that every item is ADDRESSED or N-A (with a reason, and not on
  a required=True item).

The module is intentionally side-effect free except for
``_br_show_json`` (a thin subprocess wrapper around ``br show
--json``) and ``load_bead_contract_from_bead`` (the live-bead
loader). Tests stub ``_br_show_json`` rather than spawning ``br``.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from typing import List, Literal, Optional, Tuple, Union


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

    `required` (issue #386 r3, gap 5): when True, a reviewer
    verdict of `N-A` is treated as unaddressed — the bead author
    is saying "this must be done", so the reviewer cannot opt out.
    Defaults to False for backward compatibility with r2 contracts.
    """

    id: str
    text: str
    required: bool = False


@dataclass(frozen=True)
class PriorFinding:
    """A finding from a prior round that the bead author wants the
    reviewer to address.

    `source` is free-form (e.g. "r5 reviewer", "skeptic r3") and
    `text` is the verbatim finding. The prompt embeds both so the
    reviewer can address each prior item. r3 promotes prior findings
    from prompt-only (gap 7 P2) to per-item enforced via
    `evaluate_contract_echo(report_prior_findings=...)`.
    """

    source: str
    text: str


@dataclass(frozen=True)
class BeadContract:
    """The bead's contract: the durable input to the contract-echo step.

    `id` is the bead identifier (`jleechan-pq08` for this work).
    `description` is the bead's body — the goal the worker is
    implementing. `notes` (r3, gap 1) carries operator guidance
    distinct from description (the bead author's free-form guidance
    notes from `br show <id>` reach the reviewer as a separate
    section). `prior_findings` lists findings from prior rounds
    that the bead author wants the reviewer to re-verify. Every
    `acceptance_items` entry is what the gate requires per-item
    verdicts for; if even one is `NOT-ADDRESSED` in the diff, the
    gate fails closed.
    """

    id: str
    description: str
    notes: Tuple[str, ...] = ()
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

    `prior_findings` carries the reviewer's verdict on each
    `PriorFinding` (r3, issue #386 gap 7 P2). When the contract
    has prior_findings and the reviewer omits them, the
    `evaluate_contract_echo` caller may treat them as unaddressed.
    """

    items: Tuple[ContractEchoItem, ...]
    prior_findings: Tuple["PriorFindingEcho", ...] = ()


@dataclass(frozen=True)
class ContractEchoVerdictResult:
    """Outcome of `evaluate_contract_echo`.

    `ok=True` means every acceptance item is `ADDRESSED` or
    `N-A` (with a reason). `ok=False` means at least one item
    is `NOT-ADDRESSED` or missing; `unaddressed_items` carries
    the full `AcceptanceItem` records (verbatim text) so the
    next roll's worker reads the exact problem, not a paraphrase.
    `constraint` is a human-readable constraint string suitable
    for embedding in the gate's failure comment or handing to the
    next roll.
    """

    ok: bool
    unaddressed_items: Tuple[AcceptanceItem, ...] = ()
    constraint: str = ""
    unaddressed_prior_findings: Tuple[PriorFinding, ...] = ()


@dataclass(frozen=True)
class PriorFindingEcho:
    """A reviewer verdict on a single PriorFinding.

    Pairs a `PriorFinding`'s `source` (its stable identifier) with
    a verdict and optional cite / reason. Same verdict vocabulary as
    `ContractEchoItem` but indexed by source rather than a generated id.
    """

    source: str
    verdict: ContractEchoVerdict
    cite: str = ""
    reason: str = ""


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
# PRIOR_FINDING lines mirror ITEM lines but use `source` (a stable
# identifier from the bead's prior_findings) instead of a generated id.
# Same verdict vocabulary, same CITE:/REASON: shape. The source token
# allows spaces and colons (e.g. "r2 cursor-agent" or "r2:CodeRabbit")
# so the reviewer can echo the bead author's source verbatim without
# rewriting it.
CONTRACT_ECHO_PRIOR_LINE_RE = re.compile(
    r"^\s*PRIOR_FINDING\s*:\s*(?P<source>[^\s][^\n]*?)\s+"
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

    raw_notes = data.get("notes")
    if raw_notes is None:
        notes: Tuple[str, ...] = ()
    elif isinstance(raw_notes, list):
        notes = tuple(str(n) for n in raw_notes)
    elif isinstance(raw_notes, str):
        notes = (raw_notes,)
    else:
        raise TypeError(
            "load_bead_contract: 'notes' must be a string or list of strings"
        )

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
                required=bool(it.get("required") or False),
            )
        )

    return BeadContract(
        id=bead_id,
        description=description,
        notes=notes,
        prior_findings=tuple(prior_findings),
        acceptance_items=tuple(acceptance_items),
    )


def _br_show_json(bead_id: str, br_bin: str = "br") -> str:
    """Subprocess wrapper around `br show <bead_id> --json`.

    Returns stdout as a JSON string. The single source of truth
    for the bead — used by `load_bead_contract_from_bead` to
    materialise a `BeadContract` from the live bead source instead
    of a hand-authored contract file.

    Exposed at module level so tests can monkeypatch this without
    spawning a real `br` subprocess. The function is intentionally
    a thin subprocess wrapper — fail-closed on any error so the
    caller never receives a fabricated contract (issue #386 r3,
    gap 2).
    """
    import subprocess

    proc = subprocess.run(
        [br_bin, "show", "--json", bead_id],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"_br_show_json: br show --json {bead_id!r} failed "
            f"(rc={proc.returncode}): {proc.stderr.strip() or proc.stdout.strip()}"
        )
    return proc.stdout


def load_bead_contract_from_bead(
    bead_id: str, br_bin: str = "br"
) -> BeadContract:
    """Load a `BeadContract` directly from the live bead source.

    Closes r3 gap 2: production was hand-authoring contracts and
    depending on `--contract-file` only. This reader pulls the
    bead's actual `description`, `notes`, `prior_findings`, and
    `acceptance_items` from `br show --json <id>` and feeds them
    through `load_bead_contract` for the same validation.

    Field mapping (the bead JSON shape is documented in
    `~/.claude/docs/beads.md`):
      - `description` (str)  -> description
      - `notes` (str|list)   -> notes
      - `prior_findings`     -> prior_findings (the bead author
                                embeds these as `[ {source, text} ]`
                                in the bead's notes JSON block;
                                absent `prior_findings` is allowed
                                and falls back to the most-recent
                                contract-echo report's unaddressed
                                items cached at
                                `.cache/contract_echo/<bead>.json`)
      - `acceptance_items`  -> acceptance_items; the bead JSON
                                uses `id` / `text` / `required`
                                keys per item, matching the
                                contract-echo format.

    Subprocess and parse errors raise so callers fail closed —
    never silently fabricate a contract. Tests stub
    `_br_show_json` rather than spawning `br`.
    """
    raw = _br_show_json(bead_id, br_bin=br_bin)
    payload = json.loads(raw)
    payload.setdefault("id", bead_id)
    return load_bead_contract(payload)


# ---------------------------------------------------------------------------
# Contract-echo parser (issue #386)
# ---------------------------------------------------------------------------


def _strip_contract_echo_block(output: str) -> str:
    """Remove the `CONTRACT_ECHO:` block from a reviewer output so the
    10-field `parse_verdict` can run on the remainder.

    The block is at the END of the output (after the 10 structured
    fields). The header line (`CONTRACT_ECHO:`) is dropped, every
    `ITEM:` and `PRIOR_FINDING:` line is dropped, and any blank
    lines immediately around the block are normalized. If the block
    is not present, the output is returned unchanged (so the function
    is safe to call regardless of whether a contract was supplied).

    r10 (issue #386): PRIOR_FINDING: lines were added to the
    contract-echo block. They MUST be stripped too, otherwise
    `parse_verdict` sees them as extra prose and rejects the output.
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
        if stripped.startswith("ITEM:") or stripped.startswith("PRIOR_FINDING:"):
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
    # collect the immediately-following `ITEM:` / `PRIOR_FINDING:` lines.
    # We stop at the first non-blank, non-{ITEM,PRIOR_FINDING} line (the
    # strict no-prose contract the gate enforces elsewhere — anything
    # after the block is considered out-of-block and ignored).
    header_match = CONTRACT_ECHO_HEADER_RE.search(output)
    if not header_match:
        return None
    after = output[header_match.end():]
    item_lines: List[str] = []
    prior_lines: List[str] = []
    for raw_line in after.splitlines():
        stripped = raw_line.strip()
        if not stripped:
            # Blank lines are allowed within the block.
            continue
        if stripped.startswith("ITEM:"):
            item_lines.append(raw_line)
            continue
        if stripped.startswith("PRIOR_FINDING:"):
            prior_lines.append(raw_line)
            continue
        # Out-of-block content; stop walking.
        break
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

    prior_findings_echo: List[PriorFindingEcho] = []
    for raw_line in prior_lines:
        line = raw_line.strip()
        if not line:
            continue
        m = CONTRACT_ECHO_PRIOR_LINE_RE.match(line)
        if m is None:
            # Same no-prose policy as ITEM: lines — reject the whole
            # block when a PRIOR_FINDING: line is unparseable so the
            # reviewer cannot smuggle prose through a malformed line.
            return None
        source = m.group("source")
        verdict_token = m.group("verdict").upper()
        cite = (m.group("cite") or "").strip()
        reason = (m.group("reason") or "").strip()

        if verdict_token == "ADDRESSED":
            if not cite:
                return None
            if not re.match(r"^[\w./\-]+:\d+$", cite):
                return None
        else:
            if not reason:
                return None

        prior_findings_echo.append(
            PriorFindingEcho(
                source=source,
                verdict=verdict_token,  # type: ignore[arg-type]
                cite=cite,
                reason=reason,
            )
        )

    return ContractEchoReport(
        items=tuple(items),
        prior_findings=tuple(prior_findings_echo),
    )


# ---------------------------------------------------------------------------
# Contract-echo evaluator (issue #386 — the headline invariant)
# ---------------------------------------------------------------------------


def evaluate_contract_echo(
    report: Optional[ContractEchoReport],
    contract: BeadContract,
    report_prior_findings: Optional[Tuple[PriorFindingEcho, ...]] = None,
) -> ContractEchoVerdictResult:
    """Check that every acceptance item is ADDRESSED or N-A (with caveats).

    - `ADDRESSED` (with a `CITE:` file:line): the reviewer claims the
      diff addresses this item; we trust the reviewer on the cite
      (we don't second-guess the file:line — the executor evidence
      contract in `parse_verdict` is what backs the claim).
    - `N-A` (with a `REASON:`): the reviewer says the item is not
      applicable this round; this counts as a pass IF the reason is
      non-empty AND the item is not `required=True`. An empty
      `N-A` reason is rejected at parse time (see
      `parse_contract_echo`), so by the time we get here, a
      `N-A` item is always with a reason. r3 (gap 5): if the item
      is `required=True`, `N-A` is treated as unaddressed —
      the bead author says "this MUST be done", so the reviewer
      cannot opt out.
    - `NOT-ADDRESSED` (with a `REASON:`): the reviewer flags the
      item as unaddressed; we surface it verbatim in
      `unaddressed_items` and `constraint`.
    - Missing item (the reviewer's report does not contain the
      item's ID): the item is unaddressed — the reviewer did not
      cover it.
    - `report is None` (no `CONTRACT_ECHO:` block at all): every
      contract item is unaddressed.

    Prior findings (r3, gap 7 P2): when `report_prior_findings` is
    supplied, every `PriorFinding` listed in the contract must be
    covered (ADDRESSED or N-A). Uncovered prior findings are
    surfaced in `unaddressed_prior_findings` and appended to
    `constraint` verbatim. Prior findings are prompt-only when
    `report_prior_findings=None` (preserves r2 behavior on
    contracts whose bead did not opt into prior-finding
    enforcement).

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
            # r3 gap 5: required items cannot be N-A'd away.
            if item.required:
                unaddressed.append(item)
                continue
            continue
        # NOT-ADDRESSED or anything else: unaddressed.
        unaddressed.append(item)

    unaddressed_prior: List[PriorFinding] = []
    if report_prior_findings is not None:
        emitted_pf = {pf.source: pf for pf in report_prior_findings}
        for pf in contract.prior_findings:
            verdict_pf = emitted_pf.get(pf.source)
            if verdict_pf is None:
                unaddressed_prior.append(pf)
                continue
            if verdict_pf.verdict == "ADDRESSED":
                continue
            if verdict_pf.verdict == "N-A" and verdict_pf.reason:
                continue
            unaddressed_prior.append(pf)

    if not unaddressed and not unaddressed_prior:
        return ContractEchoVerdictResult(
            ok=True,
            unaddressed_items=(),
            constraint="",
            unaddressed_prior_findings=(),
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
        required_marker = " [REQUIRED]" if item.required else ""
        lines.append(f"- {item.id}{required_marker}: {item.text}")
    if unaddressed_prior:
        lines.append("")
        lines.append(
            f"Prior findings NOT-ADDRESSED: {len(unaddressed_prior)} "
            "(verbatim from prior-round reviewer / CodeRabbit etc.):"
        )
        for pf in unaddressed_prior:
            lines.append(f"- {pf.source}: {pf.text}")
    return ContractEchoVerdictResult(
        ok=False,
        unaddressed_items=tuple(unaddressed),
        constraint="\n".join(lines),
        unaddressed_prior_findings=tuple(unaddressed_prior),
    )


__all__ = [
    "AcceptanceItem",
    "BeadContract",
    "CONTRACT_ECHO_HEADER_RE",
    "CONTRACT_ECHO_LINE_RE",
    "CONTRACT_ECHO_PRIOR_LINE_RE",
    "ContractEchoItem",
    "ContractEchoReport",
    "ContractEchoVerdict",
    "ContractEchoVerdictResult",
    "PriorFinding",
    "PriorFindingEcho",
    "evaluate_contract_echo",
    "load_bead_contract",
    "load_bead_contract_from_bead",
    "parse_contract_echo",
]
