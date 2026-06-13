Identify and delete old, legacy, or dead code that is no longer needed, before implementing new logic.

Goal:
${goal}

Review the codebase and the plan in `spec.md` to identify:
1. Functions, classes, variables, or files that will be replaced, deprecated, or made obsolete by the new implementation.
2. Leftover commented-out code, debugging helpers, or unused imports.

Rules:
- You MUST delete obsolete code first rather than leaving it in place or commenting it out.
- Do not implement the new features yet; focus strictly on pruning and cleaning up the codebase to prepare for the implementation step.
- Verify that your deletions do not break existing compilation or unrelated tests.
- Record deleted entities and files in your final response.
