Investigate the codebase and produce the findings needed to build this goal.

Goal:
${goal}

Rules:
- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.
- Identify the exact files, functions, and call sites the change must touch, with
  `path:line` references.
- Surface existing modules or helpers to reuse rather than reimplement.
- Call out constraints, edge cases, and prior art relevant to the goal.
- Do not seek out, read, or summarize sealed holdout scenarios or evaluator
  internals; work only from the visible spec and repository.
- End with a concise, actionable plan the implement node can follow directly.
