# factory-lite CONTRACT — shared daemon contract for LLM-skill runners

**Load this file FIRST**, before `factory-lite-coder` or `factory-lite-verifier` do
anything else. It is the binding data contract both skills honor so the future
Rust daemon (`docs/auto-factory-daemon-design-rust.md`) can drop in against the
exact same `~/.dark-factory/daemon-cxdb.sqlite` file and the exact same
`~/Library/Logs/dark-factory/daemon.jsonl` telemetry stream with zero migration.

Source of truth for behavior: `docs/auto-factory-daemon-spec.md` (Final r1).
Source of truth for shape: `docs/auto-factory-daemon-design-rust.md`.
If this file and the spec ever disagree, **the spec wins** — fix this file.

---

## 0. Config

Read `config/daemon.toml` (git-ignored, per-deployment). If it does not exist,
fall back to `daemon/contracts/daemon.toml.example` and treat every value as
the effective default. Never invent config keys not in the example file.

```bash
CONF="config/daemon.toml"
[ -f "$CONF" ] || CONF="daemon/contracts/daemon.toml.example"
TARGET_REPO=$(grep -m1 '^target_repo' "$CONF" | sed -E 's/.*"(.*)".*/\1/')
BASE_BRANCH=$(grep -m1 '^base_branch' "$CONF" | sed -E 's/.*"(.*)".*/\1/')
STAGE=$(grep -m1 '^stage' "$CONF" | sed -E 's/[^0-9]*([0-9]+).*/\1/')
MAX_WORKERS=$(grep -m1 '^max_workers' "$CONF" | sed -E 's/[^0-9]*([0-9]+).*/\1/')
MAX_BATCH=$(grep -m1 '^max_batch' "$CONF" | sed -E 's/[^0-9]*([0-9]+).*/\1/')
AUTONOMY_TIMEBOX_SECS=$(grep -m1 '^autonomy_timebox_secs' "$CONF" | sed -E 's/[^0-9]*([0-9]+).*/\1/')
```

`stage=1` (the only stage these LLM skills implement) means: **re-roll
verdicts are RECORDED, never EXECUTED.** No skill in this pair ever runs the
Re-Roll Engine (branch reset, spec mutation, old-PR closure) — that is Stage 2,
Rust-daemon-only, and out of scope until the config flag flips.

---

## 1. Overlay states (spec §4.2.7 / design-rust.md `OverlayState`)

Exactly 8 states, `SCREAMING_SNAKE_CASE`, matching `schema.sql`'s CHECK constraint verbatim:

| State | Meaning |
|---|---|
| `QUEUED` | bead accepted by intake, awaiting dispatch |
| `DISPATCHED` | worker/pipeline running, no PR yet |
| `ATTESTED` | PR open, under verification |
| `RE_ROLL` | re-roll in progress (Stage 2 only) |
| `RECOVERY` | spec mutated, awaiting re-dispatch (Stage 2 only) |
| `REDISPATCHED` | handed back to the queue (Stage 2 only) |
| `BUDGET_HELD` | budget exhaustion (monitoring-only in Stage 1/2) |
| `HUMAN_HELD` | terminal until human action |

### Legal transitions

Pre-PR path (both skills touch this):
```
QUEUED --(dispatch: capacity free + routing verdict)--> DISPATCHED
DISPATCHED --(gh pr list --head finds an open PR)--> ATTESTED
```

Post-PR path (spec §4.2.7, verbatim edges — Stage 2 states included for
completeness even though Stage 1 never executes them):
```
ATTESTED --(PR rejection observed + 1-tick cooldown)--> RE_ROLL
RE_ROLL  --(abort)--> ATTESTED
RE_ROLL  --(success)--> RECOVERY
RE_ROLL  --(failure)--> HUMAN_HELD
RECOVERY --(spec validation pass)--> REDISPATCHED
RECOVERY --(budget exhaustion)--> BUDGET_HELD
RECOVERY --(conflict)--> HUMAN_HELD
BUDGET_HELD --(reset)--> REDISPATCHED
HUMAN_HELD --(human action)--> RE_ROLL (or closed, terminal)
```

**Stage-1 substitution rule (binding for these skills):** whenever the spec's
transition graph would enter `RE_ROLL` (i.e. a re-roll-worthy verdict on
`CHANGES_REQUESTED`), these skills do NOT create that row transition. Instead
they emit `REROLL_VERDICT_RECORDED` and move the bead straight to `HUMAN_HELD`,
per spec §4.2.9: *"Stage 1 — verifier plane only (re-roll disabled)... re-roll
verdicts are recorded but not executed (beads park in HUMAN_HELD)."*
In-place-fixable verdicts do NOT change state — the owning `aow`/Agent-tool
session keeps remediating; the skill only records the verdict.

