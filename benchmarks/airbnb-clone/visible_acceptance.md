# Visible Acceptance — airbnb-clone

These checks are **visible to the implementing agent**. They cover the happy path and the rules / contracts that are explicitly part of `spec.md`. The held-back adversarial probes (race conditions, leak attempts, exact scenario values) live in the holdout repo and are **not** described here.

This file follows the same disclosure model as `benchmarks/amazon-clone/visible_acceptance.md` (recommendation A from the 2026-06-27 benchmark spec-isolation audit): the visible thresholds the implementing agent should target — Lighthouse Performance ≥ 70, axe-core violations < 5, viewport breakpoints 360/414/768, etc. — are listed in the per-sprint sections below as concrete pass/fail targets. The held-back layer adds adversarial probes (race conditions, leak attempts, exact adversarial payloads) that go beyond these visible thresholds; the implementing agent cannot enumerate them by reading this file.

The implementing agent should treat this file as a self-check list: if a section here fails, the corresponding sprint output is not done. Passing every section here is necessary, **not sufficient**, for a passing run.

---

## Sprint 1 — Data Layer

### S1.1 Emulator boots clean

- `firebase emulators:exec --only firestore,auth,storage,functions "echo ok"` exits 0.
- All four emulator UIs are reachable on their configured ports.
- `firestore.rules`, `storage.rules`, and `firestore.indexes.json` all parse without warnings.

### S1.2 Schemas exist

For every collection listed in `spec.md §1.1`, a TypeScript file under `src/lib/schema/` exports a Zod schema. Each schema has at least:
- a `safeParse` test against a known-good fixture
- a `safeParse` test against an invalid input

### S1.3 Security Rules — positive cases

Using `@firebase/rules-unit-testing`, the following operations all **succeed** when authenticated as the correct user:
- Read any published listing as an unauthenticated user.
- Write `users/{uid}` as the owning user.
- Create a listing as a host.
- Read the host's own bookings as that host.
- Read the guest's own bookings as that guest.
- Toggle a favorite under `users/{uid}/favorites/`.

### S1.4 Cloud Functions fire

- Creating a booking via the admin SDK results in availability dates being written for the date range.
- Creating a review via the admin SDK updates `listings.rating` and `listings.reviewCount`.
- Uploading an image to `listings/{id}/images/` produces sm/md/lg thumbnail entries within 10 seconds.

### S1.5 Seed runs

`pnpm seed` (or `npm run seed`) populates the emulator with the counts listed in `spec.md §1.5`. A second run is a no-op or idempotent overwrite. Total time < 60 seconds on a developer laptop.

---

## Sprint 2 — Backend Layer

### S2.1 Auth round-trips

- Email sign up creates the matching `users/{uid}` document.
- The auth ID token is forwarded with every server action and is verified server-side.
- Sign out clears the session cookie and subsequent server actions return `{ ok: false, error: { code: "unauthenticated" } }`.

### S2.2 Server actions return the envelope

Every server action conforms to:
```ts
type Result<T> = { ok: true; data: T } | { ok: false; error: { code: string; message: string } }
```
A unit test for **each** server action asserts both shapes are reachable.

### S2.3 Listing CRUD

- A host can create, update, and soft-delete listings they own.
- A different user gets `{ ok: false, error: { code: "permission_denied" } }` when attempting to mutate a foreign listing.
- The public read action returns the listing regardless of auth state.

### S2.4 Booking flow

- A guest can book a listing for a free date range.
- The total returned to the client equals `nights * pricePerNight + fees`.
- A Stripe PaymentIntent is created in test mode; its `client_secret` is included in the result.
- The webhook handler, given a signed `payment_intent.succeeded` event for the booking, transitions the booking to `confirmed`.

### S2.5 Reviews

