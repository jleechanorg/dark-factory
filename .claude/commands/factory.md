---
description: "/factory — run Dark Factory; auto-select pipeline when omitted"
type: quality
execution_mode: immediate
aliases: [df]
---

# /factory — Dark Factory DOT Pipeline Runner

Dispatches to the `dark-factory` skill → **`dark-factory` binary**
(`install.sh` → `~/.local/bin/dark-factory`).

**Usage**:

```bash
/factory <goal>                          # auto-select pipeline
/factory --pipeline gates <goal>
/factory --pipeline minimal_pr <goal>
/factory --backend echo <goal>
```

## Action

Run the `dark-factory` skill with `$ARGUMENTS`. When `--pipeline` is omitted,
the skill **must** classify the task and pick a graph from
`docs/pipeline-selection.md` — do not default to `gates.dot` or any single file.
