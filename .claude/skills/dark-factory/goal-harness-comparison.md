<!-- On-demand detail for .claude/skills/dark-factory/SKILL.md (/f). -->

# `/f` versus `/h`

Versus `/h` / `/goal_harness`:

| Aspect | `/h` (goal_harness) | `/factory` (this skill) |
|--------|---------------------|-------------------------|
| Engine | Claude session dispatches subagents | **`dark-factory` binary** (uv install) |
| Artifact | Skill prose + tool calls | `.dot` graphviz file |
| Recording | Conversation transcript | SQLite CXDB row-per-step |
| Holdouts | Adversarial subagent reads diff | Sealed repo agent never sees |
| Reuse | Tied to this session | Replayable from `.dot` alone |
| Cost | High (multi-agent context) | Low (one CLI per node) |

Use `/factory` when you want the goal_harness idea as a reproducible external
pipeline; use `/h` when you want an interactive in-session loop.
