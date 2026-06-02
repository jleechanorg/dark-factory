---
description: "/factory — run Dark Factory; auto-select pipeline when omitted"
type: quality
execution_mode: immediate
aliases: [df]
---

# /factory — Dark Factory DOT Pipeline Runner

Dispatches to the `dark-factory` skill → **`dark-factory` binary**
(`./install.sh` → `~/.local/bin/dark-factory`).

Unlike `/h` (in-Claude subagent dispatch), `/factory` runs the external
`.dot`-driven pipeline. Install once; run from any target repo cwd.

**Usage**:

```
/factory <goal>                          # auto-select pipeline
/factory --pipeline gates <goal>
/factory --pipeline minimal_pr <goal>
/factory --backend echo <goal>
/factory --feature hello <goal>
```

## Action

Run the `dark-factory` skill with `$ARGUMENTS`. When `--pipeline` is omitted,
the skill **must** classify the task (factory-spec Step 0) and pick a graph from
[docs/pipeline-selection.md](../../docs/pipeline-selection.md) — do not default to
`gates.dot`, `minimal_feature.dot`, or any single file for every run.

Uses `dark-factory` on PATH — not `python -m runner` from source.
