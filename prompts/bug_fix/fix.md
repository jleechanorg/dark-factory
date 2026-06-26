Fix the bug so the test at `${state.bug_fix.test_path}` passes.

## Test as oracle

That test was written by the previous `reproduce` node from observed
behavior, not from the bug report. Treat it as the source of truth for
"what correct looks like." Make the test pass without weakening it.

## Workflow

1. Read `${state.bug_fix.test_path}` to see the exact assertions.
2. Locate the production code under test (use the explore findings if
   present, otherwise grep for the relevant module/function).
3. Make the smallest change that turns the test green.
4. Do not weaken or skip the test.
5. Do not add new public surface area unless the test requires it.
6. Do not refactor unrelated code in the same change.

## Verification

The green gate will run `${state.bug_fix.test_path}` after you finish
and assert it PASSES. If it still fails, the pipeline routes you back
here for another iteration (up to 3 visits total).

## Rules

- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed.
- Address only the failing evidence.
- Preserve working code.
- Do not restart the implementation from scratch.

## Output

End your response with a one-line summary of the change:

```
fix: <one-line summary of the minimal fix>
```
