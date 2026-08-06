# skeptic_gate_cli main() split — risks & invariants

> Phase: explore / risks angle
> Source: `runner/skeptic_gate_cli.py:720` (`main`) + dispatcher + workflow yml
> Goal of the parent task: define exact sub-phase boundary first.
> This doc lists edge cases, races, persistence hazards, and invariants
> the design must preserve BEFORE planning how to split `main()`.
> Every invariant names the assertion or test (or notes "no check").

Headline finding while reading: `runner/skeptic_gate_cli.py:720 main()`
already has TWO local-git fallbacks, two contract-load blocks, and one
mid-run flag flip. Any split that does not first settle those into
clean transitions will propagate those cracks into every sub-function.
The split design therefore needs to start from the desired state
machine — not from current `main()` line numbers.

---

## 1. Edge cases (input shapes, env conditions, race windows)

### 1.1 `args.pr_number` missing ⇒ silent fallback
`main` (`runner/skeptic_gate_cli.py:848`) checks `if args.pr_number:` then
else branch sets `use_local_git = True` (`L874`). When `--pr-number 0`
(not a real PR) is passed, the script silently runs a local-git HEAD
review and silently flips `args.dry_run = True` (`L887`). There is NO
test in `tests/test_skeptic_gate.py` or
`tests/test_skeptic_gate_cli_contract_echo.py` that exercises
`args.pr_number = 0` (grep `test_cli_uses_local_git|test_cli_falls_back`
returns nothing). If a worker dispatches with a missing/invalid PR
number, the gate never publishes, but the workflow still records
"success" because `return 0 if aggregate.check_state == "success" else 1`
(`L1160`) is gated by `args.dry_run` only.
**Test gap: no check.** Worktree de-facto behavior is "never publishes".

### 1.2 API fails before SHA equality check ⇒ silent local-git
`runner/skeptic_gate_cli.py:869-872`: any exception inside
`get_pr_head_sha_via_api` or `get_pr_diff` flips to local-git mode AND
`args.dry_run = True` on `L887`. The script never records or reports
that mode flip to the operator other than a stderr line. Workflows
running this CLI in CI cannot tell whether they got a real review or a
local-only one. Risk: a flaky day ships a "PASS" status when the gate
reviewed `HEAD` of the runner checkout, not the PR head.
**Invariant proposed:** local-git mode must surface as a distinct
return code (e.g. `3`) so CI can branch — no current test enforces this.

### 1.3 Duplicate reviewer identities
`_parse_reviewers` (`runner/skeptic_gate_cli.py:661`) rejects duplicates
and subsets via the `MANDATORY_REVIEWERS` set at `L658`. Test:
`tests/test_skeptic_gate.py:1267 test_adversarial_cli_rejects_duplicate_reviewer_json`
and `:1275 test_adversarial_cli_reviewers_default_is_distinct`. OK.
**But:** the dispatcher (`runner/dispatcher.py:79-104`) does not
re-check what `_parse_reviewers` already enforced. If a future caller
uses the dispatcher without `_parse_reviewers`, the gate can be
downgraded silently. The split should preserve the
"`_parse_reviewers` runs once at boot" invariant and treat the
dispatcher as a downstream actor that trusts it.

### 1.4 `MOCK_*_RESPONSE` env vars
`invoke_reviewer` (`runner/skeptic_gate_cli.py:553-606`) checks
`MOCK_CODEX_RESPONSE` and `MOCK_GEMINI_RESPONSE`. Both paths produce a
stdout that always succeeds (`VERDICT: PASS`). Risk: a workflow step
that inherits a polluted env from a prior job may run the gate in
mock-mode for one PR and post a real "PASS" comment in production.
**No test asserts mock-mode must be off in production.** Splitting
main() should add a hard fail-closed if `MOCK_*_RESPONSE` is set
without an explicit `--allow-mock` flag.

### 1.5 `--gemini-bin` resolves to `agy`
`invoke_reviewer` (`runner/skeptic_gate_cli.py:598-607`) overrides the
gated "sandbox" command with the call
`gemini_bin or "agy" --model $MODEL --dangerously-skip-permissions
--print <prompt>` — i.e. `agy` runs UNSANDBOXED with prompt-injected
content from the PR diff. Defenses:
- `_reviewer_env` (`L161`) strips `GITHUB_TOKEN`, `HOME` (see `L94`).
- The prompt itself is built from a deterministic prefix-match identity.
That said, `--dangerously-skip-permissions` removes ANY tool-level
gate. Risk: a prompt-injected comment in the PR body could instruct
agy to invoke `gh pr edit` or similar — actually a NO-OP because agy's
token is stripped, but a tool call out to any non-`gh` API could leak
the diff content. **No test asserts that agy has no outbound network
ACL.** Patch-trap: see §5.3.

