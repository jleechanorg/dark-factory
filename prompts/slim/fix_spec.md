Revise `spec.md` to address the findings from the spec review.

Goal:
${goal}

Read the review verdict and findings from the previous `spec_review` step. Then open `spec.md` (or `.dark-factory/spec.md` if present) and apply the minimum changes required to resolve each blocking finding.

Rules:
- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.
- Address only the failing findings. Do not rewrite sections that passed.
- Do not add implementation code, stubs, or skeleton files — the spec describes intent, not code.
- Do not invent acceptance criteria or test commands that are not grounded in the goal and explore findings. If you cannot derive a deterministic test command from the codebase, state the gap explicitly in the spec rather than fabricating one.
- If the review found a missing file-ownership matrix (parallel lanes without single-writer assignment), add the matrix now: list every file, assign exactly one owning lane, and flag serialization requirements. Do not introduce new parallel lanes as a fix.
- Preserve the overall spec structure. Edit in place rather than rewriting from scratch.
- After editing, state which findings you addressed and confirm the blocking items are resolved.

Do not implement yet. Do not change any production code files.
