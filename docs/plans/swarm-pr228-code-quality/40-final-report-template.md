# 40 — Final report (template, populated after publishability gate)

## Header

- Mission: /swarm run /thermo and /code-standards on dark-factory (pr228)
- Branch: pr228 @ 658a715
- Date: 2026-07-11
- Lane agents: lane-a-runner, lane-b-tests, lane-c-config (sonnet)
- Cross-model reviewer: codex (model: TBD by dispatch)

## Executive summary

[2-3 sentences: scope, key findings, recommended action]

## Confirmed findings (after 3-lens verify + cross-model review)

[From 10-synthesis, only confirmed items]

### Blocker (must fix before merge)
1. ...

### Strong (should fix before merge)
1. ...

### Nit (nice-to-fix)
1. ...

## Rejected / downgraded findings

[From 10-synthesis, only rejected items with reason]

## Recommended actions

[Concrete next steps: which PR to file, which bead to open, etc.]

## Files in this docset

- `00-synthesis-template.md` (template, replaced by 10-synthesis)
- `01-lane-a-runner.md`
- `02-lane-b-tests.md`
- `03-lane-c-config-and-working-tree.md`
- `10-synthesis-confirmed-findings.md`
- `20-cross-model-review.md`
- `30-publishability-gate.md`
- `40-final-report.md` (this file)

## Provenance

- Workflow runId: TBD
- Agent counts: 3 mining + N verify + 1 cross-model
- Token spend: TBD
- Lane commits: TBD (each lane doc committed individually + final synthesis commit)

## Lessons for /learn

- [Pattern 1: symptom-patching vs root-cause-first in this codebase]
- [Pattern 2: silent invocation removal in shared scripts is a recurring regression class]
- [Pattern 3: third-party stub files masquerading as project hooks]