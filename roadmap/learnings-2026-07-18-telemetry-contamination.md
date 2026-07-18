# 2026-07-18 — Telemetry contamination from a duplicate `daemon --help` (jleechan-bze8.4)

## What happened (live reproduction)

- An ostensibly read-only diagnostic invocation of
  `daemon/target/release/daemon --help` (PID 3486748) on **2026-07-18
  around 18:35Z–18:41Z** entered the live tick loop while the
  systemd-managed daemon (PID 1621182) was already active.
- The binary ignored `--help` because `parse_args()` silently accepted
  unknown flags: `Some(first_flag) => { for arg in all_args { match
  arg.as_str() { "--once" => ..., "--dry-run" => ..., _ => {} } } }`.
  The fallback `_ => {}` arm meant `--help`/`--version`/anything else
  fell through to `CommandMode::Daemon(args)` with no side effect from
  the parser; `main()` proceeded straight to `run(args)` which
  begins tick reconciliation.
- Two daemons interleaved telemetry (overlapping `tick_index` ranges
  0..5 vs 748..755). Both dispatched df-184/185/186, both mutated
  overlays, and *neither* telemetry stream is trustworthy for that
  window. The accidental process was terminated, leaving only the
  systemd daemon; the leftover df-* dispatches were re-dispatched by
  the systemd process, which is why each bead shows two `DISPATCHED`
  events with no intermediate `HUMAN_HELD`.
- Postmortem found the missing field: telemetry events emitted by
  `tick.rs::emit` carried no `instance_uuid`, so there was no way to
  reconstruct which process emitted which line. The duplicate process
  had no startup-only fingerprint stamped onto later ticks.

## What is excluded from autonomy evidence

All daemon-side telemetry between **2026-07-18T18:35:00Z and
2026-07-18T18:41:00Z UTC** is contaminated and excluded from `/af`
autonomy evidence and from hand-rolled `/goal`-driven missions:

1. The `daemon.jsonl` lines emitted in that window.
2. The `DISPATCHING`/`DISPATCHED` overlay rows in
   `~/.dark-factory/daemon-cxdb.sqlite` mutated during the window.
3. PR dispatches `df-184`, `df-185`, `df-186` (re-dispatched by the
   systemd daemon after the duplicate process died — they show two
   `DISPATCHED` events each).

The exclusion list is the canonical reference for any audit that
claims `/af` reached some state in mid-July 2026: any such claim must
either step outside the contaminated window OR prove it pulled
telemetry that survived the duplicate process.

## The fix (PR jleechanorg/dark-factory#332)

1. **Strict CLI parsing** — `parse_daemon_flags` rejects any flag
   that is not `--once`, `--dry-run`, `--help`, `-h`, `--version`,
   or `-V`. `--help` and `--version` return a `DaemonPreFlight`
   directive so `main()` exits 0 *without* touching the lease,
   CXDB, or timers. Unknown flags return `Err` so `main()` exits
   non-zero with no side effects.
2. **Single-instance lease** — `daemon::instancelock::acquire`
   performs an atomic `mkdir(2)` of `<cxdb_dir>/daemon.lock.d/`
   (POSIX guarantees the `mkdir` race is atomic across the kernel).
   The lease carries `LeasePayload{pid, start_time_unix_secs,
   instance_uuid, executable_sha256, config_identity}`. A second
   daemon sees `AlreadyHeld { holder, ... }` and exits non-zero
   without dispatching or writing telemetry. Stale leases are
   reclaimed by `kill(holder.pid, 0)` — if the recorded PID is no
   longer alive, the daemon `rmdir`s the lease dir and acquires
   fresh.
3. **Per-process instance UUID** — `telemetry::emit` now stamps
   every line with `instanceUuid`, sourced from
   `telemetry::instance_uuid()` (a `OnceLock<String>` set at
   startup, default `"none"` for subcommands like `recover-held`
   that bypass the tick loop). The startup `DAEMON_STARTED` event
   also carries `pid`, `startTimeUnixSecs`, `executableSha256`,
   `configIdentity`, and the lock dir path so postmortem operators
   can verify which process produced which telemetry.
4. **Acceptance test**: `daemon/tests/instancelock_integration.rs`
   spawns the binary as a subprocess and exercises (a) `--help`
   exits 0 with no telemetry; (b) `--version` exits 0 with no
   telemetry; (c) unknown flags exit non-zero with no telemetry;
   (d) two concurrent daemon processes — exactly one wins the
   lease, the other exits non-zero, and `DAEMON_STARTED` count
   on disk is at most one.

## Related beads (cross-references)

- `jleechan-98v3` — manual overlay writers vs the daemon (distinct
  but adjacent — that bead is about *external* writers touching the
  CXDB; bze8.4 is about *internal* duplicate daemons).
- `jleechan-goal-unattended-e2e-2026-07-17-bze8.4` — the goal in
  which this fix lives.
