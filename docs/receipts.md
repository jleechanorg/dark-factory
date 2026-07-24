# Receipt contract — issue #406 supersession map

Issue #406 defined the dark-factory's original receipt contract for
codergen lanes: a `commands_run.md` artifact (lines of `$ <command>`
followed by `exit code: N`) recording what the coder actually executed.
That contract predates the merged receipt stack. This note maps each
#406 item to its current home and records what was intentionally not
carried forward, so a reader can navigate from the original contract to
the implementation that enforces it today.

## Mapping

| #406 original item | Current home | Source |
| --- | --- | --- |
| `commands_run.md` artifact — codergen structured-receipt **source** | Parsed into structured receipts and stashed on the run state, then honored by the structured receipt check at the same trust tier as engine-captured receipts. | `runner/handler_codergen.py::_stash_codergen_receipt` (parses the #406 artifact) · `runner/handler_verdict.py::_check_structured_receipt` (accepts `codergen_receipts=`) · `runner/handler_dispatch.py::_run_gate_once` (gathers codergen receipts from `ctx.state` into `metadata["_codergen_receipts"]`) |
| Regex reproduction-receipt gate — the low-trust fallback that requires a re-run build/test runner line plus a captured `exit code: 0` in the transcript | Issue #407. A reviewer PASS is only trustworthy when the transcript shows the re-run; otherwise the gate fails. | `runner/handler_verdict.py::_reproduction_receipt_gap` · `_RECEIPT_RUNNER_RE` · `_RECEIPT_EXIT_RE` (see `docs/skeptic-gate.md` for the PR #407 reference) |
| Per-lane enforcement — each reviewer lane (primary + shadows) must carry its own reproduction receipt **before** transcripts are concatenated | Issue #429. Prevents a read-only primary PASS from surviving when only a shadow lane reproduced. | `runner/handler_parallel_reviewer.py::_enforce_reproduction_receipt_for_lane` |
| Structured receipts — engine-captured subprocess receipts recorded at execution time | Issue #432 (ZFC fix). The structured path is authoritative; the regex gate is the low-trust fallback used when no structured receipt is present. | `runner/handler_verdict.py::_check_structured_receipt` (consumer) · `runner/handler_dispatch.py::_run_gate_once` (builds `metadata["_reviewer_receipts"]`) · state key `ctx.state["_reviewer_receipts"]` |

## What is intentionally NOT implemented

Issue #406's **final-status helpers** — helpers that would classify a
codergen run's final status (pass/fail) from the `commands_run.md`
artifact — are **superseded by gate outcomes**. The reviewer gate's
verdict combined with the receipt check already determines pass/fail:
the structured receipt proves the run executed with exit code 0, and
the gate verdict classifies the result. No separate final-status
classifier is needed or implemented.

## Key source files

- `runner/handler_codergen.py` — `_stash_codergen_receipt` parses the
  #406 `commands_run.md` artifact into structured receipts and attaches
  them to the run state.
- `runner/handler_verdict.py` — `_check_structured_receipt` (structured
  consumer, honors `codergen_receipts=` at the same trust tier as
  `_reviewer_receipts`); `_reproduction_receipt_gap`,
  `_RECEIPT_RUNNER_RE`, `_RECEIPT_EXIT_RE` (regex fallback, #407).
- `runner/handler_dispatch.py` — `_run_gate_once` builds
  `metadata["_reviewer_receipts"]` (engine-captured, #432) and
  `metadata["_codergen_receipts"]` (codergen-sourced, this change).
- `runner/handler_parallel_reviewer.py` —
  `_enforce_reproduction_receipt_for_lane` enforces the per-lane
  receipt before aggregation (#429).
