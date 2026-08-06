# Reuse & centralization — skeptic_gate_cli.main() split

**Sub-phase boundary:** `main()` is 640+ lines of an 11-step orchestrator.
It already delegates most non-orchestration work to helpers; the goal of
the split is to leave `main()` as the dispatcher and lift each numbered
step into a small typed function. This doc surfaces (a) the existing
helpers each step must reuse, (b) one centralization win for the new
package layout, and (c) the false friends.

Ponytail ladder rung definitions:

- **Rung 2** = already in this tree (`runner/`, `daemon/`, `scripts/`).
- **Rung 3** = Python stdlib (`argparse`, `dataclasses`, `pathlib`,
  `re`, `subprocess`, `concurrent.futures`, `contextlib`).
- **Rung 4** = platform / shell tooling (`gh`, `git`, `br`, `agy`).
- **Rung 5** = already-installed PyPI / vendored dep (`PyYAML`,
  no others in scope). Plan agent owns new-dep decisions.
- **Rung 6** = "can this be one line?" — if yes, the candidate
  replaces a whole module.

---

## 1. Boundary map (the 11 numbered steps in `main()`)

| Doc-step | Lines (skeptic_gate_cli.py) | Helper that owns it now | New sub-function name (target) |
|---|---|---|---|
| 0. argparse mutual-exclusivity | 820-830 | `argparse` (stdlib, rung 3) | `_validate_argparse(args)` |
| 1. Resolve PR + SHA (API equality) | 833-887 | `get_pr_head_sha_via_api`, `LocalGitScm` | `_resolve_pr_and_sha(args, env)` |
| 2. Diff capture (`gh pr diff`) | 889-919 | `get_pr_diff` | `_capture_diff(...)` |
| 3. Implementation identity | 920-944 | `extract_implementation_identity_from_commit`, `get_implementation_identity` | `_derive_implementation_identity(...)` |
| 3a. Load bead contract (--bead-id / --contract-file) | 946-1000 | `load_bead_contract`, `load_bead_contract_from_bead` | `_load_contract(args)` |
| 4. Load rules, build prompt, dispatch reviewers, aggregate | 1002-1126 | `RuleLoader`, `VerifierDispatcher`, `ConsensusAggregator`, `LocalGitScm` | `_run_verification_pipeline(...)` |
| 5-7. Per-reviewer binding, provenance | inside dispatcher 118-160 | `bind_reviewer_identity`, `verify_provenance` (skeptic_gate.py:1338, 1366) | no new code, dispatcher already wired |
| 8. Pre-publish API head re-check | 1128-1137 | `get_pr_head_sha_via_api` | `_pre_publish_sha_check(repo, pr, head_sha)` |
| 9. Side effects — pending status, upsert comment, readback, final status | 1146-1360 | `set_commit_status`, `post_or_update_comment`, `read_back_comment`, `verify_published_comment`, `_force_failure_status` | `_publish_results(...)` |
| 10. Perf log | 1351-1359, 1398-1440 | `_emit_perf_log` (local one-line JSONL) | `_emit_perf_log(...)` (replace with `runner.perf_log.open_run`/`close_run`) |
| 11. argv / `--help` / exit code | 817 + 1541-1542 | `argparse` | stays inline |

The **sub-phase boundary first**: `main()` becomes a 30-line dispatcher
that calls `_resolve_pr_and_sha → _capture_diff → _derive_implementation_identity
→ _load_contract → _run_verification_pipeline → _pre_publish_sha_check
→ _publish_results → _emit_perf_log`, with one `try/except` wrapper
per step using `pathlib`-style fail-closed returns. No new domain
logic — every step already has a helper.

---

## 2. Reuse candidates (the in-tree rung-2 wins)

### 2.1 The verdict pipeline is already a class

