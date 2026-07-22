# Receipt contract reconciliation (issue #406 → #407 → #425 → #426 → #441)

**Date:** 2026-07-21
**Bead:** jleechan-9s3d
**Issues:** jleechanorg/dark-factory#406, #441

## Background

Issue #406 (closed 2026-07-08) originally asked for a file-backed
`commands_run.md` artifact integrated through
`runner/handler_codergen.py::_codergen` and named final-status/review-node
helpers, so a reviewer's PASS would only hold when the review transcript
showed a real build/test runner AND a captured exit code 0. The issue was
closed as not applicable because the upstream patch (snap-factory) targeted
a different repo.

PR #407 then landed an opt-in `receipt_required` parallel-reviewer gate
that classifies free-text prose with regexes
(`runner/handler_verdict._reproduction_receipt_gap`). PR #425 layered
per-lane enforcement pre-aggregation. PR #426 closed the regex-fabrication
ceiling by capturing the *subprocess execution itself* into
`ctx.state["_reviewer_receipts"]` — a structured record that the gate
verifies, not the reviewer's narrative. The regex path remains as a
low-trust fallback only.

## Reconciliation (issue #441)

The merged #426 design strictly dominates the original #406 contract:

| Concern | #406 (file-backed) | #407/#425/#426 (in-memory structured) | This PR |
|---|---|---|---|
| Bind command + exit code | regex over prose | engine-captured `subprocess` record | carried forward |
| Bind reviewer lane | implied by file path | explicit `lane_id` field | carried forward |
| Bind head SHA | not addressed | `_worktree_head_sha` lookup at capture time | carried forward |
| Bind artifact hash | "commands_run.md" file | `output_sha256` field on each receipt | carried forward |
| Apply to codergen review nodes | YES (explicit ask) | NO — codergen handler never recorded receipts | **FIXED** |
| Persist on disk | YES (`commands_run.md`) | NO (in-memory only) | **OPTIONAL** via `write_commands_run_sidecar` |
| ZFC compliance | NO (regex prose classification) | YES (engine-captured) | preserved |

The structured receipt in `ctx.state["_reviewer_receipts"]` IS the
contract. A `commands_run.md` sidecar is exposed as an opt-in writer
(`runner.handler_verdict.write_commands_run_sidecar`) for graphs that
want a durable on-disk artifact alongside the in-memory list — this
honors the #406 acceptance "durable commands_run.md **or equivalent
structured sidecar**" without leaking review transcripts to disk by
default.

## Surviving subset (this PR)

1. **`runner/handler_codergen.py::_codergen`** now emits a structured
   receipt for every subprocess backend (codex, claude, agy). The receipt
   binds command + cwd + exit_code + head_sha + lane_id + output_sha256
   — the five fields the #406 contract calls out.

2. **`runner/handler_dispatch.py::_build_reviewer_receipt`** threads
   `output_sha256` into the gate-subprocess receipt so the captured
   record binds to the actual subprocess output text.

3. **`runner/handler_verdict.py::_record_reviewer_receipt`** accepts the
   new `output_sha256` and `ts` fields and emits them on the structured
   event log so audit readers can correlate the receipt with the
   output.

4. **`runner/handler_verdict.py::write_commands_run_sidecar`** — new
   opt-in writer that produces a markdown `commands_run.md` artifact
   under `~/.dark-factory/runs/<run_id>/commands_run/`. Not invoked by
   default; available for graphs that need a durable sidecar.

5. **`tests/test_codergen_reproduction_receipt.py`** — new test module
   covering:
   - `valid` — receipt recorded on a successful codex/claude/agy run.
   - `failed` — receipt reflects a real nonzero exit (no laundering).
   - `stale-head` — receipt with wrong `head_sha` fails the structured check.
   - `cross-lane` — primary + shadow lanes have distinct receipts.
   - `commands_run.md` sidecar writer.

## Superseded items

The following parts of the original #406 spec are **formally superseded**
by the merged #407/#425/#626 design:

- Regex-based prose classification of reviewer transcripts (the
  fabrication ceiling) — closed by #426; the regex path is now a
  documented low-trust fallback only.
- File-on-disk `commands_run.md` as the primary artifact — the
  in-memory receipt list is authoritative; the sidecar writer is opt-in.

Issue #406 itself stays closed as not applicable; the surviving
implementation is documented here as the canonical migration.

## Operator runbook

No action required for existing graphs. New behavior:

- Codergen-backed review nodes (i.e. `type="codergen"` with
  `class="review"` and `receipt_required="true"`) now produce a real
  receipt that the structured gate verifies, instead of falling back
  silently to the regex path.
- Operators who want a durable on-disk receipt can call
  `write_commands_run_sidecar(run_id=..., node_name=..., attempt=...,
  receipts=ctx.state["_reviewer_receipts"])` from a custom tool node
  after the gate completes. The default runner does not invoke this
  writer.