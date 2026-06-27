# Amazon Full-Stack Commerce Benchmark Specification

**Version:** 3.0
**Created:** 2026-05-24
**Type:** Full-stack e-commerce application benchmark

## Overview

Build a production-shaped Amazon-style commerce application with a real frontend, a real backend API, and a local Firestore database running through the Firebase emulator.
The application must exercise product discovery, search, filtering, product detail, cart, checkout, account state, order history, seller inventory, admin moderation, notifications, diagnostics, seed/reset flows, and a validation harness.
This benchmark is intentionally larger than a frontend exercise. It is meant to force durable full-stack architecture, persistent state, role boundaries, and evaluator-visible behavior.

## Public Versus Held-Back Contract

The full product surface is public and must be given to the coding agent.
The coding agent receives all user stories, data model requirements, route requirements, launch commands, and quality expectations in this file.
The split between visible product requirements and operator-only evaluation guidance — including exact seed identifiers, adversarial inputs, browser interaction order, viewport sizes, security-rule probes, malformed payloads, coupon combinations, race cases, and scoring thresholds — is defined in `README.md` ("Revealed vs. operator-only") and `SCORING.md`. This file (the public `spec.md`) describes the product; it intentionally does not enumerate which categories of probes the held-back evaluator contains.
This split tests conformance without hiding core product requirements.

## Source Size Requirement

- The submitted application must contain at least 5,000 non-generated source lines.
- At least 2,000 lines must be frontend source, including components, routing, state, styling, API client code, and browser tests.
- At least 1,500 lines must be backend source, including routes, validation, Firestore repositories, auth/session handling, totals logic, and integration tests.
- At least 500 lines must cover seed data, Firestore rules, emulator setup, validation scripts, or test fixtures.
- Generated build output, dependency directories, lockfiles, copied framework templates, minified assets, screenshots, and vendored libraries do not count.
- The validation harness must report counted files, excluded files, total non-generated source lines, and pass/fail status for the source-size requirement.

## Target Stack

- Frontend: React, Vue, Svelte, Angular, or plain TypeScript with modular components and route-backed views.
- Backend: Node, Python, Go, Rust, or another HTTP runtime with JSON API routes and structured error responses.
- Database: Firestore emulator as the local database of record for persistent commerce state.
- Local runtime: one documented command starts frontend, backend, and Firestore emulator together.
- Tests: unit tests for pure logic, integration tests for API plus Firestore behavior, and browser tests for a complete checkout flow.
- Seed data: a repeatable command clears and repopulates Firestore with a realistic commerce dataset.
- Configuration: local environment variables are documented and have sensible development defaults.
- Runtime logs: startup output prints frontend URL, backend URL, emulator host, and diagnostics URL.

## Global Product Requirements

- The first screen must be the usable storefront, not a marketing landing page.
- The application must render on desktop, laptop, tablet, and mobile widths without horizontal page scroll.
- The app must be usable without external paid services or cloud project credentials.
- Product, order, cart, review, wishlist, coupon, notification, address, moderation, and inventory records must persist in Firestore during a local run.
- Reloading the browser must restore session, cart, filters, and relevant account state.
- Backend API responses must be JSON and must include stable machine-readable error codes.
- Sensitive checkout fields must not appear in console output, URLs, analytics payloads, Firestore records, local storage, or test artifacts.
- All money values must be stored as integer cents and formatted for display at the UI edge.
- All user-facing product and account text must be specific, readable commerce content.
- Every role boundary must be enforced by the backend and represented in Firestore rules.
- Every major user story must have seeded data and at least one automated validation path.
- The browser experience must remain usable with keyboard navigation and screen-reader labels.

## User Story 1: Browse Product Catalog

As a shopper, I want to browse a product catalog so I can compare items before opening a detail page.

Primary UI surfaces:
- catalog grid.
- department rail.
- header cart badge.
- quick view panel.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Catalog displays at least 60 seeded products across at least 8 departments.
- Each product card includes title, brand, department, price, rating, review count, image, delivery promise, stock status, and seller name.
- Cards expose add to cart, save, and quick view actions.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `reviews` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 2: Search and Sort

As a shopper, I want to search and sort products so I can narrow a large catalog quickly.

