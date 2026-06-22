# Git/PR Forensics — Cold Reviewers vs. In-Pipeline Factory Reviewers

**Repo:** `jleechanorg/dark-factory`
**Question:** Where did an INDEPENDENT/cold review (codex `codex exec` / chatgpt-codex-connector bot, Cursor Bugbot, CodeRabbit, skeptic, or a manual `/reviewdeep`) catch a real issue that the dark-factory IN-PIPELINE reviewer nodes (`gate_er`, `gate_es`, `holdout_eval`, the agy/codex reviewer `tool` node, the `_resolve_gate_backend` priority queue) did NOT catch or were never run for?
**Date:** 2026-06-21

---

## Structural finding (the meta-answer, frames everything below)

**The dark-factory in-pipeline reviewer nodes are NOT wired into this repo's own PR CI.** Only two workflows run on a PR:

```
.github/workflows/ci.yml          -> "test"     (pytest)
.github/workflows/skeptic-gate.yml -> "skeptic"  (cron-skeptic claim verifier)
```

`grep -rln "gate_er|gate_es|holdout_eval|dark-factory --pipeline|review_pr.dot" .github/workflows/` returns **NONE**. The check-run set on a representative PR (#26) is: `skeptic`, `test`, `Cursor Bugbot`, `CodeRabbit` — plus the `chatgpt-codex-connector[bot]` (codex code-review) inline comments.

**Consequence:** Every concrete bug the cold reviewers caught on a dark-factory PR is, by construction, a bug the factory's own `gate_er`/`gate_es`/`holdout_eval`/reviewer-`tool` nodes **were never run against** — the factory does not eat its own dog food at PR time. The factory reviewer nodes are exercised against *target* repos (worldarchitect.ai, airbnb-clone, etc.), not against changes to the runner itself. So the "factory reviewer did instead" column for nearly every example below is **"not wired — never ran on this diff."** This is the single most important systemic gap.

---

## Examples (strongest first)

### 1. PR #26 — codex/Cursor caught `_execute_gate` ignored the resolved backend ("priority queue is metadata theatre")
- **PR / file:** [#26](https://github.com/jleechanorg/dark-factory/pull/26), `runner/handlers.py`. Fix landed at squash `7ef9f91`. Bead `jleechan-qb7`.
- **Cold reviewer caught:** Cursor Bugbot (HIGH) "Gate ignores resolved backend" — *"The new adversarial `backend_priority` resolver can pick `codex`, `minimax`, or `claude-sonnet`, but `_execute_gate` only runs `agy` specially and always invokes `_run_gate_once("claude", …)` for every other name."* In other words the priority queue **resolved** a backend name but the dispatch **still fired `claude --print` for all four** — the cross-vendor "real LLM review" guarantee was paper-only; `Result.metadata["reviewer_backend"]` was hard-coded `claude`.
- **Cursor also caught (same PR):** "Empty queue skips default probing" / "Adversarial filter skipped after fallback" (MEDIUM) — `_resolve_gate_backend` short-circuited to `claude-sonnet` without probing `_DEFAULT_ADVERSARIAL_PRIORITY` when `prefer_adversarial` emptied the list; "Minimax probe mismatches gate dispatch" — `which minimax` probe but the dispatch ran the Claude CLI with a minimax base-URL override.
- **What the factory reviewer node did instead:** The very subject of this bug *is* the factory reviewer dispatch path — and it was **broken in exactly the dimension the factory reviewer is supposed to guarantee** (cross-vendor adversarial verdict). No factory `gate_er` runs on dark-factory's own PRs, so the broken dispatcher could not have caught itself. A `claude` coder would silently have gotten a `claude` reviewer.
- **Why missed:** The label/metadata was decorative — the resolver and the dispatcher were two separate code paths and only the resolver shipped. Self-review impossible (factory gates not in CI). Caught only because Cursor/codex read the *full* handler control flow, not just a diff hunk.
- **Provenance:** memory `feedback_2026-06-09_priority_queue_dispatch.md`, `project_2026-06-09_pr26_production_verify.md`. The fix was later verified live (claude coder 778s → real codex reviewer verdict=`fail`, CXDB `reviewer_backend="codex"`).

### 2. PR #70 — codex caught a real SEALED-HOLDOUT CONTENT LEAK that the factory's own isolation mechanism missed
- **PR / file:** [#70](https://github.com/jleechanorg/dark-factory/pull/70), `runner/engine.py` `_write_transcript_sidecar`.
- **Cold reviewer caught:** chatgpt-codex-connector (P1) "Redact holdout transcripts by handler type" — the redaction that protects sealed holdout output *keys off the node NAME* (looks for the substring `holdout`), but real checked-in `type="holdout_eval"` nodes are named `verify` / `check` (e.g. `benchmarks/amazon-clone/pipelines/slim.dot:10`, `tracker.dot:10`). Those nodes write **raw evaluator stdout** to `~/.dark-factory/runs/.../transcripts`, *"which leaks sealed holdout details into operator logs that later agents can read."* Fix: classify by handler **type**, not name convention.
- **What the factory reviewer node did instead:** This is a hole in the factory's flagship guarantee — the `holdout_eval` isolation — and `holdout_eval` itself has no reviewer that checks its own redaction. The name-convention redaction is the factory's mechanism; nothing in-pipeline audits whether the convention actually matches the deployed node names.
- **Why missed:** Name-convention heuristic (a ZFC-adjacent smell) that silently fails for any holdout node not literally named `*holdout*`. Required a reviewer to cross-reference the redaction predicate against the *actual node names in the benchmark `.dot` files* — exactly the kind of repo-wide cross-file read a diff-scoped gate won't do.
- **Same PR, codex P2:** "Add audit gates to the parallel read-only set" — new `gate_audit` type not in `engine._READ_ONLY_BRANCH_TYPES`, so a fanned-out audit gate resolves `evidence.jsonl` against an empty `branch_*` subdir and falsely reports "missing evidence artifacts."
- **Also PR #70:** skeptic gate itself returned **VERDICT: FAIL** (`specs/skeptic-report.json`, `modelUsed: codex`) on the PR's "working" claim — an independent claim-verifier contradicting the PR's own readiness assertion.
- **Provenance:** `gh api repos/jleechanorg/dark-factory/pulls/70/comments`; `specs/skeptic-report.json`.

### 3. Beads #28 (`jleechan-7je`) & #29 (`jleechan-4pa`) — the Level-5 autonomy team-review caught a MISSING reviewer node and a holdout env leak
- **Source:** 3-agent `df-review` team audit (`/reviewdeep`-style independent review of all 28 graphs + runner), filed as GitHub issues #28/#29, shipped in [#30](https://github.com/jleechanorg/dark-factory/pull/30) (merged `2ae5aa6`).
- **Caught (#28, HIGH):** `bug_fix.dot` had **no adversarial reviewer node at all** — it ran to exit with no `gate_er`/independent-review step, unlike `review_pr.dot`. The factory pipeline that fixes bugs had no factory reviewer in it. Fix added an `evidence` `gate_er` node (prefer_adversarial, codex-first) after `gate_green`.
- **Caught (#29, MEDIUM):** `_holdout_eval`'s `eval_env` was **not** built from `_sanitized_env()` (handlers.py:1813) and the minimax gate env (handlers.py:1136) didn't layer on the sanitized base — i.e., a holdout-env-leak path in the very handler that is supposed to be the isolation boundary.
- **What the factory reviewer did instead:** Nothing — a graph with a *missing* reviewer node cannot review itself; the gap is the absence of the node. Only an external whole-graph audit catches "this pipeline has no reviewer."
- **Why missed:** Scope. In-pipeline gates review the *diff/artifact a node produced*; they never reason about *graph topology* ("does this pipeline contain a reviewer?") or about the runner's own env-sanitization correctness.
- **Provenance:** `project_2026-06-09_level5_autonomy_review.md`.

### 4. PR #11 — codex/Cursor/CodeRabbit caught parallel fan-out correctness bugs (visit limits, quorum bounds, double-execution, branch-crash misrouting)
- **PR / file:** [#11](https://github.com/jleechanorg/dark-factory/pull/11) (parallel fan-out/fan-in), `runner/engine.py`, `runner/handlers.py`.
- **Cold reviewers caught (sample):**
  - codex P1 "Enforce visit limits inside branch workers" — a parallel branch with a fix/verify loop never checks `max_steps`/`max_visits`; infinite loop possible.
  - codex P2 "Respect edge conditions before launching branches" — fan-out collected every non-join successor without evaluating conditional edges.
  - CodeRabbit (Major) "Validate `k_of_n` quorum bounds" — `k<=0` makes `k_of_n` always succeed; `k>n` silently accepted → **failed joins misrouted as success**.
  - CodeRabbit (Major) "Prevent double fan-out execution" — legacy `parallel=true` path AND new `type=parallel` path both fire → duplicated branch execution + duplicate side effects.
  - Cursor (HIGH) "Branch stuck treated as success", "Concurrent branches share workdir" (file-mutating backends clobber each other), "Branch crash ignored by join quorum", "Join edge masks branch crash", "Parallel crash wrong final outcome".
- **What the factory reviewer did instead:** Not wired; and even conceptually a `gate_er` grades *the implemented feature's artifact*, not the *runner's own concurrency/quorum logic*. A `false=success` quorum bug means a reviewer node could itself report success on a failed parallel join.
- **Why missed:** These are engine-correctness/race bugs in the orchestrator, found by reviewers reading the new control flow holistically — outside the artifact-grading scope of any gate node. Several were acknowledged as deferred/known limitations (shared workdir, nested parallel), i.e. real gaps the cold review surfaced and the factory could not.
- **Provenance:** `gh api .../pulls/11/comments`; `project_2026-05-31_pr11_7green_session.md`.

### 5. PR #13 — codex/Cursor caught crash-handling gaps in the orchestrator (parallel-branch crashes bypass the handler; unconditional-exit-on-crash) + CodeRabbit caught CI credential hardening
- **PR / file:** [#13](https://github.com/jleechanorg/dark-factory/pull/13) (runner crash-resilience), `runner/engine.py`, `.github/workflows/*.yml`.
- **Cold reviewers caught:**
  - Cursor (HIGH) "Parallel branch crashes bypass handler" — the new try/except wrapped only the primary node; `parallel=true` branch handlers still crashed the whole runner. Fixed in `411676d`.
  - codex P2 "Record crashes from parallel branch nodes" — same blind spot from a different reviewer (cross-vendor corroboration).
  - Cursor (MEDIUM) "Edge shorthand conditions always fail" — the new `_EDGE_OP_RE` guard rejected valid conditional edges lacking `=`/`!=`/`contains`/`in` → `_evaluate_expression` shorthands silently broke.
  - Cursor (LOW) "Log handle skipped on CXDB failure" — per-run log not closed if `cxdb.end_run`/`cxdb.close` raises.
  - CodeRabbit (Major) — `persist-credentials: false` missing on checkout steps in both `ci.yml` and `skeptic-gate.yml` (supply-chain hardening); fixed next commit.
- **What the factory reviewer did instead:** Not wired; out of scope — these are runner-resilience and CI-security issues with no artifact for a gate to grade.
- **Why missed:** Crash-path + CI-config bugs are invisible to artifact-grading gates and to unit tests that pass on the happy path; caught only by reviewers reading the exception-routing control flow and the workflow YAML.
- **Provenance:** `gh api .../pulls/13/comments`; `feedback_2026-05-31_runner_resilience_reviewer_gates.md`, `project_2026-05-31_pr13_7green_session.md`.

---

## Additional supporting examples (lower-ranked but on-point)

- **PR #26, codex P1 "Populate bug_fix.test_path before the red gate" + Cursor "Reproduce never sets test path" (HIGH):** the `bug_fix.dot` lane expects `state.bug_fix.test_path` after the `reproduce` codergen step, but `_codergen` never parses the agent's `reproduce:` output into `Result.context_updates`, so `gate_red`/`gate_green` run pytest against an empty path. A *factory pipeline* (`bug_fix.dot`) was structurally non-functional; caught by two cold reviewers, no factory gate runs on the runner repo to catch it.
- **PR #26, CodeRabbit (Critical) "Command construction breaks on paths with spaces":** f-string + `shlex.split` mis-parses `test_path = "tests/my file.py"`; (Major) "Add include-cycle detection" — `_resolve_includes()` recurses unboundedly on `a.dot -> b.dot -> a.dot`. Parser/handler robustness bugs, no factory self-review.
- **PR #26, Cursor (HIGH) "Include paths ignore factory home":** `include="@pipelines/_base.dot"` resolution didn't try `DARK_FACTORY_HOME` — a real include-resolution bug. Related to the separate PR #22 `_base.dot` conformance-validator blocker (`jleechan-u8e`, issue #23).
- **PR #74, codex P2 "Add backend_priority to honor the claude-only reviewer":** `redgreen_claudeaf.dot` gate documented as claude-bound but lacked `backend_priority`, so `_resolve_gate_backend` fell through to `ctx.backend` — the lane's intended reviewer binding silently didn't apply. The reviewer-selection mechanism mis-bound itself; caught by codex, fixed in `79c5000`.
- **PR #74, Cursor (MEDIUM) "Std two pass drops timing":** the `gate_code_standards` PASS rule could be satisfied while dropping the required execution-duration disclosure — i.e., the gate's own pass criterion was too weak (a factory gate's weak prompt, caught by a cold reviewer).

## Cross-cutting "why missed" taxonomy

| Miss class | Examples |
|---|---|
| **Factory gate not wired into runner-repo CI (never ran)** | ALL of the above — the dominant cause |
| **Graph-topology blindness (gate can't see "no reviewer exists")** | #28 missing `bug_fix.dot` reviewer; #11 double fan-out |
| **Self-referential: the bug is IN the reviewer dispatch / isolation** | #26 dispatch theatre; #70 holdout-name-redaction leak; #29 holdout env leak; #74 mis-bound reviewer |
| **Engine/concurrency/crash correctness (no artifact to grade)** | #11 quorum bounds, shared workdir; #13 parallel-branch crash, edge-shorthand |
| **Weak gate pass criterion** | #74 "Std two pass drops timing" |
| **Name-convention heuristic that silently mismatches reality** | #70 redaction-by-name; relates to ZFC discipline |

## How to read this for "factory evolve"
The empirical record says the factory's own reviewer layer has **never been the thing that caught a bug on the factory's own code** — cold reviewers (codex bot, Cursor/Bugbot, CodeRabbit, skeptic, team `/reviewdeep`) did all of it. The highest-leverage evolution is (a) wire a dark-factory pipeline (`review_pr.dot` / `gate_er`) into the repo's own PR CI so it dogfoods, and (b) add graph-topology and reviewer-self-audit checks, since the strongest misses (#70 holdout leak, #26 dispatch theatre, #28 missing reviewer, #29 env leak) were all *in the reviewer/isolation machinery itself* — exactly what a same-scope gate is blind to.
