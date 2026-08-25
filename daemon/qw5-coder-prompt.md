# Coder prompt — bead jleechan-qw5 (N-shadow reviewer fan-out)

> **Audience:** the minimax-pair-coder subagent dispatched by daemon/qw5-pilot-dispatch.sh
> **Read this entire file before doing anything.** It is the binding specification for the pilot.
> **Backend:** MiniMax-M3 ONLY (via `claudem` bash wrapper — `ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic`, `ANTHROPIC_MODEL=MiniMax-M3`, `ANTHROPIC_API_KEY=$MINIMAX_API_KEY`). Never use claude sonnet/opus/fable, agy (Antigravity CLI), or agentf/cursor.

## 0. Why this exists

Bead `jleechan-qw5`. Goal: extend `runner/handler_parallel_reviewer.py` from "1 primary + 1 shadow" to "1 primary + N shadows", where N is configurable per node, then rewrite `pipelines/factory/gates.dot` Phase 5 to use that. Pilot scope: Phase 5 ONLY. Do not touch other phases.

Serial baseline (2026-06-29 pilot run, /tmp/cxdb-phase5-serial-baseline.sqlite, run_id=1a9e9deb): **545s wall-clock** for the full chain. gate_skeptic produced a 4096-char BLOCKING verdict.

Acceptance target: **wall-clock <= 180s** AND **verdict-quality >= baseline**.

## 1. Branch + worktree

```bash
cd /Users/jleechan/projects/dark-factory
git fetch origin main
git worktree add /tmp/qw5-coder-wt -b feat/qw5-n-shadow-fanout origin/main
cd /tmp/qw5-coder-wt
git config user.name "jleechan2015"
git config user.email "jleechan2015@users.noreply.github.com"
```

NEVER check out `feat/qw5-n-shadow-fanout` in `/Users/jleechan/projects/dark-factory` — that is the user's working tree and you must NOT mutate it.

NEVER force-push. NEVER push to main. NEVER close bead jleechan-qw5 (orchestrator owns close).

## 2. TDD-first (binding)

1. Read `tests/test_parallel_codex_reviewer.py` and `tests/test_parallel_fanout.py` end-to-end. Understand the existing test surface.
2. Identify the minimal set of new tests that exercise the N-shadow behavior:
   - `test_parallel_reviewer_n_shadows_all_success`
   - `test_parallel_reviewer_n_shadows_one_failure`
   - `test_parallel_reviewer_n_shadows_one_error`
   - `test_parallel_reviewer_n_shadows_default_is_one_shadow` (back-compat)
   - `test_parallel_reviewer_n_shadows_distinct_artifacts` (each shadow writes its own output sidecar)
3. Run `.venv/bin/python -m pytest tests/test_parallel_codex_reviewer.py tests/test_parallel_fanout.py -v` and confirm the new tests FAIL.
4. Implement the N-shadow generalization in `runner/handler_parallel_reviewer.py` (and extend `runner/handler_dispatch.py` if N-shadow spawn helpers are needed there).
5. Re-run the test command and confirm the new tests now PASS, no regressions in existing tests.
6. Refactor for clarity (ponytail rung 7: minimum code that works).

DO NOT touch `runner/engine_parallel.py` — that file handles DOT-declared parallel branches; this change is about a single node that internally fans out reviewers.

## 3. Coalesce semantics (binding — NO reviewer-shopping)

`_coalesce_n_outcomes()` must be **conservative**:

```
all "success"           → "success"
any "error"             → "error"   (infra failure, not reviewer disagreement)
any "failure" or mixed  → "failure"
```

Do NOT implement majority-vote or weighted-merge. The prior /advice Reviewer A verdict (2026-06-27) was unanimous on conservatism: a reviewer failure must surface, not be swallowed by averaging.

## 4. gates.dot rewrite

Replace the Phase 5 subgraph:

