# Visible acceptance for Attractor Spec Review

Run this command locally or in automation:

```bash
python scripts/validate_spec.py --spec spec/feature.md --report spec_review/validation_report.json
```

Pass criteria:

- Exit code `0`
- `spec_review/validation_report.json` contains valid JSON with:
  - top-level key `verdict` set to `"pass"` or `"fail"`,
  - top-level key `coverage`,
  - top-level key `line_checks`,
  - top-level key `issues`,
  - `coverage.reviewable_lines >= 90%` of all non-empty reviewable lines.
- A second independent review node writes JSON evidence at
  `spec_review/independent_reviewer.json`.

This is not the full guarantee of spec quality; it is the explicit public gate used
by the benchmark runtime before closing a loop.
