Refactor the code touched by this goal without changing its observable behavior.

Goal:
${goal}

Rules:
- Behavior must be preserved exactly: the same tests that pass before must pass
  after, with no assertion changes.
- Reduce duplication, clarify names, and tighten structure; do not add features.
- Keep each edit small and reversible; prefer extracting and reusing existing
  modules over introducing new abstractions.
- Net production lines should not grow — a refactor that adds code without
  removing at least as much is suspect; justify any increase.
- Record changed files and the test command proving behavior is unchanged.
