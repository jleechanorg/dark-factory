# `bead-closed-check` — spec (jleechan-tdl)

## Problem

`bead_overlay` rows track factory-lite's *own* dispatch lifecycle
(QUEUED → DISPATCHED → ATTESTED → READY), but the underlying `br` bead can be
closed by a path the harness never observes: a coder subagent discovers the
work already shipped (e.g. via an earlier, differently-labeled PR) and closes
the bead directly instead of opening a new PR. See jleechan-9bi / PR #111
(merged 2026-06-27, closed the wrong bead id in its message) for the concrete
incident this fixes.

When that happens, the overlay row is stuck: `state=DISPATCHED` (or
`ATTESTED`) forever, because no PR will ever arrive to advance it, and no
existing subcommand ever re-checks the bead's own `br` status once it has
been intake-upserted.

## Contract

New subcommand: `bead-closed-check <bead_id>`.

- **Usage:** `bead-closed-check <bead_id>` — exactly one argument.
- **Applicability:** callable for a row in ANY state, but intended call sites
  are `DISPATCHED` and `ATTESTED` rows specifically (see §Callers below).
  Calling it on a row already terminal (`READY`, `HUMAN_HELD`, `BUDGET_HELD`)
  is a harmless no-op — it still runs the check, but never transitions the
  row again once the row is not in `DISPATCHED`/`ATTESTED`.
- **Behavior:**
  1. Shell out to `br show <bead_id> --json` and parse the `status` field
     (values are `open`/`closed`) with `python3 -c 'import json,sys; ...'`,
     matching the harness's existing JSON-parsing convention (see
     `gate-assessment`'s inline python3 validator). `br` failures (nonexistent
     bead id, `br` not on PATH, malformed JSON) are NOT treated as "closed" —
     they die with a clear message, same as every other harness precondition
     failure, so intake data-quality bugs surface immediately rather than
     silently parking beads.
  2. If `status != closed`: no-op. Print `open` and exit 0. (Row untouched —
     this is the common case, checked every tick, for the overwhelming
     majority of active beads.)
  3. If `status == closed` AND the row's current state is `DISPATCHED` or
     `ATTESTED`: transition the row to `HUMAN_HELD` (reusing the existing
     terminal parking state — see §Design decision below) and emit
     `PARKED_HUMAN_HELD` with `reason="bead_closed_underneath"` in the
     telemetry context, plus the row's last known `branch`/`pr_number` (if
     any) so a human auditing `HUMAN_HELD` rows can see whether a stray
     branch/PR needs manual cleanup. Print `parked` and exit 0.
  4. If `status == closed` AND the row is already `READY`/`HUMAN_HELD`/
     `BUDGET_HELD`/any other state: no-op (already terminal or already
     parked). Print `already_terminal` and exit 0.

- **Idempotency:** safe to call every tick for every DISPATCHED/ATTESTED row.
  Calling it twice on an already-parked bead is a no-op the second time
  (state is no longer DISPATCHED/ATTESTED, so branch 4 fires, not branch 3).
- **Never invents state:** uses the existing 8-state vocabulary
  (`HUMAN_HELD`), the existing `PARKED_HUMAN_HELD` event type. No new state,
  no new event type. See CONTRACT.md §1 and §2 — this subcommand is additive
  data-routing over the existing vocabulary, not new binding surface.
- **No merge/push/branch-delete path:** this subcommand only ever reads
  (`br show`) and writes the CXDB `bead_overlay.state` column — same NEVER
  rules as every other subcommand (harness file header + CONTRACT.md §5).

## Design decision: reuse `HUMAN_HELD`, do not invent a 9th state

Considered and rejected: a 9th terminal state (e.g. `CLOSED_EXTERNALLY` or
`STALE`). Rejected because:

1. CONTRACT.md's own Stage-1 substitution rule already establishes the
   pattern of routing "automated system can't safely proceed" cases into
   `HUMAN_HELD` rather than growing the state machine (e.g. `reroll_worthy`
   verdicts park in `HUMAN_HELD` instead of entering `RE_ROLL`). This case is
   the same shape: an anomaly the Stage-1 harness cannot safely resolve on
   its own (was the closure correct? Is there an orphaned branch/PR to clean
   up?), so a human should look, exactly like the existing `HUMAN_HELD`
   contract already documents ("terminal until human action").
2. `HUMAN_HELD` is generic on purpose — the `reason` field in telemetry
   context already carries the distinguishing information (compare
   `"reason":"autonomy_timebox_exceeded"`, `"reason":"coder_silent"`,
   `"reason":"session_stalled"`, `"reason":"reroll_worthy_stage1_disabled"`
   from the existing `autonomy-tick`/verifier-sweep/`reroll-verdict` code
   paths). `"reason":"bead_closed_underneath"` is a natural addition to that
   same enum-by-convention, not a schema change.
3. A 9th state would require a `schema.sql` CHECK-constraint migration and a
   CONTRACT.md state-table edit for what is, behaviorally, identical to
   existing `HUMAN_HELD` semantics ("stop driving this bead automatically;
   a human decides next"). No downstream behavior needs to distinguish
   "parked because bead closed" from "parked because reroll-worthy" at the
   state level — only at the telemetry/audit level, where `reason` already
   does the job.

## Callers (both skills, added same-PR)

- **Coder tick** (`factory-lite-coder` SKILL.md step 6, "Detect PRs opened by
  dispatched coders"): before checking `gh pr list --head <branch>` for a
  DISPATCHED row, call `$H bead-closed-check <bead_id>` first. If it prints
  `parked`, skip the PR-list check for that bead this tick (it is no longer
  DISPATCHED).
- **Verifier tick** (`factory-lite-verifier` SKILL.md step 5, "Stalled-session
  sweep"): for every DISPATCHED and ATTESTED row swept in this step, call
  `$H bead-closed-check <bead_id>` before the existing stalled-session
  checks. If it prints `parked`, skip the silent-coder/stalled-session logic
  for that bead this tick.

## Out of scope (explicitly not fixed by this subcommand)

- Root cause #1 from the bead description — "PR merge/close messages don't
  reliably reference the bead they close" — is a separate, harder problem
  (would require the coder's PR-close/merge message to always cite the
  correct bead id). This subcommand only fixes the *symptom*: cleaning up the
  overlay row once a mismatch has already happened. Tracked separately if a
  fix is later scoped (not part of jleechan-tdl).
