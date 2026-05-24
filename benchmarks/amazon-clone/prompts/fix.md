# Fix Task

The evaluator has run and detected failures in your implementation. You need to fix the issues.

## What Happened

1. The evaluator ran automated tests against your implementation
2. Some checks failed
3. You need to analyze the feedback and make targeted fixes

## Feedback (Redacted)

The evaluator feedback is intentionally redacted — you cannot see the test source code. This is by design. You must work from the failure descriptions alone.

You will receive feedback in this format:

```
[Evaluator Output]
<failure description>

[Root Cause Analysis]
<problem identified>

[Suggested Fix]
<recommendation>
```

## Your Task

1. Read the failure description carefully
2. Identify the root cause in your code
3. Make the minimal fix that addresses the problem
4. Verify the fix doesn't break other functionality

## Important Constraints

- **Do not read the `holdouts/` directory** — this contains the evaluator tests and is sealed
- **Do not read sibling sealed evaluator paths** — exact scenarios and selectors are hidden
- **Do not attempt to game the tests** — make genuine fixes
- **Focus on the specific failure** — don't refactor unrelated code
- **Verify your fix** — run `make test` to confirm

## How to Debug

When you encounter a failure:

1. **Understand the flow** — Trace through what should happen
2. **Check the endpoints** — Ensure API routes exist and respond correctly
3. **Check the data** — Ensure models return expected data structures
4. **Check the frontend** — Ensure UI components call correct endpoints
5. **Run tests locally** — `make test` to reproduce the issue

## What NOT to Do

- Don't blame the evaluator or tests
- Don't greenwash ("it works on my machine" is not a fix)
- Don't skip failed criteria
- Don't add workarounds that mask the real issue
- Don't add debugging console.log statements

## Success Criteria

Your fix is complete when:
- The specific failure is resolved
- `make test` passes
- No new failures are introduced