- A guest can submit a review for a booking whose status is `completed`.
- A guest cannot submit a review when no such booking exists — the action returns `{ ok: false, error: { code: "no_completed_booking" } }`.
- Submitting a review eventually causes the listing's aggregate rating to update.

### S2.6 Search

- `searchListings({ city: "London" })` returns only listings with `city == "London"` and `status == "published"`.
- Adding `priceMin` / `priceMax` filters narrows the result set.
- Adding `guests` filters out listings with `maxGuests < guests`.
- Pagination via `cursor` returns the next page without overlap.

---

## Sprint 3 — Frontend Layer

### S3.1 Home page renders

- `/` returns 200.
- The hero searchbar is visible and collapsed by default.
- The featured grid shows 12 listing cards.
- A skeleton placeholder is shown while the grid loads.

### S3.2 Searchbar happy path

- Clicking the collapsed searchbar expands it.
- Clicking the "Where" segment opens a location popover, accepts a city, and keeps the searchbar expanded.
- Clicking "Check in" opens a calendar popover; selecting a date does **not** close the searchbar.
- Clicking "Check out" extends the date range; selecting a date does **not** close the searchbar.
- Clicking "Who" opens a stepper popover; changing the count does **not** close the searchbar.
- The "Search" button navigates to `/search` with all four parameters in the URL.

### S3.3 Search results

- `/search?city=London` shows only listings in London on both the grid and the map.
- Hovering a grid card highlights the corresponding map marker.
- Clicking a map marker highlights the card and scrolls it into view.
- Markers cluster at low zoom and expand at high zoom.

### S3.4 Listing detail

- `/listings/[id]` shows the gallery, title, host, amenities, map, reviews, and booking widget.
- The booking widget's "Reserve" button is disabled until a valid date range and guest count are chosen.
- Selecting a date range that overlaps a blocked range shows an inline error and disables Reserve.

### S3.5 New listing flow

- `/listings/new` walks through 5 steps.
- Forward navigation requires the current step to validate.
- Backward navigation preserves entered values.
- Submit creates a draft, then publishes.

### S3.6 Dashboard

- `/dashboard` shows Profile / My Listings / My Bookings / Favorites tabs for an authenticated user.
- An unauthenticated user is redirected to sign-in.
- Each tab shows an empty state when no data is present.

### S3.7 Mobile

- All four routes above render without horizontal scroll at viewport widths 360, 414, and 768.
- The searchbar collapses to a touch-friendly single row on mobile.
- The dashboard's tabs collapse to a bottom-nav on mobile.

### S3.8 Loading + errors

- Each route has a `loading.tsx` that shows during initial render.
- Each route has an `error.tsx` that catches thrown errors and shows a Retry button.
- A 404 page exists at `app/not-found.tsx`.

### S3.9 Performance + accessibility budgets (visible thresholds)

These are the **visible thresholds** the implementing agent should target on
the home page (`/`) and the search results page (`/search`):

- Lighthouse Performance score ≥ 70.
- axe-core accessibility audit reports < 5 violations of `serious` or
  `critical` impact.
- No horizontal scroll at viewport widths 360, 414, and 768 (see S3.7).
- Total transferred bytes for the home route ≤ 500 KB gzipped (excluding
  user-supplied listing photos).

The held-back layer may probe additional adversarial payloads, race conditions,
or stricter thresholds beyond the visible targets above.

---

## Master happy path (operator-runnable smoke)

1. Sign up as `host@example.com`.
2. Create a listing in "Paris" with 6 photos.
3. Sign out, sign in as `guest@example.com`.
4. Search `/search?city=Paris` and find the listing.
5. Book it for two nights.
6. Confirm the Stripe PaymentIntent via the test webhook.
7. Mark the booking complete (via a `markComplete` admin helper).
8. Submit a 5-star review.
9. Favorite the listing.
10. Visit `/dashboard` and verify the booking, review, and favorite all appear.

If steps 1–10 complete without manual database edits, **the visible acceptance bar is met.**