### 1.6 Exit-code semantics
`main()` returns: `0` (PASS), `1` (FAIL/non-PASS), `2` (gate-precondition
failure — SHA mismatch, contract load fail, head changed mid-run).
The workflow yml at `.github/workflows/skeptic-gate.yml:382` runs as
`python -m runner.skeptic_gate_cli ...` with no explicit `if: failure()`
branch — any exit ≠ 0 fails the workflow. Tests:
- `tests/test_skeptic_gate.py:974 test_cli_forced_fail_with_missing_reviewer`
  asserts rc=1.
- `tests/test_skeptic_gate.py:936 test_cli_forced_pass_with_both_reviewers`
  asserts rc=0.
- `tests/test_skeptic_gate_cli_contract_echo.py:234 test_cli_fails_closed_on_missing_contract_file`
  asserts rc=2 (after the bead-id branch).
**Invariant needed:** exit code 2 must mean "precondition failed" and
the dispatch orchestrator (read: dark-factory daemon) must treat 2 as
"stalled / reroll" not "voted NO" — `df/daemon` has separate logic for
that. Current code returns 2 on a *subset* of precondition failures; the
split should enumerate the full set and not silently add new return
paths.

### 1.7 Repeated invocation against same SHA ⇒ comment "owned by bot" is sticky
`post_or_update_comment` (`runner/skeptic_gate_cli.py:280`) +
`find_existing_bot_comment` (`L242`) + the `expected_actor` filter
(`L269`) means that a re-run *after* a previous verdict will update the
existing bot-owned marker comment. The 6-field read-back (`L1208-1243`)
then asserts the comment's body equals what was just written, so an
edited prior comment that the read-back fetched instead of the post
would fail. **Risk:** if `comment_id` (returned by `gh api POST`) is
different from `id` returned by a subsequent `GET` (GitHub occasionally
serves a stale body during eventual consistency), the read-back is
racy. No current check enforces "fetch the same body twice → must
agree"; the test simply asserts the post-then-get round trip.
**Invariant proposed:** the read-back must additionally re-fetch a
second time and confirm equality, OR fall back to `pending`-status
state for that SHA before publishing `success`.

### 1.8 `MARKER` literal in caller-controlled text
`_extract_field` (`runner/skeptic_gate_cli.py:1497-1532`) raises
`ValueError` if a field appears >1 times — but only at PARSE time, not
at format time. `format_comment` lives in `runner/skeptic_gate.py`; if
a future caller passes a `reason` string containing `**VERDICT: PASS**`
verbatim, the read-back fails. Test:
`tests/test_skeptic_gate.py:2047 test_format_comment_sanitizes_reason_canonical_field_injection`
checks that. The patch-trap risk: a new "informative" caller adds a
field that contains a substring matching an existing pattern.

### 1.9 `aggregate.check_state = "failure"` when `aggregate.verdict = "PASS"`
`runner/skeptic_gate_cli.py:1118-1126 FakeAggregate` maps:
`check_state = "success" if verdict == "PASS" else "failure"`. If a
reviewer's `check_state` was `"failure"` (e.g. execution-evidence field
missing per `runner/skeptic_gate.py:1206`) but `verdict=None`, the
aggregator (`runner/consensus.py:11-13`) treats that as `has_invalid =
True` and returns `None` (never `PASS`). The CLI then forces
`check_state="failure"`. OK. **But:** `compile_report` uses
`bound[0].parsed.head_sha` (`runner/consensus.py:55`) for the comment
embedded SHA. If the bound list is empty (no rule produced a parsed
verdict), `bound[0]` would IndexError. Tests cover the happy path; the
`has_invalid` branch where every rule produced a failure result is
implicitly trusted to have at least one `bound`. Worth a defensive
test before splitting.

### 1.10 Comment-format mismatch if `implementation_identity == "unknown"`
`runner/skeptic_gate_cli.py:937` returns `"unknown"` on no commit
prefix; `verify_provenance` (`runner/skeptic_gate.py:1390`) refuses any
reviewer whose IDENTITY equals `"unknown"`. That means a PR with a
`gemini/...` subject reviewed by `codex` should pass, but a PR with a
**buggy commit subject** reviewed by both codex + gemini fails closed —
the bug sits in the commit subject, NOT the gate, but the gate is the
one that surfaces it. Document as an invariant, not a risk.

