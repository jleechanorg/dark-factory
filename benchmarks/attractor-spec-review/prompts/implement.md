Implement `scripts/validate_spec.py` and supporting project files needed by the selected pipeline.

Goal:
${goal}

Requirements:

1. Build a robust line-aware validator for `spec/feature.md`.
2. Emit report JSON matching `visible_acceptance.md` / `spec.md` expectations.
3. Keep output deterministic and offline.
4. Exit non-zero when report quality is insufficient.

Expected command behavior:

```bash
python scripts/validate_spec.py --spec spec/feature.md --report spec_review/validation_report.json
```

Implementation shape:

- Use only stdlib modules.
- Read file safely and preserve UTF-8 line ordering.
- Report per-line status and reasons.
- Include a deterministic `generated_at` ISO timestamp.

Rules:
- Do not add hidden eval dependency.
- Do not read evaluator internals.
- Preserve existing files unless changes are required by the spec.
- If full pipeline checks are failing, ensure `backend/main.py`,
  `frontend/index.html`, and `firestore.rules` are present and meaningful.
