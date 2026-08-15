Fix the current failing /ready gate and drive the PR to merge-ready.

Goal:
${goal}

PR /ready Gate Contract:
A PR is READY when ALL of the following hold, verified at the CURRENT head SHA:
1. /es — published evidence bundle gist linked in the PR body (`**Evidence**: <gist-url> (head <sha>)`)
2. /er — adversarial evidence review verdict PASS at current head
3. /advice — independent review APPROVE, with all review findings resolved
4. /green — deterministic test suite green and mergeable with no conflicts
5. Comments handled — all review comments and threads addressed

Previous node result (output from the failing gate):
${state._last_output}

Review Verdict:
${state._last_verdict}

Coder Handoff:
${state._last_coder_handoff}

Last test failure output (if test gate failed):
${state.last_test_output}

Rules:
- You have full read-write tool access to the workspace. Inspect, build, fix, and verify code locally.
- Fix only the failing gate findings; preserve all existing working contracts and tests.
- When /es or /er fails due to missing or stale evidence, generate and update the evidence bundle marker in the PR description pointing to the current HEAD SHA.
- When /advice or review comments fail, resolve each finding and address review feedback directly.
- When CI tests fail, fix root cause in production code or targeted tests without deleting valid assertions.
- After fixing, the runner will re-evaluate from deterministic CI tests through all /ready gates.
