# Airbnb Clone MVP Specification

**Version:** 1.0
**Created:** 2026-05-24
**Type:** Full-stack short-term-rental marketplace
**Stack constraint:** Next.js 14 App Router + Firebase Emulator Suite (Firestore + Auth + Storage + Cloud Functions) + Stripe (test mode) + Tailwind + Shadcn UI + Zustand + TanStack Query

> Inspired by AgentLoop's [Airbnb Clone case study](https://www.agentloop.run/blog/airbnb-clone-case-study). Where the source uses Supabase, this benchmark uses **Firestore (local emulator only)** end-to-end. Do not introduce Postgres, RLS, or Supabase libraries.

---

## Overview

Build a production-grade short-term rental marketplace: hosts list properties, guests search and book, both leave reviews. The application is delivered in three sprints (Data → Backend → Frontend). All persistence is via the Firebase Emulator Suite — no cloud project — so the entire system runs locally and deterministically.

Behavioral targets:
- Authenticated users can list, search, book, review, and favorite properties.
- All security rules deny by default; access is granted explicitly.
- Booking creation is atomic with availability blocking (no double-booking).
- Lighthouse perf ≥ 80 and a11y ≥ 90 on the home page and a listing detail page.

---

## Sprint 1 — Data Layer

### 1.1 Firestore Schema & Collections

**Acceptance Criteria:**
- `users/{uid}` — display name, photo URL, host flag, joined timestamp; extends Firebase Auth.
- `listings/{listingId}` — title, description, type, country, city, lat/lng, nightly price (USD cents), max guests, bed/bath counts, amenity slugs, owner uid (denormalized), rating, reviewCount, status, timestamps.
- `listings/{listingId}/images/{imageId}` — storage path, position, thumbnail paths (sm/md/lg).
- `amenities/{slug}` — name, icon, category. Reference collection seeded from fixture.
- `availability/{listingId}/dates/{yyyy-mm-dd}` — boolean blocked + bookingId if blocked.
- `bookings/{bookingId}` — listingId, guestId, hostId, checkIn, checkOut, nights, guests, subtotal, fees, total, currency, status, stripePaymentIntentId, timestamps.
- `reviews/{reviewId}` — listingId, authorId, rating (1-5), text, response, timestamps.
- `users/{uid}/favorites/{listingId}` — favorite marker doc.

### 1.2 Security Rules

**Acceptance Criteria:**
- Default deny across Firestore + Storage.
- `users/{uid}` — read public, write self only.
- `listings/{listingId}` — read public when `status == "published"`, write owner only.
- `bookings/{bookingId}` — read guest + host only; create authenticated; update guest (cancel) + host (decision) only.
- `reviews/{reviewId}` — read public, create author only after a completed booking exists.
- `users/{uid}/favorites/{listingId}` — read/write self only.
- Storage paths under `listings/{listingId}/images/*` — read public, write owner only.

### 1.3 Indexes

**Acceptance Criteria:**
- Composite index on listings supporting `(city, status, price)`, `(country, status, guests)`, and ordered by `createdAt`.
- Composite index on bookings supporting `(hostId, status, checkIn desc)` and `(guestId, checkIn desc)`.
- Composite index on reviews supporting `(listingId, createdAt desc)`.

### 1.4 Cloud Functions

**Acceptance Criteria:**
- **Availability blocker** — on `bookings/onCreate`, write the per-night availability documents in a single transaction with the booking write so the listing cannot be double-booked.
- **Rating aggregator** — on `reviews/onCreate|onUpdate|onDelete`, recompute `listings.rating` and `listings.reviewCount`.
- **Booking total calculator** — on `bookings/onCreate`, verify `total == nights * pricePerNight + fees` and reject mismatched writes.
- **Thumbnail pipeline** — on Storage object finalize under `listings/*/images/*`, generate sm/md/lg variants and persist their paths back on the image doc.

### 1.5 Seed Data

**Acceptance Criteria:**
- `scripts/seed.ts` populates the emulator with: 20 host users, 100 published listings (varied countries, cities, types, price bands), 6 images per listing, 30 amenities, 50 sample bookings, 200 reviews.
- Seeding is deterministic — re-running produces identical IDs and timestamps.
- Seed must complete within 60 seconds.

---

## Sprint 2 — Backend Layer

### 2.1 SDK Initialisation

**Acceptance Criteria:**
- Admin SDK (server) and Client SDK (browser) both auto-detect the emulator via `FIRESTORE_EMULATOR_HOST`, `FIREBASE_AUTH_EMULATOR_HOST`, etc.
- A single `getServerApp()` / `getClientApp()` helper exposes Firestore, Auth, Storage, Functions.

### 2.2 Authentication

**Acceptance Criteria:**
- Email/password sign up + sign in + sign out via Firebase Auth emulator.
- Google + GitHub providers wired against the emulator's OAuth stub.
- Session is propagated to server actions via the Firebase Auth ID token / Next.js cookies.
- Sign-up creates the matching `users/{uid}` document via a server action.

### 2.3 Validation

**Acceptance Criteria:**
- Zod schemas exist for: listing create/update, booking create, review create, search filters.
- Server actions reject invalid input with a `{ ok: false, error }` envelope.
- Schemas are the single source of truth for types — TypeScript types are derived from them.

### 2.4 Server Actions