---

## 2. Persistence risks

### 2.1 Perf log dir under `/tmp` is banned, sub-paths are allowed
`_emit_perf_log` (`runner/skeptic_gate_cli.py:1398-1440`) refuses bare
`/tmp` but allows nested paths (test:
`tests/test_skeptic_gate.py:2076 test_cli_exposes_perf_log_args`).
**Risk:** if the test harness uses a top-level `/tmp/dark-factory` root
at any point (a one-line typo), the gate silently drops all perf logs.
A split MUST keep this guard at the call site of any new sub-phase,
not move it inside the helper's caller.

### 2.2 Diff-capture side effects when fallback ran
When the API path fails and `use_local_git` succeeds
(`L869-887`), the size check at `L892` runs but `_publish_failure` at
`L909` is skipped because `args.dry_run` was just set to True. So a
diff-too-large failure in offline mode is LOUD on stderr but never
publishes. **Invariant:** if the CLI fell back to local-git because
the API was unreachable, the commit status MUST be set to `failure`
even in offline mode — otherwise merge protection is bypassed for the
entire PR. No current test enforces this. Patch-trap: see §5.1.

### 2.3 The pre-publish head re-check returns 2 with NO status write
`runner/skeptic_gate_cli.py:1128-1137`: if the API head changed
mid-run, the script returns 2 and prints to stderr. **No
`_force_failure_status` is called.** Result: the `pending` status
written at `L1165` stays on the previous head SHA, and merge protection
that requires `skeptic=success` (or anything other than `pending`)
gets either `pending` (treated as in-flight) OR — if a second run
overwrites with `pending` on the NEW head — nothing about the OLD head.
Test: `tests/test_skeptic_gate.py:1836 test_status_overwritten_failure_never_becomes_success`
does NOT exercise this case (no `mid_run_head_change` test exists for
SHA drift between `pending` and final write).
**Invariant needed:** mid-run SHA drift must overwrite `pending` to
`failure` on the *original* SHA so any orchestrator polling the prior
SHA observes the rejection.

### 2.4 Two comment load paths leave different fingerprints
`find_existing_bot_comment` returns the comment ID for the most recent
bot-owned marker. If two bot processes (a parallel CI re-run, a
manually-invoked `gh api`) post marker comments concurrently, only one
will be the canonical one read by the next run. The orchestrator does
not have a single-writer lock. Test
`tests/test_skeptic_gate.py:1957 test_find_existing_bot_comment_filters_by_actor`
only checks single-process behavior. **Invariant:** the comment upsert
must be idempotent at the GitHub layer (POST 422 → PATCH fallback).

### 2.5 Final-success write failure path silently returns 1 (no audit trail in comment)
`runner/skeptic_gate_cli.py:1305-1343` handles a final-write failure by
retrying once with `state="failure"` and then returning 1. The comment
body that was just published (still containing the original PASS body)
is NOT updated to reflect the failure. Result: the bot comment says
"VERDICT: PASS" but the commit status says "failure" — divergent state.
Test: `tests/test_skeptic_gate.py:1758
test_status_readback_mismatch_overwrites_to_failure` covers status
read-back but NOT final-write failure. **Invariant needed:** if the
final-write retry also fails, the bot comment must be PATCHed to a
"VERDICT: FAIL" body with reason "status write failed; do not merge"
BEFORE returning 1, OR a tracker issue must be opened.

### 2.6 Contract load can race with bead mutation
`load_bead_contract_from_bead` (delegated from `runner/skeptic_gate.py`
— not in this file) shells out to `br show --json $BEAD`. If the bead
is mutated between the call and the dispatch, the contract used by
the reviewer differs from the contract used by the gate. **Test gap:**
no current test injects a "bead-mutated-mid-run" failure mode. Patch-
trap: see §5.2.

---

## 3. Concurrency risks

### 3.1 ThreadPoolExecutor `max_workers = len(matching_rules)`
`runner/dispatcher.py:164` spawns one worker per matching rule. Each
worker calls `subprocess.run(codex/gemini)` with `timeout=900`. With
the default `["*"]` glob from `runner/skeptic_gate_cli.py:1032`,
exactly N=2 workers (codex + gemini) run in parallel. **Risk:** a
slow codex blocks one worker but does not block the gemini worker — OK.
**But:** if `--reviewers-json` were ever extended to add a 3rd
reviewer, the dispatcher fans out 3-way and each subprocess holds the
GH rate-limit budget. No current rate-limit guard exists in the
dispatcher; PR #281 round 2 reportedly found a related concern. Split
the dispatcher (and main()) in a way that allows a rate-limit guard to
be added later without restructuring the spawn pool.

