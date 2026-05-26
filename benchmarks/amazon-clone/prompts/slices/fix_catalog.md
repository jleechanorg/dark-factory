# Amazon Slice Fix: Catalog/Search/Reviews

The catalog/search/reviews public gate failed.

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

Repair only the catalog/search/reviews slice while preserving foundation
behavior:

- Product list/detail/search/filter/sort APIs.
- Review list/create/report/helpful APIs.
- Frontend catalog/detail/review surfaces.
- Seed data and tests needed by this slice.

Run:

```bash
bash benchmarks/amazon-clone/scripts/public_catalog.sh .
```

Fix the first concrete failure and do not weaken the public gate.
