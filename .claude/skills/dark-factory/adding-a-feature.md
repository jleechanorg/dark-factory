<!-- On-demand detail for .claude/skills/dark-factory/SKILL.md (/f). -->

# Adding a new sealed-holdout feature

To add a new sealed-holdout feature `foo`:

1. Write `$DARK_FACTORY_HOME/specs/foo.md` (agent-visible).
2. Write `$DARK_FACTORY_HOME/prompts/foo/{plan,implement,fix}.md`.
3. Write `~/projects/dark-factory-holdouts/holdouts/foo/scenarios.yaml`
   in the sealed repo (the implementing agent must never read this).
4. Either reuse `pipelines/factory/hello.dot` (which is feature-agnostic) by
   passing `--feature foo`, or write `pipelines/factory/foo.dot` with custom
   nodes.
