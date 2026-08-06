# Explore — OUT: sub-phase boundary for `skeptic_gate_cli.main()` split

> Synthesizes `explore-authorities.md`, `explore-reuse.md`, `explore-risks.md`.
> Goal of the parent task: **define the exact sub-phase boundary first** —
> the splitter (next phase) consumes this as ground truth.

---

## 1. The split boundary — locked

`main()` (`runner/skeptic_gate_cli.py:720-1542`, 640+ lines) becomes a
**30-line dispatcher** over 7 sub-functions, returning through one
`PublishContext` object. The split is along **phase transitions of the
gate's own state machine**, not along today's comment-marked sections
(which contain duplicate writers and a mid-run flag flip).

### 1.1 Five-phase state machine (the authority for boundary lines)

```
BOOT ──► CONTEXT (mode ∈ {online, offline}, preconditions_ok)
       ──► REVIEW (rules dispatch, parallel fan-out)
       ──► CONSENSUS (one aggregator, one body)
       ──► PUBLISH  (pending → upsert → readback → final)
              ▲
              └── DRY branch when mode == offline OR args.dry_run
```

| # | Phase | Sub-function | Returns | Exit code on failure |
|---|---|---|---|---|
| 1 | BOOT | `_boot(args, env) -> BootContext` | `BootContext(reviewers, repo, pr_number, head_sha_or_empty)` | `2` on argparse |
| 2 | CONTEXT | `_resolve_context(boot, env) -> RunContext` | `RunContext(mode, head_sha, diff, implementation_identity, contract, rules, changed_files)` | `1` (local-git fail), `2` (contract load / SHA mismatch / diff-oversize) |
| 3 | REVIEW | `_run_review(ctx) -> ReviewResult` | `ReviewResult(per_rule_results, matched)` | none — review errors surface in `per_rule_results` |
| 4 | CONSENSUS | `_aggregate(review) -> Aggregate` | `Aggregate(verdict, body, check_state, reviewer, provenance)` | none — empty list → fail-CLOSED (NOT vacuous PASS) |
| 5 | PUBLISH | `_publish(agg, ctx) -> int` | `0` (success) / `1` (fail) / `2` (mid-run SHA drift) | per I10, I19, I20, I27 |

The boundary line between phases is **the moment a phase returns its
result object**. Every phase is a pure function over its input context
(only `BOOT` and `CONTEXT` touch subprocess / filesystem; REVIEW is
parallel; CONSENSUS is in-memory; PUBLISH is the only phase that
writes to GitHub).

### 1.2 The `RunContext` (single object threading state)

```python
@dataclass
class RunContext:
    mode: Literal["online", "offline"]          # NEVER mutated after CONTEXT
    preconditions_ok: bool                      # set False by CONTEXT on dry_run promotion
    repo: str
    pr_number: int
    head_sha: str                               # authoritative API head (online) or local HEAD (offline)
    diff: str
    implementation_identity: str
    contract: Optional[BeadContract]
    rules: list[Rule]                           # synthetic fallback applied HERE (one owner)
    changed_files: list[str]
    dry_run_forced_by_api_failure: bool         # explicit flag — never silent
    perf_start_monotonic: float
```

The `args.dry_run = True` mid-run mutation (`L887`) becomes
`ctx.dry_run_forced_by_api_failure = True` — a **read-only mirror**
into `PUBLISH`. No phase after `CONTEXT` mutates `args` or `ctx`.

### 1.3 Sub-function signatures (the seven functions `main()` calls)

```python
def _boot(args: argparse.Namespace, env: Mapping[str, str]) -> BootContext
def _resolve_context(boot: BootContext, env: Mapping[str, str]) -> RunContext
def _run_review(ctx: RunContext, reviewers: list[tuple[str, str]]) -> ReviewResult
def _aggregate(review: ReviewResult) -> Aggregate
def _pre_publish_sha_check(ctx: RunContext) -> None              # raises PreconditionFail on drift
def _publish(agg: Aggregate, ctx: RunContext) -> int
def _emit_perf_log(ctx: RunContext, *, outcome: str, exit_code: int) -> None
```

