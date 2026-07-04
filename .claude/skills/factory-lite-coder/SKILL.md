---
name: factory-lite-coder
description: factory-lite coder tick — intake, route, dispatch minimax coders; honors auto-factory daemon contracts
---

# /factory-lite-coder — one dispatch tick

An LLM-skill bootstrap of the Auto-Factory Daemon's intake/router/dispatcher
(`docs/auto-factory-daemon-design-rust.md` `intake.rs`/`router.rs`/`dispatch.rs`),
built to run BEFORE the Rust daemon exists, against the exact same
`~/.dark-factory/daemon-cxdb.sqlite` and telemetry contract so the daemon can
later drop in with zero data migration.

Designed to run under `/loop 10m /factory-lite-coder`. **One invocation = one
tick.** Assume zero conversation context — re-derive everything from disk each
time.

## 0. Load contract + config

Read `.claude/skills/factory-lite/CONTRACT.md` in full — it defines the 8
overlay states, the 10-event vocabulary, the sqlite3 one-liners, the telemetry
schema, and the safety envelope. Then load config per CONTRACT.md §0
(`config/daemon.toml` falling back to `daemon/contracts/daemon.toml.example`).

Run the CONTRACT.md §3 init one-liner if `~/.dark-factory/daemon-cxdb.sqlite`
does not yet exist.

## 1. Intake

```bash
br list --status open --label factory --json
```

For each returned bead whose `id` has NO row in `bead_overlay` yet (check via
CONTRACT.md §3 "read one bead's overlay" — empty result = new):

1. Run the CONTRACT.md §3 insert-if-absent one-liner (`state='QUEUED'`,
   `attempt=1`).
2. Emit `INTAKE_BEAD_CREATED` (lifecycle_state=`QUEUED`, metrics=`{}`,
   context=`{"title":"<bead title>"}`).

This step is idempotent by `bead_id` — re-running it on a bead already in
`bead_overlay` is a silent no-op (the `ON CONFLICT DO NOTHING` handles it).

## 2. Route each QUEUED bead

```bash
sqlite3 -json ~/.dark-factory/daemon-cxdb.sqlite \
  "SELECT bead_id FROM bead_overlay WHERE state='QUEUED';"
```

For each `bead_id`: read the bead's title/description (`br show <bead_id>`)
and use **your own judgment as the LLM** to decide `SMALL_PATH` (direct
coder, single-file/trivial change) vs `STANDARD_PATH` (full `/fs` spec-gen +
`/f` gated pipeline territory). This is a model judgment call, not a keyword
match — never write `if title.contains("fix")` or similar; if you genuinely
cannot judge, treat it as `STANDARD_PATH` (the safer, more-supervised default)
and say so in `context.note`.

Emit `TASK_ROUTED` per bead: `context={"routingVerdict":"SMALL_PATH"|"STANDARD_PATH"}`.
Do NOT change `bead_overlay.state` here — routing verdict lives only in
telemetry until dispatch (step 4) actually moves the bead.

## 3. Capacity check

```bash
sqlite3 -json ~/.dark-factory/daemon-cxdb.sqlite \
  "SELECT count(*) AS active FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED');"
```

`free_slots = max_workers - active` (from config, default 30). Cap this tick's
dispatch count at `min(free_slots, max_batch)` (default `max_batch=15`). If
`free_slots <= 0`, skip straight to step 6 (PR detection) and step 8 (TICK
summary) — never dispatch over cap, no exceptions.

## 4. Dispatch

For up to `free_slots` (capped at `max_batch`) routed `QUEUED` beads, most
recently routed first:

1. Branch name: `factory/<bead_id>-r<attempt>` (attempt from `bead_overlay.attempt`,
   default 1 on first dispatch).
2. Spawn via the Agent tool: `subagent_type: "minimax-pair-coder"`, prompt
   containing the bead's title/description/spec path and the exact branch name
   to work on (the coder subagent is responsible for checking out/creating
   that branch and pushing its own commits — this skill does not `git` on its
   behalf).
3. Register the branch:
   ```bash
   sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
     "INSERT INTO branch_registry (branch, bead_id, created_at)
      VALUES ('factory/<bead_id>-r<attempt>', '<bead_id>', strftime('%Y-%m-%dT%H:%M:%SZ','now'))
      ON CONFLICT(branch) DO NOTHING;"
   ```
4. Transition state:
   ```bash
   sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
     "UPDATE bead_overlay SET state='DISPATCHED', branch='factory/<bead_id>-r<attempt>',
      updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE bead_id='<bead_id>';"
   ```
5. Emit `TASK_DISPATCHED`: `context={"activeModel":"minimax","branch":"factory/<bead_id>-r<attempt>","routingVerdict":"..."}`.

## 5. (STANDARD_PATH note)

If a bead's routing verdict was `STANDARD_PATH`, the minimax coder subagent
prompt should direct it to run the repo's own `/fs` + `/f` flow itself rather
than hand-rolling a diff — this skill's job is dispatch, not pipeline
execution. Small-path beads get a direct implementation prompt instead.

## 6. Detect PRs opened by dispatched coders

For every bead in `state='DISPATCHED'` with a registered `branch`:

```bash
gh pr list --repo "$TARGET_REPO" --head "<branch>" --state open --json number,url
```

If a PR is found:
```bash
sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
  "UPDATE bead_overlay SET state='ATTESTED', pr_number=<number>,
   updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE bead_id='<bead_id>';"
```
Emit `PR_OPENED`: `context={"pr_number":<number>,"url":"<url>"}`.

## 7. Autonomy time-box

For every bead in `state IN ('DISPATCHED','ATTESTED')`, increment
`autonomy_secs` by the wall-clock seconds elapsed since this bead's
`updated_at` was last touched by a time-box check this loop cadence (use the
`/loop` interval, e.g. 600 for a 10m loop, as `$ELAPSED_SECS` — do not attempt
sub-second precision):

```bash
sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
  "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + $ELAPSED_SECS,
   updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE bead_id='<bead_id>';"
```

Then re-read `autonomy_secs` per bead:
- `>= 0.8 * autonomy_timebox_secs` (e.g. 8640s of 10800): emit `BUDGET_WARNING`.
- `> autonomy_timebox_secs` (e.g. > 10800s = 3h): transition to `HUMAN_HELD`
  and emit `PARKED_HUMAN_HELD` (`context={"reason":"autonomy_timebox_exceeded"}`).
  This overrides everything else — a bead over the box is parked even if a PR
  just opened this tick.

## 8. End-of-tick summary

Emit one `TICK` event: `metrics={"queued":N,"dispatched":N,"attested":N,"human_held":N}`
(counts from a final `SELECT state, count(*) FROM bead_overlay GROUP BY state;`).

## NEVER

- NEVER force-push or push directly to `base_branch`.
- NEVER run `gh pr merge` — dispatch/intake is not the merge authority.
- NEVER delete a branch — that is a Stage-2 Re-Roll Engine action, out of scope.
- NEVER dispatch past `max_workers`/`max_batch` caps (CONTRACT.md §5.5).
- NEVER keyword-route (`if title.contains(...)`) — routing is model judgment
  (CONTRACT.md §5.7, ZFC).
- NEVER reset `attempt` or `autonomy_secs` on an existing bead during intake —
  the insert-if-absent one-liner already guards this.
- NEVER dispatch a bead already `DISPATCHED`/`ATTESTED`/`HUMAN_HELD` — check
  state before every dispatch.