Primary UI surfaces:
- header search.
- result toolbar.
- sort menu.
- shared URL state.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Search covers title, brand, department, tags, and description.
- Sort modes include relevance, price ascending, price descending, rating, newest, and delivery speed.
- Search and sort state are reflected in the URL.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `reviews` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 3: Filter Catalog

As a shopper, I want to filter products by multiple attributes so I can find products that match constraints.

Primary UI surfaces:
- filter drawer.
- active filter chips.
- result count.
- empty result panel.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Filters include department, price range, minimum rating, fast delivery, stock state, seller, discount, and review count.
- Active filters appear as removable chips.
- Filter application happens before pagination on the backend.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 4: Product Detail

As a shopper, I want to view a complete product detail page so I can decide whether to buy a product.

Primary UI surfaces:
- image gallery.
- buy box.
- specification table.
- related product shelf.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Detail page shows gallery, title, brand, seller, price, discount, rating, reviews, delivery estimate, return policy, and specifications.
- Quantity selector respects stock and maximum-per-order limits.
- Related products come from the backend.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `reviews` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 5: Cart Management

As a shopper, I want to manage a persistent cart so I can adjust items before checkout.

Primary UI surfaces:
- cart drawer.
- cart route.
- header count.
- saved item controls.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Cart rows include thumbnail, title, seller, quantity, unit price, line total, delivery promise, and stock warning.
- Quantity updates recompute subtotal, discounts, shipping estimate, tax estimate, and grand total.
- Guest cart can merge into signed-in cart.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `carts` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `coupons` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 6: Save for Later and Wishlist

As a shopper, I want to save products outside the active cart so I can return to products later.

Primary UI surfaces:
- wishlist route.
- save controls.
- account navigation.
- cart saved section.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Wishlist persists per signed-in user.
- Saved items can move back to cart.
- Duplicate save operations are idempotent.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `wishlists` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 7: Account Registration and Session

As a shopper, I want to create and use an account session so I can keep commerce state across devices.

Primary UI surfaces:
- auth modal.
- account menu.
- profile route.
- session banner.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Registration validates email, name, and password rules.
- Session survives refresh.
- Duplicate email registration returns a stable error code.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `carts` collection as part of the normal feature flow.
- Reads or writes the `wishlists` collection as part of the normal feature flow.
- Reads or writes the `addresses` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 8: Address Book

As a shopper, I want to manage saved addresses so I can make checkout faster.

Primary UI surfaces:
- address list.
- address editor.
- default selector.
- checkout address step.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Address records include recipient, street, unit, city, region, postal code, country, and phone.
- One address can be default.
- Past order address snapshots do not mutate when address book records change.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `addresses` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 9: Checkout Flow

As a shopper, I want to complete checkout so I can validate shipping, payment, and totals before placing an order.

Primary UI surfaces:
- checkout stepper.
- payment form.
- review panel.
- total summary.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Checkout includes shipping, delivery, payment, review, and confirmation steps.
- Backend revalidates inventory, coupon, address, payment mask, and totals before creating the order.
- Payment data is never stored as a full card number.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `carts` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `coupons` collection as part of the normal feature flow.
- Reads or writes the `addresses` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 10: Order Confirmation

As a shopper, I want to see order confirmation details so I can know that the order was placed exactly once.

Primary UI surfaces:
- confirmation route.
- order summary.
- copy order action.
- continue shopping action.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Confirmation route shows order ID, date, item list, address snapshot, payment mask, delivery window, and total.
- Refresh does not create another order.
- Cart clears only after order write succeeds.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `carts` collection as part of the normal feature flow.
- Reads or writes the `notifications` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 11: Order History and Order Detail

As a shopper, I want to review prior purchases so I can track and repeat past orders.

Primary UI surfaces:
- order list.
- order detail.
- reorder action.
- history search.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Order history lists status, date, total, item count, and delivery estimate.
- Order detail uses immutable snapshots.
- Order history search supports order ID and product title.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `notifications` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 12: Product Reviews

As a shopper, I want to read and write product reviews so I can judge product quality and share feedback.

Primary UI surfaces:
- review list.
- rating histogram.
- review form.
- report action.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Reviews show average rating, distribution bars, and sortable review list.
- Only users with matching purchase history can create a review.
- Reported reviews enter moderation workflow.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `reviews` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `moderationEvents` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 13: Promotions and Coupons

