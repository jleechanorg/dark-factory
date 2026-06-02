---
description: "/factory-spec — create or review a Dark Factory spec with task-based pipeline selection"
type: quality
execution_mode: immediate
aliases: [fs]
---

# /factory-spec — Spec Create & Review Workflow

Create or review a spec through the Dark Factory pipeline. Classifies the task
(greenfield vs brownfield), picks the matching `.dot`, then plans — reports
choices for user approval.

**Install (once):**

```bash
./install.sh
export PATH="$HOME/.local/bin:$PATH"
```

Runs use **`dark-factory`** on PATH — not `python -m runner` from source.
Pipeline decision table:
[docs/pipeline-selection.md](../../docs/pipeline-selection.md)

**Usage**:

```
/fs <spec description>          # create / classify (default mode)
/fs --review <spec_path>        # review an existing spec
/fs --review                    # review spec/feature.md (default path)
/fs --show                      # show pipeline graphs (read-only reference)
/factory --pipeline <dot> ...   # execute after spec is ready
```

## Action

Parse `$ARGUMENTS` and execute the `factory-spec` skill workflow:

1. **Detect mode**: `--review` → review; `--show` → graph reference only;
   otherwise → create/classify mode
2. **Step 0:** greenfield vs brownfield (mandatory — see skill)
3. **Select pipeline** from `docs/pipeline-selection.md` when execution is next
4. **Run workflow** as defined in `.claude/skills/factory-spec/SKILL.md`
5. Report auto-chosen pipeline + options; ask user to confirm

To **run** after spec approval: `/f` or `/factory` with `--pipeline <chosen>`.