### 3.2 Thread-safety of `_emit_perf_log`
`_emit_perf_log` (`runner/skeptic_gate_cli.py:1398`) opens the
`perf-log-dir/skeptic-gate.jsonl` file with `path.open("a")` and writes
one line. If two `main()` instances run concurrently (parallel CI
matrix), both can append interleaved — but Python's `open("a")` is
POSIX-atomic at the kernel level for writes below `PIPE_BUF` (4096
bytes on Linux). A perf log line is well below that. **Risk: low but
not zero on macOS** where `PIPE_BUF` semantics differ. The
`SkepticResult` JSONL line could grow past 4 KB if `reason` fields
expanded. **Test gap:** no concurrent-write test.

### 3.3 Subprocess env mutation side-effects
`_reviewer_env` (`runner/skeptic_gate_cli.py:161-192`) is a pure
function — it builds a new dict and never mutates `parent_env`. The
caller at `L618-624` calls `subprocess.run(..., env=env, ...)` and
Python does not inherit unset env vars when `env=` is supplied — so
**all env vars not in the merged dict are dropped.** This is a feature
not a bug, but if a future reviewer needs (say) `NO_COLOR` for terser
output, the absence is silent. **Risk:** silent capability loss for
the reviewer. Splitting should keep `_reviewer_env` as the single
source of truth for what's passed to ANY subprocess, including future
helpers.

### 3.4 Race between API head re-check (L1129) and comment publish (L1178)
`runner/skeptic_gate_cli.py:1129-1137` re-resolves `api_head_2`. If a
push happens between line 1129 and line 1178, the comment is posted on
the new SHA, but the read-back at `L1192` checks against the OLD `head_sha`
recorded earlier. Mismatch → `_force_failure_status` on the OLD SHA. But
the comment itself is on the NEW SHA. **Invariant:** the comment must be
checked against `api_head_2` not the initial `head_sha`. No current
test exercises "two pushes during the run."

### 3.5 `_perf_start` is monotonic but `_emit_perf_log` is not thread-safe in dispatcher
In the dispatcher's parallel fan-out, each rule's `run_one` runs to
completion in one worker. But the dispatcher itself returns BEFORE
`_emit_perf_log` runs (main() calls it after `dispatch()`). So timing
captures correctly. **However**, the per-rule `print` to stderr at
`L1106` can interleave with the aggregator's print at `L1139`. Cosmetic
only.

### 3.6 Read-back comment body parse is single-pass
`_extract_field` uses `pat.findall` to count occurrences. **Race
window:** if the same run posts a comment twice (GitHub occasional
201→200 retry returning a different `id`), the read-back fetches
either copy. The 6-field equality check at `L1225-1234` would treat
identical bodies as success — but if one copy truncates the
`extra_reviewer_lines` (consensus.py:41-45), the verifier still passes
because those fields are not in the 6-field set.
**Invariant:** when `aggregate.verdict == "PASS"`, the read-back must
also confirm `body_verdict == "PASS"` (already done at `L1231`) and
that the body contains the substring passed by `format_comment`. Check
parses OK today but a future field-added patch could break it.

### 3.7 `expected_reviewer = aggregate.reviewer or "(aggregate)"`
`runner/skeptic_gate_cli.py:1224`: when the consensus aggregator
returns `verdict=None` because every rule failed (consensus.py:18),
`aggregate.reviewer = "(aggregate)"` (set in `FakeAggregate` at
`L1120`). The read-back verifies this matches the body, where
`format_comment` receives `reviewer="(aggregate)"` from
`consensus.compile_report` (L33). OK. But — under a future patch where
consensus sets `reviewer="codex+gemini"`, the read-back mismatch is
silent. **Invariant:** the FakeAggregate's `reviewer` field and the
verifier's `expected_reviewer` must be derived from the same source.

---

## 4. Invariants the design must preserve

For each: name, current code anchor, current test (or "no check").

