# Kilroy Review Task

The evaluator has found issues with your implementation. Time to review and fix.

## What Happened

The evaluator ran automated tests and detected failures in your implementation.

You will receive feedback describing:
- What test failed
- What behavior was expected
- What behavior was observed

## Your Task

### Step 1: Understand the Failures

Read each failure carefully:
- What is the test trying to verify?
- What is the actual behavior?
- Where in your code might the issue be?

### Step 2: Root Cause Analysis

Don't just look at the symptoms — find the root cause:

**Common root causes:**
- Missing endpoint implementation
- Wrong data structure returned
- Frontend not calling the right endpoint
- Input validation rejecting valid input
- Race condition in async code
- State not updating after mutation

### Step 3: Plan the Fix

Before writing code:
- Identify the exact change needed
- Consider how this affects other flows
- Plan the minimal change that fixes the issue

### Step 4: Implement and Verify

1. Make the fix
2. Run `make test` to verify
3. Manually test the affected flow
4. Check for regressions in related flows

## What NOT to Do

### Don't Blame the Evaluator

The evaluator is testing real behavior. If a test fails:
- Assume the test is correct
- Find why your code doesn't match the expected behavior
- Don't rationalize why the test might be wrong

### Don't Greenwash

"It works when I test it manually" is not evidence. If automated tests fail:
- The tests define the expected behavior
- You need to make the tests pass
- "Works on my machine" is not a valid argument

### Don't Skip Criteria

If you don't understand a failure:
- Read the spec again
- Trace through the code
- Ask clarifying questions

Don't skip the failing criteria and move on.

### Don't Add Workarounds

Don't add code that:
- Detects test environment and behaves differently
- Hardcodes expected values
- Masks the real issue

## Example: How to Think About Failures

**Failure: "Add to Cart does not update cart count"**

Bad response: "The evaluator must be testing it wrong. It works for me."

Good response:
1. Check if the frontend sends the correct API request
2. Check if the backend updates the cart
3. Check if the frontend updates the UI after the request
4. Find the missing link (maybe CORS, maybe missing state update, etc.)
5. Fix the root cause

## Success Criteria

Your fix is complete when:
- All previously failing tests pass
- `make test` passes
- No new failures introduced in related tests
- The actual user-facing behavior works correctly