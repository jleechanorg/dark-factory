# Spec Review Task

Review `benchmarks/amazon-clone/spec.md` before implementation.

## Goal

Identify the public product contract, implementation-size requirement, launch
contract, role boundaries, data collections, required views, and validation
surfaces.

## Output

Return a concise implementation checklist grouped by:

- Product surfaces.
- Backend API and validation.
- Firestore data and rules.
- Seed/reset data.
- Tests and validation harness.
- Source-size evidence.

Do not inspect sealed holdouts or hidden evaluator paths.
