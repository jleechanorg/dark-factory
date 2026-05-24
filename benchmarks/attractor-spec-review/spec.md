# General Attractor Spec Review Benchmark

## Goal

Validate a natural-language product spec in a way that is:

- line-aware (every visible spec line gets inspected),
- adversarial (an independent reviewer challenges ambiguity, incompleteness, and
  exploitability),
- reproducible (output and failure shape are machine-checkable).

The benchmark implements this by asking one agent to build the validator and run a
secondary independent `codex exec --yolo` reviewer.

## User story

As an operator comparing Attractor-style workflows, I want a repeatable process that
can validate whether a public spec is sufficient before giving it to a coding loop.

## Inputs

- `spec/feature.md` (the visible spec under test)
- existing repository scaffold in `starter/`

## Required implementation

Implement `scripts/validate_spec.py` with the following behavior:

1. Parse `--spec` (default: `spec/feature.md`).
2. Emit `--report` JSON file with:
   - `verdict` (`pass` | `fail`)
   - `coverage`
     - `total_lines`
     - `reviewable_lines`
     - `covered_lines`
     - `missing_lines`
   - `line_checks` (array with one entry per reviewable line)
   - `issues` (array of actionable blockers)
3. Exit code:
   - `0` when `verdict=pass`
   - non-zero when `verdict=fail`
4. Keep all checks in source files and do not depend on hidden evaluator code.

## What a "reviewable line" means

A non-empty, non-whitespace, non-comment line in `spec/feature.md` is reviewable.

## Public acceptance contract

The implementation is accepted when:

- `python scripts/validate_spec.py --spec spec/feature.md --report spec_review/validation_report.json`
- the JSON report is valid JSON,
- all required top-level keys exist,
- `coverage.reviewable_lines >= total_lines * 0.9`.

## Full variant requirements

The "full" pipeline also checks that visible stack artifacts exist:

- `backend/main.py`
- `frontend/index.html`
- `firestore.rules`

These are checked via a stack-smoke tool node and are required before the
independent reviewer node can pass.
