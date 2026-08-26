# Slim Cold-Review Fallback Plan

**Date:** 2026-08-26

## RED → GREEN record

1. Added focused tests for compact prompt semantics, exact JSON keys, empty
   `checks`, and return-code ordering.
2. RED evidence before implementation:

   ```text
   python -c 'import pytest,sys; raise SystemExit(pytest.main(["-q","tests/test_slim_cold_review.py"]))'
   3 failed, 4 passed
   ```

   The failures were the expected old checklist, hash-echo response parser,
   and subprocess parsing-before-return-code behavior.
3. Replaced the static authority with the compact semantic prompt, changed
   response validation to the exact JSON object, retained mechanical request
   and receipt fields, and added the fail-closed subprocess guard.
4. Updated contract fixtures and adjacent prompt assertions to the new public
   response seam.

## Verification

- `tests/test_slim_cold_review.py`, `tests/test_review_controller.py`, and
  `tests/test_review_cli.py`: 45 passed.
- Immutable-target, graph-controller, prompt-pinning consumers: 33 passed.
- Parallel reviewer and verdict consumers: 44 passed.
