# Sprint 3 Implement — Frontend Layer (${goal})

Implement Sprint 3 from `.dark-factory/sprint-3-plan.jsonl`. UI lives under `src/app/**` and `src/components/**`.

## What to build

- App router routes: `/`, `/search`, `/listings/[id]`, `/listings/new`, `/dashboard`, plus auth flow.
- `RootLayout` wiring Firebase Auth + TanStack Query + Zustand.
- Home page (hero + searchbar + featured grid).
- **Searchbar**: collapsed default, four popovers (Where / Check-in / Check-out / Who). The popovers must stay open while the user picks a date or changes counts; submitting navigates to `/search` with URL params.
- Search results page with URL-driven filters, a grid, and an interactive map with price-bubble markers and clustering.
- Listing detail page (gallery + amenities + map + reviews + booking widget).
- 5-step new-listing form with step validation.
- User dashboard tabs.
- Mobile responsive variants for searchbar, dashboard, forms.
- `loading.tsx` + `error.tsx` per route, plus `not-found.tsx`.
- Keyboard / focus management for searchbar, gallery, and modals.

## Hard constraints

- Use only `src/app/**` and `src/components/**` for new UI files. Server actions already exist under `src/app/actions/**` — do not duplicate them.
- TanStack Query for any server data fetch; Zustand for transient client state (searchbar open, filter draft, mobile nav).
- Shadcn primitives + Tailwind. Do not introduce a second UI library.
- Maps may use `mapbox-gl` or `leaflet` — pick one and stick with it. Cluster with `supercluster` (or library equivalent).
- The searchbar's calendar popovers must not collapse the searchbar on date selection. If you encounter the Radix onBlur race, the fix is a deferred collapse + `onInteractOutside` + controlled state.
- Do not open or push a PR; do not search for sealed paths.

## Verification before finishing

```bash
pnpm test:e2e:smoke
pnpm test:components
```

## When done

Print `sprint-3: implemented`.
