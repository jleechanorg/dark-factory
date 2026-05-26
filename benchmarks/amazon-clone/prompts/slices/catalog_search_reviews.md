# Amazon Slice: Catalog, Search, Product Detail, Reviews

Extend the current Amazon candidate with the catalog/search/reviews slice from
`benchmarks/amazon-clone/spec.md`.

## Prerequisite

Assume the Firestore/auth foundation slice already exists in this workspace.
Do not remove or weaken foundation behavior, Make targets, seed/reset,
diagnostics, auth/session, or source-size validation.

## Scope

Implement:

- Root and `/api` product endpoints:
  - `GET /products`
  - `POST /products`
  - `GET /products/:id`
  - `PATCH /products/:id`
  - `POST /products/:id/archive`
  - `POST /products/:id/restock`
- Search, department filtering, sorting, pagination, and empty-results behavior.
- Product detail payload with title, brand, department, description, price,
  list price, images, rating, review count, inventory, seller info, and active
  status.
- Review endpoints:
  - `GET /reviews`
  - `POST /reviews`
  - `POST /reviews/:id/report`
  - `POST /reviews/:id/helpful`
- Product-scoped review aliases such as `GET /products/:id/reviews` and
  `POST /products/:id/reviews` if useful for UI/browser flows.
- Frontend catalog, search results, department filter, product detail, quick
  view, reviews list, review submission, helpful/report actions, and no-results
  view.
- Tests for products/search/filter/detail/reviews.

## Execution

Use Antigravity subagents or parallel workers if available:

- one lane for backend product/review APIs,
- one lane for frontend catalog/detail/review UI,
- one lane for seed data and validation tests.

Integrate all outputs into one runnable workspace.

## Local proof

Before exiting, run:

```bash
bash benchmarks/amazon-clone/scripts/public_catalog.sh .
```

If it fails, fix the first concrete failure and rerun it.
