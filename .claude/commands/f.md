---
description: "/f — full Dark Factory loop; auto-selects pipeline for the task"
type: quality
execution_mode: immediate
aliases: [f]
---

# /f — Full Dark Factory Loop

Shortcut for `/factory` oriented toward **full production loops**, but the
pipeline is **chosen for the task** — not hardcoded to `minimal_feature.dot`.

Uses the installed **`dark-factory` binary** (`~/projects/dark-factory/install.sh`).

**Usage**:

```
/f <goal description>               # auto-select pipeline + claude backend
/f --backend echo <goal>            # dry-run wiring smoke (no LLM)
/f --feature <name> <goal>          # override holdout feature key
/f --pipeline <name> <goal>         # explicit pipeline (skips auto-select)
```

## Action

1. Parse `$ARGUMENTS`. If `--pipeline` is present, pass through to the skill.
2. If `--pipeline` is **missing**, invoke **factory-spec Step 0** (greenfield vs
   brownfield) and pick a pipeline from `docs/pipeline-selection.md`:
   - New feature, full loop → `pipelines/slim/minimal_feature.dot`
   - PR iteration → `pipelines/slim/minimal_pr.dot`
   - Replace/delete (brownfield) → brownfield goal + appropriate graph (often
     `minimal_feature.dot` with delete-first spec, or custom `.dot`)
   - Wiring only → `pipelines/factory/hello.dot`
3. **State the chosen pipeline and rationale** to the user, then run the
   `dark-factory` skill with `--pipeline <chosen> ...`.

Never inject `minimal_feature.dot` when another graph fits better.

The skill runs **`dark-factory`** from PATH; cwd = target implementation repo.