- `runner/dispatcher.py:20` — `VerifierDispatcher.dispatch(rules,
  changed_files, diff, repo, pr_number, head_sha, base_sha,
  implementation_identity, contract)` already:
  - filters rules via `rule_matches` (dispatcher.py:40),
  - builds per-rule prompts via `build_prompt` (skeptic_gate_cli.py:53,
    re-exported from skeptic_gate.py:2260),
  - calls `_cli.invoke_reviewer` (skeptic_gate_cli.py:553),
  - calls `_cli.evaluate` (skeptic_gate_cli.py:1540 / skeptic_gate.py:1540),
  - runs `bind_reviewer_identity` (dispatcher.py:119),
  - runs `verify_provenance` (dispatcher.py:141),
  - writes the FAIL comment body via `format_comment` (dispatcher.py:121).
- `runner/consensus.py:23` — `ConsensusAggregator.compile_report(...)` is
  the **authoritative** verdict aggregator. `main()` re-implements it
  with a `class FakeAggregate` (skeptic_gate_cli.py:1118-1126). The
  FakeAdapter is a wart: it bolts SkepticResult-shaped attributes onto
  an aggregator result that already produced a verdict + comment body.
  **Replace with `ConsensusAggregator().compile_report(...)` directly —
  no wrapper class needed.** This is the single biggest rung-6 win.

### 2.2 The diff / head-SHA resolvers are already there

- `runner/scm_provider.py:18` — `LocalGitScm.get_diff(target)` and
  `get_changed_files(target)` are exactly the two local-git helpers
  `main()` reimplements inline at lines 877-886 (`subprocess.run(["git",
  "rev-parse", "HEAD"])`) and 1014-1020 (`scm_cf.get_changed_files(...)`).
- `runner/skeptic_gate_cli.py:209` — `gh_api(method, path, *, body)` is
  the only gh-API wrapper. `main()` correctly delegates to it everywhere
  but the **regex field extractors** (`_extract_field`, `_extract_int`,
  skeptic_gate_cli.py:1497-1538). The extractors were widened to 6 fields
  (post-audit 4953116428); a regex dict-in-`get` branch on the field name
  is the rung-6 one-liner, but a `runner/contract_fields.py` helper that
  registers one regex per canonical field would eliminate the
  string-typed-name indirection the splitter would otherwise carry.

### 2.3 Performance-logging already lives in `runner/perf_log.py`

- `runner/perf_log.py:154` — `open_run(...)`, `runner/perf_log.py:305` —
  `close_run(...)`, plus `PerfRun` / `GitContext` at lines 25-47. These
  are the runner's per-run perf-log API used by `runner/__main__.py:418`.
  The skeptic CLI **duplicates this with `_emit_perf_log`** at
  skeptic_gate_cli.py:1398-1440, writing a different JSONL shape
  (`skeptic-gate.jsonl` vs `runner/perf_log.py:115`'s
  `_write_json(perf, record)`). The split must replace `_emit_perf_log`
  with `open_run` + `close_run` from `runner/perf_log.py` so the gate
  emits the same record shape as the rest of the runner — and stops
  having its own opinionated `--perf-log-dir` default. **Centralization
  proposal: `_emit_perf_log` is removed; the splitter calls
  `perf_log.open_run`/`close_run` like `__main__.py` does.**

### 2.4 Env sanitization already partial

- `runner/skeptic_gate_cli.py:161` — `_reviewer_env(parent_env, reviewer)`
  is the only env sanitizer. There is no rung-2 helper to factor this
  out — every other reviewer-invocation path (e.g.
  `runner/handler_parallel_reviewer.py`) builds its own env from a
  different allow-list. The split should **keep `_reviewer_env` where
  it is** for the gate (it has the per-provider credentials for codex /
  gemini, post-audit 4953116428) and **NOT** generalize it across the
  runner; the allow-list semantics differ (see anti-reuse traps, §4.1).

### 2.5 Bead contract loading and parser already exist

- `runner/skeptic_gate.py:544` — `load_bead_contract(source)`.
- `runner/skeptic_gate.py:691` — `load_bead_contract_from_bead(bead_id,
  br_bin)`.
- `runner/skeptic_gate.py:909` — `evaluate_contract_echo(report,
  contract)` is the r3 fail-closed contract-echo evaluator, and
  `dispatcher.py:79` already threads `contract=` through.