| # | Invariant | Anchor | Current check |
|---|---|---|---|
| I1 | Both reviewers MUST run | `_parse_reviewers` (L658-712) + `_build_reviewer_cmd` (L455) | `tests/test_skeptic_gate.py:499 test_parse_reviewers_rejects_only_codex_or_only_gemini` |
| I2 | Diff > 1 MiB MUST fail-closed | `MAX_DIFF_BYTES` (L76), `get_pr_diff` (L349-380), main L892-918 | `tests/test_skeptic_gate.py:1343 test_adversarial_diff_oversize_fails_closed` |
| I3 | API head SHA MUST equal event SHA | main L848-867 (steps 1+9) | Indirect: `tests/test_skeptic_gate.py:222 test_bind_to_pr_rejects_stale_sha` checks evaluate() only |
| I4 | HEAD cannot change mid-run without committing `failure` status | main L1128-1137 | **No dedicated test** — covered indirectly by `tests/test_skeptic_gate.py:1758` which uses diff SHAs only |
| I5 | Reviewer's IDENTITY must equal its CLI | `bind_reviewer_identity` (skeptic_gate.py) called from dispatcher L119 | `tests/test_skeptic_gate.py:1136 test_adversarial_bind_reviewer_identity_codex_must_declare_codex` (and 3 siblings) |
| I6 | Implementation identity cannot equal reviewer identity | `verify_provenance` called from dispatcher L141 | `tests/test_skeptic_gate.py:288 test_verify_provenance_rejects_self_review_claude_codex` |
| I7 | Comment body must contain exactly six required fields | `format_comment` + `_extract_field` (L1497) | `tests/test_skeptic_gate.py:609 test_verify_published_comment_accepts_correct_readback` |
| I8 | Read-back must reject mismatched actor/SHA/repo/PR/verdict/reviewer/provenance on any of the 6 fields | `ReadBackCheck` + `verify_published_comment` | `tests/test_skeptic_gate.py:632 test_verify_published_comment_rejects_wrong_actor` (and 5 siblings) |
| I9 | Comment upsert must filter non-bot marker comments | `find_existing_bot_comment` (L242-277) | `tests/test_skeptic_gate.py:1957 test_find_existing_bot_comment_filters_by_actor` |
| I10 | Mandatory commit status order: `pending` first, `failure` on every read-back fail, `success` last | main L1162-1343 | `tests/test_skeptic_gate.py:1665 test_status_publish_order_pending_then_success` |
| I11 | Contract load failure (`--contract-file`, `--bead-id`) MUST exit 2 | main L958-1000 | `tests/test_skeptic_gate_cli_contract_echo.py:234 test_cli_fails_closed_on_missing_contract_file` and 4 siblings |
| I12 | `--bead-id` and `--contract-file` are mutually exclusive | main L825-830 | `tests/test_skeptic_gate_cli_bead_id_r3.py:209 test_bead_id_and_contract_file_mutually_exclusive` |
| I13 | Reviewer env MUST strip `GITHUB_TOKEN`, `HOME`, etc. | `_reviewer_env` (L161-192) + `REVIEWER_SECRET_ENV_DENY` (L91-123) | `tests/test_skeptic_gate.py:730 test_reviewer_env_strips_secrets` |
| I14 | Per-reviewer credentials only (`codex` → OpenAI, `gemini` → Google) | `_reviewer_env` provider allowlist (L182) | `tests/test_skeptic_gate.py:771 test_reviewer_env_isolates_per_reviewer_credentials` |
| I15 | Mock reviewer responses MUST NOT post to GitHub | `MOCK_CODEX_RESPONSE` / `MOCK_GEMINI_RESPONSE` early-return | **No test asserts mock must not post in production** |
| I16 | Performance log dir MUST NOT be bare `/tmp` | `_emit_perf_log` L1417-1424 | `tests/test_skeptic_gate.py:2076 test_cli_exposes_perf_log_args` (path-coverage only) |
| I17 | Every reviewer subprocess MUST have a timeout | `invoke_reviewer` timeout kwarg (L559) | `tests/test_skeptic_gate.py:848 test_invoke_reviewer_nonzero_exit_returns_error` |
| I18 | `MOCK_IMPLEMENTER_IDENTITY` env var respected (test seam) | `get_implementation_identity` L384-386 | Used by tests; no dedicated test |
| I19 | Fallback to local-git MUST publish `failure` status, not silently downgrade | main L887 (currently does NOT do this — see §5.1) | **No test** |
| I20 | Two pushes during the run MUST leave a `failure` status on the ORIGINAL SHA | main L1128-1137 (currently does NOT call `_force_failure_status` — see §2.3) | **No test** |
| I21 | `compile_report` body must use bound SHA when bound is non-empty | `runner/consensus.py:55` | Implicit; no dedicated test |
| I22 | PR_NUMBER must be threaded to `_publish_failure` (not parsed from description) | `_publish_failure` (L1443) + main L909-918 | `tests/test_skeptic_gate.py:1906 test_publish_failure_threads_pr_number_not_zero` |
| I23 | A reviewer claiming `TEST_RUN_EVIDENCE` with `failed > 0` MUST be rejected | `parse_verdict` (skeptic_gate.py) | `tests/test_skeptic_gate.py:2279 test_parse_verdict_rejects_test_run_evidence_when_tests_failed` |
| I24 | `LINT_RUN_EVIDENCE` with `errors > 0` MUST be rejected | `parse_verdict` | `tests/test_skeptic_gate.py:2292 test_parse_verdict_rejects_lint_run_evidence_with_errors` |
| I25 | The dispatcher MUST reject duplicate reviewer identities if `_parse_reviewers` was somehow bypassed | dispatcher (currently does NOT re-check) | **Test gap** |
| I26 | The reviewer env MUST include `LD_LIBRARY_PATH` (libpython on self-hosted CI runners) | `_reviewer_env` + `REVIEWER_ENV_BASE_ALLOWLIST` L137-142 | Not in code; documented in memory `project_2026-07-18_libpython_dt_needed_double_load.md` |
| I27 | Final-write `success` MUST be preceded by read-back equality on comment AND status | main L1245-1290 | `tests/test_skeptic_gate.py:1665 test_status_publish_order_pending_then_success` |
| I28 | Run on `MOCK_*_RESPONSE` MUST short-circuit before `set_commit_status` | main L1159 returns early only when `args.dry_run` | **No test** — depends on operator not combining mocks with `not dry_run` |

