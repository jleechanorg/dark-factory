# dark-factory roadmap

## Recent activity (rolling)

### 2026-06-09 — adversarial-review priority queue dispatch fix (PR #26)

- **[PR #26](https://github.com/jleechanorg/dark-factory/pull/26) MERGED at `7ef9f91`** (squash, admin-merge). The conformance walker fix + cherry-picked PR #22 work + dispatch fix + polish. CI test + skeptic PASS, Bugbot skipping, CodeRabbit perpetual-nitpick stall (per [`feedback_2026-05-31_pr10_coderabbit_stall.md`](file:///Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-05-31_pr10_coderabbit_stall.md)) → admin-merged on operator OK.
- **Bugbot high-severity finding, fixed in commit `81b9ea6`.** Cursor Bugbot thread 6: the priority queue `codex > minimax > agy > claude-sonnet` was decorative. `_execute_gate` only handled `agy` specially and forced `claude` for every other name. A resolved `codex` / `minimax` still ran the Claude subprocess. End-to-end dispatch now honors the resolved name:
  - `_gate_subprocess_args` routes 4 backends: `agy` (`--add-dir`), `codex` (`codex exec --yolo --skip-git-repo-check`), `minimax` (Claude CLI + `ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic`), and `claude-sonnet` / bare `claude` (Claude CLI).
  - new `_gate_subprocess_env(backend)` overrides `ANTHROPIC_BASE_URL` only for `minimax`; other backends use `_sanitized_env()` unchanged.
  - `_run_gate_once` records the resolved backend in `Result.metadata["reviewer_backend"]` (was hard-coded to `claude` for non-agy). CXDB now shows what actually graded the diff.
  - `_execute_gate` no longer collapses to claude. agy keeps its infra-fallback (claude); codex / minimax / claude-sonnet / claude do not fall back silently.
- **Empty-list fallback** in `_resolve_gate_backend` (commit `81b9ea6`): when `prefer_adversarial` empties the post-filter list (e.g. lane says `backend_priority=claude`, coder is `claude`), fall back to `_DEFAULT_ADVERSARIAL_PRIORITY` so the queue is re-probed (codex > minimax > agy > claude-sonnet) instead of short-circuiting to `claude-sonnet` and silently dropping cross-vendor review. Resolves Bugbot thread 7.
- **Polish (commit `4285cb2`)**: addressed 3 of 6 CodeRabbit actionable comments — `_run_pytest_test` argv-as-list (paths with spaces in `sys.executable`), `_resolve_includes` cycle detection via visited tuple, `tests/test_include.py` raw-string lint. The other 3 are docstyle / pre-existing in the PR.
- **Tests** (`tests/test_gates.py`, 26 total; 8 new):
  - `test_gate_subprocess_args_routes_{codex,claude_sonnet,bare_claude}_to_*_cli`
  - `test_gate_subprocess_env_routes_minimax_through_minimax_gateway`
  - `test_gate_subprocess_env_does_not_set_minimax_for_other_backends` (stubs `_sanitized_env` to a clean baseline)
  - `test_execute_gate_runs_codex_subprocess_when_priority_resolves_codex`
  - `test_execute_gate_runs_minimax_with_correct_env`
  - `test_resolve_adversarial_backend_falls_back_to_default_when_post_filter_empty`
- **End-to-end verification post-merge**: `_resolve_gate_backend` with `backend_priority="codex,minimax,agy,claude-sonnet"`, `prefer_adversarial="true"`, `ctx.backend="claude"` → resolves to `codex` (the first installed entry on the operator's PATH; `claude` is filtered out by `prefer_adversarial`). The dispatch fix is real, not metadata.
- **Cross-vendor intent = real subprocess** (per [`feedback_2026-06-09_adversarial_review_real_llm.md`](file:///Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-06-09_adversarial_review_real_llm.md) + [`feedback_2026-06-09_priority_queue_dispatch.md`](file:///Users/jleechan/.claude/projects/-Users-jleechan-projects-dark-factory/memory/feedback_2026-06-09_priority_queue_dispatch.md)). The priority queue is chosen at run-config time, not a retry cascade. A real `fail|partial` from one reviewer is authoritative and never re-tried on a different model. The dispatch (argv + env) must match the resolved name end-to-end, or the label lies.
- **Full suite on `7ef9f91` (main)**: 268 passed / 0 failed. Conformance `score` rc 0 (local_mock: pass). Smoke run of `benchmarks/airbnb-clone/pipelines/sprint-1-data.dot` --backend echo executes start → plan → implement → verify (fails on `${state.ao.worktree}` unresolved, expected without AO setup) → fix loop with `max_visits=5`. Bead: [jleechan-qb7](https://github.com/jleechanorg/dark-factory/issues/25).

### 2026-06-09 — PR #22 follow-up: explore-stitch + adversarial-review priority queue

- Landed the PR #22 follow-up on branch `fix/pr22-explore-stitch-and-adversarial-priority` (off `1220ef2083`). **Two coupled changes** driven by beads [jleechan-n3m](https://github.com/jleechanorg/dark-factory/issues/24) and [jleechan-qb7](https://github.com/jleechanorg/dark-factory/issues/25):
  1. **`explore_stitch` codergen** in `pipelines/_base.dot` between `explore_join` and `explore_out`. Reads the 4 partials (`.dark-factory/explore-{concepts,authorities,reuse,risks}.md`) and writes the consolidated `.dark-factory/explore-findings.md` that `prompts/slim/plan.md:6` hard-requires. Prompt is the repurposed `prompts/slim/explore.md` (vestigial after the 4-way fanout; lowest-LOC path). New `tests/test_base_dot.py::test_base_dot_has_explore_stitch_between_join_and_out`; `test_engine.py` and `test_slim.py` trajectory assertions updated.
  2. **Adversarial-review priority queue** (`runner/handlers.py`): `_resolve_adversarial_backend(priority, ctx)` returns the first installed entry from the queue; `_resolve_gate_backend` honors a new `backend_priority=...` node attr (wins over `backend=` and `ctx.backend`). Default queue `codex > minimax > agy > claude-sonnet`, overridable via `DARK_FACTORY_ADVERSARIAL_PRIORITY` (comma-separated). `prefer_adversarial: true` drops the run-level coder backend from the queue (so a `claude` coder run never gets a `claude` reviewer). Resolver audit lands in `Result.metadata` (priority list, resolved name, skipped entries). Wired into `pipelines/slim/review_pr.dot` `evidence` node. 4 new tests in `tests/test_gates.py`.
- **Adversarial-review priority queue:** `codex > minimax > agy > claude-sonnet` (`DARK_FACTORY_ADVERSARIAL_PRIORITY` env var, chosen at run-config time — not a retry cascade). A real `fail|partial` from one reviewer is authoritative and is never retried on a different model (no-reviewer-shopping rule per `feedback_2026-05-31_runner_resilience_reviewer_gates.md`).
- Governing design doc: [`roadmap/agy-reviewer-and-base-dot-2026-06-09.md`](agy-reviewer-and-base-dot-2026-06-09.md). Handoff: [`/Users/jleechan/roadmap/nextsteps-2026-06-09-dark-factory-pr22-stitch-and-adversarial.md`](file:///Users/jleechan/roadmap/nextsteps-2026-06-09-dark-factory-pr22-stitch-and-adversarial.md).

### 2026-06-09 — PR #22 review and adversarial-review priority queue (earlier)

- Reviewed [PR #22](https://github.com/jleechanorg/dark-factory/pull/22) ("agy reviewer backend + claude infra-fallback; remove Wafer GLM-5.1; pipeline includes & bug_fix pipeline") at head `1220ef2083`. Wafer/claudew removal is clean; agy reviewer + claude infra-fallback is correctly scoped (verified by 3 agy tests passing); `_base.dot` and `bug_fix.dot` are well-designed. **Blocked on a real contract gap**: the 4-way explore fanout lands without a stitch step, so `prompts/slim/plan.md:6` (unchanged) will read a non-existent `.dark-factory/explore-findings.md` and abort every first run of `minimal_feature.dot`, `minimal_pr.dot`, and `bug_fix.dot`.
- New beads: [jleechan-n3m](https://github.com/jleechanorg/dark-factory/issues/24) (P1, add `explore_stitch` codergen between `explore_join` and `explore_out` in `_base.dot`); [jleechan-qb7](https://github.com/jleechanorg/dark-factory/issues/25) (P1, adversarial-review priority queue `codex > minimax > agy > claude-sonnet` for `gate_er`/`gate_es` — *real* LLM review, not webhook-ping). 3 existing beads covered the shipped work: [jleechan-81l](https://github.com/jleechanorg/dark-factory/issues/NN) (_base.dot), [jleechan-gqe](https://github.com/jleechanorg/dark-factory/issues/NN) (bug_fix.dot), [jleechan-sse](https://github.com/jleechanorg/dark-factory/issues/NN) (gate_red/gate_green).
- Design doc for the work: [`roadmap/agy-reviewer-and-base-dot-2026-06-09.md`](agy-reviewer-and-base-dot-2026-06-09.md). Governs PR #22 + the follow-up fix-PR (so the PR body has a governing-doc URL per repo policy).
- Full suite on `1220ef2083`: 252 passed / 1 failed. The 1 failure is pre-existing (`test_conformance_score_is_deterministic_mock_surface`); `tests/test_conformance.py` is not in the diff vs `main`, so this is a known baseline flake, not introduced by PR #22.
- **Correction / root cause (2026-06-09 later):** that conformance failure is **not** an irreducible flake — it is **fixable and IS gated by the PR #22 diff**. `bin/conformance validate` walks every `pipelines/*.dot` and enforces `start`/`exit`; the include-only fragment `pipelines/_base.dot` (added by this PR's includes lane, no start/exit by design) fails that check → conformance `score` rc 1 → `test` + `skeptic` CI gates RED. The walker reads the *directory* not git's tracked set, which is why it presented as "pre-existing on main" (the fragment sat untracked during the `6c6a2a3` baseline). Fix is in the walker's file filter (skip `_*.dot`), never the parser invariant. Tracked: [jleechan-u8e](https://github.com/jleechanorg/dark-factory/issues/23). This is the actual gating fix for PR #22 merge.
- Handoff: [`/Users/jleechan/roadmap/nextsteps-2026-06-09-dark-factory-pr22-stitch-and-adversarial.md`](file:///Users/jleechan/roadmap/nextsteps-2026-06-09-dark-factory-pr22-stitch-and-adversarial.md); CI-blocker detail in [`/Users/jleechan/roadmap/nextsteps-2026-06-08-dark-factory-agy-reviewer-pr22.md`](file:///Users/jleechan/roadmap/nextsteps-2026-06-08-dark-factory-agy-reviewer-pr22.md).

### 2026-06-08 — explore-phase rollout and PR review

- Reviewed commit `6c6a2a3` ("feat(slim): add explore phase and role-based model routing"). Verdict: ship it. Concerns 2 (class=plan) and 3 (plan.md reads explore artifact) from the first read were false alarms after a second look.
- User follow-up ask, verbatim: **"i want explore for all the pipelines not just one."** 5 of 7 `.dot` files currently lack `explore`; the slim-only rollout is now a tracked P1 rollout.
- New beads (with linked GitHub issues): [jleechan-2wx](https://github.com/jleechanorg/dark-factory/issues/18) (P1, propagate to `factory/hello.dot` and selectively `factory/gates.dot`), [jleechan-80r](https://github.com/jleechanorg/dark-factory/issues/19) (P3, explore early-exit on infeasible verdict), [jleechan-x57](https://github.com/jleechanorg/dark-factory/issues/20) (P3, `DARK_FACTORY_PLAN_MODEL` env var), [jleechan-4gx](https://github.com/jleechanorg/dark-factory/issues/21) (P3, clean up untracked leftovers from prior sessions).
- Diagram pipeline work landed: 3 semantic-color Excalidraw diagrams + an HTML README (commits `802fdb4`, `1f4443a`). Color contract doc at `docs/diagram-color-semantics.md`.
- Full suite baseline on `6c6a2a3`: 2 pre-existing failures (`test_conformance_score_is_deterministic_mock_surface`, `test_parallel_branches_run_in_separate_threads`) on a clean main checkout. Documented in the nextsteps doc; not blocking the rollout PR.
- Handoff: `/Users/jleechan/roadmap/nextsteps-2026-06-08-dark-factory-explore-rollout.md`.

### 2026-05-31 - runner crash-resilience and reviewer-gate roadmap

- Opened/organized the crash-resilience roadmap under [jleechan-o8q](br show jleechan-o8q), with PR #13 at `411676d5c634ec6d5902529dd8d50dfbffbf6428` for the first engine boundary/per-run-log slice.
- Added [jleechan-x33](br show jleechan-x33) for a repo-agnostic reviewer-node miss class: file-backed current-head diff/evidence audits require `target_repo`, `target_pr`, `target_head_sha`, `base_sha`, PR description snapshot, evidence paths/SHA, and fail-closed checks for brownfield delete-first, net-LOC, and dead-code in any target repo. PR #7178 is evidence only, not scope.
- Verification: targeted crash/engine tests passed (`17 passed`); full suite remains at `101 passed, 2 failed` on conformance score and malformed-edge fail-closed hardening.
- Handoff: `/Users/jleechan/roadmap/nextsteps-2026-05-31-dark-factory-resilience.md`.

### 2026-05-24 — airbnb-clone Sprint 1 holdout debug

- Wrote `/Users/jleechan/roadmap/nextsteps-2026-05-24-darkfactory-airbnb-holdout.md` as the handoff for the airbnb-clone holdout bootstrap.
- Five infra root causes fixed in `runner/handlers.py:_holdout_eval`: Java PATH for emulators, replace `time.sleep(60)` with port polling, kill emulator process group on cleanup (not just CLI wrapper), strip `GOOGLE_APPLICATION_CREDENTIALS`/`GCLOUD_PROJECT` from `eval_env`, pre-clean emulator ports before launching. Plus one spec ambiguity (`hostId` vs `ownerUid`) patched in `benchmarks/airbnb-clone/spec.md`.
- Opened beads: [orch-0bne](https://github.com/jleechanorg/dark-factory/issues/orch-0bne) (auto-seed emulator), [orch-ecwu](https://github.com/jleechanorg/dark-factory/issues/orch-ecwu) (capture first real Sprint 1 score), [orch-a9dh](https://github.com/jleechanorg/dark-factory/issues/orch-a9dh) (sweep spec for ambiguous field names), [orch-2fze](https://github.com/jleechanorg/dark-factory/issues/orch-2fze) (orphan emulator cleanup on SIGKILL).
- All 91 tests still pass. Benchmark boundary check now green. Handler changes + entire `benchmarks/airbnb-clone/` tree remain uncommitted on `main`.

### 2026-05-22 — sealed holdouts and Attractor parity queue

- Wrote `/Users/jleechan/roadmap/nextsteps-2026-05-22-dark-factory-sealed-holdouts.md` as the independent handoff for the current review.
- Closed review beads `orch-s17v` and `orch-0rwy`; implementation remains open in `orch-pf62`, `orch-7z3e`, `orch-ac6q`, `orch-sdy0`, and `orch-rxfs`.
- Current highest-risk gaps: visible all-nodes benchmark holdout, AO backend mechanical isolation, partial Attractor edge semantics, missing AttractorBench conformance surface, and static Healer/CXDB prescriptions.
- Live discovery found both `/Users/jleechan/projects/dark-factory` and `/Users/jleechan/projects/dark-factory-holdouts` clean on `main...origin/main`, with no current PRs for `https://github.com/jleechanorg/dark-factory`.

### 2026-05-29 — generic beads implemented, two PRs open

- Resumed session after ~5-day gap; repo stable at HEAD `bd50ded` (AO lifecycle worker patched).
- All five `_holdout_eval` infra fixes confirmed on main.
- **orch-0bne + orch-2fze**: [PR #7](https://github.com/jleechanorg/dark-factory/pull/7) `feat/holdout-infra-seed-atexit` — Firebase emulator seed step + atexit SIGKILL cleanup. Both beads → `review`.
- **orch-2oc6**: [PR #8](https://github.com/jleechanorg/dark-factory/pull/8) `fix/attractor-boundary-audit` — strengthened `check_boundary.py` with agent-facing-dir scan, no-holdouts-dir check, sealed-not-in-specs check. Bead → `review`.
- **orch-ecwu** (Sprint 1 holdout score capture) now unblocked pending PR #7 merge.

### 2026-05-30 — PR #7 and PR #8 merged; queue clean

- [PR #7](https://github.com/jleechanorg/dark-factory/pull/7) merged — seed step + atexit cleanup (orch-0bne, orch-2fze) → `done`.
- [PR #8](https://github.com/jleechanorg/dark-factory/pull/8) merged at `c99a119` — boundary audit hardening (orch-2oc6) → `done`.
- **Main HEAD:** `c99a119`. Follow-up commit `acd39e1` (seed-fallback fix, not in PR #7) is being landed in a new PR off `fix/holdout-seed-fallback`.
- **Next:** orch-ecwu (Sprint 1 airbnb-clone holdout score capture, now fully unblocked once the seed-fallback PR merges).