The CLI's `--bead-id` / `--contract-file` mutual-exclusivity check
(skeptic_gate_cli.py:825-830) is a copy/paste of the same check that
should live once inside the dispatcher. The splitter should delete the
copy and let `_run_verification_pipeline(args)` carry the contract
selection forward.

### 2.6 The whole bot-actor constant is local

- `EXPECTED_BOT_ACTOR = "github-actions[bot]"` at skeptic_gate_cli.py:84
  is referenced 5 times in this file alone (lines 84, 243, 281, 1190,
  1326, 1450). No other module in `runner/` references it. **Move it
  into the new sub-module** the split creates
  (e.g. `runner/skeptic_gate_io.py`) so the splitter doesn't carry
  the surface verbatim.

---

## 3. Centralization proposal (where the single authority lives)

Two candidates dominate — every other choice is local to the split.

### 3.1 `runner/skeptic_gate_io.py` — the GitHub-API I/O surface

Move the entire side-effect surface into a new module:

```
runner/skeptic_gate_io.py
   EXPECTED_BOT_ACTOR                # the constant
   MARKER                            # re-export from skeptic_gate
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
   read_back_status(repo, sha, context) -> Optional[str]   # NEW (rung-6)
   MAX_DIFF_BYTES
```

**Why this is the centralization win.** `skeptic_gate_cli.py` is
already an I/O orchestrator; the new sub-function
`_publish_results(...)` does **nothing but wire these helpers in fail-
closed order**. Splitting the I/O out:

