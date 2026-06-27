Fix the current failing gate.

Goal:
${goal}

Use the latest evidence bundle, test output, review findings, and holdout output.

Previous node result (free-form output from the gate/reviewer/tool that routed
here):
${state._last_output}

The runner wires the most recent goal_gate test failure into ctx.state
when present. If `last_test_output` is set, treat it as the source of
truth for what is actually broken — read the failing test stderr/stdout
below before proposing a fix.

Test command that failed:
${state.last_test_command}

Test returncode:
${state.last_test_rc}

Last test output (stdout + stderr, head-truncated to 4000 chars):
${state.last_test_output}

Rules:
- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.
- Address only the failing evidence.
- Preserve working code.
- Do not restart the implementation from scratch.
- Treat sealed holdout feedback as a redacted gate verdict. Do not ask for,
  infer, or encode hidden evaluator scenarios.
- If the test command references files that do not exist (e.g.
  `file or directory not found`), the fix is in the test command or
  state injection, not in the implementation. Update the test_command
  in the goal/spec to point at files that actually exist (or to use
  pytest test discovery), then re-run.
- After fixing, the runner will go back to deterministic tests.
