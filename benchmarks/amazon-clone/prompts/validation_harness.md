# Validation Harness Task

Implement the validation harness required by `benchmarks/amazon-clone/spec.md`.

## Required Work

- `make test` runs lint, unit tests, API integration tests, browser checkout
  flow, Firestore rules probes, and source-size checks.
- `make validate-size` reports counted files, excluded files, total
  non-generated source lines, frontend lines, backend lines, and support lines.
- Browser validation covers search, filter, detail, add to cart, login, address
  selection, coupon, checkout, confirmation, and order history.
- Validation fails when the Firestore emulator is unavailable.
- Validation writes a JSON summary with pass/fail status for each major area.

Do not inspect sealed holdouts or hidden evaluator paths.
