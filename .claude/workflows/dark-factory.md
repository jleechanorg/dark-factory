---
name: dark-factory
description: Orchestrate a Level-5 dark-factory pipeline (spec_validation → explore → plan → implement → gate → bounded fix loop). Each phase is a separate dark-factory invocation against a single-phase .dot graph; the .dot engine stays simple, this workflow handles multi-phase orchestration and the G3 (runtime-determined fan-out) closure by re-dispatching phases as separate runs with shared CXDB.
phases: [spec_validation, explore, plan, implement, gate, fix]
---

# Dark Factory — Level-5 Pipeline Orchestrator

This workflow replaces the in-engine `type="dynamic"` custom Python handler
(Lane A). The `.dot` engine becomes a single-phase runner: each phase below is
a separate `dark-factory` invocation against a phase-specific `.dot` graph.
The workflow owns phase ordering, shared state (CXDB), fix-loop bounding, and
the final summary.

## Setup (run once before any phase)

```bash
export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
export PATH="$HOME/.local/bin:$PATH"
command -v dark-factory >/dev/null || {
  echo "ERROR: install dark-factory first (cd $DARK_FACTORY_HOME && ./install.sh)"
  exit 1
}

# One CXDB file shared across all phases so the trace is continuous and
# `df-healer --cxdb $CXDB` can cluster failures across phase boundaries.
export CXDB="${CXDB:-$HOME/.dark-factory/cxdb.sqlite}"

# Backend: 'claude' for production, 'echo' for smoke. All phases must agree.
export BACKEND="${BACKEND:-claude}"

# Working directory of the TARGET repo (the one being built/modified).
# Pipelines and prompts resolve from $DARK_FACTORY_HOME; code lands here.
export TARGET_REPO="${TARGET_REPO:-$PWD}"
```

Every phase invocation below follows the same shell:

```bash
cd "$TARGET_REPO"
dark-factory \
  --pipeline pipelines/<PHASE>.dot \
  --goal "<PHASE GOAL — see below>" \
  --backend "$BACKEND" \
  --feature "$FEATURE" \
  --cxdb "$CXDB"
```

The workflow operator (you) is responsible for passing the previous phase's
`final_outcome` and `output_head` into the next phase's `--goal` so context
flows across phase boundaries (the `.dot` engine is stateless across
invocations by design; this workflow is the cross-phase memory).

## Phase 1: spec_validation

**Goal of this phase:** Run the public spec through the spec-validation
benchmark so the spec itself is graded before any code is written. This
mirrors `benchmarks/attractor-spec-review/pipelines/review_slim.dot`.

**Dispatch:**

```bash
cd "$TARGET_REPO"
dark-factory \
  --pipeline benchmarks/attractor-spec-review/pipelines/review_slim.dot \
  --goal "Validate spec: ${FEATURE_SPEC_PATH:-specs/$FEATURE.md}" \
  --backend "$BACKEND" \
  --feature "$FEATURE" \
  --cxdb "$CXDB"
```

**Pass criterion:** `final_outcome == "success"` and the spec-validation
trace shows all line-items passed (or were marked `warn` with documented
mitigation).

**On failure:** Stop. Surface the spec_validation trace. Do not proceed to
explore — the spec itself is broken. The user must revise
`specs/<feature>.md` and rerun Phase 1.

**State to carry forward:** the spec-validation SHA and verdict summary
(prefixed into the Phase 2 goal).

## Phase 2: explore

**Goal of this phase:** Read the spec, run the 4-way explore fan-out
(concept / authorities / reuse / risks), and produce the consolidated
`.dark-factory/explore-findings.md` that Phase 3's plan node requires.

