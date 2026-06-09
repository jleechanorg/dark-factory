# Agy reviewer + base dot + adversarial-review priority queue

**Status:** Design / governing doc for PR #22 + the follow-up fix-PR.
**Date:** 2026-06-09
**Owner:** jleechan
**Repo:** `jleechanorg/dark-factory`

## 1. Scope

This document covers three coupled design decisions:

1. The `_base.dot` shared library and the 4-way explore fanout (PR #22).
2. The `bug_fix.dot` red/green TDD lane and its `_red`/`_green` gates (PR #22).
3. The real-LLM evidence reviewer on Antigravity (`agy`) with a claude infra-fallback only — plus a per-PR adversarial-review priority queue with the dark-factory default `codex > minimax > agy > claude-sonnet` (PR #22 + follow-up).

The `claudew`/Wafer/GLM-5.1 backend removal (also in PR #22) is covered in the commit message of `1220ef2083` and is not re-justified here.

## 2. `_base.dot` — 4-way parallel explore fanout

Every feature lane (lanes that have a `plan` and an `implement` step) must `include="@pipelines/_base.dot"` at the graph level. The base provides the explore phase as a `wait_all` 4-way parallel fanout:

```
explore_in -> explore_fanout -> { explore_concept
                                , explore_auth
                                , explore_reuse
                                , explore_risks }
            -> explore_join (policy=wait_all)
            -> explore_stitch (this doc, §2.1)   ← new in fix-PR
            -> explore_out
```

Lanes wire the base into their own topology with two edges:

```
start -> explore_in
explore_out -> <lane-next>     // e.g. plan
```

Exempt lanes (do NOT include this base):

- `pipelines/parallel_demo.dot` — fanout-topology demo
- `pipelines/slim/review_pr.dot` — review-only, no plan/implement

### 2.1 The explore-stitch step (new in fix-PR)

The original `_base.dot` shipped in PR #22 did not include a stitch step, but `prompts/slim/plan.md:6` (unchanged) hard-requires `.dark-factory/explore-findings.md` to exist before `plan` runs. The 4 new sub-agents each write a different partial file (`.dark-factory/explore-{concepts,authorities,reuse,risks}.md`). Without a stitch step, every first run of `pipelines/slim/minimal_feature.dot`, `pipelines/slim/minimal_pr.dot`, and `pipelines/bug_fix.dot` aborts at `plan` with "explore artifact missing — do not invent a design from scratch".

The fix-PR adds:

- `explore_stitch [type="codergen", prompt="@prompts/slim/explore_stitch.md"]` between `explore_join` and `explore_out` in `_base.dot`.
- `prompts/slim/explore_stitch.md` (or repurposed `explore.md`) reads the 4 partials and writes the consolidated `.dark-factory/explore-findings.md`. Strict "do not invent" rule preserved.
- The old `prompts/slim/explore.md` (vestigial; no lane references `prompt="@prompts/slim/explore.md"` anymore) is either deleted or repurposed as the stitch prompt (lowest-LOC path).

Topology after the fix:

```
explore_join -> explore_stitch -> explore_out
```

## 3. `bug_fix.dot` red/green discipline

`pipelines/bug_fix.dot` is the lane the user asked for in the previous turn. The discipline is a real TDD red-then-green sequence, not a paraphrase of the bug description:

1. **`reproduce`** (codergen) — runs the production code, observes the wrong output, writes a fresh failing test, and sets `state.bug_fix.test_path`. **Forbidden from reading the bug report, the issue, prior repros, or any code comment that describes the bug.** Reading those sources and paraphrasing them into a test is *not* red/green — the existing code might already pass such a test.
2. **`gate_red`** — pytest shell-out. **Success** means `rc != 0` (test failed as expected — bug reproduced). **Failure** means `rc == 0` (test passed under current code — bug not reproduced). On `failure`, the pipeline exits early: no fix is attempted because the bug was not reproduced. On `error` (unresolved test path, sandbox missing), the pipeline also exits — distinct outcome class from a real `failure`.
3. **`fix`** (codergen, `max_visits=3`) — TDD oracle against the test; smallest change that turns the test green, no weakening.
4. **`gate_green`** — pytest shell-out. **Success** means `rc == 0`. **Failure** means `rc != 0` — the pipeline routes back to `fix` (up to 3 visits total).

The red/green discipline is enforced by the gates, not by the agent. The reproduce node can pass any test path; the gates verify the direction.

## 4. The reviewer-gate policy (load-bearing)

This is the policy that **must not be violated** by the adversarial-review priority queue. From `feedback_2026-05-31_runner_resilience_reviewer_gates.md` and the dark-factory memory:

> Reviewer nodes must be gates, not chat summaries. For any target repository's brownfield delete/refactor PRs they must write file-backed verdicts and compare target repo, target PR, target_head_sha, base_sha, current-head diff summary, PR description snapshot, evidence paths and evidence SHA, deletion intent, net LOC, and dead-code reachability.

> A real `fail|partial` verdict from one reviewer is **authoritative and never reviewer-shopped onto a second backend.** A factory monitor must pin exact `run_id` and require an exit step. `runs.final` or "latest run" queries are not enough when CXDB is shared across concurrent runs.

PR #22 implements this for `agy`:

- `runner/handlers.py` adds `_is_gate_infra_failure` (missing binary / sandbox / timeout / unparseable output).
- `_resolve_gate_backend` (node `backend=` attr overrides `ctx.backend`).
- `_execute_gate` (run agy once; fall back to claude only on infra failure; never on a real verdict).
- A genuine agy `fail`/`partial` is **kept**, never retried on a different backend.

The 3 agy tests in `tests/test_gates.py` lock this down:

- `test_gate_er_runs_agy_when_backend_agy`
- `test_gate_agy_falls_back_to_claude_on_infra_failure`
- `test_gate_agy_real_fail_verdict_not_retried` ← the no-reviewer-shopping test

## 5. The adversarial-review priority queue (the real LLM review)

### 5.1 What "real LLM review" means

A real adversarial review is one issued by a model from a *different vendor* than the implementing coder, ideally with inverted incentive. The agent-orchestrator skeptic pattern (see `packages/core/src/skeptic-reviewer.ts`) uses a hardcoded fallback chain `["codex", "claude", "gemini", "cursor"]`; the dark-factory default is `["codex", "minimax", "agy", "claude-sonnet"]` to (a) start with the strongest independent reviewer, (b) use minimax (MiniMax / Hermes's `claude` API) as the secondary, (c) use agy (Google Gemini) as the tertiary, (d) fall back to claude-sonnet only when none of the above are installed.

The priority queue is for the **first** adversarial pass — *chosen at run-config time*, not a retry cascade. A real `fail` verdict from one reviewer is kept, not retried on a different model.

### 5.2 Why not a retry cascade

A retry cascade ("agy said fail, try codex; codex said pass, use codex's verdict") violates the no-reviewer-shopping rule. If two reviewers disagree, the resolution is *human review*, not another model roll. The priority queue runs once at run-config time and picks one reviewer for the entire run; the runner pins `run_id` and the reviewer cannot be swapped mid-run.

### 5.3 Design

- `runner/handlers.py` adds `_resolve_adversarial_backend(priority: list[str], ctx) -> str` that returns the first priority entry that is installed and authenticated (probe via `which <binary>` + `--version` shell call), skipping any that are not.
- Default priority list comes from `DARK_FACTORY_ADVERSARIAL_PRIORITY` env var (comma-separated) with the dark-factory default `codex,minimax,agy,claude-sonnet`.
- A new `prefer_adversarial: true` flag on `gate_er`/`gate_es` makes the resolver skip the run-level coder backend to guarantee the reviewer is *different* from the coder.
- `pipelines/slim/review_pr.dot` evidence node gains `backend_priority="codex,minimax,agy,claude-sonnet"` and `prefer_adversarial: true`.

### 5.4 Tests

- `test_adversarial_priority_picks_first_installed`
- `test_adversarial_priority_skips_coder_backend`
- `test_adversarial_priority_env_override_honored`
- `test_adversarial_priority_falls_through_to_claude_sonnet_when_nothing_else`

## 6. Open items

- Per-project override: should `agent-orchestrator.yaml` have a per-project `adversarialPriority`? (Yes, follow-up; not in this fix-PR.)
- The `DARK_FACTORY_PLAN_MODEL` env var (existing bead `jleechan-x57`) — orthogonal to the adversarial queue, separate PR.
- The reviewer file-backed audit (existing bead `jleechan-x33`) — already shipped in PR #22's `gate_er` path; the new priority queue plugs into the same handler.
- Heartbeat for hung runs (existing bead `jleechan-ol7`) — not in scope of the fix-PR; would help detect adversarial reviewer hangs.