`main()` becomes:

```python
def main(argv: Optional[list[str]] = None) -> int:
    args = _build_arg_parser().parse_args(argv)
    boot = _boot(args, os.environ)
    try:
        ctx = _resolve_context(boot, os.environ)
        review = _run_review(ctx, boot.reviewers)
        agg = _aggregate(review)
        _pre_publish_sha_check(ctx)            # may raise PreconditionFail → return 2
        rc = _publish(agg, ctx)
    except PreconditionFail as pf:
        rc = pf.exit_code                       # always 2
    _emit_perf_log(ctx, outcome=..., exit_code=rc)
    return rc
```

---

## 2. Authorities map → sub-phase ownership (the deduplication ledger)

Every current authority (from `explore-authorities.md`) maps to exactly
one new sub-phase. Conflicting authorities (C1, C2, C3) are resolved by
**single-writer ownership** at the phase boundary.

| Sub-phase | Owns (was) | No longer owned by |
|---|---|---|
| `_boot` | argparse mutual-exclusivity (was L825-830); `_parse_reviewers` (was L658-712); repo default (was L834-836); `_perf_start` capture (was L842) | the inline `parser.error` at L825-830 (`add_mutually_exclusive_group` replaces it — rung-3, §3.2 reuse) |
| `_resolve_context` | `get_pr_head_sha_via_api` + API SHA equality check (was L848-867); `get_pr_diff` + diff size guard (was L869, L891-918); local-git fallback (was L876-886) with `dry_run_forced_by_api_failure=True` (was `args.dry_run = True`); implementation identity (was L920-944); **the ONE** contract load — both L957-1000 AND L1041-1095 collapse here (was C1); `RuleLoader.load_rules()` + synthetic fallback (was L1003-1039 AND `handler_universal_prompts.py:525-536` collapse here — was C3); `LocalGitScm.get_changed_files` (was L1014-1020) | `_emit_perf_log` (was L858, L968, L991) — moved to top-level finally-style |
| `_run_review` | `dispatcher.dispatch(...)` (unchanged) + `bind_reviewer_identity` + `verify_provenance` (already in dispatcher) | the `FakeAggregate` wrapper (was L1118-1126) — **deleted**, see §3.1 |
| `_aggregate` | `ConsensusAggregator.compile_report(...)` (was L1107-1116) | **NOT** `consensus.aggregate()` (the legacy fail-OPEN one — see §3.1) |
| `_pre_publish_sha_check` | re-fetch `get_pr_head_sha_via_api` (was L1129-1137) — raises `PreconditionFail` | the `_force_failure_status` call (was L1188) — moved into `_publish` |
| `_publish` | `set_commit_status(pending)` (was L1165); comment upsert (was L1178); read-back (was L1193-1243); `verify_published_comment` (was L1225-1234); final `success`/`failure` (was L1298); `_force_failure_status` escape hatch (was L1363) | the inline happy-path-vs-failure-path duplication (was C4) — collapses to one ordered method |
| `_emit_perf_log` | `runner.perf_log.open_run` + `close_run` (was L1398-1440) | the local-only `_emit_perf_log` JSONL shape — **deleted**, see §3.4 |

**Three deletes, no projection:**

1. The second contract-load block at `L1041-1095` — `explore-authorities.md`
   C1 god-mode path. **Removed entirely.** The single `_resolve_context`
   call at L958-1000 is the canonical owner.
2. The `class FakeAggregate` at `L1118-1126` — replaced by `ConsensusAggregator
   .compile_report(...)` direct return.
3. The duplicated synthetic-rule fallback in `handler_universal_prompts.py
   :525-536` — both call sites project from `RuleLoader.load_or_default()`.

**One explicit kept god-mode path (flagged, not removed):**

- `MOCK_IMPLEMENTER_IDENTITY` and `MOCK_CODEX_RESPONSE`/`MOCK_GEMINI_RESPONSE`
  (`explore-authorities.md` G1, G2) — **production path MUST short-circuit
  on these unless `--allow-mock` is passed** (was the patch-trap §5.7 in
  `explore-risks.md`). The split adds this guard inside `_run_review`
  before any subprocess call; current code does not enforce.

