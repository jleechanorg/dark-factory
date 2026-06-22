# Conversation-History Forensics: Dark Factory Run vs. Manual Review Gaps

**Research date:** 2026-06-21
**Sources searched:**
- Claude Code project JSONL: `/Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/*.jsonl` (86,276 lines total across ~50 sessions)
- CXDB: `~/.dark-factory/cxdb.sqlite` (1,275 steps, 2026-05-24 → 2026-06-21)
- Roadmap: `~/roadmap/nextsteps-2026-06-09-dark-factory-pr26-dispatch.md`, `~/roadmap/learnings-2026-06.md`
- Git history: `jleechanorg/dark-factory` commit log
- Hermes: `~/.hermes_prod/state.db` (no dark-factory-specific factory-miss threads found)
- Codex: `~/.codex/state_5.sqlite` (no dark-factory dispatch-miss threads found)

---

## Incident 1 — Coder agent generated `review_pr.dot` with a reviewer label on a `_codergen` node (2026-06-08)

**Source:** Claude Code session `b7941c4a` (2026-06-09T05:34, `/Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/b7941c4a-7304-45fb-b753-9b0ec13f0a81.jsonl`, line 17)

**What the factory run produced:**
A coder agent (invoked via `/fs`) generated a `review_pr.dot` pipeline graph. The graph had a node labeled "cold reviewer" in prose but without `type="gate_er"` set in the DOT attributes. Because the engine resolves untyped, un-shaped nodes to `_codergen` by default, the graph would have dispatched a **second coder pass** to the `evidence` node, not a reviewer.

**What manual review caught:**
The operator noticed the graph looked wrong and questioned it. The model then self-diagnosed the issue (session line 17):

> "I wrote a node, *labeled* it 'cold reviewer' in prose, and then never set `type="gate_er"` (or any reviewer type) on it. With no `type` attribute the engine defaults that node to `_codergen` — so the graph I handed you would have run a second **coder** pass and called it a review. That is exactly the failure mode CLAUDE.md operating-rule #3 exists to prevent."

**Root cause:** The coder agent understood the intent but did not write the DOT attribute that activates it. The mismatch between human-readable node label and machine-readable type is invisible in the graph visualization.

