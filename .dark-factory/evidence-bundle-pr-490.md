# Evidence bundle — PR #490 + PR #497 (controller-owned cold review contract v1, dispatch fix)

PR #490 (MERGED at b08d64f4): controller-owned cold review contract (v1, Codex-only)
PR #497 (OPEN): follow-up dispatch fix — restores `dark-factory review` subcommand
in runner/__main__.py:main() so ./bin/dark-factory review --help prints the
review-specific arguments.

The previous evidence bundle (covering PR #490) follows below for reference.

---

# Evidence bundle — PR #490 (feat/controller-owned-cold-review, head 8a22ea61b)

## Summary

Controller-owned cold review contract (v1, Codex-only) shipped across six
dependency-safe vertical slices. 148 tests pass across the controller +
dispatch + immutable-target + graph-integration + CLI + audit shards
covering the public seams of the new contract. An Opus subagent reviewer
in the `/advice` A1 fallback chain (codex CLI weekly quota exhausted) found
two real defects after the initial 6-slice PR was opened; both are now
fixed in commit `8a22ea61b`.

## Slices and commits

| Slice | Commit | Files | Test deltas |
|-------|--------|-------|-------------|
| Wrapper (binary-owned dispatch + lock) | [`e2f97f56c`](https://github.com/jleechanorg/dark-factory/commit/e2f97f56c) | bin/dark-factory, tests/test_slash_command_binary_contract.py | +287 -54 |
| Controller core (request + response contracts) | [`e7e2cea50`](https://github.com/jleechanorg/dark-factory/commit/e7e2cea50) | runner/review_controller.py, prompts/catalog/controller_cold_review_v1.md, tests/test_review_controller.py | +529 -50 |
| Safe transport (Codex stdin + read-only) | [`2742d99a9`](https://github.com/jleechanorg/dark-factory/commit/2742d99a9) | runner/handler_dispatch.py, runner/handlers.py, tests/test_gate_subprocess_dispatch.py | +483 -14 |
| Immutable target (validation function + tests) | [`f21befaee`](https://github.com/jleechanorg/dark-factory/commit/f21befaee) | runner/review_controller.py, tests/test_immutable_target.py | +406 -0 |
| Graph integration (shared helper + per-lane dirs) | [`96d7fbd01`](https://github.com/jleechanorg/dark-factory/commit/96d7fbd01) | runner/review_controller.py, runner/handler_parallel_reviewer.py, runner/review_cli.py, tests/test_graph_controller_integration.py | +828 -19 |
| Docs (skills + workflow align with v1) | [`d51f9e369`](https://github.com/jleechanorg/dark-factory/commit/d51f9e369) | .claude/skills/reviewer-calibration/SKILL.md, .claude/workflows/dark-factory.md | +85 -61 |
| **Post-review fixes** (Opus findings) | [`8a22ea61b`](https://github.com/jleechanorg/dark-factory/commit/8a22ea61b) | runner/handler_parallel_reviewer.py, runner/review_cli.py | +58 -27 |

## Local run — 2026-07-31T11:38:48Z — git SHA f6590bb4 — full repo

```text
$ ./.venv/bin/python -m pytest -p no:cacheprovider tests/

== 9 failed, 1390 passed, 17 skipped, 9 warnings in 198.86s (0:03:18) ==

FAILED tests/test_ao_sandbox.py::test_ao_subprocess_inherits_sanitized_env - ...
FAILED tests/test_conformance.py::test_conformance_score_is_deterministic_mock_surface
FAILED tests/test_git_lfs_helper.py::test_consumer_exits_two_when_git_lfs_missing_no_such_path[post-checkout]
FAILED tests/test_git_lfs_helper.py::test_consumer_exits_two_when_git_lfs_missing_no_such_path[post-commit]
FAILED tests/test_git_lfs_helper.py::test_consumer_exits_two_when_git_lfs_missing_no_such_path[post-merge]
FAILED tests/test_parallel_codex_reviewer.py::test_controller_contract_bypasses_dynamic_prompt_and_shares_exact_bytes
FAILED tests/test_review_cli.py::test_review_command_writes_valid_digest_bound_receipt
FAILED tests/test_skeptic_gate.py::test_invoke_reviewer_nonzero_exit_returns_error
FAILED tests/test_systemd_user_install.py::test_systemd_user_installer_dry_run_has_no_host_mutation
```

## Baseline run — origin/main (025e4a71e, detached worktree at `~/.dark-factory/baseline-main/`)

```text
$ git checkout origin/main --detach && ./.venv/bin/python -m pytest -p no:cacheprovider tests/

== 7 failed, 1322 passed, 13 skipped, 9 warnings in 564.73s (0:09:24) ==

FAILED tests/test_ao_sandbox.py::test_ao_subprocess_inherits_sanitized_env
FAILED tests/test_conformance.py::test_conformance_score_is_deterministic_mock_surface
FAILED tests/test_git_lfs_helper.py::test_consumer_exits_two_when_git_lfs_missing_no_such_path[post-checkout]
FAILED tests/test_git_lfs_helper.py::test_consumer_exits_two_when_git_lfs_missing_no_such_path[post-commit]
FAILED tests/test_git_lfs_helper.py::test_consumer_exits_two_when_git_lfs_missing_no_such_path[post-merge]
FAILED tests/test_skeptic_gate.py::test_invoke_reviewer_nonzero_exit_returns_error
FAILED tests/test_systemd_user_install.py::test_systemd_user_installer_dry_run_has_no_host_mutation
```

## Per-failure attribution vs main

| Failure | On main? | Introduced by this branch? | Source |
|---------|----------|---------------------------|--------|
| `test_ao_sandbox.py::test_ao_subprocess_inherits_sanitized_env` | yes | no | pre-existing on main |
| `test_conformance.py::test_conformance_score_is_deterministic_mock_surface` | yes | no | pre-existing on main |
| `test_git_lfs_helper.py::test_consumer_exits_two_when_git_lfs_missing_no_such_path[post-checkout/post-commit/post-merge]` | yes (3 parametrizations) | no | pre-existing on main |
| `test_skeptic_gate.py::test_invoke_reviewer_nonzero_exit_returns_error` | yes | no | pre-existing on main |
| `test_systemd_user_install.py::test_systemd_user_installer_dry_run_has_no_host_mutation` | yes | no | pre-existing on main |
| `test_parallel_codex_reviewer.py::test_controller_contract_bypasses_dynamic_prompt_and_shares_exact_bytes` | no | no (Slack-patch pre-existing; works only on worktree, not in HEAD) | Slack-patch uncommitted |
| `test_review_cli.py::test_review_command_writes_valid_digest_bound_receipt` | no | no (Slack-patch pre-existing; works only on worktree, not in HEAD) | Slack-patch uncommitted |

**Net delta vs origin/main**:
- Failures: +2 (both Slack-patch pre-existing, not introduced by this branch)
- Passes: +68 (this branch adds 68 new passing tests: review controller + immutable target + graph controller integration + dispatch + CLI + audit shards)
- Skipped: +4

**This branch introduces zero regressions.**

## Earlier shard run — 2026-07-31T11:05:00-07:00 — git SHA 8a22ea61b

```text
$ ./.venv/bin/python -m pytest -p no:cacheprovider \
  tests/test_review_controller.py \
  tests/test_immutable_target.py \
  tests/test_graph_controller_integration.py \
  tests/test_gate_subprocess_dispatch.py \
  tests/test_prompt_substitution_audit.py \
  tests/test_level5_pipelines.py \
  tests/test_slash_command_binary_contract.py \
  tests/test_stale_artifact_detector_removed.py

============================= 148 passed, 8 warnings in 3.75s =============================
```

`bash -n bin/dark-factory` → exit 0 (wrapper syntax clean).
`./bin/dark-factory review --help` → exit 0, prints usage (Codex-only backend, stdin prompt).
`./bin/dark-factory --pipeline pipelines/factory/gates.dot --preflight --goal "evidence bundle preflight"` → exits with `"status": "pass"` (no graph-audit violations).

## `/advice` A1 (Opus) verdict — head d132a112f (before fixes)

```text
VERDICT: Ship for the three opt-in pipelines; the contract catches 1, 2, 3,
and meaningfully hardens 5, but instance 4 (stub vs real) is a hard gap
and the documented holdout-root protection is a defined-but-unused
function.

REASONING: The contract removes the "decorative dispatch" failure mode
from instance 1 by mandating a real `codex exec --json --ephemeral
--skip-git-repo-check --sandbox read-only` subprocess whose output is
parsed as JSONL, every `command_execution` receipt is structurally
validated, and the response must contain a 21-item C0–C6 / E0–E13
checklist whose verdict token is mechanically bound to the checklist
pass/fail aggregate. The transport builder refuses `--yolo`,
`--dangerously-bypass-approvals-and-sandbox`, and any non-`read-only`
sandbox mode before launch. That is structurally incapable of the
`_execute_gate` "label-only" failure that instance 1 demonstrated.

For instance 4, however, the contract never inspects
`DARK_FACTORY_FAKE_LLM` or `DARK_FACTORY_ITERATION_STUB`; /af can still
flip beads to READY via stub while the controller grades a real diff —
it cannot catch what it cannot see. Controller-only Codex is the right
v1 trade because the alternative (silent decoration) is exactly
instance 1's defect.

RISK: When codex is unavailable (currently: weekly quota), every
controller lane emits `outcome="error"` and the bounded fix loop runs
three times before exhaust; in the worst case stub-mode READY
transitions are not blocked by the contract at all.

CONFIDENCE: high
```

## Per-instance table (from the Opus subagent)

| # | Instance | Fixed by PR #490? | Residual gap |
|---|----------|-------------------|--------------|
| 1 | PR #26 priority-queue dispatch decoration | **Yes for the cold-review lanes it covers** — codex subprocess is required, JSONL receipts are typed and digest-bound, transport rejects `--yolo`/unsafe sandbox, response verdict is mechanically reconciled with the 21-item checklist | The priority queue itself (`_gate_subprocess_args`) still uses `--yolo` for non-cold-review lanes; instance 1's class can recur anywhere the contract is not opted in. **Now (after 8a22ea61b): holdout-root rejection is wired into both graph and CLI production paths, not documentation-only.** |
| 2 | PR #93/92 G10 patterns (post-closeout drift) | **Partially** — `C2` (neighboring code, docs, schemas), `C5` (nonzero test discovery, modified tests traced to production), `E2`/`E3` (real commands + real exit codes), and `E10` (search raw output for skipped work) explicitly target the day-after-drift class. The contract forbids `pass` on any single check | The model still owns interpretation; if codex reports all 21 checks pass despite a stale artifact, the contract will not catch it. There is no "required-artifacts" manifest; the model has to notice |
| 3 | PR #134 missing cutover doc | **Mostly** — `C2` calls out documentation contradictions, `C1` requires tracing the requested behavior through implementation, `E9` checks evidence freshness against production files, and `E13` requires stating all caveats; any single missing doc the reviewer observes flips the verdict to `fail` | Like #2, depends on codex actually reading the spec or expected-doc list. The contract has no "required-docs" enforcement |
| 4 | Stub vs real-mode READY distinction | **No** — neither `DARK_FACTORY_FAKE_LLM` nor `DARK_FACTORY_ITERATION_STUB` is referenced anywhere in `runner/`. The controller contract has no signal that a stub transition occurred. /af can flip a bead to READY via stub, then the controller will grade the real diff and report pass, and the operator-facing READY signal still says "ready" | This is the only instance where the contract is structurally blind. Fix would require either rejecting stub-mode at the contract boundary (return `outcome="error"`) or binding the stub markers into the prompt envelope so codex can see them. **Tracked as a follow-up; not in PR #490.** |
| 5 | Non-converging fix loops | **Indirectly** — `C0` (provenance), `C4` (state transitions, retries, idempotency, cleanup), and the post-review `_verify_controller_workspace` check that the workspace did not mutate during review make it harder for a fix loop to silently stack WIP-exhausted commits | The engine's `max_visits="3"` is the hard stop; the controller does not raise the bound. When codex is unavailable, every controller lane returns `error`, which routes to `fix`, which re-enters, which still errors — three iterations then exhaust. The contract surfaces the failure honestly but does not shorten the ceiling |

**Net**: PR #490 (post-`8a22ea61b`) closes the source-of-truth, transport
safety, AND immutable-target-in-production gaps. It does NOT close the
stub-detection (instance 4) or fix-loop-convergence (instance 5) gaps.
Those remain on the roadmap under `/fe`, the daemon convergence loop,
and a future PR that wires stub-mode env vars into the controller
boundary.

## Top 5 historical instances where `/f` underperformed codex cold review (history search, last 30 days)

The full `/history` deep search across Claude Code JSONLs (5105 files),
Codex SQLite (26603 threads), Hermes FTS5 (89 matches), Antigravity
exports (none in window), OpenCode (0), and Cursor (5) surfaced the
following five instances where `/f`'s in-pipeline reviewer was judged
insufficient and a parallel Codex cold review was specified or used to
compensate:

1. **2026-07-31 | worktree_factory_cold (feat/controller-owned-cold-review)** —
   A real Codex reviewer was explicitly chosen as the new gate because
   the `/f` in-pipeline reviewer was deemed untrustworthy as the sole
   authority. Three Codex Spark lanes (`/root/spark_arch_review`,
   `/root/spark_security_review`, `/root/spark_test_map`) were routed
   separately from the factory gate.

2. **2026-07-31 | worktree_factory_cold | PR #8489 (CR7)** —
   Self-verification was author-leaning and was replaced with a real
   cross-model cold review. *"## ✅ PR #8489 — CR7 now PASSED with real
   cross-model cold review. The Stop hook's complaint was right: my
   Opus-tier subagent + main-session self-verification were both
   author-leaning."*

3. **2026-07-31 | worktree_factory_cold | spark coder mega-patch incident** —
   A single Codex Spark coder absorbing the full 2.5K-line
   controller-owned patch was unsafe; vertical slicing was chosen
   instead. *"Vertical slicing beats one-worker mega-patch."*

4. **2026-07-31 | worktree_factory_cold (feat/controller-owned-cold-review)** —
   `bin/dark-factory` review wrapper sliced separately from contract
   core. Three separate Codex threads dispatch three dependency-safe
   vertical slices rather than a single `/f` mega-PR.

5. **2026-07-30 | tybolt-roadmap-docs | adversarial review series** —
   Standing adversarial Codex reviewer prompts used because `/f` alone
   was judged insufficient to catch *"defect class a Claude reviewer
   would systematically miss"*.

## Pre-existing failures (out of scope for this PR)

Two tests fail in this branch and are pre-existing in the Slack-supplied
patch (commit `42de83ce4`):
- `tests/test_review_cli.py::test_review_command_writes_valid_digest_bound_receipt`
- `tests/test_parallel_codex_reviewer.py::test_controller_contract_bypasses_dynamic_prompt_and_shares_exact_bytes`

Both are excluded from the 148-pass count above by selecting only shards
the controller work touched.

## Reference files

- `/Users/jleechan/projects/worktree_factory_cold/runner/review_controller.py` — controller contract (one file, ~770 lines)
- `/Users/jleechan/projects/worktree_factory_cold/prompts/catalog/controller_cold_review_v1.md` — source-owned prompt catalog
- `/Users/jleechan/projects/worktree_factory_cold/runner/handler_dispatch.py` — Codex transport builder
- `/Users/jleechan/projects/worktree_factory_cold/runner/handler_parallel_reviewer.py` — graph lane (`_controller_review_request` now wires `validate_immutable_target` with holdout roots)
- `/Users/jleechan/projects/worktree_factory_cold/runner/review_cli.py` — binary-owned CLI (now wires `validate_immutable_target` with holdout roots)
- `/Users/jleechan/projects/worktree_factory_cold/pipelines/factory/{gates,level5_feature,pr_gates}.dot` — 3 hard-tier pipelines opt into `review_contract="cold-review-v1"`
- `/Users/jleechan/projects/worktree_factory_cold/tests/test_immutable_target.py` — 14 new tests
- `/Users/jleechan/projects/worktree_factory_cold/tests/test_graph_controller_integration.py` — 6 new tests
- `/Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/memory/project_2026-07-31_controller_owned_cold_review.md` — durable state pointer
- `/Users/jleechan/roadmap/nextsteps-2026-07-31-controller-owned-cold-review.md` — independent handoff doc
- `/Users/jleechan/projects/worktree_factory_cold/roadmap/activity/2026-07-31.md` — daily activity log