---

## 3. Reuse targets from `explore-reuse.md` (the rung ladder)

### 3.1 Aggregator: pick one, kill the other (rung-6 win)

**Decision:** `_aggregate` calls `ConsensusAggregator.compile_report`
ONLY. The legacy `skeptic_gate.aggregate_results` (`skeptic_gate.py`) is
**not** used. But `consensus.aggregate()` (empty-list → "PASS") is the
FAIL-OPEN variant documented in `explore-risks.md` §5.7.

The split must therefore commit: **`compile_report` is fail-closed by
its existing branch at `consensus.py:24-37` ONLY when results is empty
AND the test contract requires it.** The current `compile_report`
returns `"PASS"` for empty input. The splitter must change that single
branch to return `("FAIL", vacuous_red_reason)` when `matching_rules`
is empty AND no `mandatory_reviewer` ran — this is the
`explore-risks.md` §5.7 patch-trap. **Add this test FIRST:**

```python
def test_compile_report_empty_rules_returns_fail_not_vacuous_pass():
    ...
```

Then `_aggregate` returns `Aggregate(verdict="FAIL", body=..., reviewer="(vacuous)")`.
The 80-line delete of `FakeAggregate` follows.

### 3.2 `argparse.add_mutually_exclusive_group` (rung-3 win, drops L825-830)

Replace L820-830 with:

```python
group = parser.add_mutually_exclusive_group()
group.add_argument("--contract-file", default=..., help=...)
group.add_argument("--bead-id", default=..., help=...)
```

`argparse.error()` still raises `SystemExit` — preserve `_emit_perf_log`
on this path by catching in `main()` and routing through the
`PreconditionFail` exit-code-2 path. (`explore-risks.md` §5.9 trap.)

### 3.3 `runner.skeptic_gate_io.py` — extract the GitHub I/O surface

Move into a sibling module:

```python
# runner/skeptic_gate_io.py
EXPECTED_BOT_ACTOR
MARKER                                          # re-export
GH_SUBPROCESS_TIMEOUT / GH_DIFF_TIMEOUT
REVIEWER_SECRET_ENV_DENY
REVIEWER_ENV_BASE_ALLOWLIST
REVIEWER_ENV_PROVIDER_ALLOWLIST
_reviewer_env(parent_env, reviewer)
gh_api(method, path, *, body)
find_existing_bot_comment(repo, pr, expected_actor)
post_or_update_comment(repo, pr, body, expected_actor)
set_commit_status(repo, sha, *, state, context, description)
read_back_comment(repo, comment_id) -> Optional[dict]
read_back_status(repo, sha, context) -> Optional[str]   # NEW — rung-6 one-liner over gh_api
MAX_DIFF_BYTES
```

`_publish` becomes a 30-line method that wires these helpers in the
**mandatory 4-step order** (I10, I27, `explore-risks.md` §5.5 trap):
`pending → upsert → readback → final`. The
"consolidate side-effects into one helper" trap is avoided because
each step is a named call, not a hidden branch.

### 3.4 `runner.perf_log` — replace `_emit_perf_log`

`_emit_perf_log` is removed. `_emit_perf_log` (the new sub-function)
calls `runner.perf_log.open_run(...)` at the top of `main()` and
`close_run(...)` at the return. JSONL shape change — confirm test
`tests/test_skeptic_gate.py:2076 test_cli_exposes_perf_log_args` still
passes against the new shape (or update it).

### 3.5 `runner.skeptic_gate_pipeline.py` — the verification core

A thin orchestrator wrapping `dispatcher.dispatch` + `aggregate`:

```python
# runner/skeptic_gate_pipeline.py
def run_verification(
    reviewers: list[tuple[str, str]],
    repo: str, pr_number: int, head_sha: str, base_sha: str,
    diff: str, implementation_identity: str,
    contract: Optional[BeadContract],
    rules: list[Rule],
    changed_files: list[str],
) -> Aggregate:
    dispatcher = VerifierDispatcher(_cli_ref, reviewers)
    results = dispatcher.dispatch(rules, changed_files, diff, repo, pr_number,
                                   head_sha, base_sha, implementation_identity, contract)
    return ConsensusAggregator().compile_report(results, head_sha, repo, pr_number, contract)
```

