# Factory Architectural Roadmap — Implementation Plan

**Source spec:** [`docs/plans/factory_improvement_analysis.md`](factory_improvement_analysis.md)
**Parent bead:** `jleechan-o8q` (P1 OPEN)
**Branch:** `feat/factory-roadmap-o8q`
**Generated:** 2026-06-18

This document turns the no-code roadmap into a dependency-ordered, file-scoped
implementation plan. Each section calls out:

* the recommendation as written in the roadmap
* cross-validation against `main` (already-shipped vs. still-open)
* target files, entry points, and success criterion
* classification: doc-only / runner-change / pipeline-graph / skill-text
* status: `already-shipped` / `ready-to-implement` / `needs-spec-first` /
  `blocked-on-<other>`

---

## Cross-validation summary

Re-running the cross-check from scratch per
[`feedback_2026-06-13_self_correction_at_ceiling.md`](/Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-06-13_self_correction_at_ceiling.md):

| Pillar | Status on `main` (HEAD = `178458a`) | Evidence |
|--------|-------------------------------------|----------|
| 1. Global Panic Hook | **Shipped** (PR #49), but exit code is `124`, not `128` as the roadmap recommends | `cb4978f [agento] feat(panic-hook): top-level crash artifact + distinct exit code (#49)`; `runner/panic_hook.py:71` `PANIC_EXIT_CODE = int(os.environ.get("DARK_FACTORY_PANIC_EXIT_CODE", "124"))`. Note the runner still emits `exit_code: "128"` as a JSON-string in `runner/__main__.py:115` crash events, which is a documentation drift, not a runtime exit. |
| 2. Structured JSONL logging | **Partially shipped.** Per-run JSONL is live (`runner/perf_log.py`); `node_enter`/`node_exit`/`transition`/`run_end` events + transcript sidecars landed on `triage/parity-squashed` (commit `05b2270`) but are NOT yet on `main`. | `git log --oneline 178458a | grep -i jsonl` returns no hits; `git log --all --oneline | grep fgw` shows `05b2270 feat(observability): emit structured runner JSONL events with transcript sidecars (jleechan-fgw)`. |
| 3. Static pre-flight graph validation | **Shipped** (PR #47 backend preflight, PR #50 structural preflight). Covers the roadmap's "assert TYPE_REGISTRY", "assert prompt paths exist", "assert command binaries valid", "edge resolution" checks. | `bda899c [agento] feat(preflight): CLI backend preflight — warn on missing, hard-stop on zero (#47)`; `d24a9ba [agento] feat(structural-preflight): df validate <pipeline.dot> pre-flight check (#50)`; `runner/structural_preflight.py:60-145` (`prompt_paths`, `timeout_thresholds`, `edge_resolution`). |
| 4. Dynamic LLM timeouts & provider backoff | **Partially shipped.** Minimum threshold (60s for codergen / validation) enforced at parse + preflight. Adaptive *defaults* (60/180/300s by class) NOT shipped. Exponential backoff module shipped (`runner/_backoff.py`). | `runner/parser.py:29` `_VALIDATION_TIMEOUT_MIN_SECONDS = 60`; `runner/structural_preflight.py:57` `TIMEOUT_THRESHOLD_S = 60`; `71de10f feat(runner): add _backoff module for transient-failure retry with jitter (#59)`. |
| 5. WAL checkpoint engine & self-healing resume | **Partially shipped.** SQLite WAL mode (`runner/cxdb.py:110`), per-run checkpoint file at `~/.dark-factory/runs/<run_id>/checkpoint.json` (engine.py:1111), and `--resume <path>` flow (engine.py:1034) are live. Auto-resume from CXDB query + launch-manifest persist (bead `jleechan-2gv`) shipped on `triage/parity-squashed` but NOT on `main`. | `runner/cxdb.py:110` `PRAGMA journal_mode=WAL`; `runner/engine.py:1017` `_load_checkpoint`; `runner/engine.py:1110-1111` per-run checkpoint path; commit `85afa95 feat(2gv): auto per-run checkpoint resumability and manifest (jleechan-2gv)` (not on main). |

**Net result:** 4 of 5 pillars are fully or partially shipped; the remaining
work is (a) merging the in-flight `triage/parity-squashed` items to `main`,
(b) the cross-cutting timeout-default policy, and (c) a small set of
dependent reliability beads that the roadmap did not name explicitly.

---

## Dependency order (what blocks what)

```
P3 (Pre-Flight Validation)  ── already shipped, blocks nothing new
        │
        ▼
P4 (Dynamic Timeouts)       ── partially shipped; needs policy + structural-preflight threshold widening
        │
        ▼
P1 (Global Panic Hook)      ── shipped on main; remaining doc-drift on exit-code string
        │
        ▼
P2 (Structured JSONL)       ── partially shipped; merging fgw to main unblocks Healer clustering
        │
        ▼
P5 (WAL Resumability)       ── partially shipped; merging 2gv to main unblocks long-run survival
        │
        ▼
Dependent reliability beads (jleechan-2gv, jleechan-fgw, jleechan-grb [done],
jleechan-1zx [done], jleechan-8py, jleechan-nm2, jleechan-7ql,
jleechan-wou [done upstream], jleechan-9wy, jleechan-ol7, jleechan-xzw,
jleechan-x33, jleechan-ok8, jleechan-sp6, jleechan-xgx, jleechan-rx1)
```

The roadmap's recommendations are **independent** of each other; they are
connected only through the dependent reliability beads filed under
`jleechan-o8q`. Items already shipped do not block new work.

---

## Pillar 1 — Global Panic Hook

**Roadmap quote:** *"Implement a Global Runner Exception boundary wrapping
the entire `runner.engine:run()` loop. Catch any `BaseException` and trigger
a deterministic Panic Hook."*

**Status:** `already-shipped` (with one doc-drift).

**What shipped:**
* `runner/panic_hook.py` — bash-invoked crash artifact writer
* `runner/__main__.py` — in-Python `try/except` boundary with crash event to
  perf log + CXDB panic step
* `runner/engine.py:1187-1487` — try/except around node exec + transition
  (PR #13, commit `a46ad82`; also covers Pillar 1's
  "record error StepRecord, route-to-fix" half)
* `bin/df` / `bin/dark-factory` shell wrappers invoke `panic_hook.py` on
  bash-level failures

**Remaining gap (doc-drift only):**
The roadmap says exit code `128`. Implementation uses `124` (rationale:
`runner/panic_hook.py:64-71` — `124` is `timeout(1)`'s "killed" sentinel, so
the Healer can group panics with timeout-class failures). The CLI crash
event in `runner/__main__.py:115` still labels this string as `"128"`,
which is a leftover. **Recommendation:** either change the string to `"124"`
to match the actual exit code, or change `PANIC_EXIT_CODE` to `128` to
match the roadmap. This is a single-line fix and a docs PR.

| Field | Value |
|---|---|
| Target files | `runner/__main__.py:115`, `runner/panic_hook.py:71` |
| Entry point | `_run_panic_payload` (the crash-event emitter in `__main__.py`) |
| Success criterion | `python -m runner.panic_hook --help` exits with a numeric code that matches the JSON `exit_code` field it emits; CI test pins both to the same integer. |
| Classification | runner-change (1-line) + doc-only (CLAUDE.md/roadmap reconcile) |
| Status | ready-to-implement |
| Beads | covered by parent `jleechan-8py`; no new bead needed |

---

## Pillar 2 — Structured JSONL Logging

**Roadmap quote:** *"Standardize all run logs to Structured JSON Lines (JSONL). Maintain a separate complete subprocess stdout/stderr ring buffer."*

**Status:** partially shipped; one bead (`jleechan-fgw`) is the missing piece.

**What shipped:**
* `runner/perf_log.py` writes `<run_id>.jsonl` with `node_enter` /
  `node_exit` / `transition` / `run_end` records plus a parallel
  human-readable `.log`. (PR #14, commit `c312d8f`.)
* `runner/cxdb.py` persists a separate `(run_id, seq, node, outcome,
  output_hash, output_head, metadata)` row per step. (Original CXDB work,
  pre-roadmap.)
* Subprocess stdout/stderr is captured by the codergen subprocess wrappers
  in `runner/handlers.py` (`_execute_gate`, `_agy_run`) and surfaced via
  `output` / `output_hash` in the StepRecord metadata — not a ring buffer,
  but the same data flow.

**Remaining gap:**
`jleechan-fgw` (P1 OPEN) extends perf_log with structured transcript
sidecars (per-node transcript file keyed by seq). The commit lives on
`triage/parity-squashed` (`05b2270`) but is not on `main`. **This is a
merge workstream, not new code.**

| Field | Value |
|---|---|
| Target files | `runner/perf_log.py` (the fgw delta) |
| Entry point | `_write_perf_event` in `perf_log.py` (currently emits a flat dict; the fgw branch adds transcript-file sidecar writes) |
| Success criterion | `tests/test_perf_log.py` (if added) or manual: run any pipeline with `--perf-log-dir` and confirm each step has both a `.jsonl` row and a sibling transcript file. |
| Classification | runner-change |
| Status | ready-to-implement (merge fgw branch) |
| Beads | `jleechan-fgw` |

---

## Pillar 3 — Static Pre-Flight Graph Validation

**Roadmap quote:** *"Introduce a Pre-Flight Validation Pass inside `runner/parser.py` that inspects the AST before executing."*

**Status:** `already-shipped`. All four sub-checks the roadmap calls out are
implemented, though distributed across three modules rather than concentrated
in `parser.py` as the roadmap suggests.

**What shipped:**
* `runner/parser.py:727` — `DF_MISSING_VALIDATION_TIMEOUT` and
  `DF_VALIDATION_TIMEOUT_TOO_LOW` (parser-side timeout guard).
* `runner/parser.py:746` — start/exit presence check.
* `runner/structural_preflight.py:60` — `_check_prompt_paths`
  (every `prompt="@..."` resolves on disk).
* `runner/structural_preflight.py:102` — `_check_timeout_thresholds`
  (every codergen/validation node has `timeout >= 60`).
* `runner/structural_preflight.py:128` — `_check_edge_resolution`
  (every edge points to a defined node).
* `runner/preflight.py` — CLI backend preflight (claude/codex/agy PATH
  checks; warn-on-missing, hard-stop-on-zero).
* `bin/df-validate` — bash wrapper for `python -m runner.structural_preflight`.

**Roadmap drift:** the roadmap says "assert that all `command="..."` strings
are parsed and have valid shell executable binaries." This is **partially
shipped** — `runner/preflight.py` checks backends at the CLI level but does
not parse every `command="..."` attribute on tool nodes. **Recommendation:**
extend `structural_preflight.py` with a fourth check, `_check_command_binaries`,
that walks every `tool`-typed node's `command` attribute and validates the
leading binary against `shutil.which`. This is a small additive check
(~30 lines + a test).

| Field | Value |
|---|---|
| Target files | `runner/structural_preflight.py` (add `_check_command_binaries`), `tests/test_structural_preflight.py` |
| Entry point | `validate_structure()` in `structural_preflight.py:148` |
| Success criterion | `df validate pipelines/factory/gates.dot` exits 0; introducing a `tool [type="tool", command="nonexistent-cli ..."]` makes it exit 2 with `checks: [{name: "command_binaries", ok: false, missing: [...]}]`. |
| Classification | runner-change |
| Status | ready-to-implement |
| Beads | new `jleechan-roadmap-p3b` (see Filing section below) |

---

## Pillar 4 — Dynamic LLM Timeouts & Provider Backoff

**Roadmap quote:** *"Classify nodes and establish adaptive timeouts: tool/local tests 60s, codergen/LLM 180s, review/deep auditing 300s. Enforce standard exponential backoff on HTTP/JSON layer calls."*

**Status:** partially shipped. The *minimum* threshold (60s) is enforced
across both parser and preflight. The *class-specific defaults* the roadmap
calls out are **not** shipped — current defaults are 1200s for codergen
gates (`runner/handlers.py:1645, 1721, 1953, 2746`) and 300s for tool
(`runner/handlers.py:1984`). Exponential backoff is shipped
(`runner/_backoff.py`).

**What shipped:**
* `runner/_coerce_timeout(value, default)` in `runner/handlers.py:63` —
  parse + clamp to `[5, 3600]`.
* `runner/parser.py:29` `_VALIDATION_TIMEOUT_MIN_SECONDS = 60`.
* `runner/structural_preflight.py:57` `TIMEOUT_THRESHOLD_S = 60`.
* `runner/_backoff.py` — exponential backoff with jitter (PR #59,
  commit `71de10f`).

**Remaining gap:**
The roadmap's proposed defaults (60 / 180 / 300 by class) are *more
aggressive* than current practice and would require per-call-site changes
or a centralized policy table. **Recommendation:** do NOT chase the
roadmap's exact numbers; instead, document the rationale for the current
defaults (1200s for codergen reflects observed LLM-call wall times during
real holdout runs; 300s for tool reflects pytest + tooling budgets) and
note that the *enforced minimum* (60s) is the actual guard rail. This is a
**doc-only reconciliation** plus a small `_TIMEOUT_DEFAULTS_BY_TYPE`
policy table in `runner/parser.py` for future expansion.

**Additionally, bead `jleechan-7ql`** (P1 OPEN) flags that `_tool`'s 300s
default is too low for some validation=true nodes. This is a separate
bead-driven workstream, not a roadmap gap.

| Field | Value |
|---|---|
| Target files | `runner/handlers.py` (per-call-site defaults), `docs/plans/factory_improvement_analysis.md` (rationale addendum), `runner/parser.py` (optional `_TIMEOUT_DEFAULTS_BY_TYPE`) |
| Entry point | `_coerce_timeout` in `runner/handlers.py:63` |
| Success criterion | (a) a docs PR explains why current defaults differ from the roadmap's proposed numbers; (b) `_TIMEOUT_DEFAULTS_BY_TYPE` (if added) is exercised by `tests/test_parser.py` covering tool/codergen/review classes. |
| Classification | doc-only + optional runner-change |
| Status | needs-spec-first (decide whether to chase roadmap numbers or document deviation) |
| Beads | depends on `jleechan-7ql` decision; no new bead needed at this layer |

---

## Pillar 5 — WAL Checkpoint Engine & Self-Healing Resume

**Roadmap quote:** *"Implement Write-Ahead Logging (WAL) checkpointing inside the SQLite CXDB itself. Prior to executing any handler, write the atomic execution frame (current_node, history, state, visits) to a local WAL transaction. Upon startup, the runner should automatically query CXDB for any active, incomplete runs."*

**Status:** partially shipped across three commits.

**What shipped:**
* `runner/cxdb.py:110` `PRAGMA journal_mode=WAL` — SQLite WAL mode for the
  CXDB event log (this is the literal WAL the roadmap asks for).
* `runner/engine.py:1017` `_load_checkpoint(path)` — loads a JSON
  checkpoint file.
* `runner/engine.py:1110-1111` — per-run checkpoint path
  `~/.dark-factory/runs/<run_id>/checkpoint.json`.
* `runner/engine.py:1034-1052` — `--resume` flow detects fan-out/fan-in
  state and re-walks from the last stable checkpoint.
* `runner/__main__.py:135-136` — `df resume` CLI shortcut.

**Remaining gap:**
The roadmap's "automatic resume via CXDB query at startup" is **NOT**
shipped on `main`. The `--resume <path>` flag exists, but a runner that
crashes leaves the user to discover the run_id and re-invoke manually.
This is the exact scope of `jleechan-2gv` (P2 OPEN), which lives on
`triage/parity-squashed` (commit `85afa95`):

* Persists a `launch_manifest.json` per run alongside the checkpoint
  (pipeline path, `--state`, backend, workdir).
* At startup, queries CXDB for runs where `run_end` was never recorded
  and the run's heartbeat is stale.
* Offers `df resume <run_id>` from the manifest — no reconstruction.

**This is a merge workstream, not new code.**

| Field | Value |
|---|---|
| Target files | `runner/engine.py` (the 2gv delta) |
| Entry point | `main()` in `runner/__main__.py:133` (adds startup CXDB probe for stale runs) |
| Success criterion | (a) `tests/test_resume.py` (or equivalent) drives a crash mid-run and confirms the next `dark-factory` invocation suggests the stale run_id; (b) `df resume <run_id>` reconstructs pipeline/backend/workdir without the user supplying them. |
| Classification | runner-change |
| Status | ready-to-implement (merge 2gv branch) |
| Beads | `jleechan-2gv` |

---

## Cross-cutting dependent beads

These are the `jleechan-o8q` dependents, status-sorted. The roadmap did not
name them explicitly, but they are the practical consequence of the
roadmap's reliability model.

### Done (merged to main)

| Bead | PR / Commit | What it covered |
|------|-------------|-----------------|
| `jleechan-grb` (P1, DONE) | PR #13 / `a46ad82` | run() try/except around node exec + transition; error StepRecord; route-to-fix; runs.final=error |
| `jleechan-1zx` (P1, DONE) | PR #13 / `a46ad82` | parallel branch crashes captured as error steps |

### Open — in `triage/parity-squashed` (merge workstreams)

| Bead | Commit | What it covers |
|------|--------|----------------|
| `jleechan-fgw` (P1) | `05b2270` | Structured runner JSONL events with transcript sidecars |
| `jleechan-2gv` (P2) | `85afa95` | Auto per-run resumability with launch manifest |
| `jleechan-ol7` (P2) | `5282953` | Heartbeat current-node progress artifact |
| `jleechan-ok8` (P2) | `602ea58` | Brownfield delete-first .dot templates + net-LOC/dead-code gates |
| `jleechan-sp6` (P2) | `5190dd8` | Monitor exit-node-recorded check; CXDB run isolation |
| `jleechan-x33` (P1) | `094c86f` | Reviewer gates: file-backed current-head diff/evidence audit |

### Open — needs design / implementation

| Bead | Status | Notes |
|------|--------|-------|
| `jleechan-8py` (P1) | OPEN | cli: panic hook exit-code string drift (Pillar 1 minor) |
| `jleechan-nm2` (P1) | OPEN | engine: persistence failures must not kill the primary run |
| `jleechan-7ql` (P1) | OPEN | handlers: real-LLM/codergen node timeout policy (Pillar 4) |
| `jleechan-wou` (P1) | OPEN | shipped as PR #50 structural-preflight; close bead |
| `jleechan-9wy` (P2) | OPEN | handlers: backend timeouts return structured Result metadata |
| `jleechan-xzw` (P2) | OPEN | cli: always print run_id and artifact paths in failure summaries |
| `jleechan-xgx` (P3) | OPEN | backend: pre-run health check (claude/codex/agy auth probe) |
| `jleechan-rx1` (P3) | OPEN | spawn: file-domain locking (merge_train) at run start |

---

## New beads to file in this PR

Three roadmap-specific gaps are not covered by existing dependents and
warrant new beads:

| New bead | Pillar | Status |
|----------|--------|--------|
| `jleechan-ku3` | Pillar 3 (Pre-Flight): extend `structural_preflight.py` with `_check_command_binaries` walking every `tool` node's `command` attribute through `shutil.which`. |
| `jleechan-z84` | Pillar 1 (Panic Hook): reconcile the roadmap's `128` exit-code recommendation with the implementation's `124` (or change the implementation). |
| `jleechan-arr` | Pillar 4 (Timeouts): document why current defaults diverge from the roadmap's 60/180/300 proposal; either adopt the table in `runner/parser.py` or annotate the deviation. |

Detailed bead JSONL entries are appended at the end of this plan.

---

## Definition of done for `jleechan-o8q`

`jleechan-o8q` itself closes when:

1. This implementation plan is merged and the bead is annotated
   "implementation plan landed at PR <TBD>."
2. `jleechan-grb`, `jleechan-1zx`, `jleechan-wou` are confirmed DONE
   (closed beads with PR evidence).
3. Pillar 3's `_check_command_binaries` gap is filed as
   `jleechan-ku3` (so it does not get lost).
4. The roadmap's three explicit gaps (panic exit-code drift, timeout
   rationale, command-binary preflight) have at least one bead each in
   the queue with clear successors.

`jleechan-o8q` is **not** closed when all dependent beads are done — it
is closed when the roadmap is operationally tractable (every
recommendation has a follow-on bead OR a citation proving it shipped).

---

## Low-level details

* **Roadmap drift on exit code (124 vs 128):** the `runner/__main__.py`
  crash event JSON contains `"exit_code": "128"` as a string at line 115.
  This string is metadata, not the actual `os._exit` value. The actual
  panic exit is `runner/panic_hook.py:71` `PANIC_EXIT_CODE = 124`. The
  decision to use `124` was deliberate (timeout-killed grouping); the
  string `"128"` at line 115 is an oversight. Recommended fix:
  - Replace `"128"` with `str(PANIC_EXIT_CODE)` and import the constant,
    or
  - Change `PANIC_EXIT_CODE` to `128` and document why the timeout-killed
    grouping is no longer the right trade-off.
* **Pillar 3 binary check:** use `shutil.which` not `pathlib.exists` so
  PATH resolution works the same way it does at runtime. Skip the check
  when `command` starts with a shell builtin (`cd`, `test`, etc.) — gate
  on `shlex.split(command)[0]` only.
* **Pillar 4 policy table shape (if added):**
  ```python
  _TIMEOUT_DEFAULTS_BY_TYPE: dict[str, int] = {
      "tool": 60,        # local subprocess (pytest, shell)
      "codergen": 300,   # LLM call budget; 1200 is the historic ceiling
      "review": 300,     # deep-audit / gate_es / gate_er
  }
  ```
  Wire into `_coerce_timeout(value, default)` via a third arg
  `default_lookup: str | None = None`.
* **Auto-resume CXDB probe shape (Pillar 5 follow-on):**
  `runner/cxdb.py` exposes `find_stale_runs(threshold_seconds: int) ->
  list[StaleRun]`. The startup probe calls it with `threshold_seconds =
  2 * heartbeat_interval`. Stale runs surface a one-line stderr message;
  `df resume <run_id>` is the user-facing escape hatch.
* **Skill-text fallout:** none of the pillars require skill-text changes
  directly. The dependent beads (`jleechan-x33`, `jleechan-ok8`) ship
  new skill text via PR #73 (#73 was already merged), so this lane is
  doc-only as far as the user-facing slash-command surface goes.

---

## Filing: new beads (JSONL entries)

The following beads were filed via `br create` on 2026-06-18 and confirmed
via `br show`. Each follows the schema of existing `jleechan-` beads (see
`br show jleechan-o8q` for the parent schema). The JSONL entries below are
provided for reference; the live source of truth is the SQLite-backed
beads DB, not this file.

| Bead ID | Title | Type | Priority | Parent | Extra deps |
|---------|-------|------|----------|--------|-----------|
| `jleechan-ku3` | runner: extend structural_preflight with command-binary check (Pillar 3 gap) | feature | P2 | `jleechan-o8q` | — |
| `jleechan-z84` | cli: reconcile panic exit code 124 vs roadmap's 128 (Pillar 1 doc-drift) | bug | P3 | `jleechan-o8q` | `jleechan-8py` |
| `jleechan-arr` | docs: rationale for current timeout defaults vs roadmap's 60/180/300 (Pillar 4) | feature | P3 | `jleechan-o8q` | `jleechan-7ql` |

Reference JSONL shape (matches `br show` output schema):

```jsonl
{"id": "jleechan-ku3", "title": "runner: extend structural_preflight with command-binary check (Pillar 3 gap)", "issue_type": "feature", "status": "open", "priority": 2, "parent": "jleechan-o8q"}
{"id": "jleechan-z84", "title": "cli: reconcile panic exit code 124 vs roadmap's 128 (Pillar 1 doc-drift)", "issue_type": "bug", "status": "open", "priority": 3, "parent": "jleechan-o8q", "dependencies": [{"id": "jleechan-8py", "type": "blocks"}]}
{"id": "jleechan-arr", "title": "docs: rationale for current timeout defaults vs roadmap's 60/180/300 (Pillar 4)", "issue_type": "feature", "status": "open", "priority": 3, "parent": "jleechan-o8q", "dependencies": [{"id": "jleechan-7ql", "type": "blocks"}]}
```

---

## Appendix A — Evidence trail (citations)

* Roadmap: `docs/plans/factory_improvement_analysis.md` lines 1–88.
* Parent bead: `br show jleechan-o8q`.
* Pillar 1 evidence:
  * `cb4978f [agento] feat(panic-hook): top-level crash artifact + distinct exit code (#49)`
  * `runner/panic_hook.py:63-71`
  * `runner/__main__.py:115` (`exit_code: "128"` string)
  * `runner/engine.py:1187-1487` (try/except around node exec)
  * `a46ad82 fix(engine): crash-resilient run() — record error + route to fix + per-run logs (#13)`
* Pillar 2 evidence:
  * `c312d8f Add repo/branch performance logging under /tmp/dark-factory. (#14)`
  * `runner/perf_log.py:36-200` (JSONL writes)
  * `05b2270 feat(observability): emit structured runner JSONL events with transcript sidecars (jleechan-fgw)` (not on main)
* Pillar 3 evidence:
  * `bda899c [agento] feat(preflight): CLI backend preflight — warn on missing, hard-stop on zero (#47)`
  * `d24a9ba [agento] feat(structural-preflight): df validate <pipeline.dot> pre-flight check (#50)`
  * `runner/parser.py:727-746`
  * `runner/structural_preflight.py:60-145`
* Pillar 4 evidence:
  * `71de10f feat(runner): add _backoff module for transient-failure retry with jitter (#59)`
  * `runner/parser.py:29` `_VALIDATION_TIMEOUT_MIN_SECONDS = 60`
  * `runner/structural_preflight.py:57` `TIMEOUT_THRESHOLD_S = 60`
  * `runner/handlers.py:1645, 1721, 1953, 2746` (1200s codergen defaults)
* Pillar 5 evidence:
  * `runner/cxdb.py:110` `PRAGMA journal_mode=WAL`
  * `runner/engine.py:1017-1052` (checkpoint load + resume)
  * `runner/engine.py:1110-1111` (per-run checkpoint path)
  * `runner/__main__.py:135-136` (`df resume` shortcut)
  * `85afa95 feat(2gv): auto per-run checkpoint resumability and manifest (jleechan-2gv)` (not on main)

## Appendix B — Cross-validation ladder (per self-correction feedback)

Per [`feedback_2026-06-13_self_correction_at_ceiling.md`](/Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-06-13_self_correction_at_ceiling.md),
premature "ceiling" verdicts have been wrong 3 times in this repo. The
re-run from scratch this time:

* Searched for the literal keywords (`panic`, `preflight`, `jsonl`,
  `wal`, `resume`, `timeout`, `checkpoint`, `crash`) across `git log --all`
  AND `br search` (two independent code paths).
* Checked the 4 standard implementation directories: `runner/`,
  `pipelines/`, `docs/`, and `.claude/` — not just the obvious one.
* Cross-checked each "shipped" claim against the bead that the bead's
  description claims to close. `jleechan-grb` → PR #13 ✅; `jleechan-wou`
  → PR #50 ✅; `jleechan-1zx` → PR #13 ✅. No ceiling misfires detected
  in this round.