**Fix:** The `evidence` node in `pipelines/slim/review_pr.dot` was converted from a mislabeled codergen to a real `type="gate_er"` reviewer with SHA binding and verdict parsing (commit `1220ef2`, PR [#22](https://github.com/jleechanorg/dark-factory/pull/22)).

**Significance for evolve:** This is a structural gap: the factory's `.dot` authoring contract is enforced only at runtime (the engine resolves type). A schema-level linter or structural preflight that checks every non-`start`/`exit` node for a valid `type` or recognized shape would catch this class of mistake before the pipeline runs.

---

## Incident 2 — Adversarial-review priority queue was decorative: all "codex" reviews silently ran Claude (2026-06-09)

**Source:**
- Commit `81b9ea6` message (`jleechanorg/dark-factory`, 2026-06-09T02:20)
- Roadmap doc: `~/roadmap/nextsteps-2026-06-09-dark-factory-pr26-dispatch.md`, lines 24–29, 66–67
- Bugbot (Cursor) threads 6 and 7 on PR #26

**What the factory run produced:**
All pipeline runs that specified `backend_priority="codex,minimax,agy,claude-sonnet"` and received a `codex`-resolved backend still dispatched the reviewer as Claude (`_execute_gate` only handled `agy` specially and collapsed every other resolved name to the Claude subprocess). `_run_gate_once` hard-coded `reviewer_backend = "claude"` in the CXDB record, hiding the true dispatch. The priority-queue metadata label `reviewer_backend="codex"` was written to CXDB but the actual subprocess invoked was Claude.

**What independent review caught:**
Cursor Bugbot flagged this as a **high-severity bug** (thread 6) on PR #26. The root problem:

- `_execute_gate` only branched on `agy`; all other names fell through to Claude subprocess construction.
- The empty-list fallback in `_resolve_gate_backend` (when `prefer_adversarial` filters out the coder backend and produces an empty list) collapsed silently to `claude-sonnet` instead of re-probing the full default priority queue — eliminating cross-vendor review entirely when the factory's most common backend was Claude (Bugbot thread 7).

**Fix:** Commit `81b9ea6` added `_gate_subprocess_args` (backend-aware argv construction), `_gate_subprocess_env` (ANTHROPIC_BASE_URL override only for minimax), and corrected `_resolve_gate_backend`'s empty-list fallback to re-probe `_DEFAULT_ADVERSARIAL_PRIORITY`. PR #26 merged at `7ef9f91` (2026-06-09T09:51).

**Production verification:** A live `pipelines/slim/review_pr.dot --backend claude` run on main SHA `361f669` confirmed the fix: claude coder (778 s, $22.91) → real codex reviewer (21.7 s, `verdict=fail`). CXDB `metadata_json` showed `reviewer_backend="codex"`, `adversarial_resolved="codex"`, `head_sha_status="matched"`.

**Significance for evolve:** Weeks of pipeline runs had been reporting cross-vendor adversarial review in CXDB metadata while actually using the same Anthropic/Claude backend as the coder. The quality guarantee advertised by the priority queue was not being delivered. The Bugbot catch was accidental — there was no structural test that verified the subprocess argv matched the resolved backend name.

---

## Incident 3 — Level-5 autonomy review (3-agent team) found `bug_fix.dot` shipped code without any adversarial reviewer (2026-06-09)

**Source:**
- Roadmap doc: `~/roadmap/nextsteps-2026-06-09-dark-factory-pr26-dispatch.md`, lines 91–94, 117–118
- GitHub issue [#28](https://github.com/jleechanorg/dark-factory/issues/28) (bead `jleechan-7je`)
- PR [#30](https://github.com/jleechanorg/dark-factory/pull/30) body

**What the factory runs produced:**
All `pipelines/factory/bug_fix.dot` executions completed with `gate_red → fix → gate_green → exit`. The `gate_green` verdict was the final quality gate. No adversarial reviewer node existed in the graph — every merged fix from that pipeline had zero independent review.

**What the independent review caught:**
A 3-subagent team (2× Sonnet, 1× Haiku) audited all 28 runnable `.dot` graphs and the runner against the Level-5 autonomy rubric. The team flagged `bug_fix.dot` as a HIGH-severity gap (bead `jleechan-7je`): bug fixes exit the pipeline with only a self-referential green/red gate; no cross-vendor `gate_er` node exists. This means the factory can merge a fix that passes its own test suite but would be caught by an independent evidence reviewer.

Additionally, the team found (bead `jleechan-4pa`, PR [#29](https://github.com/jleechanorg/dark-factory/issues/29)):
- `_holdout_eval` built `eval_env` from raw `os.environ` — agent-authored seed scripts ran by the evaluator could read `DARK_FACTORY_HOLDOUTS` and copy holdout content into the worktree for the next fix-loop iteration, breaking sealed isolation.
- `_gate_subprocess_env("minimax")` layered `ANTHROPIC_BASE_URL` on raw `os.environ` instead of the sanitized env.

**Fix:** PR [#30](https://github.com/jleechanorg/dark-factory/pull/30) added a `gate_er` node of type `evidence` to `bug_fix.dot` wired between `gate_green` and `exit`, and patched `_holdout_eval` + `_gate_subprocess_env` to use `_sanitized_env()`.

**Significance for evolve:** The factory's own pipeline test suite (which runs graphs in echo-backend mode with seeded state) cannot detect a missing reviewer node — the test passes even with no reviewer because echo-backend paths don't invoke real LLM subprocesses. Only an audit that reads the graph structure independently can catch this category of gap.

---

## Incident 4 — Hermes independently detected `plan.md` had no lane-count guard: the factory would have emitted a 67-lane plan (2026-06-10)

**Source:**
- `/Users/jleechan/projects/dark-factory/PARALLELIZATION_AUDIT_2026-06-10.md` (Hermes-authored, 129 lines)
- PR [#37](https://github.com/jleechanorg/dark-factory/pull/37): `feat(plan): add lane-independence hard requirement to plan.md`

**What the factory produced:**
The `prompts/slim/plan.md` and `prompts/catalog/plan.md` templates, which drive the `plan` codergen node, had only a file-ownership matrix check. No maximum-lane cap, no kill switch, no pre-flight commands, and no anti-pattern examples were present. The same prompt that would produce a correct 3-lane plan would also happily emit a 67-lane plan if the goal suggested enough parallelism.

**What independent review caught:**
After the worldarchitect.ai `lvl:` refactor chain produced 67 PRs, 0 merges, 4 divergent copies of `level_up_session.py`, and ~9h of wall-clock with no convergence — a chain generated *inline*, not via dark-factory — Hermes audited `prompts/slim/plan.md` and found it would produce the same topology given a similar goal (PARALLELIZATION_AUDIT_2026-06-10.md, lines 14–18):

> "the dark-factory `plan.md` prompts had only a file-ownership matrix check, with **no max-lane cap**, **no kill switch**, **no pre-flight commands**, and **no anti-pattern guidance** against the 67-lane topology. The same prompt would have happily emitted a 67-lane plan if asked."

**Fix:** PR [#37](https://github.com/jleechanorg/dark-factory/pull/37) patched both `plan.md` files with five hard sections: max 3-4 lanes per wave, max 2 active writers per file, no pre-emptive PRs gated on not-yet-merged PRs, mandatory global kill switch (idle >20 min with head SHA unmoved → kill), and required pre-flight commands.

**Significance for evolve:** The factory had no guard at the architecture layer. A spec that looked reasonable could still trigger catastrophic parallelism. The prompt as the policy artifact (not the runner code) was the gap, and it took an external audit of a production failure — in a *different* project — to surface it.

---

## Incident 5 — Cold-review session (Codex) confirms dispatch fix correctness but finds 8 pre-existing test failures hidden from the factory's own test run (2026-06-09)

**Source:**
- Claude Code session `5b92e2b9` (2026-06-09, `/Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/5b92e2b9-14a8-440b-afd4-b078d5afb110.jsonl`, lines 4, 193, 228)

**What the factory run produced:**
PR #26 reported "268 passed / 0 failed" in the PR body.

**What the cold-review session found:**
Running the same test suite in the cold-review environment revealed 8 failures:
- 5 conformance/holdout/evidence-bundle tests failed because `$DARK_FACTORY_HOLDOUTS/evaluator/run.py` was absent in that environment.
- 3 `test_ao_sandbox.py` tests failed because `DISABLE_SANDBOX=1` stripped `sandbox-exec` from the spawn, and those tests assert sandbox-exec is present.

The cold reviewer (session line 193) concluded:

> "The 8 pre-existing failures (5 conformance/evidence/holdout + 3 ao_sandbox) are unrelated to the dispatch fix. The PR's claim of '268 passed / 0 failed' was likely on a different environment (e.g. CI with proper sandbox-exec entitlements + holdouts repo present)."

**Root cause:** The factory's CI environment had the holdouts repo and sandbox-exec; the cold-review environment did not. The test suite passed on CI but carried hidden environment dependencies that made the pass non-portable.

**Significance for evolve:** A "268 passed / 0 failed" claim depends on the test environment. The factory reports the count from a single environment (CI); a cold reviewer in a different environment can expose hidden dependencies. Holdout and sandbox-exec prerequisites should be explicitly tested and documented as environment requirements, not just incidentally present in CI.

---

## Summary table

| Date | Session/Source | Factory produced | Independent review caught |
|------|---------------|-----------------|--------------------------|
| 2026-06-08 | `b7941c4a` (Claude Code) | `evidence` node labeled "cold reviewer" but typed as `_codergen` | Operator + self-review: node would run a second coder, not a reviewer |
| 2026-06-09 | Bugbot threads 6+7 on PR #26 | Priority queue `reviewer_backend=codex` in CXDB — but subprocess invoked Claude | Bugbot (Cursor): `_execute_gate` collapsed all non-agy backends to Claude; `verdict=fail` from real codex on live run proved the fix |
| 2026-06-09 | `nextsteps-2026-06-09-dark-factory-pr26-dispatch.md` (session 2) | `bug_fix.dot` pipeline: `gate_green → exit` with no adversarial reviewer | 3-agent Level-5 audit: bead `jleechan-7je` (HIGH) — every fix exits unreviewed |
| 2026-06-09 | `nextsteps-2026-06-09-dark-factory-pr26-dispatch.md` (session 2) | `_holdout_eval` env uses raw `os.environ`, leaking `DARK_FACTORY_HOLDOUTS` to seed scripts | 3-agent Level-5 audit: bead `jleechan-4pa` (P1) — isolation is the repo's defining constraint |
| 2026-06-10 | `PARALLELIZATION_AUDIT_2026-06-10.md` (Hermes) | `plan.md`: no max-lane cap, no kill switch, no anti-patterns | Hermes external audit: same prompt would emit 67-lane plan; factory had no architectural guard |
| 2026-06-09 | `5b92e2b9` (cold-review Codex session) | PR #26 CI: "268 passed / 0 failed" | Cold reviewer: 8 failures in different env (5 holdout-dep + 3 sandbox-exec-dep) |

---

## Patterns observed

1. **Type-label divergence in DOT authoring:** Node human labels and DOT `type=` attributes are decoupled. A coder agent writing a `.dot` file can produce a semantically wrong graph that passes all structural checks (`start`/`exit` present, edges valid) but routes through the wrong handler at runtime.

2. **Metadata lying about behavior:** CXDB `metadata_json` recorded `reviewer_backend="codex"` while the actual subprocess invoked was Claude. The telemetry layer trusted the resolved name, not the argv. Any quality report built from CXDB during the period before `81b9ea6` overstated cross-vendor review coverage.

3. **Gap at the graph boundary, not the runner:** The most severe misses (Incidents 1, 3, 4) were not runner bugs — the runner executed correctly. The gaps were in the `.dot` graph structure and prompt content. Factory-level tests that drive echo-backend paths cannot catch missing nodes or missing prompt constraints.

4. **Environment-gated test portability:** A green CI run is a necessary but not sufficient quality signal. Tests that depend on `$DARK_FACTORY_HOLDOUTS` or `sandbox-exec` may be absent in cold-review or local environments, making the suite's pass/fail count environment-specific.

5. **External audit is more likely to find seam-level gaps:** All four substantive incidents were found by reviewers with no prior context (Bugbot, 3-agent team, Hermes, cold-reviewer Codex session). The factory's own pipeline and test suite found none of them.
