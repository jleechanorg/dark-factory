# Amazon Slice Fix: Firestore/Auth Foundation

The Firestore/auth foundation public gate failed.

Use `benchmarks/amazon-clone/spec.md` as the visible product contract. Do not
read sealed holdouts, hidden scenarios, hidden evaluator source, or sealed
repositories.

## Visible Failure Context

- Last node: `${state._last_node}`
- Last outcome: `${state._last_outcome}`
- Last output:

```text
${state._last_output}
```

## Task

Repair only the foundation slice:

- Firestore emulator configuration and rules.
- Deterministic seed/reset.
- Root and `/api` auth/session/health/metrics/diagnostics endpoints.
- Makefile runtime/test/size commands.
- Foundation tests.

Run:

```bash
bash benchmarks/amazon-clone/scripts/public_foundation.sh .
```

Fix the first concrete failure and do not weaken the public gate.