Any state → `HUMAN_HELD` when cumulative `autonomy_secs` exceeds
`autonomy_timebox_secs` (default 10800 = 3h), or the owning session is
stalled/dead. Nothing on the automated path resets `autonomy_secs`.

---

## 2. Canonical event-type vocabulary

Exactly these 10 `event_type` values. Never invent a new one; if a step does
not map cleanly to one of these, it is out of scope for Stage 1.

| event_type | Emitted by | When |
|---|---|---|
| `TICK` | both | once per invocation, summarizing counts |
| `INTAKE_BEAD_CREATED` | coder | new `factory`-labeled bead gets a `QUEUED` row |
| `TASK_ROUTED` | coder | model judgment produced `SMALL_PATH`\|`STANDARD_PATH` |
| `TASK_DISPATCHED` | coder | a coder subagent was spawned on a fresh branch |
| `PR_OPENED` | coder | `gh pr list --head` found the dispatched branch's PR |
| `GATE_ASSESSMENT` | verifier | all 7 gates evaluated for one PR |
| `READY_FOR_MERGE` | verifier | all 7 gates green; readiness comment posted |
| `REROLL_VERDICT_RECORDED` | verifier | model judged in-place-fixable vs re-roll-worthy |
| `PARKED_HUMAN_HELD` | both | a bead entered `HUMAN_HELD` this tick |
| `BUDGET_WARNING` | both | `autonomy_secs` crossed 80% of the time-box |

---

## 3. CXDB (SQLite) — copy-pasteable one-liners

DB file: `~/.dark-factory/daemon-cxdb.sqlite`. Schema is BINDING —
`daemon/contracts/schema.sql` — never add/rename columns from a skill; if the
schema is insufficient, that is a design gap to report, not to patch locally.

**Init (idempotent, run once per host):**
```bash
mkdir -p ~/.dark-factory
sqlite3 ~/.dark-factory/daemon-cxdb.sqlite < daemon/contracts/schema.sql
```

**Read one bead's overlay:**
```bash
sqlite3 -json ~/.dark-factory/daemon-cxdb.sqlite \
  "SELECT * FROM bead_overlay WHERE bead_id='$BEAD_ID';"
```

**Read all beads in a given state:**
```bash
sqlite3 -json ~/.dark-factory/daemon-cxdb.sqlite \
  "SELECT bead_id, attempt, pr_number, branch, autonomy_secs FROM bead_overlay WHERE state='QUEUED';"
```

**Insert-if-absent (intake; never resets an existing row):**
```bash
sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
  "INSERT INTO bead_overlay (bead_id, state, attempt, reroll_count, autonomy_secs, spend_usd, updated_at)
   VALUES ('$BEAD_ID', 'QUEUED', 1, 0, 0, 0, strftime('%Y-%m-%dT%H:%M:%SZ','now'))
   ON CONFLICT(bead_id) DO NOTHING;"
```

**Transition state (generic upsert-by-update):**
```bash
sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
  "UPDATE bead_overlay
   SET state='$NEW_STATE', pr_number=COALESCE(NULLIF('$PR_NUMBER',''), pr_number),
       branch=COALESCE(NULLIF('$BRANCH',''), branch),
       updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now')
   WHERE bead_id='$BEAD_ID';"
```

**Increment autonomy_secs (call every tick for active beads):**
```bash
sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
  "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + $ELAPSED_SECS,
   updated_at=strftime('%Y-%m-%dT%H:%M:%SZ','now') WHERE bead_id='$BEAD_ID';"
```

**Register a branch (deletion guard — spec §4.2.8):**
```bash
sqlite3 ~/.dark-factory/daemon-cxdb.sqlite \
  "INSERT INTO branch_registry (branch, bead_id, created_at)
   VALUES ('$BRANCH', '$BEAD_ID', strftime('%Y-%m-%dT%H:%M:%SZ','now'))
   ON CONFLICT(branch) DO NOTHING;"
```

**Read owned branches (the ONLY refs any deletion may ever target):**
```bash
sqlite3 -json ~/.dark-factory/daemon-cxdb.sqlite "SELECT branch FROM branch_registry;"
```

---

## 4. Telemetry — copy-pasteable append snippet