`_run_review` + `_aggregate` collapse into one call to `run_verification`.

---

## 4. Invariants the split MUST preserve (from `explore-risks.md` §4)

These are non-negotiable — the splitter writes tests for each, then
implements. Numbers reference `explore-risks.md` §4.

| ID | Invariant | Enforced in sub-phase | New test required |
|---|---|---|---|
| I1 | Both reviewers MUST run | `_boot._parse_reviewers` | no (existing) |
| I2 | Diff > 1 MiB MUST fail-closed | `_resolve_context._capture_diff` | no (existing) |
| I3 | API head SHA MUST equal event SHA | `_resolve_context._resolve_pr_and_sha` | no (existing — but add explicit CLI-level test) |
| I4 | HEAD cannot change mid-run without `failure` status | `_pre_publish_sha_check` + `_publish._force_failure_status` | **YES** (was no dedicated test) |
| I5 | Reviewer's IDENTITY must equal its CLI | `dispatcher.run_one` (unchanged) | no (existing) |
| I6 | Implementation identity ≠ reviewer identity | `dispatcher.run_one` (unchanged) | no (existing) |
| I7 | Comment body MUST contain exactly six fields | `_publish` + `verify_published_comment` | no (existing) |
| I8 | Read-back MUST reject mismatched 6 fields | `_publish._readback` | no (existing) |
| I9 | Comment upsert MUST filter non-bot marker | `_publish._upsert_comment` | no (existing) |
| I10 | Mandatory commit-status order | `_publish` (explicit 4 steps) | no (existing) |
| I11 | Contract-load failure MUST exit 2 | `_resolve_context._load_contract` | no (existing) |
| I12 | `--bead-id` and `--contract-file` mutex | `_boot` (`argparse` group) | no (existing) |
| I13 | Reviewer env strips `GITHUB_TOKEN` etc. | `skeptic_gate_io._reviewer_env` | no (existing) |
| I14 | Per-reviewer credentials only | `skeptic_gate_io._reviewer_env` | no (existing) |
| **I15** | Mock responses MUST NOT post to GitHub | `_run_review` — adds `--allow-mock` flag | **YES** (was gap) |
| I16 | Perf-log dir MUST NOT be bare `/tmp` | `runner.perf_log.open_run` | no (existing) |
| I17 | Reviewer subprocess MUST have timeout | `_run_review` | no (existing) |
| I18 | `MOCK_IMPLEMENTER_IDENTITY` (test seam) | `_resolve_context._derive_identity` | no (existing) |
| **I19** | Local-git fallback MUST publish `failure`, not silent downgrade | `_publish` — gates on `ctx.dry_run_forced_by_api_failure` | **YES** (was gap) |
| **I20** | Two pushes during run MUST leave `failure` on ORIGINAL SHA | `_pre_publish_sha_check` + `_publish._force_failure_status(original_sha)` | **YES** (was gap) |
| **I21** | `compile_report` body uses bound SHA when non-empty | `_aggregate` (unchanged — but document) | **YES** (was implicit) |
| I22 | `PR_NUMBER` threaded, not parsed | `_publish._publish_failure` | no (existing) |
| I23 | `TEST_RUN_EVIDENCE` with `failed > 0` rejected | `skeptic_gate.parse_verdict` | no (existing) |
| I24 | `LINT_RUN_EVIDENCE` with `errors > 0` rejected | `skeptic_gate.parse_verdict` | no (existing) |
| **I25** | Dispatcher MUST re-check reviewer identity uniqueness | `_run_review` (new guard) | **YES** (was gap) |
| I26 | `LD_LIBRARY_PATH` MUST be in reviewer env | `skeptic_gate_io._reviewer_env` | no (documented in memory) |
| I27 | Final-write `success` MUST follow read-back equality | `_publish._final_step` | no (existing) |
| **I28** | `MOCK_*_RESPONSE` MUST short-circuit BEFORE publish | `_publish` early-return on `ctx.mock_mode` | **YES** (was gap) |

