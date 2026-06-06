Fix the current failing gate.

Goal:
${goal}

Use the latest evidence bundle, test output, review findings, and holdout output.

Rules:
- Address only the failing evidence.
- Preserve working code.
- Do not restart the implementation from scratch.
- Treat sealed holdout feedback as a redacted gate verdict. Do not ask for,
  infer, or encode hidden evaluator scenarios.
- After fixing, the runner will go back to deterministic tests.
