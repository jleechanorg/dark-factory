---
description: "/factory-spec — create or review a Dark Factory spec (main + attractor) with codex cold review of both"
type: quality
execution_mode: immediate
aliases: [fs]
---

# /factory-spec — Two-Phase Spec Generation (Main + Attractor)

Create **two** reviewed specs via the `spec_gen` pipeline: a **main spec**
(`spec.md`) and an **attractor spec** (`attractor_spec.md`). The main spec
describes the implementation path; the attractor spec describes the
convergence goal — the stable end state the system must reach, plus the
anti-attractor states it must NOT converge to. A cold adversarial codex
reviewer signs off on **both** before exit.

**Install (once):**

```bash
./install.sh
export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$(pwd)}"
export PATH="$HOME/.local/bin:$PATH"
```

Runs use **`dark-factory`** on PATH — not `python -m runner` from source.
Pipeline decision table:
[docs/pipeline-selection.md](../../docs/pipeline-selection.md)

**Usage**:

```
/fs <spec description>          # create mode — runs spec_gen (main + attractor)
/fs --review <spec_path>        # review an existing main spec (no pipeline run)
/fs --review                    # review spec/feature.md (default path)
/fs --review-attractor <path>   # review an existing attractor spec
/fs --skip-attractor <desc>     # create mode but skip the attractor phase
/fs --show                      # show pipeline graphs (read-only reference)
/factory --pipeline <dot> ...   # execute feature pipeline after both specs ready
```

## Action

Parse `$ARGUMENTS` and execute the `factory-spec` skill workflow:

1. **Detect mode**: `--review` → review main spec; `--review-attractor` →
   review attractor spec; `--show` → graph reference only; `--skip-attractor` →
   create mode but skip the attractor phase; otherwise → full create mode
   (main + attractor).
2. **Step 0 (create mode only):** classify greenfield vs brownfield — feeds
   the goal/context string passed to the pipeline (see skill).
3. **Create mode only:** report auto-chosen options (pinned pipeline + goal
   string) and ask user to confirm **before any pipeline invocation**. Review
   (`--review` / `--review-attractor`) and show (`--show`) modes are
   in-session and read-only — no pipeline is invoked, so no confirmation
   step applies.
4. **Run workflow** as defined in `.claude/skills/factory-spec/SKILL.md`.
   Create mode is **pinned to `pipelines/slim/spec_gen.dot`** and runs it
   only after the step-3 confirmation. The pipeline produces BOTH
   `spec.md` (main) AND `attractor_spec.md` (attractor) by default;
   `--skip-attractor` short-circuits the attractor phase to a single-spec
   run.

## Pipeline summary (create mode)

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

The cold codex reviewer is the same vendor priority in both phases
(`codex,minimax,agy,claude-sonnet`, `prefer_adversarial=true`). Both phases
must sign off before the pipeline exits. The attractor spec is generated
**after** the main spec passes review (sequential, not parallel), so the
attractor spec can reference the codex-signed-off `spec.md` for the
consistency check.

The `spec_gen` pipeline's skill-side reference for both nodes (table
above) is the source of truth for node behavior; review modes do not
invoke any pipeline.
