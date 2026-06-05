Write or extend deterministic tests for the feature described in `spec.md`.

Goal:
${goal}

Rules:
- Cover the public behavioral expectations stated in the visible spec.
- Prefer fast, hermetic unit tests; add an integration test only when the spec's
  behavior cannot be proven at the unit level.
- Each test must assert observable behavior, not implementation details.
- Do not write, infer, or encode hidden holdout scenarios or evaluator cases.
  Sealed validation runs separately in the runner.
- Record the exact test command and its raw pass/fail output in your final
  response so the runner can re-run it deterministically.
