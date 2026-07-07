# Auto-Factory Daemon — bootstrap overview

Bootstrap for the Auto-Factory Daemon: the automated backward-recovery loop
that manages PR feedback (`CHANGES_REQUESTED` → re-dispatch) without human
intervention. Behavior is specified in
[`docs/auto-factory-daemon-spec.md`](../docs/auto-factory-daemon-spec.md)
(**Final r3.1** — Round 4 AAR of external spec variant ASF-SR-2.4 plus the
READY terminal overlay state from the 2026-07-03 adversarial review). Shape (crate layout,
traits, LOC budget) is specified in
[`docs/auto-factory-daemon-design-rust.md`](../docs/auto-factory-daemon-design-rust.md).
**If the two disagree, the spec wins.**

## Skills-as-executable-spec (today's bootstrap)

Before the Rust daemon exists, the same behavior runs as Claude Code skills
that treat the spec as directly executable rather than translated by hand:

- `.claude/skills/auto-factory/SKILL.md` — orchestrator: load contract,
  intake (beads + GH issues), route (LLM-classified tier), dispatch
  parallel coders, run verifier ticks, advance state through
  QUEUED → DISPATCHED → ATTESTED → READY.
- `daemon/factory-overlay.sh` — binding deterministic harness; restored in
  [PR #167](https://github.com/jleechanorg/dark-factory/pull/167) from
  `e60b5a31b~1:daemon/factory-lite-harness.sh`. Owns all sqlite3 mutations
  to `~/.dark-factory/daemon-cxdb.sqlite`. 19 subcommands: `init`,
  `intake-upsert`, `route-record`, `capacity`, `dispatch-record`,
  `pr-opened`, `autonomy-tick`, `gate-assessment`, `prev-gate-assessment`,
  `ready`, `reroll-verdict`, `park`, `park-duplicate`, `bead-closed-check`,
  `tick-summary`, `recover-held`, `unstick-dispatching`, `redrive-pr`,
  `list`.
- `daemon/factory-af-tick.sh` — one deterministic /af tick (intake +
  recover + AO dispatch for drive-existing-pr beads).
- `daemon/factory-ao-remediate.sh` — spawns AO worker for an ATTESTED bead
  with isolated worktree at `/tmp/<bead>-wt`.
- `daemon/factory-tick.sh` — recover → unstick → intake-from-gh → callpath.

All state mutation goes through `daemon/factory-overlay.sh`, a
deterministic (non-LLM) harness that validates every enum transition and
refuses illegal ones — **the LLM supplies judgment, the harness owns
mutations**, so stray model output can't corrupt state. This is the split
the eventual Rust daemon implements natively.

> Historical: the original factory-lite-coder / factory-lite-verifier skills
> and `daemon/factory-lite-harness.sh` were removed in commit `e60b5a31b`
> (2026-07-05, jleechan-xrdx). Their contracts were folded into
> `daemon/factory-overlay.sh` + `.claude/skills/auto-factory/SKILL.md`.

## Binding contracts

`daemon/contracts/` is the data contract both the skills and the future
Rust daemon honor identically, zero migration:

- **`schema.sql`** — schema for `~/.dark-factory/daemon-cxdb.sqlite`.
  `bead_overlay` (9-state `CHECK`: `QUEUED`, `DISPATCHED`, `ATTESTED`,
  `READY`, `RE_ROLL`, `RECOVERY`, `REDISPATCHED`, `BUDGET_HELD`, `HUMAN_HELD`) plus
  `branch_registry`, the deletion guard — only branches recorded here may
  ever be deleted (spec §4.2.8).
- **`daemon.toml.example`** — config contract (spec §4.2.9, design doc §5):
  target repo, stage gate (`stage = 1` = verifier-plane-only; re-roll
  verdicts recorded, never executed), worker/batch caps, tick intervals,
  3h cumulative autonomy time-box.

Both are **binding** — a new field needed by a skill or the daemon is a
design gap to report upstream, not to patch locally.

## Telemetry

Every tick emits JSONL to one flat file:
`~/Library/Logs/dark-factory/daemon.jsonl` (deliberately *not* the
per-repo/branch tree the Python pipeline runner's perf-log uses — spec
§4.2.9). Each event is `{timestamp, bead_id, attempt_id, lifecycle_state,
event_type, metrics, context}`, matching design doc §5's `TelemetryEvent`.
The 10 canonical `event_type` values (`TICK`, `INTAKE_BEAD_CREATED`,
`TASK_ROUTED`, `TASK_DISPATCHED`, `PR_OPENED`, `GATE_ASSESSMENT`,
`READY_FOR_MERGE`, `REROLL_VERDICT_RECORDED`, `PARKED_HUMAN_HELD`,
`BUDGET_WARNING`) are enumerated in `CONTRACT.md` §2; none may be invented.

## Linux user service

The durable Linux trigger for the Rust daemon is the systemd user unit in
`daemon/systemd/`. It runs the release binary directly, not the legacy shell
tick lane:

```bash
daemon/systemd/install-systemd-user.sh
systemctl --user status ai.dark-factory.daemon.service --no-pager
journalctl --user -u ai.dark-factory.daemon.service -n 100 --no-pager
```

The installer builds `daemon/target/release/daemon`, renders
`ai.dark-factory.daemon.service`, verifies user lingering for boot persistence,
then runs `systemctl --user enable --now`. The unit uses `Type=notify`,
`Restart=on-failure`, and `WatchdogSec=7200s`; the daemon sends `READY=1`
after startup and `WATCHDOG=1` after each classified tick. Use `--dry-run` to
inspect commands without mutating the host, `--render-only` to print the
rendered unit, and `--uninstall` to disable and remove it.

## Planned Rust Stage-1 crate layout

Per design doc §2: single-threaded poll loop, no async runtime
(concurrency lives in AO workers, not the daemon); shells out to
`git`/`gh`/`br`/`ao`/LLM CLIs rather than linking SDKs. Stage 1 (verifier
plane) budget ≈ 1,220 LOC:

```
daemon/src/
├── main.rs         ~180  poll loop, tick tiers, startup reconciliation, stage gate
├── config.rs        ~60  config/daemon.toml via `toml` crate
├── telemetry.rs     ~50  JSONL events → ~/Library/Logs/dark-factory/daemon.jsonl
├── state.rs        ~160  CXDB overlay store (rusqlite, WAL, own daemon-cxdb.sqlite)
├── tools.rs        ~220  the 5 tool traits + subprocess implementations
├── intake.rs       ~140  issue→bead normalizer + write-tier auth (spec §4.2.3)
├── router.rs        ~70  ZFC task router: render prompt, 1 LLM call, parse verdict
├── dispatch.rs     ~120  slot supervisor (≤30 workers, batch ≤15) + handoff
└── verifier.rs     ~220  7/8-green gates, ETag cache, evidence floor (spec §4.2.5)
```

`reroll.rs` (~180) and `constraints.rs` (~100) are Stage 2 (re-roll writer
plane) — present in the crate but never invoked while `stage = 1`. Total ≈
1,500 LOC including Stage 2. Dependencies capped at five: `rusqlite`,
`serde`, `serde_json`, `toml`, `thiserror`.

## Status

This bootstrap (skills + contracts) is the pilot; the Rust crate under
`daemon/src/` is being scaffolded separately (bead `jleechan-907`). The
contracts here are the frozen interface both implementations must honor.
