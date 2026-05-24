# Sprint 1 Plan — Data Layer (${goal})

You are the **Engineer** for Sprint 1 of the airbnb-clone benchmark.

## Setup — do this first

Before writing any plan items, bootstrap your working directory:

```bash
# Copy the starter scaffold to the worktree root (skip if files already present)
cp -rn benchmarks/airbnb-clone/starter/. ./
```

This gives you `package.json`, `firebase.json`, `src/`, `functions/`, `Makefile`, and all other scaffold files at the repo root.

## What you have access to

- `benchmarks/airbnb-clone/spec.md` — full visible product spec. Read **§ Sprint 1 — Data Layer** carefully (sections 1.1 through 1.5).
- `benchmarks/airbnb-clone/visible_acceptance.md` — happy-path self-checks for Sprint 1 (S1.1 – S1.5).
- `benchmarks/airbnb-clone/starter/` — Next.js 14 + Tailwind + Shadcn + Firebase emulator config. Already copied above; treat its files as the starting scaffold.

## What you do NOT have access to

- Hidden adversarial tests, hidden scoring rubric, sealed evaluator source.
- `holdouts/` paths anywhere on disk.
- Any production Firebase project credentials. **Emulator only.**

## Your task in this step

Produce a detailed implementation plan for Sprint 1 only. The plan is a JSON-lines document at `.dark-factory/sprint-1-plan.jsonl`, one record per work item:

```jsonl
{"id":"firestore-emulator-config","summary":"...", "files":["firebase.json",".firebaserc"], "depends_on":[]}
{"id":"collection-users","summary":"...", "files":["src/lib/schema/user.ts"], "depends_on":["firestore-emulator-config"]}
```

Constraints:
- Cover every item described in `spec.md §1.1` (schemas), `§1.2` (rules), `§1.3` (indexes), `§1.4` (functions), `§1.5` (seed).
- `depends_on` is the ID of any prior plan item this one needs.
- Plan items must be small enough that one implement step can complete each in < 5 minutes of wall time.
- Do not write production code yet. Only the plan file.

## Stack constraint reminder

Firestore + Firebase Auth + Firebase Storage + Cloud Functions, **all via the local emulator suite**. No Postgres, no Supabase, no RLS. Use TypeScript everywhere.

## When you are done

Print a one-line summary: `plan written: N items`. Do not open or push a PR.