As a shopper, I want to apply promotions and coupon codes so I can see discounts before paying.

Primary UI surfaces:
- coupon input.
- discount summary.
- coupon error region.
- checkout review.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Coupons support percent discount, fixed discount, department constraint, minimum subtotal, expiration, and active state.
- Backend recomputes coupon discount during checkout.
- Order snapshot stores applied coupon details.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `coupons` collection as part of the normal feature flow.
- Reads or writes the `carts` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 14: Inventory Reservation

As a shopper, I want to receive accurate inventory feedback so I can avoid buying unavailable products.

Primary UI surfaces:
- stock labels.
- low stock warning.
- checkout validation panel.
- seller stock status.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Checkout atomically verifies stock and reduces inventory.
- Low stock appears between 1 and 5 remaining units.
- Out-of-stock products cannot complete checkout.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `carts` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 15: Seller Console

As a seller, I want to manage owned products so I can change catalog content without editing code.

Primary UI surfaces:
- seller product table.
- product editor.
- stock editor.
- seller metrics strip.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the seller role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Seller console is visible only to seller accounts.
- Sellers can create, edit, archive, and restock owned products.
- Sellers cannot mutate products owned by another seller.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `moderationEvents` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 16: Admin Moderation

As a admin, I want to moderate catalog and review content so I can handle reported or unsafe content locally.

Primary UI surfaces:
- moderation queue.
- review detail.
- product suspension controls.
- audit trail.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the admin role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Admin console is visible only to admin accounts.
- Admins can hide or restore reported reviews.
- Admins can suspend products from search results without deleting records.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `moderationEvents` collection as part of the normal feature flow.
- Reads or writes the `reviews` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 17: Notifications Center

As a shopper, I want to read order and account notifications so I can track important account changes.

Primary UI surfaces:
- notification bell.
- notification drawer.
- notification route.
- read controls.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the shopper role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Unread notification count appears in the header.
- Users can mark one notification or all notifications as read.
- Notification list paginates.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `notifications` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `coupons` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 18: Operational Dashboard

As a operator, I want to inspect local operational health so I can understand whether the benchmark app is functioning.

Primary UI surfaces:
- diagnostics route.
- health panel.
- metrics table.
- seed status panel.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the operator role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Health route reports service status, Firestore connectivity, app version, and current time.
- Metrics route reports request counts, error counts, checkout attempts, orders created, and latency summary.
- Diagnostics route shows API health, seed status, current user, cart count, and emulator target.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 19: Local Seed, Reset, and Firestore Rules

As a evaluator, I want to reset deterministic local data so I can repeat benchmark runs reliably.

Primary UI surfaces:
- seed command output.
- emulator UI.
- rules file.
- test setup.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the evaluator role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Seed command clears and repopulates all benchmark collections.
- Seed command is idempotent.
- Firestore rules reflect user, seller, and admin ownership boundaries.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `users` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `reviews` collection as part of the normal feature flow.
- Reads or writes the `coupons` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## User Story 20: End-to-End Validation Harness

As a evaluator, I want to run automated validation so I can distinguish a real full-stack app from a superficial implementation.

Primary UI surfaces:
- validation CLI.
- browser test report.
- API test report.
- source-size report.
- Loading, empty, populated, error, and permission-denied states where relevant.
- Route-backed browser state for any surface a user would reasonably bookmark or refresh.

Required states:
- Initial load with seeded Firestore data.
- User-driven update that changes visible UI state without a full page reload.
- Refresh recovery after browser reload.
- Backend unavailable or Firestore unavailable response.
- Mobile width behavior at narrow phone dimensions.
- Keyboard-only interaction path for primary controls.

