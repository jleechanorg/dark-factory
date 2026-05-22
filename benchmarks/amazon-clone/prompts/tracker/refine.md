# Tracker Refine Task

The evaluator has found issues with your implementation. Analyze the failures and refine your approach.

## Context

The evaluator ran automated tests against your implementation and found failures. You need to:
1. Analyze what failed
2. Understand the root cause
3. Refine your approach
4. Make targeted improvements

## What You Know

You will receive feedback describing:
- Which tests failed
- What behavior was expected
- What behavior was observed

The evaluator feedback is intentionally anonymized — you cannot see the test source code. Work from the failure descriptions.

## Your Task

### Step 1: Categorize Failures

Group failures into categories:

**Implementation Gap**
- Missing feature or incomplete implementation
- Fix: Implement the missing piece

**Integration Bug**
- Feature exists but doesn't work correctly
- Fix: Debug and fix the specific issue

**Architectural Issue**
- The approach itself is flawed
- Fix: Refactor the underlying structure

**Edge Case**
- Works in normal flow but fails for boundary conditions
- Fix: Add handling for the edge case

### Step 2: Root Cause Analysis

For each failure category:

**What specifically failed?**
Be specific — not "checkout doesn't work" but "order creation fails when cart has more than 5 items"

**Why did it fail?**
Trace the code path:
- Where does the expected behavior diverge from actual?
- Is it in the frontend, backend, or both?

**What's the minimal fix?**
- What's the smallest change that fixes the issue?
- Does this change affect other flows?

### Step 3: Refine Approach

Based on your analysis, consider if your approach needs refinement:

**Questions to ask:**
- Is the data model correct?
- Are the API endpoints structured correctly?
- Is the frontend calling the right endpoints?
- Is error handling sufficient?
- Are there race conditions or timing issues?

**When to refactor:**
- The root cause is in the architecture, not the implementation
- Multiple failures share a common underlying issue
- A targeted fix would require hacks

**When NOT to refactor:**
- The issue is isolated
- A targeted fix will work
- Refactoring introduces risk of new failures

### Step 4: Implement Fixes

For each failure:

1. Make the targeted fix
2. Run tests to verify
3. Check for regressions

### Step 5: Verify

After all fixes:
- `make test` passes
- No new failures in other tests
- The actual user-facing behavior works

## Don't

**Don't chase symptoms:**
- "Cart doesn't update" is a symptom
- Find the root cause (wrong endpoint? wrong data? missing state update?)

**Don't over-engineer:**
- Fix what's broken
- Don't rebuild everything from scratch

**Don't greenwash:**
- If tests fail, the tests define the expected behavior
- Make the tests pass, don't argue they should pass

## Output

Update the relevant code files. Keep a log of:
- What failed
- What you changed
- Why this fixes it

## Success Criteria

- All previously failing tests pass
- No new failures introduced
- The implementation is more robust than before