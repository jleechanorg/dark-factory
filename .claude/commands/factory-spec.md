---
description: "/factory-spec — display Dark Factory pipeline node graphs and handler registry"
type: reference
execution_mode: immediate
aliases: [fs]
---

# /factory-spec — Dark Factory Spec Node Graph

Show the factory pipeline graph structure — node types, edges, conditions,
and handler mappings — without running a pipeline.

**Usage**:

```
/factory-spec        # show all pipelines + handler registry
/fs                  # alias
```

## Action

Render the `factory-spec` skill content inline: pipeline ASCII flow diagrams,
handler type registry, edge condition syntax, and backend routing table.

When the user wants to **run** a pipeline (not just view the graph), use
`/factory` or `/f` — those invoke the installed **`dark-factory` binary**
(`~/projects/dark-factory/install.sh`), not `python -m runner` from source.

No side effects for reference-only `/fs` — read-only graph display.
