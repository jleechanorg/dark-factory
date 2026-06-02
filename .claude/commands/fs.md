---
description: "/fs — alias for /factory-spec — spec graphs, classification, pipeline pick"
type: quality
execution_mode: immediate
aliases: [fs]
---

# /fs — Alias for /factory-spec

Short alias for `/factory-spec`. See `factory-spec.md` for full documentation.

**Install (once):** `./install.sh` from this repo → `dark-factory` on PATH.

## Action

Execute `/factory-spec` with the same arguments passed to `/fs`.

Parse `$ARGUMENTS` and run the `factory-spec` skill workflow:

1. **Detect mode**: `--review` → review mode; `--show` → graph reference only;
   otherwise → create/classify mode
2. **Step 0 (mandatory before any run):** greenfield vs brownfield classification
3. **Pipeline pick:** use [docs/pipeline-selection.md](../../docs/pipeline-selection.md)
   when the user will run `/f` or `/factory` next — never assume one default `.dot`
4. **Run workflow** as defined in `.claude/skills/factory-spec/SKILL.md`
5. Report auto-chosen options and ask user to confirm or modify

When executing a pipeline after spec work, invoke `/factory` or `/f` with the
chosen `--pipeline` via the **`dark-factory` binary**.

Equivalent to: `/factory-spec $ARGUMENTS`