Log file: `~/Library/Logs/dark-factory/daemon.jsonl` (single flat file per
spec §4.2.9 — NOT the per-repo/branch tree the Python runner's perf-log uses).
Schema is BINDING — every field, every event, matches
`docs/auto-factory-daemon-design-rust.md` §5 `TelemetryEvent` exactly:
`timestamp, bead_id, attempt_id, lifecycle_state, event_type, metrics, context`.

```bash
mkdir -p ~/Library/Logs/dark-factory
emit_telemetry() {
  # args: bead_id attempt_id lifecycle_state event_type metrics_json context_json
  local bead_id="$1" attempt_id="$2" lifecycle_state="$3" event_type="$4" metrics="${5:-{}}" context="${6:-{}}"
  jq -nc \
    --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg bead_id "$bead_id" \
    --argjson attempt_id "$attempt_id" \
    --arg lifecycle_state "$lifecycle_state" \
    --arg event_type "$event_type" \
    --argjson metrics "$metrics" \
    --argjson context "$context" \
    '{timestamp:$ts, bead_id:$bead_id, attempt_id:$attempt_id,
      lifecycle_state:$lifecycle_state, event_type:$event_type,
      metrics:$metrics, context:$context}' \
    >> ~/Library/Logs/dark-factory/daemon.jsonl
}
# Example:
emit_telemetry "bead-abc123" 1 "DISPATCHED" "TASK_DISPATCHED" \
  '{}' '{"activeModel":"minimax","branch":"factory/bead-abc123-r1"}'
```

Use `printf` directly only for the trivial case where `metrics`/`context` are
already-known-safe literal JSON; prefer `jq -nc` above for anything containing
bead titles, PR text, or model output (unescaped `printf '%s'` on untrusted
text will produce invalid JSONL).

---

## 5. Safety envelope — absolute rules (spec §4.2.8)

These are **NEVER** rules. No routing verdict, no model judgment, no time
pressure overrides them.

1. **Force-push: NEVER.** Neither skill ever runs `git push --force*`. Re-rolls
   (Stage 2, not implemented here) always create fresh branches instead.
2. **Merge: NEVER.** Neither skill ever runs `gh pr merge` or equivalent.
   Terminal state is `READY_FOR_MERGE` + a posted readiness comment. A human
   (or a separately-approved flow) merges.
3. **Branch deletion: registry-only.** Only branches present in
   `branch_registry` (§3 above) may ever be deleted, and Stage 1 skills do not
   delete branches at all (deletion is a Stage-2 Re-Roll Engine action).
4. **3-hour cumulative time-box.** `autonomy_secs` accumulates across a bead's
   entire chain and never resets on the automated path. Crossing
   `autonomy_timebox_secs` (default 10800) forces `HUMAN_HELD` +
   `PARKED_HUMAN_HELD`, unconditionally.
5. **Spawn caps: ≤30 concurrent, ≤15 per batch.** Count active beads as
   `state IN ('DISPATCHED','ATTESTED')` before dispatching more; never dispatch
   past `max_workers`, never dispatch more than `max_batch` in one tick.
6. **`stage=1` ⇒ re-roll verdicts are RECORDED, not EXECUTED.** See §1 above.
   Never perform a branch reset, spec mutation, or old-PR closure while
   `stage=1`.
7. **ZFC — no keyword routing.** `TASK_ROUTED` and the in-place-fixable vs
   re-roll-worthy verdict are model judgments made by the skill executor (the
   LLM itself), never `if title.contains(...)` or similar. If a verdict can't
   be produced, that is a parse failure → the bead goes `HUMAN_HELD`, never a
   silently-defaulted path.
8. **Read-only on PRs in Stage 1.** The verifier skill never pushes commits,
   never closes PRs, never edits existing review threads — it only reads PR
   state and posts new comments (readiness summary).

---

## §7 Harness supremacy (added 2026-07-03 per /advice Round: Opus mitigation)

All binding mutations — CXDB writes, state transitions, cap enforcement,
telemetry emission — MUST go through `daemon/factory-lite-harness.sh`
(subcommands: init, intake-upsert, route-record, capacity, dispatch-record,
pr-opened, autonomy-tick, gate-assessment, prev-gate-assessment, ready,
reroll-verdict, park, tick-summary, list). The sqlite3/telemetry one-liners in
§3-§4 above are the REFERENCE SPEC of what the harness does internally — skills
must NOT run them directly. The LLM supplies only typed judgment verdicts
(routing, gate values, reroll verdicts) as harness arguments; the harness
validates every enum and refuses illegal transitions. This keeps the telemetry
and CXDB deterministic — they are the data the Rust daemon inherits.