- makes `_publish_results` testable with mocked `gh_api`,
- lets `post_or_update_comment` be reused if any other gate-level
  workflow ever needs an upsert-bot-comment helper (currently it
  doesn't — but the surface is generic enough to share),
- eliminates the duplicate `set_commit_status` / comment calls between
  `_publish_failure` (skeptic_gate_cli.py:1443) and the inline happy-
  path (skeptic_gate_cli.py:1165-1184).

Also add `read_back_status(repo, sha, context)` as a one-liner over
`gh_api(... + "/commits/{sha}/statuses")` — `main()` does this
inline at lines 1248-1254 and again at lines 1320-1330 (failure
retry). Centralize it.

### 3.2 `runner/skeptic_gate_pipeline.py` — the verification core

Move the "dispatch + consensus + binding" composition into a thin
orchestrator function:

```
runner/skeptic_gate_pipeline.py
   def run_verification(
       reviewers: list[tuple[str, str]],
       repo: str, pr_number: int, head_sha: str, base_sha: str,
       diff: str, implementation_identity: str,
       contract: Optional[BeadContract],
       rules: list[Rule] | None,           # None → use mandatory fallback
       changed_files: list[str],
   ) -> SkepticAggregate                  # tuple of (verdict, body, results)
```

This wraps `VerifierDispatcher.dispatch(...)` + `ConsensusAggregator()
.compile_report(...)` and is the only thing `main()`'s step 4 needs
to call. The `FakeAggregate` class becomes obsolete.

### 3.3 Migration notes

Legacy surfaces that become projections only:

| Old call site | New owner |
|---|---|
| skeptic_gate_cli.py:1118 `class FakeAggregate` | deleted; `ConsensusAggregator.compile_report` is the truth |
| skeptic_gate_cli.py:1398 `_emit_perf_log` | deleted; `runner.perf_log.open_run/close_run` |
| skeptic_gate_cli.py:1003-1011 inline RuleLoader/global_dir/local_dir resolution | moved into `skeptic_gate_pipeline.run_verification` so the CLI no longer owns the path |
| skeptic_gate_cli.py:825-830 inline argparse mutual-exclusivity | argparse `add_mutually_exclusive_group` (rung 3 / 6 — one-liner) |
| skeptic_gate_cli.py:1014-1020 inline `LocalGitScm(...)` for changed_files | moved into the pipeline step (the only consumer) |

The CLI itself becomes a thin shell: parse args, call
`run_verification`, wire the publish pipeline, emit perf-log, return
0/1/2.

### 3.4 Tests that pin the new boundaries

- `tests/test_skeptic_gate.py` — already hits `skeptic_gate.py`
  (the parser), no change.
- `tests/test_skeptic_gate_cli_bead_id_r3.py` and
  `tests/test_skeptic_gate_cli_contract_echo.py` — `monkeypatch.setattr(
  cli_mod, "invoke_reviewer", ...)` reaches into `dispatcher.py`'s
  `_cli.invoke_reviewer` lookup; the split **must keep
  `runner.skeptic_gate_cli.invoke_reviewer` as the public import path**
  (dispatcher.py:7-9 does the late-bound lookup exactly so these
  tests can patch).
- `tests/test_dispatcher.py`, `tests/test_consensus.py` — already
  exercise the pipeline as data; the split moves wire-up only, not
  algorithm.

### 3.5 The seven-rung ladder per step

| Step | Rung 2 (extend) | Rung 3 (stdlib) | Rung 4 (platform) | Rung 6 (one-line?) |
|---|---|---|---|---|
| 0. argparse | — | `argparse.add_mutually_exclusive_group` | — | yes — 3 lines |
| 1. PR + SHA | `get_pr_head_sha_via_api`, `LocalGitScm` | — | `gh api … /pulls/{n}`, `git rev-parse` | `get_pr_head_sha_via_api(repo, args.pr_number)` |
| 2. Diff capture | `get_pr_diff` | `subprocess.run(timeout=...)` | `gh pr diff --repo` | `_capture_diff = get_pr_diff` |
| 3. Identity | `extract_implementation_identity_from_commit` | — | `git log -1 --format=%s`, `gh api …/commits/{sha}` | yes — pure function |
| 3a. Contract | `load_bead_contract`, `load_bead_contract_from_bead` | `pathlib.Path` | `br show --json <bead>` | yes — two branches, no inline copy |
| 4. Pipeline | `RuleLoader`, `VerifierDispatcher`, `ConsensusAggregator`, `LocalGitScm` | `concurrent.futures` (already in dispatcher) | — | yes — `run_verification(...)` is the whole step |
| 5-7. Binding | `bind_reviewer_identity`, `verify_provenance` | — | — | yes — already centralized in dispatcher.py:118-160 |
| 8. Pre-publish SHA | `get_pr_head_sha_via_api` | — | `gh api …/pulls/{n}` | yes — wrap with `if api_head.lower() != head_sha.lower(): ...` |
| 9. Publish | `set_commit_status`, `post_or_update_comment`, `read_back_comment`, `verify_published_comment` | — | `gh api …/commits/{sha}/statuses` | yes — `_publish_results(...)` |
| 10. Perf-log | `runner.perf_log.open_run/close_run` | `contextlib.contextmanager`? | — | yes — but **shape change** required |
| 11. Entry | `argparse` | `sys.argv` | — | no change |

---

## 4. Anti-reuse traps (patterns that look reusable but aren't)

### 4.1 `_reviewer_env` / `REVIEWER_ENV_*` sets — DO NOT generalize

These three sets at skeptic_gate_cli.py:91-153 are scoped to the
skeptic-gate reviewer process. CodeRabbit MAJOR finding on PR #281
r2 proves the prior version dumped ANTHROPIC_API_KEY into both
reviewers; the gate intentionally keeps a strictly smaller env than
other runner subprocesses (the gate is the only reviewer whose
sandbox model CAN'T see HOME or SSH_AUTH_SOCK). Generalizing to a
shared `runner/reviewer_env.py` would re-broaden the blast radius
the post-audit 4953064910 narrowed. Keep these local.

### 4.2 `verify_published_comment`'s ReadBackCheck — DO NOT share

`runner/skeptic_gate.py:1986` (`verify_published_comment`) is the
**only** reader of the six field regexes (`_RE_SHA_LINE`, etc.,
skeptic_gate_cli.py:1487-1494). Centralizing the regexes into a
generic "contract-field matcher" would tempt the next reviewer CLI
to write its own verifier on top of it — at which point a regex
change to drop a field would silently break the skeptic-gate's
contract-echo readback. The contract fields are not "fields in a
generic document"; they are **bot-comment invariants**. A
`runner/contract_fields.py` is only safe if it returns the regexes
*and* raises on unknown names — never a generic dict.

### 4.3 `CONSENSUS_VERDICT = "PASS"` when results is empty — DO NOT copy

`runner/consensus.py:24-37` returns `"PASS"` when no rules match.
This is a deliberate "vacuous green" — it exists because the
skeptic-gate test contract for empty-rule scenarios requires it.
A new centralization that moves this into the splitter will lock
the same behavior in two places. The splitter must NOT replicate
this branch; it must keep calling
`ConsensusAggregator.compile_report(...)`.

### 4.4 `EXPECTED_BOT_ACTOR` — DO NOT move to `runner/__init__.py`

There's exactly one producer (the gate bot). Five call sites are
argparse defaults plus three filter callbacks. The string is **not**
domain-bound (`github-actions[bot]` is GitHub-platform truth), but
making it a runner-wide constant invites handlers to filter on it
for their own read-back — and they shouldn't, because non-gate
comments aren't bot-owned markers.

### 4.5 `MOCK_CODEX_RESPONSE` / `MOCK_GEMINI_RESPONSE` branches — DO NOT collapse

The two mock branches at skeptic_gate_cli.py:574-592 are
near-duplicates (text differs in identity only). Generalizing them
to a `_mock_reviewer_output(reviewer, prompt)` helper is **fine**
(rung 6 — one-liner), but the `re.search(...)` triple-OR fallback
to defaults must remain per-mock: a regex miss with no
`MOCK_*_RESPONSE=1` should fall back to the embedded literal, not
to the regex-derived default, because the literal is the contract
the production parser expects. A "smart" mock that picks the
defaults from re.search would silently regress for malformed
prompts.

### 4.6 `_force_failure_status` — DO NOT generalize

skeptic_gate_cli.py:1363-1380 already documents that errors here
are swallowed; that's intentional (fail-closed paths have to be
observable even when status writes themselves are busted). A
shared "force-failure-status" helper across the runner would lose
this property because the next caller would want to raise.

---

## 5. Summary

- **Sub-phase boundary:** 30-line `main()` orchestrator + 7 sub-
  functions aligned to the docstring's 11 steps, plus a thin
  `skeptic_gate_io.py` (I/O) and `skeptic_gate_pipeline.py`
  (dispatch+aggregate). The boundary lines up exactly with the
  existing helpers.
- **Highest-value centralization win:** kill the `FakeAggregate`
  class and replace `_emit_perf_log` with `runner.perf_log`'s
  `open_run`/`close_run`. Those two changes alone delete ~80 lines
  without losing behavior.
- **Second win:** extract `_reviewer_env` + the three reviewer
  env sets into a sibling `skeptic_gate_io.py`, but keep them
  private to the skeptic-gate package (see §4.1).
- **Do not:** generalize the comment-readback verifier, the bot
  actor constant, or `_force_failure_status`.
- **Tests that must keep passing unchanged:**
  `test_skeptic_gate_cli_bead_id_r3.py`,
  `test_skeptic_gate_cli_contract_echo.py`,
  `test_dispatcher.py`, `test_consensus.py`,
  `test_skeptic_gate.py`. They all hit late-bound dispatcher
  imports; the splitter must preserve `runner.skeptic_gate_cli.
  invoke_reviewer` as the public surface.

---

## 6. Rung-5 note

No rung-5 (new dependency) candidate surfaced in this audit. The
existing stack — PyYAML (rule frontmatter), stdlib `argparse` /
`subprocess` / `dataclasses` / `pathlib` / `concurrent.futures`,
and the platform CLIs (`gh`, `git`, `br`, `agy`) — covers every
sub-step the splitter would create. The plan agent does not need a
new dep decision.
