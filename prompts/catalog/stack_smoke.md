Run a full-stack smoke check that the implemented feature boots and serves its
primary path end to end.

Goal:
${goal}

Rules:
- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.
- Exercise the real runtime path (start the service / invoke the entrypoint),
  not just unit mocks.
- Confirm the primary user-visible behavior from `spec.md` responds successfully
  at least once; capture the command and its raw output.
- Keep the check fast and deterministic; tear down any process you start.
- Report `success` only if the stack came up and the primary path returned the
  expected result; otherwise report `failure` with the captured error.
- Do not invoke or reference sealed holdout scenarios; this is a visible-path
  smoke only.
