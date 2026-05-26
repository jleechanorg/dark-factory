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

Render the `factory-spec` skill content inline: the three factory pipelines
(`gates.dot`, `hello.dot`, `minimal_feature.dot`) as ASCII flow diagrams,
the handler type registry, edge condition syntax, and backend routing table.

No side effects — read-only reference.