Acceptance criteria:
- The feature is reachable from normal application navigation for the evaluator role.
- The feature reads initial state from the backend API rather than browser-only constants.
- The feature writes durable changes through the backend API when persistence is required.
- The UI shows progress feedback for network operations longer than a brief interaction.
- Successful operations update local UI state and remain correct after refresh.
- Invalid operations show field-level or action-level errors with clear recovery options.
- All displayed money values use integer-cent source data and locale-appropriate formatting.
- All displayed date values use deterministic formatting in tests and readable formatting in the app.
- The route, query string, or account state preserves meaningful user context.
- The feature works with seeded data and with user-created data during the same local run.
- The feature avoids logging sensitive account, address, or payment fields.
- The feature has at least one automated test that proves the success path.
- Validation command runs lint, unit tests, API integration tests, browser checkout flow, Firestore rules probes, and source-size check.
- Validation fails when the emulator is unavailable.
- Validation writes a JSON summary with pass/fail status for each area.

Backend and API contract:
- The backend owns validation for all persisted mutations.
- The backend returns stable success payload shapes and stable error payload shapes.
- The backend rejects malformed identifiers, negative quantities, invalid money values, and unauthorized role access.
- The backend avoids using client-supplied totals as the source of truth.
- The backend includes integration coverage for the primary success path and at least two failure paths.
- API handlers are separated from Firestore repository code and pure domain logic.
- Every mutation route records enough state for later user-visible recovery or diagnosis.

Firestore persistence contract:
- Reads or writes the `orders` collection as part of the normal feature flow.
- Reads or writes the `carts` collection as part of the normal feature flow.
- Reads or writes the `products` collection as part of the normal feature flow.
- Reads or writes the `inventory` collection as part of the normal feature flow.
- Reads or writes the `users` collection as part of the normal feature flow.
- Writes include created or updated timestamps where meaningful.
- Records use deterministic IDs in seeded fixtures and stable generated IDs at runtime.
- Security rules protect role-specific and owner-specific access.
- Tests can reset data without relying on cloud state.

Validation probes:
- Seeded happy path with a signed-in user.
- Guest or signed-out path when the feature supports guest usage.
- Unauthorized user attempting a protected operation.
- Malformed payload or invalid query parameter.
- Firestore emulator unavailable or backend unavailable.
- Browser refresh after a successful mutation.
- Mobile viewport interaction.
- Keyboard-only interaction for the main action.
- API response schema check.
- Persistence check against Firestore after the UI action.

Failure and edge states:
- Network request fails before the backend writes data.
- Backend validation rejects stale or inconsistent client state.
- The relevant Firestore record is missing, archived, hidden, suspended, or owned by another role.
- The user refreshes during or immediately after the operation.
- The user repeats the same action quickly and the app remains idempotent where required.
- The evaluator starts from a clean checkout and seeded local database.

Accessibility and responsive behavior:
- Interactive controls have accessible names and visible focus states.
- Status changes use visible text and aria-live behavior when the change is not otherwise obvious.
- The layout remains usable at mobile, tablet, laptop, and wide desktop widths.
- Text does not overlap neighboring controls or truncate critical data without an accessible full value.
- Dialog, drawer, and menu surfaces support Escape, Tab, Enter, and Space behavior where applicable.

Observability notes:
- Errors are visible to users and recorded in backend logs without sensitive fields.
- Validation artifacts identify this story by number and title.
- The diagnostics surface can help an evaluator determine whether failure is app logic, backend availability, or emulator state.

## Data Model Requirements

### `users`

- `role`
- `name`
- `email`
- `passwordHash or local auth credential reference`
- `createdAt`
- `defaultAddressId`
- `notificationPreferences`
- `sellerProfileId`

### `products`

- `sellerId`
- `title`
- `brand`
- `department`
- `description`
- `priceCents`
- `listPriceCents`
- `imageUrls`
- `ratingAverage`
- `reviewCount`
- `tags`
- `active`
- `suspended`
- `createdAt`
- `updatedAt`

### `inventory`

- `productId`
- `stockOnHand`
- `lowStockThreshold`
- `reservedCount`
- `updatedAt`

### `carts`

- `userId or guestId`
- `items`
- `couponCode`
- `totalsPreview`
- `updatedAt`

### `wishlists`

- `userId`
- `productIds`
- `createdAt`
- `updatedAt`

### `addresses`

- `userId`
- `recipient`
- `street`
- `unit`
- `city`
- `region`
- `postalCode`
- `country`
- `phone`
- `isDefault`

### `orders`

- `userId`
- `items`
- `totals`
- `couponSnapshot`
- `addressSnapshot`
- `paymentMask`
- `status`
- `createdAt`

