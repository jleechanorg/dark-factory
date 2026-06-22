---
description: "/fs — generate main + attractor specs with codex cold review. DEFAULT: dispatches Phase 1 (spec_validation) of the .claude/workflows/dark-factory.md workflow. Pass --legacy to use the legacy spec_gen pipeline directly."
type: quality
execution_mode: immediate
aliases: [fs]
---

# /fs — Spec Generation (workflow Phase 1 by default)

`/fs` produces **two** reviewed specs:
- `spec.md` — the main spec (acceptance criteria, test command, non-goals,
  lane-independence matrix)
- `attractor_spec.md` — the attractor spec (convergence target, observable
  convergence criteria, anti-attractor states)

**Default invocation path** is **Phase 1 (spec_validation) of the
`.claude/workflows/dark-factory.md` workflow** — the workflow runs the
public spec through the spec-validation benchmark first, so the spec is
graded before any code is written. The workflow then hands off to explore
/ plan / implement if Phase 1 passes; if Phase 1 fails, the workflow
stops and surfaces the spec-validation trace.

Pass `--legacy` to skip the workflow and run the original
`pipelines/slim/spec_gen.dot` pipeline directly (two-phase
main+attractor generation with codex cold review).

**Usage**:

```
/fs <spec description>                  # DEFAULT: workflow Phase 1
/fs --legacy <spec description>         # legacy spec_gen pipeline
/fs --skip-attractor <description>      # main spec only (workflow or legacy)
/fs --review <spec_path>                # review an existing main spec
/fs --review-attractor <path>           # review an existing attractor spec
/fs --show                              # graph reference only (read-only)
```

## Action

1. Parse `$ARGUMENTS`.
2. If `--review`, `--review-attractor`, or `--show` is present, handle
   in-session as before (read-only, no pipeline invoked).
3. If `--legacy` is present OR `--pipeline spec_gen` was passed, fall
   through to **Legacy dispatch** below.
4. Otherwise, this is a **workflow invocation**:

   ```
   Skill("dark-factory",
         args="--mode feature --phase-until spec_validation \
               --backend <echo|claude> --feature <name> \
               --goal 'Generate spec.md + attractor_spec.md for <description>. \
                       Stop after Phase 1 (spec_validation) passes.'")
   ```

   The workflow runs Phase 1 (spec_validation) against
   `benchmarks/attractor-spec-review/pipelines/review_slim.dot`. On pass,
   it continues to Phase 2 (explore) and Phase 3 (plan). To stop after
   Phase 1, pass `--phase-until spec_validation`.

### Step 0: detect context (workflow path only)

For the workflow path, classify greenfield vs brownfield so the workflow
can frame the spec_validation goal appropriately:

- **Greenfield** (new feature, no existing code in the target repo): the
  spec is the source of truth; workflow uses `spec.md` as-is.
- **Brownfield** (existing code, replacement/delete work): the spec must
  enumerate what stays, what goes, and the net-LOC constraint per
  `factory-spec` delete-first rules.

This is informational; the workflow's spec_validation phase will surface
both possibilities to the spec author.

### Workflow path: what gets produced

The workflow's spec_validation phase produces:

1. `spec.md` (main spec) — passed through `review_slim.dot`'s cold review.
2. `attractor_spec.md` (attractor spec) — generated **after** main spec
   passes review (sequential, not parallel). The attractor spec can
   reference the codex-signed-off `spec.md` for the consistency check.

Both files land in `specs/<feature>/` of the target repo. The CXDB
records the per-line validation verdicts.

**Pass criterion:** `final_outcome == "success"` for Phase 1 with all
spec line-items passing (or marked `warn` with documented mitigation).

### Legacy dispatch (`--legacy` or `--pipeline spec_gen`)

Read `.claude/skills/factory-spec/SKILL.md` for full mode details. The
legacy path is the original `pipelines/slim/spec_gen.dot` pipeline:

```
start → explore_in → explore_fanout → {explore_concept, explore_auth,
       explore_reuse, explore_risks} → explore_join → explore_stitch →
       explore_out
     → plan_main        [plan.md → spec.md]
     → review_main      [codex cold review of spec.md]              ─┐
     → plan_attractor   [plan_attractor.md → attractor_spec.md]     │ if main review fails → fix_main → review_main
     → review_attractor [codex cold review of attractor_spec.md]    ─┐
     → exit                                                            │ if attractor review fails → fix_attractor → review_attractor
```

## Honesty rules

- Quote the **actual** `Skill()` or `dark-factory` command run.
- If the workflow's Phase 1 fails, **surface the trace** and stop. Do not
  silently fall through to legacy or assume "we can fix the spec later."
- If `--backend echo` was used, label the run as a wiring smoke, not a
  real validation. Echo-mode review verdicts are not real LLM verdicts.
- Do not duplicate the workflow steps inline in this file. The
  workflow at `$DARK_FACTORY_HOME/.claude/workflows/dark-factory.md`
  is the single source of truth for spec_validation behavior.

## Why the workflow is the default

Same reasoning as `/f`: the user's 2026-06-22 architectural pivot
preferred default primitives over custom code. The workflow's
spec_validation phase is the spec-validation benchmark wired into a
multi-phase orchestrator — no engine changes required.

Use **legacy** dispatch when:
- The user explicitly opts in (`--legacy` / `--pipeline spec_gen`)
- The user wants to iterate spec generation in isolation from
  explore/plan/implement
- The user is debugging the spec_gen pipeline itself

## See also

- `/factory-spec` — detailed skill workflow for legacy spec generation
- `/f` — full Dark Factory loop; uses the same workflow by default
- `$DARK_FACTORY_HOME/.claude/workflows/dark-factory.md` — workflow
  source of truth for spec_validation behavior