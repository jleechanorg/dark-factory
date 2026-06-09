---
description: "/factory-spec — create or review a Dark Factory spec with task-based pipeline selection"
type: quality
execution_mode: immediate
aliases: [fs]
---

# /factory-spec — Spec Create & Review Workflow

Create a reviewed spec via the `spec_gen` pipeline, or review / browse an
existing one. Classifies the task (greenfield vs brownfield), then either
**runs the pipeline** (create mode) or inspects in-session (review/show modes).

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
/fs <spec description>          # create mode — runs spec_gen pipeline
/fs --review <spec_path>        # review an existing spec (no pipeline run)
/fs --review                    # review spec/feature.md (default path)
/fs --show                      # show pipeline graphs (read-only reference)
/factory --pipeline <dot> ...   # execute feature pipeline after spec is ready
```

## Action

Parse `$ARGUMENTS` and execute the `factory-spec` skill workflow:

1. **Detect mode**: `--review` → review mode; `--show` → graph reference only;
   otherwise → create mode
2. **Step 0 (create mode only):** classify greenfield vs brownfield — feeds the
   goal/context string passed to the pipeline (see skill)
3. **Run workflow** as defined in `.claude/skills/factory-spec/SKILL.md`
4. **Create mode only:** report auto-chosen options and ask user to confirm
   before pipeline invocation. Review (`--review`) and show (`--show`) modes
   are in-session and read-only — no pipeline is invoked, so no confirmation
   step applies.