**Before** (current shape):
```
holdout -> gate_skeptic
holdout -> fix
gate_skeptic -> adversarial_reviewer (success)
gate_skeptic -> fix
adversarial_reviewer -> gate_es (success)
adversarial_reviewer -> fix
```

**After** (N-shadow parallel):
```
holdout -> review_parallel [type="parallel_reviewer", n_shadows="3", reviewers="skeptic,adversarial,er", backend_priority="codex,minimax,agy"]
holdout -> fix
review_parallel -> gate_es (success)
review_parallel -> fix
```

The `parallel_reviewer` handler internally fans out to 3 distinct reviewer prompts (one per role) while preserving each role's review identity. The merged output is a single verdict envelope.

Do NOT delete `adversarial_reviewer`, `gate_skeptic`, or `gate_er` from the DOT — preserve them as defined shapes other pipelines may still reference. Only CHANGE the Phase 5 subgraph edges to use the new parallel node.

## 5. Pilot measurement (on a real PR after the branch lands)

Run the dark-factory pipeline against the OWN branch's diff:

```bash
cd /tmp/qw5-coder-wt   # the worktree
.venv/bin/python -m pytest tests/test_parallel_codex_reviewer.py tests/test_parallel_fanout.py -v
# all green → proceed

# Pilot run with minimax backend
export ANTHROPIC_BASE_URL=https://api.minimax.io/anthropic
export ANTHROPIC_MODEL=MiniMax-M3
export ANTHROPIC_API_KEY=$MINIMAX_API_KEY

.venv/bin/python -m dark-factory \
  --pipeline pipelines/factory/gates.dot \
  --goal "qw5 pilot: validate fan-out" \
  --backend minimax \
  --feature qw5_pilot \
  --cxdb /tmp/cxdb-qw5-pilot-$(date +%Y%m%d-%H%M%S).sqlite \
  --workdir /tmp/qw5-coder-wt
```

Capture:
- Total wall-clock (`time` the whole command, report `real` seconds).
- Each reviewer's verdict text length (parse `*_review_output_sha256` artifacts in CXDB).
- Each reviewer's outcome (success/failure/error from CXDB events).
- The CXDB sqlite file path (preserved at `/tmp/cxdb-qw5-pilot-<timestamp>.sqlite` for human reviewer audit).

## 6. PR + bead status

Once pilot runs and the implementation is locked, push the branch and open the PR:

```bash
git push -u origin feat/qw5-n-shadow-fanout
gh pr create --repo jleechanorg/dark-factory \
  --base main --head feat/qw5-n-shadow-fanout \
  --title "feat(daemon): N-shadow reviewer fan-out (jleechan-qw5)" \
  --body "Beads: jleechan-qw5

## Background
..."
```

The PR body MUST contain (no exceptions):
- A `## Testing` section listing the new tests and `.venv/bin/python -m pytest ...` output (BEFORE and AFTER).
- A `## Pilot measurement` section with the wall-clock number, per-reviewer outcomes, and verdict-text lengths in a table.
- A `## Pilot verdict vs acceptance` line stating whether the result satisfies the four acceptance criteria (TDD red→green; wall-clock <=180s; verdict-quality >=4096 chars; per-reviewer CXDB).

After opening the PR: post `/skeptic` as a PR comment to request adversarial review. Then report the PR URL back via `br comment jleechan-qw5` (do NOT close the bead).

## 7. Things you MUST NOT do

- Do not edit `/Users/jleechan/projects/dark-factory` (the user's working tree). Use the worktree.
- Do not merge the PR yourself — the orchestrator closes the bead, not the coder.
- Do not use `--dangerously-skip-permissions` substitutions like `--yolo` for review tools.
- Do not invoke `dark-factory` from any directory other than the worktree.
- Do not implement reviewer-shopping in `_coalesce_n_outcomes`. Conservative only.
- Do not run any model other than MiniMax-M3 (no sonnet, opus, haiku, fable, agy, cursor).
