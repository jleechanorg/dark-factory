---
description: "/factory — alias for /f; auto-routes to PR-mode when a PR is open, otherwise feature-mode"
type: quality
execution_mode: immediate
aliases: [df]
---

# /factory — Dark Factory DOT Pipeline Runner (alias for /f)

Dispatches to the **`dark-factory` binary** (`./install.sh` →
`~/.local/bin/dark-factory`). Single writer for the workflow is
`.claude/commands/f.md`; this file is a thin alias so the
auto-detect behavior (PR-mode vs feature-mode) is identical for both
entry points.

Unlike `/h` (in-Claude subagent dispatch), `/factory` runs the external
`.dot`-driven pipeline. Install once; run from any target repo cwd.

**Usage**:

```
/factory <goal>                          # auto-detect: PR-mode or feature-mode
/factory --pipeline gates <goal>         # explicit pipeline (skips auto-detect)
/factory --backend echo <goal>           # dry-run wiring smoke
/factory --feature hello <goal>          # override holdout feature key
```

When `--pipeline` is omitted, the skill must run the same **Step 0
detect context** logic as `/f` (see `.claude/commands/f.md`): if an
open PR exists for the current branch and the goal relates to it, route
to PR-mode; if no PR exists, route to feature-mode; if a PR exists but
the goal is unrelated, ask the user. Do not default to `gates.dot`,
`minimal_feature.dot`, or any single file for every run.

Equivalent to: `/f $ARGUMENTS`
