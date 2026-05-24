Fix the implementation using the latest pipeline evidence.

Goal:
${goal}

Use these failure sources:

- `spec_review/validation_report.json` (if it exists)
- last run output for reviewer/acceptance command.

Rules:
- Only address concrete blockers.
- Keep edits minimal and deterministic.
- Re-run your own local command from visible acceptance after each fix.
- Preserve compatibility with both slim and full pipelines.
- Do not invent acceptance tests not listed in `visible_acceptance.md`.
