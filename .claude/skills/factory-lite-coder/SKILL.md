---
name: factory-lite-coder
description: factory-lite coder tick — intake, route, dispatch minimax coders in parallel; all state mutations via the deterministic harness
---

# /factory-lite-coder — one dispatch tick

An LLM-skill bootstrap of the Auto-Factory Daemon's intake/router/dispatcher
(`docs/auto-factory-daemon-design-rust.md` `intake.rs`/`router.rs`/`dispatch.rs`).

**Division of labor (ZFC, per /advice 2026-07-03):** every binding mutation —
CXDB writes, state transitions, cap checks, telemetry — goes through the
deterministic harness `daemon/factory-lite-harness.sh` (`H` below). You, the
LLM, supply ONLY typed judgment verdicts as harness arguments. Never run
sqlite3 directly, never append to the telemetry file yourself; if the harness
refuses (`harness: ...` on stderr), respect the refusal — it is the contract.

Designed to run under `/loop 10m /factory-lite-coder` or the background runner
`daemon/run-factory-lite.sh coder`. **One invocation = one tick.** Assume zero
conversation context — re-derive everything from disk each time.

```bash
H=daemon/factory-lite-harness.sh
```

## 0. Load contract + config

Read `.claude/skills/factory-lite/CONTRACT.md` (states, events, safety
envelope). Config: `config/daemon.toml`, falling back to
`daemon/contracts/daemon.toml.example`. Then: `$H init` (idempotent).

## 1. Intake

```bash
br list --status open --label factory --json
```

For each returned bead: `$H intake-upsert <bead_id> "<title>"` — prints
`created` (new, event emitted) or `exists` (no-op). Idempotency is the
harness's job, not yours.

## 2. Route each QUEUED bead

`$H list QUEUED` → for each bead, read `br show <bead_id>` and use **your own
judgment as the LLM** to pick `SMALL_PATH` (direct coder; single-file/trivial)
vs `STANDARD_PATH` (full `/fs` + `/f` pipeline territory). Model judgment
only — never keyword rules. If you genuinely cannot judge, pick
`STANDARD_PATH` and note why. Record: `$H route-record <bead_id> <VERDICT> "<note>"`.

## 3. Capacity

`free=$($H capacity)` — the harness computes `min(max_workers - active, max_batch)`.
If `0`, skip to step 6.

## 4. Dispatch — PARALLEL, file-disjoint lanes

Select up to `$free` routed QUEUED beads, then:

1. **File-overlap check (single-writer rule):** from each bead's description,
   list the files/dirs it will touch. Beads sharing ANY mutable file are NOT
   independent — dispatch only one per overlapping group this tick; the rest
   wait for a later tick. When in doubt, serialize.
2. For each selected bead, register FIRST:
   `$H dispatch-record <bead_id> factory/<bead_id>-r<attempt>` (attempt from
   `$H list QUEUED` output). The harness enforces caps and legal state — if it
   refuses, do not spawn that coder.
3. **Spawn ALL selected coders in ONE message** — multiple Agent tool calls in
   a single response, each `subagent_type: "minimax-pair-coder"`,
   `run_in_background: true`. Never spawn serially (await one, then next) —
   parallel dispatch is the point of the fan-out. Each prompt must contain:
   the bead id, full title/description, the exact branch name (the coder
   creates it off origin/<base_branch>, commits, pushes, opens a PR with
   `Beads: <bead_id>` in the body — and NEVER merges), an isolation
   requirement (the coder MUST do its work in its own `git worktree add
   /tmp/factory-<bead_id>-r<n> -b <branch> origin/<base_branch>` and remove
   the worktree after pushing — never check out branches in the shared repo
   working tree), and for `STANDARD_PATH` beads: direct it to run the repo's
   `/fs` + `/f` flow rather than hand-rolling a diff.

## 5. (reserved)

Intentionally empty — kept so step numbers stay stable across revisions.

## 6. Detect PRs opened by dispatched coders

For every row in `$H list DISPATCHED` with a `branch`:

```bash
gh pr list --repo "$TARGET_REPO" --head "<branch>" --state open --json number,url
```

Found → `$H pr-opened <bead_id> <number> <url>`.

## 7. Autonomy time-box

`$H autonomy-tick $ELAPSED_SECS` — use the loop interval (e.g. 600 for a 10m
loop). The harness increments actives, emits `BUDGET_WARNING` at 80%, and
parks over-box beads `HUMAN_HELD` on its own.

## 8. End-of-tick summary

`$H tick-summary coder`

## NEVER

- NEVER run sqlite3 against the CXDB or write the telemetry file directly —
  every mutation goes through `$H` (drift in these files corrupts the data the
  Rust daemon inherits).
- NEVER force-push or push directly to `base_branch`.
- NEVER run `gh pr merge` — dispatch is not the merge authority.
- NEVER delete a branch (Stage-2 Re-Roll Engine action, out of scope).
- NEVER spawn a coder the harness refused to `dispatch-record`.
- NEVER dispatch two beads with overlapping files in the same tick.
- NEVER keyword-route — routing is model judgment (ZFC).
- NEVER await coder subagents inside the tick — spawn parallel, in background,
  and let the NEXT tick detect their PRs.