**Acceptance Criteria (one server action per bullet):**
- Create / update / delete listing (owner only).
- Upload listing image to Storage and return the document with thumbnail paths.
- Search listings (filters: location, dates, guests, price min/max, amenities, sort).
- Read listing by id (public).
- Create booking (validates availability, computes total, creates Stripe PaymentIntent).
- Cancel booking (guest only; triggers Stripe refund flow).
- Create review (only after the guest's booking is marked completed).
- Toggle favorite (auth only).
- List user bookings (auth scoped).
- List user favorites (auth scoped).

### 2.5 Payments — Stripe (test mode)

**Acceptance Criteria:**
- Booking creation initialises a Stripe PaymentIntent and returns its `client_secret` to the client.
- `/api/webhooks/stripe` route validates the Stripe signature and, on `payment_intent.succeeded`, transitions the booking to `confirmed`.
- Cancellation issues a refund via the Stripe API; the booking transitions to `cancelled`.
- All Stripe traffic uses test keys; no live mode.

### 2.6 Search

**Acceptance Criteria:**
- A text tokenizer turns the user's location query into Firestore-compatible prefix predicates.
- Filters combine with composite indexes from Sprint 1.
- Pagination is cursor-based using Firestore `startAfter`.

### 2.7 Reliability

**Acceptance Criteria:**
- Every server action returns `{ ok: true, data } | { ok: false, error: { code, message } }`.
- A token-bucket rate limit (stored in Firestore) caps high-volume endpoints per user.

---

## Sprint 3 — Frontend Layer

### 3.1 Shell & Routing

**Acceptance Criteria:**
- App router routes: `/`, `/search`, `/listings/[id]`, `/listings/new`, `/dashboard`, `/bookings/[id]`, plus the auth flow.
- `RootLayout` wires Firebase Auth, TanStack Query, and the Zustand store.
- Every route has `loading.tsx` and `error.tsx`.

### 3.2 Home Page

**Acceptance Criteria:**
- Hero section with the **expandable searchbar**.
- Featured grid of 12 listings with lazy-loaded images and skeleton placeholders.
- Top navigation: logo, "Become a host" link, auth menu.

### 3.3 Searchbar

**Acceptance Criteria:**
- Collapsed by default; expands on click with smooth height/width transition.
- Four segments: Location, Check-in, Check-out, Guests.
- Each segment opens its own Radix popover on click.
- The popover for Check-in / Check-out hosts an `AvailabilityCalendar` that lets the user pick a date range without the searchbar collapsing.
- The Guests popover has adults / children / infants / pets steppers (with stated limits).
- Submitting navigates to `/search` with the filters in the URL.

### 3.4 Search Results

**Acceptance Criteria:**
- URL-driven (`/search?city=...&checkIn=...&checkOut=...&guests=...`).
- Two-pane layout: results grid + interactive map.
- Map shows price-bubble markers; selecting a marker highlights the card; markers cluster at low zoom.
- Filter sheet (mobile) / sidebar (desktop) lets the user refine without leaving the page.

### 3.5 Listing Detail

**Acceptance Criteria:**
- Image gallery (carousel with thumbs).
- Title, location, rating, reviewCount.
- Amenities list with icons.
- Map showing the listing's lat/lng.
- Reviews list with rating breakdown and pagination.
- **Booking widget**: date pickers (driven by `availability`), guests stepper, price breakdown (nights × rate + fees), "Reserve" button that initiates the Stripe checkout flow.

### 3.6 New Listing Flow

**Acceptance Criteria:**
- 5-step form: property type → location → details → photos → review.
- Step state survives navigation between steps.
- Final submit creates the listing in `status="draft"` then transitions to `published`.

### 3.7 Dashboard

**Acceptance Criteria:**
- Tabs: Profile, My Listings, My Bookings, Favorites.
- Each tab paginates and shows empty states.
- Mobile collapses tabs into a bottom-nav.

### 3.8 State, Data, A11y, Performance

**Acceptance Criteria:**
- Zustand owns transient client state (search filters, modal open/close).
- TanStack Query owns server cache (listings, reviews, user data).
- Keyboard navigation works for searchbar, gallery, modals; focus traps where appropriate.
- Lighthouse on the home page and at least one listing detail page: perf ≥ 80, a11y ≥ 90, best practices ≥ 90.

---

## Non-goals

- No live Firebase project — emulator only.
- No live Stripe — test mode only.
- No Postgres / Supabase / Prisma — Firestore is the only datastore.
- No realtime cursors / i18n / PWA in the initial 90-task scope (stretch).

---

## Quality bar (operator-stated)

- Every server action that writes user data is exercised by an integration test against the emulator.
- Security Rules are checked with `@firebase/rules-unit-testing` for at least one positive and one negative case per collection.
- E2E happy path: sign up → list a property → search → book → leave review → favorite — completes without manual intervention.

---

## References (read these before implementing)

- Firebase Emulator Suite: <https://firebase.google.com/docs/emulator-suite>
- Firestore Security Rules: <https://firebase.google.com/docs/firestore/security/get-started>
- Firestore Rules unit testing: <https://firebase.google.com/docs/rules/unit-tests>
- Cloud Functions for Firebase: <https://firebase.google.com/docs/functions>
- Stripe PaymentIntents: <https://stripe.com/docs/payments/payment-intents>
- Next.js App Router: <https://nextjs.org/docs/app>
- AgentLoop case study (architectural reference, **not** a copy target): <https://www.agentloop.run/blog/airbnb-clone-case-study>