### `reviews`

- `productId`
- `userId`
- `orderId`
- `rating`
- `title`
- `body`
- `tags`
- `helpfulCount`
- `hidden`
- `createdAt`

### `coupons`

- `code`
- `type`
- `value`
- `department`
- `minimumSubtotalCents`
- `expiresAt`
- `active`

### `notifications`

- `userId`
- `type`
- `title`
- `body`
- `read`
- `createdAt`

### `moderationEvents`

- `actorId`
- `targetType`
- `targetId`
- `action`
- `reason`
- `createdAt`

### `sellerProfiles`

- `userId`
- `displayName`
- `supportEmail`
- `ratingAverage`
- `active`
- `createdAt`

### `metricsSnapshots`

- `processId`
- `requestCount`
- `errorCount`
- `checkoutAttempts`
- `ordersCreated`
- `latencySummary`
- `createdAt`

## Backend API Requirements

- `GET /health`
- `GET /metrics`
- `POST /seed/reset`
- `POST /auth/register`
- `POST /auth/login`
- `POST /auth/logout`
- `GET /session`
- `GET /products`
- `POST /products`
- `GET /products/:id`
- `PATCH /products/:id`
- `POST /products/:id/archive`
- `POST /products/:id/restock`
- `GET /cart`
- `POST /cart/items`
- `PATCH /cart/items/:productId`
- `DELETE /cart/items/:productId`
- `POST /cart/save-for-later`
- `POST /cart/apply-coupon`
- `GET /wishlist`
- `POST /wishlist/items`
- `DELETE /wishlist/items/:productId`
- `GET /addresses`
- `POST /addresses`
- `PATCH /addresses/:id`
- `DELETE /addresses/:id`
- `POST /addresses/:id/default`
- `POST /checkout`
- `GET /orders`
- `GET /orders/:id`
- `POST /orders/:id/reorder`
- `GET /reviews`
- `POST /reviews`
- `POST /reviews/:id/report`
- `POST /reviews/:id/helpful`
- `GET /notifications`
- `POST /notifications/:id/read`
- `POST /notifications/read-all`
- `GET /seller/products`
- `GET /seller/metrics`
- `GET /admin/moderation`
- `POST /admin/moderation/actions`

## Frontend View Requirements

- Storefront catalog view.
- Search results view.
- Filtered department view.
- Product detail view.
- Quick view panel.
- Cart drawer.
- Full cart route.
- Checkout shipping step.
- Checkout delivery step.
- Checkout payment step.
- Checkout review step.
- Order confirmation view.
- Account profile view.
- Address book view.
- Order history view.
- Order detail view.
- Wishlist view.
- Seller product list view.
- Seller product editor view.
- Admin moderation queue view.
- Admin moderation detail view.
- Notifications center view.
- Operator diagnostics view.
- Mobile navigation surface.

## Held-Back Evaluator Guidance

The categories of material the held-back evaluator keeps out of the public spec — and the principle that no major user story, required route family, required collection family, or required launch command is hidden — are defined for the operator in `README.md` ("Revealed vs. operator-only") and `SCORING.md`. This section exists to point at those operator docs; it does not enumerate the held-back categories in the public spec, because listing them here would defeat the purpose of keeping them out of the public spec.

## Launch Contract

- `npm install` or a documented equivalent installs dependencies.
- `npm run seed` or a documented equivalent resets Firestore data.
- `npm start` or a documented equivalent starts frontend, backend, and Firestore emulator.
- `npm test` or a documented equivalent runs lint, unit tests, integration tests, browser tests, Firestore rules probes, and source-size checks.
- The README lists frontend URL, backend URL, backend health URL, emulator UI URL, and diagnostics URL.
- The app starts from a clean checkout on a developer machine with Node 22 available.
- The validation command exits non-zero when any required surface is missing or any required check fails.

## Success Criteria Summary

The benchmark is complete when all 20 user stories work end to end, the app contains at least 5,000 non-generated source lines, Firestore emulator is the local database of record, backend APIs mediate persisted commerce operations, browser flows prove checkout and order history, role boundaries are enforced, and validation artifacts show lint, unit, integration, browser, Firestore rules, and source-size results.