**Pipeline:** `pipelines/slim/minimal_feature.dot` is the canonical
"explore → exit" lane. To run explore as its own phase, invoke it with a
goal that explicitly halts after the explore stitch (the lane's plan/implement
nodes will be bypassed by using `--max-steps` clamped to the explore phase
exit, OR by dispatching just `_base.dot`'s explore subgraph via a wrapper).
The recommended shortcut is to invoke `minimal_feature.dot` and abort
after `explore_stitch` exits — `df-healer` will cluster any premature
exit correctly.

```bash
cd "$TARGET_REPO"
dark-factory \
  --pipeline pipelines/slim/minimal_feature.dot \
  --goal "PHASE 2 (explore only): Read specs/${FEATURE}.md and produce .dark-factory/explore-findings.md. Stop after explore_stitch exits. Spec validation SHA from Phase 1: ${SPEC_VALIDATION_SHA:-unknown}. Do NOT run plan/implement." \
  --backend "$BACKEND" \
  --feature "$FEATURE" \
  --cxdb "$CXDB" \
  --state slim.phase="explore" \
  --state slim.test_command="true" \
  --max-steps 30
```

**Pass criterion:** `.dark-factory/explore-findings.md` exists and the CXDB
trace shows `explore_stitch` exited `success`. The 4 partials
(`.dark-factory/explore-{concept,auth,reuse,risks}.md`) must all be
present.

**State to carry forward:** the SHA of `explore-findings.md` (use
`git rev-parse --short HEAD:.dark-factory/explore-findings.md` or
`shasum -a 256`).

## Phase 3: plan

**Goal of this phase:** Read the explore findings, produce a written
implementation plan. The plan is the contract Phase 4 implements against.

```bash
cd "$TARGET_REPO"
dark-factory \
  --pipeline pipelines/slim/minimal_feature.dot \
  --goal "PHASE 3 (plan only): Read .dark-factory/explore-findings.md (sha ${EXPLORE_SHA:-unknown}) and write .dark-factory/plan.md. The plan must enumerate: file paths to create/modify, test cases to add, holdout scenarios to satisfy, and acceptance criteria. Stop after the plan node exits. Do NOT run implement." \
  --backend "$BACKEND" \
  --feature "$FEATURE" \
  --cxdb "$CXDB" \
  --state slim.phase="plan" \
  --state slim.test_command="true" \
  --max-steps 50
```

**Pass criterion:** `.dark-factory/plan.md` exists and the CXDB trace shows
the plan node exited `success`. The plan must reference every file the
implement phase will touch (else the implement agent has unconstrained
scope).

**State to carry forward:** the SHA of `plan.md`.

## Phase 4: implement

**Goal of this phase:** Read the plan, produce the implementation diff.
This is the only phase that may write to the target repo's source tree.

```bash
cd "$TARGET_REPO"
dark-factory \
  --pipeline pipelines/slim/minimal_feature.dot \
  --goal "PHASE 4 (implement): Implement exactly the file changes enumerated in .dark-factory/plan.md (sha ${PLAN_SHA:-unknown}). Run the tests in \$state.slim.test_command. Stop after the test node exits with success. Do NOT run review/holdout/gates — those are Phase 5." \
  --backend "$BACKEND" \
  --feature "$FEATURE" \
  --cxdb "$CXDB" \
  --state slim.phase="implement" \
  --state slim.test_command="${TEST_COMMAND:-pytest tests/test_${FEATURE}.py -x}"
```

**Pass criterion:** `final_outcome == "success"`, the test node passed,
and `git diff` shows changes scoped to the files enumerated in `plan.md`
(over-scope = route to Phase 6 fix).

**State to carry forward:** `git rev-parse HEAD` (the diff HEAD), the
`output_head` from the implement node, and `final_outcome`.

## Phase 5: gate (Level-5 hard tier)

**Goal of this phase:** Run the hard-tier gates against the implement
phase's diff. The hard tier is:
1. CXDB continuity check (every node from Phase 2–4 has a row)
2. `gate_er` — controller-owned independent evidence review
3. reviewer calibration — a second available backend reviewing the identical
   controller-bound envelope

The graph gate and terminal calibration lanes must use the binary-owned review
controller. This workflow never writes reviewer prompts and never invokes a
vendor reviewer CLI directly.

> **Controller v1 backend scope:** Codex only, via `codex exec --json
> --ephemeral --skip-git-repo-check --sandbox read-only`. The full prompt is
> sent on stdin. The reviewer cannot write the target. Exit codes: `0`
> valid pass, `2` valid fail, `1` invalid / infra. Do not advertise other
> backends for the controller until they have an equivalent safe adapter.

```bash
cd "$TARGET_REPO"

# 5a. Standard gates.dot (CXDB + /es + /er + /code_standards)
dark-factory \
  --pipeline pipelines/factory/gates.dot \
  --goal "PHASE 5a (hard-tier gates): Validate implement diff at HEAD ${IMPLEMENT_HEAD:-HEAD}. Feature: ${FEATURE}." \
  --backend "$BACKEND" \
  --feature "$FEATURE" \
  --cxdb "$CXDB"

# 5b. Controller-owned calibration review.
# The task file contains task/PR data only, never reviewer instructions.
dark-factory review \
  --workdir "$TARGET_REPO" \
  --base-sha "$(git merge-base HEAD origin/main)" \
  --head-sha "$(git rev-parse HEAD)" \
  --task-file "${FEATURE_SPEC_PATH:-specs/$FEATURE.md}" \
  --output-dir "$HOME/.dark-factory/reviews/$FEATURE/$(git rev-parse HEAD)/$BACKEND" \
  --backend "$BACKEND"

# Default-on calibration: when another independent backend is installed, run
# the identical command with only these transport/output arguments changed:
#   --output-dir <separate-controller-output-dir>
#   --backend <independent-backend>
```

**Pass criterion:** The graph gates and required controller review exited
`success`. Each accepted lane has a controller receipt whose prompt,
envelope, base, head, task, response, and backend bindings verify. When the
second calibration backend is available, both lanes must be compared from
those receipts, with the receipt, prompt, and envelope SHA-256 digests recorded
in the comparison. A skill/workflow-authored `prompt.txt`, missing receipt,
digest mismatch, stale SHA, or nonzero controller exit is inconclusive and
cannot satisfy the hard tier.

**On failure:** proceed to Phase 6 (fix loop).

## Phase 6: fix (bounded loop)

**Goal of this phase:** Apply a minimal fix to the implement-phase diff so
the gate phase passes on re-run. Loop back to Phase 4 (implement) with a
new goal that points at the gate failure.

**Loop bound:** `MAX_FIX_ATTEMPTS=3` (matches the `.dot` engine's
`max_visits="3"` on the fix node).

```bash
MAX_FIX_ATTEMPTS=3
attempt=0
while [[ $attempt -lt $MAX_FIX_ATTEMPTS ]]; do
  attempt=$((attempt + 1))
  echo "=== fix attempt ${attempt}/${MAX_FIX_ATTEMPTS} ==="

  cd "$TARGET_REPO"
  dark-factory \
    --pipeline pipelines/slim/minimal_feature.dot \
    --goal "PHASE 6 (fix ${attempt}/${MAX_FIX_ATTEMPTS}): The gate phase failed. Read the gate verdict at .dark-factory/gate-${attempt}.md and the skeptic/adversarial reviews. Apply the MINIMAL change to make Phase 5 pass without regressing Phase 4's tests. Stop after test exit." \
    --backend "$BACKEND" \
    --feature "$FEATURE" \
    --cxdb "$CXDB" \
    --state slim.phase="fix" \
    --state slim.test_command="${TEST_COMMAND:-pytest tests/test_${FEATURE}.py -x}" \
    --state slim.previous_gate_verdict="$(cat .dark-factory/gate-${attempt}.md 2>/dev/null || echo unknown)"

  # Re-run the hard-tier gates
  dark-factory \
    --pipeline pipelines/factory/gates.dot \
    --goal "Re-validate after fix attempt ${attempt}" \
    --backend "$BACKEND" \
    --feature "$FEATURE" \
    --cxdb "$CXDB"

  # Check verdict
  LAST_VERDICT=$(sqlite3 "$CXDB" "SELECT metadata FROM events ORDER BY seq DESC LIMIT 1;" 2>/dev/null \
    | jq -r '.final_outcome // empty' 2>/dev/null || echo "")
  if [[ "$LAST_VERDICT" == "success" ]]; then
    echo "fix attempt ${attempt} succeeded"
    break
  fi
done

if [[ "$attempt" -ge "$MAX_FIX_ATTEMPTS" ]]; then
  echo "fix loop exhausted at ${MAX_FIX_ATTEMPTS} attempts"
  FINAL_OUTCOME="exhausted"
fi
```

**Pass criterion:** Phase 5 gates pass within `MAX_FIX_ATTEMPTS` iterations.

**On exhaustion:** terminate with `final_outcome = "exhausted"`. Surface
`df-healer --cxdb $CXDB` output. Do not auto-merge.

## Final summary (mandatory)

After all phases complete (success or exhaustion), emit:

```
Dark Factory Workflow Summary
=============================
Feature:            <FEATURE>
Phases run:         spec_validation → explore → plan → implement → gate → fix
Backend:            <BACKEND>
CXDB:               <CXDB>   (run `df-healer --cxdb <CXDB>` to re-cluster)
Final outcome:      PASS | FAIL | exhausted | stuck
Implement HEAD:     <git rev-parse HEAD>
Total steps:        <CXDB row count>
Fix attempts:       <n>/3

Per-phase verdicts:
  Phase 1 (spec_validation):  <verdict>  <sha>
  Phase 2 (explore):          <verdict>  <sha>
  Phase 3 (plan):             <verdict>  <sha>
  Phase 4 (implement):        <verdict>  <sha>
  Phase 5 (gate):             <verdict>  <sha>
  Phase 6 (fix loop):         <verdict>  <attempts used>
```

If `Final outcome: PASS`, the diff is ready for human review / merge per the
repo's standard PR process. If anything else, run `df-healer --cxdb $CXDB`
and surface the diagnosis.

## G3 closure note

The .dot engine's `max_visits="3"` bound is preserved by the Phase 6
`while [[ $attempt -lt $MAX_FIX_ATTEMPTS ]]` loop — both layers agree.
Runtime-determined fan-out (the original G3 gap) is closed by this
workflow itself: each phase is dispatched at workflow runtime as a
separate `dark-factory` invocation, so the fan-out count is decided by
the LLM at workflow-execution time, not baked into the .dot at parse
time. The .dot engine never needs to know how many phases a workflow
will run.

## Honesty rules (inherited from the dark-factory contract)

- Always quote the actual command run, the runner's exit code, and the
  `final_outcome` field from the CXDB-trace summary verbatim.
- Never claim a holdout passed if you read the scenarios in
  `~/projects/dark-factory-holdouts/`. If you did, declare the run
  **invalid** and rerun.
- If `--backend echo` was used, label the run as a wiring smoke, not a
  real validation.
- The Phase 5 hard tier (CXDB + /er + controller-owned calibrated review) is
  the merge-confidence bar. Passing Phase 4 tests alone is not.
