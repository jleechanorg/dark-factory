Write a **fresh, failing test** that reproduces the bug described in
`${goal}`.

## Hard rules — read carefully

You MUST NOT read or paraphrase any of the following to derive the test:

- The bug report text, issue body, PR description, or any comment that
  describes the bug.
- The expected-vs-actual section of any prior ticket.
- Any prior reproduction script that was written for this bug.
- Any code comment that says "this is wrong" or "fix this" near the bug.

Reading those sources and paraphrasing them into a test is **not** red/green
discipline — the existing code might already pass such a test, and the red
gate will fail.

## What you SHOULD do

1. Run the production code (or the test that the existing test suite points
   at) with a representative input. Observe the wrong output.
2. Reduce the wrong output to the smallest input that still demonstrates
   the failure.
3. Write a pytest test that asserts the *correct* output for that input.
   The test must fail under the current code.
4. Save the test under `tests/` (use a name like
   `tests/test_bug_<short-id>_<angle>.py`).
5. Set `state.bug_fix.test_path` to the relative path of the test file
   (e.g. `tests/test_bug_xyz_parsing.py`).

The red gate will run your test and assert it FAILS. If it passes, the
pipeline exits without attempting a fix — that's the discipline.

## Output contract

End your response with:

```
reproduce: <state.bug_fix.test_path>
```

If you cannot reproduce the bug from observed behavior alone, write
`reproduce: infeasible` and explain why. The red gate will exit the
pipeline.

## Scope

- Write ONE test file. Multiple tests inside are fine; one file is the unit.
- The test must be a real pytest test, not a script that exits non-zero.
- You have full read-write tool access to the current workspace. Use your tools to inspect, build, run tests, and verify the repository state as needed. Do not modify any production code in this node — only write the test.
