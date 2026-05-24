# Benchmark Design — airbnb-clone (Dark Factory)

**Status**: Design (2026-05-24)
**Target**: 90 tasks across 3 sprints, ~36 hours of agent wall-time, full-stack production-grade Airbnb clone.
**Inspired by**: AgentLoop's [Airbnb Clone Case Study](https://www.agentloop.run/blog/airbnb-clone-case-study) — 3 prompts → 87 tasks via product-manager / engineer / qa-tester loop.
**Differences from source**: **Firestore (local emulator) replaces Supabase** end-to-end. No Postgres, no RLS, no Supabase Storage — Firestore Security Rules + Firestore + Firebase Storage emulator.

This file is **not** the spec the implementing agent sees — it's the operator-facing design. The agent gets `spec.md` and the 3 sprint prompts in `prompts/`. The sealed acceptance tests live in the holdouts sibling repo (operator-only access).

---

## 1. Why this benchmark

The existing `benchmarks/amazon-clone/` and `benchmarks/fibonacci/` exercise the **DOT runner**, **conformance surface**, and **scoring path**, but neither stresses the runner with multi-day, multi-LLM-budget, real-product workloads. AirbnbClone fills the middle of that gap:

- **Bigger than fibonacci** (a tiny algorithmic task)
- **Smaller than amazon-clone** in surface-area-per-task, but **much higher coordination cost** (auth + storage + payments + maps + real-time availability)
- **Reproduces a real public benchmark** (AgentLoop's), so dark-factory's score is directly comparable
- **Forces all six graph nodes per sprint** — every sprint has natural fix-loops (RLS bugs, popover races, map clustering edge-cases) that exercise the `verify → fix → verify → exit` cycle on real failures

## 2. Tech stack swap — Firestore for Supabase

| Concern | Source (Supabase) | This benchmark (Firestore) |
|---|---|---|
| Auth | Supabase Auth (email, Google, GitHub) | Firebase Auth emulator (email, Google, GitHub) |
| Database | Postgres + RLS | Firestore + Security Rules |
| Storage | Supabase Storage buckets | Firebase Storage emulator |
| Server-side functions | Postgres functions / Supabase Edge Functions | Cloud Functions emulator (or Next.js server actions) |
| Real-time | Supabase Realtime subscriptions | Firestore `onSnapshot` listeners |
| Migrations | Supabase CLI SQL migrations | Firestore has no schema; **structural migration scripts** under `firestore/migrations/` instead, applied at seed time |
| Type generation | `supabase gen types typescript` | Manual / `zod-to-ts` from validation schemas |
| RLS-equivalent testing | Supabase RLS test framework | Firebase Rules emulator's `@firebase/rules-unit-testing` harness |
| MCP | Supabase MCP for schema/RLS inspection | Firestore CLI + `firebase emulators:exec` + custom inspection scripts |

The agentloop case study cites Supabase MCP heavily — for our run we expose equivalent inspection via:
- `firebase emulators:start --only firestore,auth,storage,functions`
- A small `scripts/inspect_firestore.ts` helper that queries the emulator's REST API

## 3. Frozen surface for the implementing agent

The agent sees only:
- `spec.md` — high-level feature spec (acceptance, non-goals, tech stack constraints)
- `prompts/sprint-{1,2,3}-{plan,implement,fix}.md`
- `starter/` — a minimal scaffold: Next.js 14 App Router + Firebase emulator config + Tailwind + Shadcn pre-installed, **no business logic**
- `visible_acceptance.md` — public-facing acceptance criteria the agent CAN read

The agent never sees:
- The sealed scoring rubric (scenario manifest, sealed in the holdouts repo)
- Production-grade Playwright + Firestore Rules tests (sealed)
- Adversarial Lighthouse / Axe / Pa11y / RLS-leak probes

## 4. Sprint structure — 3 prompts, 90 tasks

### Sprint 1 — Data Layer (28 tasks)

The "database" sprint. Data model, security rules, indexes, seed data.

1. `firestore-emulator-config` — `firebase.json`, `.firebaserc`, emulator ports
2. `firestore-rules-skeleton` — `firestore.rules` with default deny
3. `firestore-storage-rules-skeleton` — `storage.rules` with default deny
4. `firestore-indexes-config` — `firestore.indexes.json`
5. `firestore-emulator-seed-script` — `scripts/seed.ts` deterministic fixture loader
6. `collection-users` — `users/{uid}` shape + admin SDK helpers (extends Firebase Auth)
7. `collection-listings` — `listings/{listingId}` shape + denormalized owner ref
8. `collection-listing-images` — subcollection or top-level with `listingId` foreign key
9. `collection-amenities` — `amenities/{slug}` reference collection
10. `collection-listing-amenities-join` — denormalized array on listings doc OR junction collection
11. `collection-availability` — `availability/{listingId}/dates/{yyyy-mm-dd}` (sharded by listing)
12. `collection-bookings` — `bookings/{bookingId}` with guest, host, listing refs
13. `collection-reviews` — `reviews/{reviewId}` with `listingId`, `authorId`, `rating`
14. `collection-favorites` — `users/{uid}/favorites/{listingId}`
15. `indexes-listings-search` — composite indexes (location, price, type, guests)
16. `indexes-bookings-by-host` — `bookings where hostId == X order by checkIn desc`
17. `indexes-reviews-by-listing` — `reviews where listingId == X order by createdAt desc`
18. `indexes-availability-window` — range queries on date subcollection
19. `rules-users-self-write-only` — auth `request.auth.uid == userId`
20. `rules-listings-public-read-owner-write` — anyone reads, owner mutates
21. `rules-bookings-guest-host-only` — guest + host can see; nobody else
22. `rules-reviews-author-write-public-read` — author writes, everyone reads
23. `rules-favorites-self-only` — user-scoped under `users/{uid}/favorites`
24. `rules-storage-listing-images-owner-write` — listing owner uploads, public read
25. `function-availability-blocker` — Cloud Function: on `bookings/onCreate`, mark date range unavailable
26. `function-rating-aggregator` — Cloud Function: on `reviews/onCreate|onUpdate`, recompute `listings.rating` + `reviewCount`
27. `function-booking-total-calculator` — Cloud Function: validates nightly rate × nights + fees on `bookings/onCreate`
28. `seed-100-listings-with-images` — deterministic fixture: 100 listings, 6 images each, varied locations + amenities

### Sprint 2 — Backend Layer (30 tasks)

Server actions, validation, payments, search, storage upload.

29. `firebase-admin-init` — server-side admin SDK with emulator detection
30. `firebase-client-init` — browser SDK with emulator detection in dev
31. `auth-provider-google-emulator` — Google OAuth via emulator
32. `auth-provider-github-emulator` — GitHub OAuth via emulator
33. `types-from-firestore-schema` — TypeScript types per collection (manual since no SQL)
34. `zod-listings-schema` — Zod for listing create/update
35. `zod-bookings-schema` — Zod for booking create
36. `zod-reviews-schema` — Zod for review create
37. `zod-search-filters-schema` — Zod for search query params
38. `server-action-create-listing` — multi-step form server action
39. `server-action-update-listing` — owner-only
40. `server-action-delete-listing` — owner-only soft delete
41. `server-action-search-listings` — filter+sort+paginate
42. `server-action-list-listing-by-id` — public read
43. `server-action-upload-listing-image` — Firebase Storage upload + thumbnail
44. `server-action-create-booking` — runs availability check + total calculation
45. `server-action-cancel-booking` — guest-initiated
46. `server-action-create-review` — only after stay completed
47. `server-action-toggle-favorite` — user collection write
48. `server-action-list-user-bookings` — auth scoped
49. `server-action-list-user-favorites` — auth scoped
50. `stripe-init-payment-intent` — Stripe PaymentIntent on `server-action-create-booking`
51. `stripe-webhook-handler` — `/api/webhooks/stripe` route handler with signature verification
52. `stripe-confirmation-flow` — confirm intent + finalize booking
53. `stripe-refund-flow` — refund on `server-action-cancel-booking`
54. `availability-check-helper` — pure function used by `create-booking` + UI
55. `search-text-tokenizer` — simple substring + location prefix matcher (Firestore can't do full-text)
56. `image-upload-thumbnail-pipeline` — Cloud Function trigger generates 3 sizes
57. `error-handling-server-actions` — consistent `{ok, error}` envelope
58. `rate-limit-server-actions` — Firestore-based token bucket per IP/user

### Sprint 3 — Frontend Layer (32 tasks)

UI, state, interactions, mobile.

59. `shadcn-ui-init` — neutral color, CSS variables
60. `zustand-search-filters-store` — query, dates, guests, price range, amenities
61. `tanstack-query-config` — provider, default options, devtools-off in prod
62. `app-shell-with-auth-context` — `RootLayout` + Firebase Auth provider
63. `home-page-featured-grid` — top 12 listings, lazy images
64. `expandable-searchbar` — collapsed → expanded transition with Radix Popover
65. `searchbar-location-input` — text input with country/city suggestions
66. `searchbar-date-range-popover` — AvailabilityCalendar component (controlled — see notable hard task below)
67. `searchbar-guests-popover` — adults/children/infants/pets steppers
68. `searchbar-search-button` — fires the actual query
69. `search-results-page` — grid + sidebar map, URL-synced
70. `search-results-grid` — listing cards with hover
71. `interactive-map-with-price-markers` — Mapbox or Leaflet + price-bubble markers
72. `map-marker-clustering` — Supercluster or equivalent
73. `listing-detail-page` — gallery + amenities + map + booking widget + reviews
74. `image-gallery-component` — Embla carousel with thumbs
75. `amenities-display` — categorized icons
76. `reviews-display` — paginated with rating breakdown
77. `review-creation-form` — only enabled post-stay
78. `multi-step-new-listing-form` — 5 steps (property → location → details → photos → review)
79. `booking-widget` — date pickers + guests + price breakdown + Stripe checkout button
80. `user-dashboard-tabs` — Profile / My Listings / My Bookings / Favorites
81. `mobile-responsive-search` — touch-friendly searchbar collapse
82. `mobile-responsive-dashboard` — tab → bottom-nav swap
83. `mobile-responsive-forms` — single-column at <640px
84. `loading-states-skeletons` — `app/loading.tsx` for each route
85. `error-boundaries` — `app/error.tsx` per route
86. `empty-states` — "no listings yet", "no bookings", etc.
87. `404-page` — `app/not-found.tsx`
88. `favicon-and-meta` — OG tags, favicon
89. `accessibility-keyboard-nav` — searchbar + gallery + modal focus management
90. `lighthouse-budget-pass` — perf > 80, a11y > 90 on home + listing detail (Lighthouse CI in verify)

### Notable hard tasks (per AgentLoop case study, reproduced verbatim)

- **Task 66 — `searchbar-date-range-popover`**: the AgentLoop run hit a Radix Popover onBlur race that collapsed the searchbar before the popover could open. Fix requires `setTimeout(..., 0)` deferred collapse + converting `AvailabilityCalendar` to controlled + `onInteractOutside`. This is the **forcing function** for Sprint 3's fix loop — the visible spec describes "the popover stays open while the user picks a range" but doesn't describe the bug; the hidden test asserts no premature collapse.
- **Task 25 — `function-availability-blocker`**: Cloud Function trigger ordering vs `server-action-create-booking` is racy. Hidden test creates two simultaneous booking requests for overlapping dates; only one must succeed.
- **Task 21 — `rules-bookings-guest-host-only`**: Hidden Rules test attempts to read another user's bookings unauthenticated, as a different user, and as the host of a different listing. All must be denied.

## 5. Pipeline structure

Three sprint pipelines + one master:

```
benchmarks/airbnb-clone/pipelines/
├── sprint-1-data.dot
├── sprint-2-backend.dot
├── sprint-3-frontend.dot
└── airbnb-clone.dot         # master: sprint-1 -> holdout-eval -> sprint-2 -> holdout-eval -> sprint-3 -> holdout-eval -> exit
```

Each sprint pipeline:

```
start -> plan -> decompose -> implement(loop) -> verify(holdout_eval) -> fix(max_visits=5) -> verify -> exit
```

`decompose` is a new node type proposed for this benchmark — `type="decompose"` runs `claude --print` with the sprint plan and emits a JSON list of subtasks. Implementation then iterates that list. Add as `_decompose` handler in `runner/handlers.py` — **future work**, not blocking the benchmark design.

For now, sprints can omit `decompose` and just let `implement` handle the decomposition implicitly (matches AgentLoop's "engineer" role behavior).

## 6. Holdout structure (sealed in the holdouts sibling repo)

```
holdouts/airbnb-clone/
├── <scenario manifest>     # 90 scenarios, one per task, mapped to verify probes
├── tests/
│   ├── rules/              # @firebase/rules-unit-testing tests for each Security Rule
│   ├── server-actions/     # Node + admin SDK tests hitting the emulator
│   ├── e2e/                # Playwright tests vs `next dev` + emulator
│   ├── lighthouse/         # Lighthouse CI configs + budgets
│   └── adversarial/        # Rules leak probes, race tests, OWASP top-10 scans
└── evaluator/
    └── score_airbnb.py     # Loads scenario manifest + runs each probe → emits verdict JSON
```

The scoring rubric per scenario:
- **Functional** (1 point) — feature works end-to-end against happy path
- **Hidden contract** (1 point) — adversarial / edge-case test passes (forces fix-loop)
- **Performance / a11y / security** (0.5 point) — Lighthouse + Axe + Rules probes

Max score: 90 functional + 90 hidden + 45 perf/a11y/sec = **225 points**. dark-factory's first run will likely hit 50-70%; tracker-level performance would be 85%+.

## 7. Scoring & success criteria

Per-task verdict: `pass` / `partial` / `fail` / `error`.
Aggregate run-level metrics (recorded in CXDB metadata + evidence bundle):

- `total_score` / 225
- `tasks_passed` / 90
- `tasks_failed`
- `total_tokens`, `total_cost_usd`, `total_wall_ms`
- `lighthouse_perf`, `lighthouse_a11y`, `axe_violations`
- `rules_leak_probes_blocked` / total
- `e2e_pass_rate`

Comparable axes vs AgentLoop's run:
- Amplification ratio: 3 prompts → N tasks (target ≥ 87 like AgentLoop)
- Wall time: target < 36 hours
- Cost: target < $20 (AgentLoop didn't disclose; we measure)
- Tasks completed unattended: target ≥ 90% before any human intervention

## 8. Why Firestore (and not Postgres+RLS)

1. **Reproducibility**: Firestore emulator is fully local, deterministic, no API keys. AgentLoop's Supabase run depended on a live cloud project — harder to rerun against.
2. **dark-factory already runs sealed evaluators locally** via `holdout_eval` — keeping the data layer local extends that posture.
3. **Different security model**: Firestore Rules are JavaScript-flavored, RLS is SQL. Exercising Rules is a meaningful test of the implementing agent's ability to translate intent across paradigms.
4. **Cost**: Real Stripe still costs cents per booking; Firebase emulator is free. Real Stripe will be in test mode anyway.

## 9. Stretch goals (not in initial 90-task scope)

- **Realtime collaboration** — host sees guest cursor on the booking widget via Firestore `onSnapshot` (would add 5 tasks)
- **i18n** — Spanish + French via next-intl (+ 8 tasks)
- **PWA / offline cache** — Workbox service worker (+ 6 tasks)
- **Analytics events** — wired to a local Firestore `events` collection with dashboard (+ 4 tasks)

If we want to outscale AgentLoop (87) we hit 100+ tasks with one stretch enabled.

## 10. Implementation order (for actually shipping this benchmark)

1. **Now (design done)** — this DESIGN.md
2. Write `spec.md` + `visible_acceptance.md` (operator authors; goes into the repo)
3. Build the `starter/` scaffold (empty Next.js + Tailwind + Shadcn + Firebase emulator config)
4. Write the 3 sprint prompts (`prompts/sprint-{1,2,3}-{plan,implement,fix}.md`)
5. Write the sealed scenario manifest + holdout tests in the holdouts sibling repo
6. Write the 3 pipeline DOTs + the master DOT
7. Smoke-test with `--backend echo` to verify pipeline mechanics
8. First real run with `--backend ao --ao-project airbnb-bench --ao-agent claude-code` + Sonnet override
9. Score, capture evidence bundle, publish to `benchmarks/airbnb-clone/results/`
10. Compare to AgentLoop's run, write up findings, file follow-up beads for the gaps

## 11. References

- Source case study: <https://www.agentloop.run/blog/airbnb-clone-case-study>
- AgentLoop platform: <https://www.agentloop.run/>
- Dark Factory operator-vs-implementing-agent boundary: [`../../CLAUDE.md`](../../CLAUDE.md)
- Existing benchmark precedent: [`../amazon-clone/README.md`](../amazon-clone/README.md), [`../fibonacci/README.md`](../fibonacci/README.md)
- Firebase emulator suite: <https://firebase.google.com/docs/emulator-suite>
- Firestore Rules unit testing: <https://firebase.google.com/docs/rules/unit-tests>
