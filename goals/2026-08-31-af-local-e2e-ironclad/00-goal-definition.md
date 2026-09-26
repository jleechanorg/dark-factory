# Ironclad goal: local `/af` proves a native correction loop

Authoritative Bead: `dark-factory-5qct`

## Literal goal

Make `/af` genuinely work end to end on the Linux factory host. Direct edits to
the factory are authorized, but they are enabling work and cannot count as the
terminal proof.

Default verdict: **FAIL**. Every criterion must pass simultaneously.

| Criterion | Executable check | External anchor |
|---|---|---|
| C1 — authorized release | Resolve systemd `ExecStart`, hash `/proc/$PID/exe`, verify the release manifest, then run `git merge-base --is-ancestor <source_sha> origin/main`. | Running Linux binary and merged Git history. |
| C2 — native AO readiness | Start from AO cold or polling the wrong project and observe `AO_READINESS_READY` followed by `BEAD_DISPATCHED` for the same persisted bead. Cross-check with `ao status -p dark-factory --json` and the worker session. | Live AO project inventory, process environment, and daemon telemetry. |
| C3 — safe admission | Give one bead an open blocker and introduce one identical PR owner. Assert `DEPENDENCY_BLOCKED`, no route/spawn/retry increment, and one coalesced canonical owner. | Authoritative Bead dependency graph and daemon overlay. |
| C4 — provider isolation | Correlate every unattended launch with explicit non-personal provider/config provenance; inspect the live worker environment without printing secrets. | Live process tree and launch telemetry. |
| C5 — authentic correction | Correlate one exact PR head through failed `GATE_ASSESSMENT`, native `REROLL_START`, factory worker code/test/push, a new head, and reassessment. | GitHub commit graph plus daemon/AO identities. |
| C6 — truthful READY | At the corrected exact head verify non-draft, mergeable, required checks green, approval, zero current unresolved threads, accepted evidence/web-advice, then `READY_FOR_MERGE`. | GitHub REST and GraphQL plus independent semantic review. |
| C7 — sustained funnel | Run `bin/df-funnel-lanes --since 30d --json` and `--since 48h --json`; require a mission-attributed READY with no gate weakening, then repeat after 48 hours. | Canonical daemon event log using cross-attempt `bead_id` joining. |

## Anti-stopping guardrails

- Status reporting is never terminal while a safe in-scope action remains.
- One failed command, API throttle, CI wait, blocked dependency, or exhausted
  lane triggers diagnosis and the next safe alternative; it does not end the
  mission.
- Before any final response, audit C1–C7 and execute the next material action.
- Direct repair tests, commits, and deployments cannot satisfy C2, C5, or C6.
- Mocks, dry-runs, stale heads, `#790` ancestry, unmerged draft deployments,
  manual overlay/DB mutation, manual rerolls, admin bypasses, and weakened gates
  are automatic failure.
- A genuine external block requires three consecutive audits, exhausted safe
  alternatives, and an executable retry trigger. Monitoring continues.
- Any regression reopens the goal.

## Implementation order

1. Bounded project-scoped AO readiness and recovery (`#532`).
2. Dependency-aware admission and identical-owner coalescing (`#810`, `#506`).
3. API-surface breaker separation and immutable release proof (`#804`, `#781`).
4. Independent `/factory` validation.
5. Real `/af` correction-loop and 48-hour sustain evidence.