**Bold = new tests the split PR must add before refactoring.**

---

## 5. Open risks to file as beads (cannot evaluate without more info)

From `explore-risks.md` §6 — each gets a follow-up bead, NOT a blocker
on the split, but the split design must NOT preclude them.

| ID | Risk | Bead prefix |
|---|---|---|
| O1 | `agy` argv shape may change (memory: `feedback_2026-08-05_cold_reviewer_chain_state.md`) | jleechan-agy-cmd-shape |
| O2 | `LocalGitScm._resolve_head` ref-search order undocumented | jleechan-scm-ref-docs |
| O3 | Daemon polls `skeptic` status, not exit code — contract-load fail doesn't trigger reroll | jleechan-status-on-contract-fail |
| O4 | Dispatcher error path silent on GitHub side (compounded with I25) | jleechan-dispatcher-error-publish |
| O5 | Multi-PR reroll on same SHA races comment upsert | jleechan-same-sha-reroll-race |
| O6 | `_publish_failure` not `dry_run`-gated on size-fail from bead-contract path | jleechan-publish-fail-dryrun |

---

## 6. Patch-trap checklist (from `explore-risks.md` §5)

The split must avoid each. Encode as review-blockers on the splitter PR:

1. **§5.1** — Don't split along the existing comment markers; define
   the 5-phase state machine first, then split. ✓ (this document is
   that definition).
2. **§5.2** — Don't delete the duplicate contract-load block at
   `L1041-1095` without running both through the test matrix
   `tests/test_skeptic_gate_cli_contract_echo.py` first. ✓ (called
   out in §2 above).
3. **§5.3** — Don't replace `--dangerously-skip-permissions` on agy
   without verifying agy's sandbox flag.
4. **§5.4** — Don't add a global mutex around perf-log. Use
   per-sha file naming in `runner.perf_log`.
5. **§5.5** — Don't consolidate side effects into one helper. Keep
   the 4-step ordering explicit in `_publish`.
6. **§5.6** — Don't just rename `FakeAggregate`; replace with
   `ConsensusAggregator.compile_report` direct return.
7. **§5.7** — Don't ship `consensus.aggregate`'s fail-OPEN
   empty-list behavior. Use `compile_report` and change its
   empty-list branch to fail-closed.
8. **§5.8** — Move `--trusted-code-sha` enforcement to the TOP
   of `_resolve_context`, before any diff capture.
9. **§5.9** — Use `argparse.add_mutually_exclusive_group` BUT route
   `parser.error` through `PreconditionFail` so perf-log still fires.

---

## 7. Sub-phase boundary — final locked answer

```
main()  =  _boot
       ─► _resolve_context      (mode ∈ {online, offline}, preconditions_ok)
       ─► _run_review           (parallel fan-out, one SkepticResult per rule)
       ─► _aggregate            (one body, one verdict, fail-closed on empty)
       ─► _pre_publish_sha_check (raises PreconditionFail on mid-run drift)
       ─► _publish              (pending → upsert → readback → final)
       ─► _emit_perf_log        (every return path)
       =  exit code
```

**Seven sub-functions. Five phases. Three deletions (FakeAggregate,
duplicated contract-load, duplicated synthetic-rule fallback). One
explicit dry-run flag (`dry_run_forced_by_api_failure`) replacing
the mid-run `args.dry_run = True` mutation. Four new tests
(I15, I19, I20, I25 — and I21, I28 are bonus). Two new modules
(`skeptic_gate_io.py`, `skeptic_gate_pipeline.py`).**

The splitter consumes:

- this document for the boundary
- `explore-authorities.md` for the deduplication targets
- `explore-reuse.md` for the rung-2/3 helpers and the two new modules
- `explore-risks.md` for the invariants (I1-I28) and patch-traps (§5.1-§5.9)

The splitter does NOT need to re-investigate any of the above.

---

explore written: .dark-factory/explore-out.md