---

## 5. Patch-trap warnings (places where the simple fix entrenches a wart)

### 5.1 "Just split `main` along comment markers" preserves a wart: `use_local_git` flips `dry_run` mid-run
**Trap:** a naive refactor that lifts the existing 11 numbered comment
sections (`# ---- 0. Mutual-exclusivity check ----` through `# ---- 11.
Final write ---- `) into separate functions preserves the `args.dry_run
= True` flip on line 887 AND the double contract-load block (L946-1000
vs L1041-1095) AND the silent API-failure fallback. The split would
"work" but every future caller of those helpers would inherit the
local-git + always-dry-run policy.
**Why this is a trap:** the reviewer CRITICAL finding on PR #281 was
"context-flips during a single run." A cosmetic refactor that preserves
the flip preserves the bug. The split MUST instead define the state
machine (`mode ∈ {offline, online}`, `preconditions_ok: bool`,
`ready_to_publish: bool`) and have `main()` be a thin orchestrator that
calls pure functions:
```
mode = resolve_mode(args, env, gh_subprocess_ok)  # raises on ambiguous
diff, head_sha = (offline | online)_capture_diff(args)
contract = load_contract_if_requested(args)
results = dispatch(rules, diff, head_sha, contract)
aggregate = aggregate_results(results)
publish_or_dry(aggregate, mode, args)
```

### 5.2 "Just move bead/contract-load earlier to consolidate two blocks" 
**Trap:** the contract-load block appears at L957-1000 AND L1052-1095.
The simple fix is to delete the second block. But the second block runs
AFTER `RuleLoader.load_rules()` and AFTER `LocalGitScm.get_changed_files`
(L1015). If a delete-the-duplicate patch lands, the dry-run/perf-log
emission for the rules layer will now skip the contract load on the
fast-fail path (L958-977 was inside `_perf_start` window; L1052 is too).
**Why this matters:** a discovery showing the two blocks are genuinely
identical or genuinely identical-but-buggy-in-one should be settled by
DIFF-ing each under a single test matrix, not by hunting the duplicate
by eye. The split should re-evaluate both with
`tests/test_skeptic_gate_cli_contract_echo.py` as the ground truth and
re-derive the contract-load phase as a single function called once.

### 5.3 "Just sandbox agy via `--sandbox` instead of `--dangerously-skip-permissions`"
`runner/skeptic_gate_cli.py:603` calls agy with
`--dangerously-skip-permissions --print <prompt>`. The "obvious" fix
would be to switch to a sandboxed agy invocation. But `agy` is the
`worldarchitect.ai` CLI; its permissions flag is different from
gemini's. Removing `--dangerously-skip-permissions` may force every
review into an interactive approval prompt that hangs the subprocess.
**The correct fix** is to add the same `--sandbox` arg agy supports
(if it does; cf memory `feedback_2026-08-05_cold_reviewer_chain_state.md`)
or to swap agy for the cloud-build parallel box documented in
`project_2026-07-24_jsby_cloud_dispatch.md`. A surgical patch that
deletes `--dangerously-skip-permissions` without verifying agy's
interactive mode breaks the entire gate.

