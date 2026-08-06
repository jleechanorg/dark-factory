# Explore — Authorities (skeptic_gate_cli main() split)

This file maps **who currently owns what** in the skeptic-gate pipeline so the
upcoming `main()` split can define its sub-phase boundary on top of stable
authorities rather than today's procedural flow.

Source files inspected:

- `runner/skeptic_gate_cli.py` (1542 lines) — orchestrator / CLI entrypoint
- `runner/skeptic_gate.py` (2336 lines) — domain library (parsing, evaluation, formatting)
- `runner/dispatcher.py` — multi-rule reviewer fan-out
- `runner/consensus.py` — aggregator
- `runner/rule_loader.py` — YAML front-matter rule loader
- `runner/scm_provider.py` — local-git SCM abstraction
- `runner/handler_universal_prompts.py` — alternative in-process caller
- `.github/workflows/skeptic-gate.yml` / `skeptic-gate-caller.yml` — bootstrap

---

## Current authorities

| Concept (sub-phase primitive) | Owning component | Path:line |
| --- | --- | --- |
| PR head SHA (authoritative) | `get_pr_head_sha_via_api` (only source; SHA equality-gated against `event_sha` / `args.pr_sha` / `PR_HEAD_SHA`) | `runner/skeptic_gate_cli.py:338` |
| PR diff (authoritative) | `get_pr_diff` (`gh pr diff --repo`); fallback to `LocalGitScm.get_diff` only when API fails | `runner/skeptic_gate_cli.py:349`, `runner/skeptic_gate_cli.py:881` |
| Implementation identity | `get_implementation_identity` (CLI module) — overrides environment-driven `MOCK_IMPLEMENTER_IDENTITY` (test backdoor); derived from commit-subject prefix via `extract_implementation_identity_from_commit` | `runner/skeptic_gate_cli.py:383`, `runner/skeptic_gate_cli.py:384-385`, `runner/skeptic_gate.py:1310` |
| Bead contract (authoritative source of truth) | `load_bead_contract_from_bead` via `br show --json <bead>`; `--bead-id` wins over `--contract-file` per mutual-exclusion | `runner/skeptic_gate_cli.py:958-1000`, `runner/skeptic_gate.py:691` |
| Bead contract (legacy fallback) | `load_bead_contract(<file>)` — hand-authored JSON path | `runner/skeptic_gate.py:544` |
| Reviewer list / mandatory set | `_parse_reviewers` enforces `MANDATORY_REVIEWERS = ("codex","gemini")` — single-owner, hard-coded | `runner/skeptic_gate_cli.py:658`, `runner/skeptic_gate_cli.py:661` |
| Rule list | `RuleLoader.load_rules()` — `config/skeptic/*.md` (global) + `.claude/commands/skeptic/*.md` (local); global wins on collision (`rules[id] = rule`) | `runner/rule_loader.py:22` |
| Synthetic rule fallback | Inline in `main()` (lines 1022-1039) **and** a second copy at `handler_universal_prompts.py:525-536` — both build a `Rule(reviewer=gemini, model=gemini-2.5-pro)` when `load_rules()` returns empty | `runner/skeptic_gate_cli.py:1022`, `runner/handler_universal_prompts.py:525` |
| Reviewer subprocess invocation | `invoke_reviewer` (CLI module) — late-bound through `dispatcher._cli.invoke_reviewer` so tests can monkeypatch | `runner/skeptic_gate_cli.py:553`, `runner/dispatcher.py:7-9`, `runner/dispatcher.py:100` |
| Reviewer CLI argv builder | `_build_reviewer_cmd` — single owner (only CLI module) | `runner/skeptic_gate_cli.py:455` |
| Reviewer sanitized env | `_reviewer_env` — single owner (deny-list + base allow-list + per-reviewer provider allow-list) | `runner/skeptic_gate_cli.py:161` |
| Reviewer output → `ParsedVerdict` | `parse_verdict` (skeptic_gate.py) — strict 10-field no-extra-fields contract (issue #384) | `runner/skeptic_gate.py:1041` |
| Contract-echo parse | `parse_contract_echo` + `_strip_contract_echo_block` — runs BEFORE `parse_verdict` so the 10-field invariant survives unchanged | `runner/skeptic_gate.py:766`, `runner/skeptic_gate.py:734`, `runner/skeptic_gate.py:1601-1605` |
| Per-rule outcome | `evaluate(...)` — produces `SkepticResult`; CLI→identity binding and provenance are re-asserted in `dispatcher.run_one` after `evaluate` returns (NOT inside `evaluate`) | `runner/skeptic_gate.py:1540`, `runner/dispatcher.py:114-160` |
| Rule fan-out | `VerifierDispatcher.dispatch` — `ThreadPoolExecutor(max_workers=len(matching_rules))`; uses `_cli.invoke_reviewer` and `_cli.evaluate` for late-bound monkeypatch surface | `runner/dispatcher.py:79`, `runner/dispatcher.py:164` |
| Rule matching glob | `VerifierDispatcher.match_glob` / `rule_matches` — empty `changed_files` triggers all-match when `target_globs` contains `*` / `**` / `*/\*` (test backdoor, issue #386 r3 contract-echo plumbing) | `runner/dispatcher.py:28`, `runner/dispatcher.py:40` |
| Aggregate verdict | `ConsensusAggregator.compile_report` — `None` is the "invalid" sentinel (any `check_state="failure"` or `verdict is None`); empty-results path returns `PASS` unconditionally (no rules match → vacuous green) | `runner/consensus.py:7`, `runner/consensus.py:23-37` |
| Comment body shape | `format_comment` — single owner; `MARKER` literal lives here, `ReadBackCheck` consumers extract the same 6 fields (`HEAD_SHA`, `REPO`, `PR_NUMBER`, `VERDICT`, `REVIEWER`, `IMPLEMENTATION_PROVENANCE`) | `runner/skeptic_gate.py:1454`, `runner/skeptic_gate_cli.py:1208-1218` |
| Bot comment upsert | `post_or_update_comment` → `find_existing_bot_comment` → `gh_api` (single owner) | `runner/skeptic_gate_cli.py:280`, `runner/skeptic_gate_cli.py:242` |
| Commit status (pending → success/failure) | `set_commit_status`; readback order is **pending FIRST → upsert comment → readback → final success/failure** (post-audit fix #4953116428) | `runner/skeptic_gate_cli.py:305`, `runner/skeptic_gate_cli.py:1146-1343` |
| Commit status (force-fail overwrite) | `_force_failure_status` — invoked from EVERY readback-mismatch path so stale `pending` cannot satisfy merge protection | `runner/skeptic_gate_cli.py:1363` |
| Read-back verification | `verify_published_comment` (skeptic_gate.py) — byte-equality on 6 fields; called once per run after the comment is published | `runner/skeptic_gate.py:1986`, `runner/skeptic_gate_cli.py:1225` |
| Read-back check fields | `ReadBackCheck` dataclass — single owner; CLI module constructs it inline at lines 1208-1218 | `runner/skeptic_gate.py:1975`, `runner/skeptic_gate_cli.py:1208` |
| Performance log line | `_emit_perf_log` (CLI module); emitted at every early-return and at the final return; errors are swallowed (perf never affects gate outcome) | `runner/skeptic_gate_cli.py:1398`, `runner/skeptic_gate_cli.py:858, 968, 991, 1063, 1351` |
| Dry-run authority | `args.dry_run` (CLI arg) is the single source — also **flipped to True by main()** when API is unreachable (line 887), but only for the local-git fallback path | `runner/skeptic_gate_cli.py:795`, `runner/skeptic_gate_cli.py:887` |
| Reviewer mock backdoors | `MOCK_CODEX_RESPONSE` / `MOCK_GEMINI_RESPONSE` / `MOCK_IMPLEMENTER_IDENTITY` env-var branches inside `invoke_reviewer` and `get_implementation_identity` | `runner/skeptic_gate_cli.py:574-592`, `runner/skeptic_gate_cli.py:384-385` |

---

## Conflicting authorities

### C1. Contract-source dual-writer — `--bead-id` vs `--contract-file`

**Location:** `runner/skeptic_gate_cli.py:820-1000` (main) and an unintentional **second copy** at `runner/skeptic_gate_cli.py:1041-1095`.

`main()` loads `contract` **twice**. The first block (lines 957-1000) is the
documented, mutual-exclusion-aware version. The second block (lines 1052-1095)
is a verbatim copy that fires **after** the rule-load step. Either copy could
win depending on control flow (the second block rebinds the local `contract`
variable; Python's late-binding means the second block's `contract` is the one
that reaches `dispatcher.dispatch(...)` at line 1101-1104 — but the first
block's `_emit_perf_log` on failure also still runs and would emit twice on
early return paths).

**Ponytail lens:** one guard at the shared load function is the right
authority. The single upstream writer must be `load_bead_contract_for_run()`
called once at the top of the bead-contract sub-phase. The second copy is a
god-mode path that bypasses the mutual-exclusion guard at line 825-830 —
duplicate of the upstream writer; **must be removed**, not projected.

### C2. Implementer identity — CLI module vs test env

**Location:** `get_implementation_identity` in `runner/skeptic_gate_cli.py:383-447`
checks `MOCK_IMPLEMENTER_IDENTITY` BEFORE any SHA-bound lookup, returning
the env value when present. The same function also exists at
`runner/handler_universal_prompts.py:520` (re-imported) and is invoked there
at line 568 with `expected_sha` derived from `_worktree_head_sha(ctx.workdir)`
— a DIFFERENT upstream source than `get_pr_head_sha_via_api` used by the CLI
workflow path.

**Ponytail lens:** the upstream authority for `implementation_identity` is
`runner/skeptic_gate.py:1310 extract_implementation_identity_from_commit`,
applied to the PR's HEAD commit subject. `MOCK_IMPLEMENTER_IDENTITY` is a
test-only backdoor that **must not** be honored in the production main() —
currently it is. Read-only mirrors of the production value are fine for tests,
but the production path should bypass the mock entirely.

### C3. Synthetic-rule fallback — main() vs handler

**Location:** two independent copies of "if `rules` is empty, build a single
mandatory-reviewer Rule":

- `runner/skeptic_gate_cli.py:1022-1039` (CLI main)
- `runner/handler_universal_prompts.py:525-536` (in-process universal-prompt handler)

Both build `Rule(reviewer="gemini", model="gemini-2.5-pro", target_globs=["*"])`
but the CLI version infers `model="gemini-2.5-pro"` only when the reviewer
tuple has empty model, whereas the handler hard-codes the model. They diverge
under empty-model input.

**Ponytail lens:** `RuleLoader` is the single upstream writer. When empty, the
fallback should be a read-only default produced by `RuleLoader.load_or_default()`
— one owner, both call sites project from it.

### C4. Status-publish order — implicit state machine

**Location:** `runner/skeptic_gate_cli.py:1146-1343`. The four-phase publish
(`pending → comment → readback → final`) is encoded as inline procedural
ordering with no named sub-state. If a future maintainer inserts an early
`return` between `set_commit_status(pending)` and the final `set_commit_status`
without routing through `_force_failure_status`, the gate leaves a stale
`pending` on the SHA — which merge protection treats as **NOT green**, but if
the status is later overwritten by another run, the prior gate's "green" claim
is lost.

**Ponytail lens:** the four phases (`pending`, `upsert`, `readback`, `final`)
should be a `class StatusPublish` with explicit phases and a `force_fail()`
escape hatch. The current code does this correctly but only because the inline
comments above each branch are load-bearing for human readers — fragile.

### C5. `parse_verdict` strict contract vs `parse_contract_echo` block

**Location:** `runner/skeptic_gate.py:1601-1605` — the contract-echo block is
extracted and stripped before `parse_verdict` runs, so the strict 10-field
contract is preserved. This is correct, but it is a **silent precedence rule**.
If `parse_verdict` ever grows a "must contain X" check, the strip-then-parse
ordering must be preserved or the gate silently fails.

**Ponytail lens:** `parse_contract_echo` is the upstream extractor; `parse_verdict`
is a downstream consumer that must see the echo block stripped. The current
ordering at lines 1601-1605 must be the single source of truth — a single
function `parse_review(review_output, contract) -> ParsedVerdict` that does
both, so the order cannot drift.

---

## Implicit state machines

### S1. SkepticResult lifecycle

- **Key:** `(rule_id, reviewer)` tuple derived from the per-rule `dispatch`
  result.
- **Lifecycle:** created in `dispatcher.run_one` (`runner/dispatcher.py:86`) →
  mutated (security-check override) at `runner/dispatcher.py:131-160` (CLI→
  identity binding fail / provenance fail) → collected into
  `List[Tuple[Rule, SkepticResult]]` at `runner/dispatcher.py:165-179` →
  consumed by `ConsensusAggregator.aggregate` (line 7) and
  `ConsensusAggregator.compile_report` (line 23) → `body` string is rendered
  into a `FakeAggregate` wrapper at `runner/skeptic_gate_cli.py:1118-1126` →
  posted by `post_or_update_comment` (line 1178).
- **Persistence:** transient (in-memory list across `ThreadPoolExecutor`).
  No cache, no DB.
- **Readers:** `compile_report`, `verify_published_comment` (read-back, on
  the published `body` string), `_format_*_block` helpers (read from the
  same `SkepticResult.parsed`).

### S2. BeadContract lifecycle

- **Key:** bead-id string (`jleechan-xxx`) or contract-file path.
- **Lifecycle:** resolved by `load_bead_contract_from_bead` →
  `load_bead_contract(<path>)` (`runner/skeptic_gate.py:691, 544`) →
  passed as `contract=` to `dispatcher.dispatch` → threaded into
  `_cli.build_prompt` and `_cli.evaluate` → used by `parse_contract_echo`
  to extract the echo block from reviewer output → used by
  `evaluate_contract_echo` to grade ADDRESSED / NOT-ADDRESSED / N-A per
  acceptance item.
- **Persistence:** read once, never written back. Bead DB itself is the
  upstream (br CLI is the authority).
- **Readers:** `build_prompt`, `evaluate`, `parse_contract_echo`,
  `evaluate_contract_echo` — all read-only consumers.

### S3. Comment body / status state machine (commit-status side)

- **Key:** `(repo, head_sha, status_context)`.
- **Lifecycle:** `pending` (line 1165) → `comment` upserted (line 1178) →
  read-back read (line 1193) → status re-fetched via gh api (line 1248) →
  final `success`/`failure` (line 1298). On any readback mismatch, the path
  routes through `_force_failure_status` and `return 1` WITHOUT writing
  the final status — the `pending` from line 1165 is overwritten by
  `_force_failure_status`.
- **Persistence:** GitHub API (commit status + issue comment) — `gh api`
  through `subprocess.run` with `GH_SUBPROCESS_TIMEOUT=60` /
  `GH_DIFF_TIMEOUT=120`.
- **Readers:** branch protection rules (external), `_force_failure_status`,
  the readback loop at line 1248-1290.

### S4. Marker / `MARKER` literal — implicit idempotency key

- **Key:** the `MARKER` string constant in `runner/skeptic_gate.py:1411`.
- **Lifecycle:** comment author writes `MARKER` in the body → next run's
  `find_existing_bot_comment` matches by `MARKER in body` AND
  `author == expected_actor` (line 269) → returns comment id → PATCH
  instead of POST.
- **Persistence:** GitHub issue comment. The `MARKER` is the implicit
  idempotency key.
- **Readers:** `find_existing_bot_comment` (line 272), the bot-side comment
  authors. **NOTE:** anyone can paste the marker text into their own
  comment — the `expected_actor` filter is the real guard. `MARKER` is a
  read-only mirror of the upstream identity check.

### S5. `_perf_start` monotonic timer

- **Key:** single float, captured at `runner/skeptic_gate_cli.py:842`.
- **Lifecycle:** captured at start, read at every early-return
  (`_emit_perf_log`) and at final return (line 1351). Never reset; never
  compared; only `int((monotonic() - _perf_start) * 1000)` matters.
- **Persistence:** none — process-local.
- **Readers:** every `_emit_perf_log` call. **god-mode path:** none
  (best-behaved state in the file).

### S6. Rule-id collision

- **Key:** `rules[rule_id]` dict in `RuleLoader.load_rules()` at
  `runner/rule_loader.py:23`.
- **Lifecycle:** both global and local YAML files are walked; later writes
  win (local overrides global because local is second in the iteration).
- **Persistence:** disk (YAML files).
- **Readers:** `dispatcher.dispatch` — single owner, no divergence.

---

## Streaming / non-streaming branches

### Stream-vs-sync branches

| Branch | Owner | Notes |
| --- | --- | --- |
| `ThreadPoolExecutor` reviewer fan-out (parallel per rule) | `dispatcher.run_one` at `runner/dispatcher.py:164-179` | `max_workers=len(matching_rules)` — bounded, one thread per rule. Result-collection is barrier-via-`as_completed`, no streaming. |
| `subprocess.run` reviewer invocation | `invoke_reviewer` at `runner/skeptic_gate_cli.py:625-634` | blocking, captures full stdout; `timeout=900` default. **No streaming** — diff is delivered via stdin (`-` for codex, `-p -` for gemini). |
| `subprocess.run` for `gh api` / `gh pr diff` | `gh_api` (`runner/skeptic_gate_cli.py:209`), `get_pr_diff` (`runner/skeptic_gate_cli.py:349`) | blocking; `timeout=60` and `timeout=120`. |
| `subprocess.run` for `br show --json` | implicit inside `load_bead_contract_from_bead` (`runner/skeptic_gate.py:691`) | blocking; relies on shell PATH for `br_bin`. |
| `LocalGitScm.get_diff` / `get_changed_files` | `runner/scm_provider.py` (only consumed by fallback at `runner/skeptic_gate_cli.py:877`) | blocking, in-process git subprocess. |
| Sequential phase ordering | `main()` itself (lines 720-1360) | strict linear; `pending → comment → readback → final` is **non-streaming, non-parallel** for safety — every readback must happen synchronously before the final write. |

### Backpressure / cancellation

**None.** `subprocess.run` timeouts are the only cancellation primitive.
There is no signal handler, no shared kill-switch, no `concurrent.futures`
`wait(FIRST_COMPLETED)` partial consumption.

ponytail: stream-vs-sync kept non-streaming intentionally — read-back must
complete synchronously before final status publish; an async-await split here
would re-introduce the post-audit #4953116428 stale-green class of bug.

---

## God-mode paths

### G1. `MOCK_CODEX_RESPONSE` / `MOCK_GEMINI_RESPONSE` env vars

- **Location:** `runner/skeptic_gate_cli.py:574-592` (inside `invoke_reviewer`).
- **Bypass:** short-circuits `invoke_reviewer` BEFORE any subprocess call,
  returns a hand-crafted `PASS` verdict without invoking codex or gemini.
- **Bypassed call set:** `subprocess.run` of `codex` / `gemini` binary,
  `_build_reviewer_cmd` argv construction, `_reviewer_env` sanitization,
  `_extract_codex_message` JSONL destructuring.
- **Defense:** env vars only — controlled by the runner env, not by PR head.
- **Risk:** if any prod path leaks `MOCK_*_RESPONSE=1` into the env, the
  gate passes without ever calling the reviewer. **No log line distinguishes
  mock-hit from real-reviewer run in `evaluate()`'s downstream code path.**

### G2. `MOCK_IMPLEMENTER_IDENTITY` env var

- **Location:** `runner/skeptic_gate_cli.py:384-385` (first two lines of
  `get_implementation_identity`).
- **Bypass:** returns the env value BEFORE any GitHub API call, defeating
  the SHA-bound implementer-detection invariant.
- **Bypassed call set:** `gh_api GET /repos/.../commits/{head_sha}`,
  `gh_api GET /repos/.../pulls/{pr}/commits?per_page=100`,
  `extract_implementation_identity_from_commit`.
- **Defense:** env-only.
- **Risk:** same as G1 — gate can be made to self-PASS by setting
  `MOCK_IMPLEMENTER_IDENTITY=codex` and feeding the same env to both
  reviewer and implementer paths.

### G3. `--dry-run` auto-promotion on API failure

- **Location:** `runner/skeptic_gate_cli.py:887` —
  `args.dry_run = True` is set unconditionally inside the local-git
  fallback branch (after `use_local_git = True`).
- **Bypass:** when `get_pr_head_sha_via_api` raises, the gate flips to
  dry-run WITHOUT consulting the operator — the comment upsert path at
  line 1178 is then SKIPPED (because `if args.dry_run: return 0/1` at
  line 1159 short-circuits). On a transient API outage, **the gate runs
  reviewers against local git but never publishes a verdict**.
- **Bypassed call set:** `set_commit_status`, `post_or_update_comment`,
  `read_back_comment`, the entire readback-and-final-status block.
- **Defense:** the gate DOES still run reviewers; a non-dry-run caller
  who wants the verdict posted must retry. No alarm is raised.
- **Risk:** silent no-op on API outage. Caller workflow sees
  `exit 0`/`exit 1` from `main()` but no comment or status is written,
  leaving merge protection in whatever state it was before.

### G4. `find_existing_bot_comment` pagination loop with `expected_actor` only

- **Location:** `runner/skeptic_gate_cli.py:242-277`.
- **Bypass:** if NO bot-owned marker comment exists, returns `None` →
  `post_or_update_comment` creates a NEW comment (POST). If a bot-owned
  marker comment exists but its body has been edited externally to remove
  the marker, the search misses it → a SECOND bot comment gets created,
  both with the marker. There is no de-dupe step on body content.
- **Bypassed call set:** the implicit idempotency contract — multiple
  comments with the same marker can coexist.
- **Risk:** comments pile up over time; readback verifies the most
  recently published comment, which may not be the one operators expect.

### G5. Inline SHA-equality check inside `get_pr_head_sha_via_api`

- **Location:** `runner/skeptic_gate_cli.py:338-346` is the trusted
  source. `runner/skeptic_gate_cli.py:850-867` (in main) does an
  event-SHA equality check against this value; `runner/skeptic_gate_cli.py:1130`
  re-fetches at pre-publish time.
- **Bypass:** none — this is the canonical SHA authority. But the
  `_worktree_head_sha(ctx.workdir)` call in
  `handler_universal_prompts.py:500` is a DIFFERENT authority for the
  in-process handler path — it reads from local git, NOT from the GitHub
  API. **Two implementations of "authoritative head SHA" exist.**
- **Risk:** the in-process path can bind its verdict to a local-git SHA
  that disagrees with the GitHub API head SHA — readback would still
  verify against the local value but no comment is posted in that path,
  so the disagreement is invisible.

### G6. `invoke_reviewer` re-rewrites the gemini argv

- **Location:** `runner/skeptic_gate_cli.py:598-614`.
- **Bypass:** if the reviewer is `gemini`, `invoke_reviewer` replaces the
  arg list from `_build_reviewer_cmd` with a different argv that calls
  `agy --dangerously-skip-permissions --print <prompt>` instead of the
  `gemini -s --approval-mode default -p -` that `_build_reviewer_cmd`
  returned. If the reviewer is `codex`, it strips `--sandbox read-only`
  from the argv that `_build_reviewer_cmd` returned.
- **Bypassed call set:** `_build_reviewer_cmd` is called and its return
  value is partially or fully discarded. The documented sandbox posture
  (`--sandbox=read-only`, `agy --dangerously-skip-permissions`) is NOT
  what actually runs.
- **Defense:** the docstring on `invoke_reviewer` (lines 553-572)
  describes the unsanitized posture; the re-write at 598-614 is a
  silent override.
- **Risk:** **HIGH.** The two reviewer binaries (`codex`, `agy`) are
  invoked with weaker isolation than `_build_reviewer_cmd` documents,
  and the prompt is delivered via argv (not stdin) for `agy`,
  re-introducing E2BIG risk on large diffs. This is the largest
  god-mode divergence in the file.

### G7. `args.dry_run = True` mutation side-effect

- **Location:** `runner/skeptic_gate_cli.py:887`.
- **Bypass:** mutates the parsed args namespace from inside the
  local-git fallback branch. Subsequent code reads `args.dry_run` and
  behaves as if the operator invoked `--dry-run`.
- **Bypassed call set:** the entire publish+readback+final-status block
  (lines 1146-1343).
- **Risk:** see G3.

### G8. `_reviewer_env` allow-list gaps

- **Location:** `runner/skeptic_gate_cli.py:161-192` + `REVIEWER_SECRET_ENV_DENY`
  (lines 91-123) + `REVIEWER_ENV_BASE_ALLOWLIST` (lines 129-143).
- **Bypass:** env vars not in the deny list AND not in the base allow-list
  AND not in the per-reviewer provider allow-list are silently DROPPED.
  This is intentional (deny-by-default), but `LD_LIBRARY_PATH` is in the
  base allow-list per the comment at lines 137-142 — explicit carve-out
  that may surprise future maintainers.
- **Risk:** a future env var added to the parent env that the reviewer
  process needs (e.g. `PYTHONHOME`) will be silently stripped.

---

## Sub-phase boundary recommendation (authorities angle)

The main() flow today mixes 11 sub-phases inline. The cleanest split, given
the authorities above, is **one function per sub-phase, all returning a
result object, with `_perf_start`/`repo`/`head_sha` flowing through a
shared `RunContext`**:

| # | Sub-phase | Current owner | Authority kept |
| --- | --- | --- | --- |
| 1 | CLI parse + mutual-exclusion | `main()` lines 720-831 | unchanged (argparse) |
| 2 | PR + SHA + diff + implementer resolution | lines 833-944 | unchanged (single owner: `get_pr_head_sha_via_api` + `get_pr_diff`) |
| 3 | Bead contract load (single canonical owner) | lines 957-1000 **plus the duplicate at 1041-1095** | deduplicate to one `load_contract(args)` |
| 4 | Rule load + synthetic fallback (single canonical owner) | lines 1003-1039 + `handler_universal_prompts.py:525-536` | deduplicate to one `load_or_default_rules(args, reviewers)` |
| 5 | Rule dispatch | `dispatcher.dispatch` | unchanged |
| 6 | Aggregate + FakeAggregate wrapper | `consensus.compile_report` + `FakeAggregate` at 1118 | unchanged |
| 7 | Pre-publish SHA re-check | lines 1129-1137 | unchanged |
| 8 | Status-publish state machine | lines 1146-1343 | extract to `class StatusPublish` with `pending()`, `readback()`, `final()` |
| 9 | Perf-log emission (every return) | inline `_emit_perf_log` | unchanged |
| 10 | Dry-run promotion on API failure | line 887 | move to `run_resolve_pr_diff()` with explicit `dry_run_forced` flag (god-mode path flagged) |
| 11 | `_force_failure_status` escape hatch | line 1363 | unchanged |

The **two duplicative copies** (C1: contract load; C3: synthetic rule) are
the highest-leverage cleanups — one guard at the shared function eliminates
two writers and removes the `C1` god-mode duplicate (the second contract
load at line 1041-1095 is currently a shadow writer that re-runs the same
fail-closed paths as the first copy).

The **`invoke_reviewer` argv rewrite (G6)** is the largest god-mode
divergence and must be flagged for the plan sub-agent: either restore
`_build_reviewer_cmd`'s contract or document the override in
`_build_reviewer_cmd` itself.

---

explore written: .dark-factory/explore-authorities.md