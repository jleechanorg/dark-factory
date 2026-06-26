Revise `attractor_spec.md` to address the findings from the attractor
spec review.

Goal:
${goal}

Read the review verdict and findings from the previous
`review_attractor` step. Then open `attractor_spec.md` (or
`.dark-factory/attractor_spec.md` if present) and apply the minimum
changes required to resolve each blocking finding.

Rules:
- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.
- Address only the failing findings. Do not rewrite sections that
  passed.
- Do not add implementation code, stubs, or skeleton files — the
  attractor spec describes the end state, not code.
- Do not invent convergence criteria or verification commands that
  are not grounded in the goal and explore findings. If you cannot
  derive a deterministic verification command from the codebase,
  state the gap explicitly in the attractor spec rather than
  fabricating one.
- Preserve `spec.md` (the main spec) as the source of truth for the
  implementation path. The attractor spec is the goal-state
  complement; if a fix to the attractor spec would invalidate the
  main spec, the fix is wrong — report and stop.
- If the review found an anti-attractor state gap, add at least one
  specific, observable state the system MUST NOT converge to, with a
  deterministic check that proves the system has not reached it.
  Vague anti-states such as "the system should not be broken" are
  not acceptable — be specific (e.g., "the system is NOT converged
  when `world_logic.py` still contains the source=server 2nd writer"
  with the verification command `grep -n 'source=server' mvp_site/
  world_logic.py`).
- If the review found a consistency-with-main-spec gap, update the
  attractor spec to reference the same lanes, file-ownership
  matrix, test commands, and acceptance criteria as `spec.md`. Use
  file paths + line ranges (or section headers) for cross-references.
- Preserve the overall attractor spec structure. Edit in place
  rather than rewriting from scratch.
- After editing, state which findings you addressed and confirm the
  blocking items are resolved.

Do not implement yet. Do not change any production code files.
Do not change `spec.md` (the main spec) — that is fixed by the
main-spec review loop, not the attractor-spec loop.