### 5.4 "Add a global mutex around `_emit_perf_log`" 
**Trap:** the perf log is append-only JSONL. Two writers contend on the
same line. A `threading.Lock` around `_emit_perf_log` would serialize
all concurrent skeptic-gate runs on the same machine. The right
sub-phase is "perf-log append" inside a host-local fcntl lock OR (better)
move perf logs to per-run paths (`<perf-log-dir>/skeptic-gate-<sha>.jsonl`).
The lock-wart: any caller that runs N gates in parallel now serializes
on a host-wide lock. The per-sha-file fix is 4 lines.

### 5.5 "Move all side effects into one `_publish()` helper" 
**Trap:** `set_commit_status` is called 5× (L1165 `pending`,
`_force_failure_status` L1188, L1201, L1237, L1265, L1284, plus
`_publish_failure`'s call inside). A "consolidate" PR will write a
single helper that does set-commit-status-then-comment-then-readback.
But the comment body and status context differ between PASS and FAIL;
the read-back path differs by what fields it verifies. The wart would
be re-introducing the `success`-before-readback ordering bug fixed in
round 3 (post-audit 4953116428). Preserve the 4-step ordering
explicitly:
1. `pending` (status only)
2. comment upsert
3. read-back (comment + status)
4. `success` (status only) — LAST

### 5.6 "Refactor `FakeAggregate` into a real `Aggregate` class"
The `class FakeAggregate` defined at `runner/skeptic_gate_cli.py:1118`
inside `main()` looks like a code smell. A "clean-up" PR would lift
it to module scope and rename. **Trap:** the class name says
`FakeAggregate` but the verifier (`verify_published_comment`) treats
its `verdict` and `reviewer` as the source of truth for the read-back.
If lifted without renaming, a future caller could think
`SkepticResult` (from `runner/skeptic_gate.py:255`) is the right
abstraction and end up confusing the two. Right move: replace
`FakeAggregate` with a `ConsensusAggregate` dataclass that wraps a
`SkepticResult` AND a body string, with the same fields the verifier
reads. Don't just rename — reify.

### 5.7 "Promote `FakeAggregate.verdict = "PASS"` to the real aggregator"
`runner/consensus.py:9`: when `results` is empty, `aggregate()` returns
`"PASS"`. That's a fail-OPEN. The CLI then posts a `VERDICT: PASS`
comment. Test: `tests/test_skeptic_gate.py:425 test_aggregate_results_both_pass_yields_success`
and `:455 test_aggregate_results_empty_list_yields_failure` — the empty-
list case is in the legacy `aggregate_results` (`skeptic_gate.py`) which
DOES return `FAIL`, while `consensus.py:9` returns `"PASS"`. **Two
aggregators disagree.** The split MUST use ONE aggregator — and it
must be fail-closed (empty list → FAIL).

### 5.8 "Add `--trusted-code-sha` enforcement" 
The flag is declared at L762-766 but only the read-flow checks it (if
at all). Without a pre-run `git rev-parse --verify $REF == $sha` gate
on the *workflow's* ref (separate from the PR head SHA), an attacker
who controls a fork could pin a stale/immutable code SHA. Look at
memory `pr285_gqsj_blocked` for a recent attempt. The wart: a one-line
check inside `main()` that runs AFTER the diff capture means the diff
is captured against un-trusted code first. Move the check to the
top, before any side effect.

### 5.9 "Use argparse mutually-exclusive group with dest=contract_source"
The current implementation uses a manual `if args.bead_id and
args.contract_file` after parse (L825-830). Argparse supports
`add_mutually_exclusive_group()` natively. A refactor PR would use the
group. **Trap:** `argparse.error()` raises SystemExit which is
untracked — neither `_emit_perf_log` nor `_force_failure_status` runs
in that path. The current code does the same (also raises from
`parser.error`), but a future PR adding `--contract-stdin` to the
group would inadvertently route a contract load through stdin while
still treating the check as fail-closed at the argv layer. Move the
check to a dedicated function so the perf-log path still fires.

---

## 6. Open risks (cannot evaluate without more info)

### O1. `worldarchitect.ai` agy invocation shape
`runner/skeptic_gate_cli.py:599-607` invokes agy directly with
`--dangerously-skip-permissions --print <prompt>`. The reviewer
process boundary, exit code behavior, and partial-output handling are
implementation-specific to agy's current version. The split design
should not assume agy will continue to support `--print` (memory
`feedback_2026-08-05_cold_reviewer_chain_state.md` notes gemini CLI
EOLed; agy may follow). **Risk:** a sub-phase that holds the agy
argv shape in stone will need to be re-split when agy changes.

### O2. `runner.scm_provider.LocalGitScm` API stability
The local-git fallback (`L876-887`) shell-outs to `git merge-base` and
`git diff --name-only` via `LocalGitScm` (`runner/scm_provider.py`).
The `_resolve_head` at `scm_provider.py:23-41` searches for refs in a
fixed order. None of those refs (`origin/pr/<num>`, `pr/<num>`,
`private/cb-pr<num>`, `private/cb-skeptic`) are documented; only the
tests at `tests/test_scm_provider.py` exercise it. **Risk:** if a
worktree does not have any of those refs and is not at HEAD, the
fallback's diff is whatever `git rev-parse HEAD` returns. The split
cannot make this safer without re-architecting `scm_provider`.

### O3. Daemon-level reroll interaction
The skeptic gate does not call `gh` for "is this PR still relevant"
queries; the daemon (cf `project_2026-08-05_jleechan_8mlh_done.md`)
issues reroll commands when `skeptic` flips red. **Risk:** the gate
flipping red on the *contract-load* path (exit 2, no status set) does
NOT reach the daemon because the daemon polls `skeptic` status, not
exit codes. Splitting main() should ensure contract-load failures also
emit `failure` to the status context, otherwise reroll never fires.

### O4. Dispatcher error path does not publish
`runner/dispatcher.py:170-178`: if `run_one` raises, a
`SkepticResult(check_state="failure", ...)` is appended but no status
write or comment upsert happens — control returns to main() which
proceeds to `compile_report` and the normal publish path. **Risk:**
the dispatch error is silent on the GitHub side if the post-dispatch
publish path itself succeeds for the OTHER results. Compounded by I25
above. Cannot fully evaluate without running with a forced dispatcher
fault injection (no current test does this).

### O5. Multi-PR concurrency on the same SHA via reroll
If a reroll hits the same SHA twice in quick succession (daemon issue
PR #383), two `main()` runs would both post `pending` then race on the
comment upsert and final write. Current invariant I10 does not cover
this. Patch-trap risk: a "naive retry" added later would entrench
this race.

### O6. Whether `_publish_failure`'s comment upsert should be `dry_run`-gated on size-fail
`runner/skeptic_gate_cli.py:908`: `_publish_failure` is wrapped in
`if not args.dry_run`. But `_publish_failure` is also called from the
bead-contract-fail path (L958-977) which does NOT respect dry_run.
Need to verify the bead-contract-fail path always explicitly emits
`failure` status. (Tests `tests/test_skeptic_gate_cli_contract_echo.py:234`+
exercise only the dry_run=True case; need a non-dry_run contract-fail
test before splitting.)

---

## 7. Summary — what the split MUST do first

In priority order:

1. Define the 5-phase state machine:
   `BOOT → CONTEXT (offline/online) → REVIEW (rules dispatch) →
    CONSENSUS → PUBLISH (or DRY)`.
2. Pick ONE aggregator (`runner/skeptic_gate.aggregate_results` is the
   fail-closed one; `runner/consensus.aggregate` is fail-OPEN at
   empty-results — see §5.7). The split must commit to one.
3. Verify I19 (local-git MUST publish `failure`) and I20 (mid-run
   SHA drift MUST publish `failure` on original SHA) with new tests
   BEFORE the refactor PRs.
4. Lift `FakeAggregate` to a real `ConsensusAggregate` dataclass (§5.6).
5. Settle the duplicate bead-contract-load block (§5.2) by running both
   against the existing contract_echo test matrix, then commit.
6. Apply patch-trap warnings: don't promote the agy-flag-rotation,
   don't ADD a global lock, don't refactor `use_local_git` into a
   helper that preserves `args.dry_run = True`, don't move all side
   effects into one `_publish()` without preserving the 4-step ordering.
7. Document I25 (dispatcher MUST re-check reviewer identity uniqueness)
   and I28 (mock-mode MUST short-circuit before any publish) as new
   invariants.

Open risks O1-O6 should each get a follow-up bead before the split
lands; the explore phase cannot resolve them with current evidence.

---

explore written: .dark-factory/explore-risks.md
