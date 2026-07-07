# Open-PR review sweep — 2026-07-06 (4 parallel reviewers, gap-review-informed)

Four parallel review agents, each primed with `docs/factory-goal-gap-review-2026-07-06.md` and
`docs/adversarial-review-miss-retrospective-2026-07-06.md`, reviewed the substantive open PRs.
Reviews ran against fetched branches; both #174 test suites were executed (9/9, 30/30 pass) and
#163 was compiled + lib-tested (73/73, clippy clean). GraphQL was rate-limited; REST + git only.

| PR | Verdict | Gap-review impact |
|---|---|---|
| [#174](https://github.com/jleechanorg/dark-factory/pull/174) structured dispatch codes + un-hardcode bead IDs | **APPROVE** | Closes `jleechan-ydr` on merge |
| [#172](https://github.com/jleechanorg/dark-factory/pull/172) launchd installer + 7 fixes | **REQUEST CHANGES** | Orthogonal to `jleechan-1m4` (Linux systemd) |
| [#173](https://github.com/jleechanorg/dark-factory/pull/173) CI bash tests + vendored callpath probe | **REQUEST CHANGES** | Stale; half superseded by `2a57f8c` |
| [#163](https://github.com/jleechanorg/dark-factory/pull/163) reviewer gaps (pqip/t4g5/q525/kngu) | **NEEDS-REBASE + REQUEST CHANGES** | Does NOT move `jleechan-qqq`/`jleechan-240` |

## #174 — APPROVE (merge; file 2 follow-ups)

- Hardcoded allowlist fully removed: default `bead_filter=""`; SELECT returns any
  `QUEUED/ATTESTED` bead with a PR, ordered by `updated_at`; `AFD_BEAD_FILTER`/`AFD_PRIORITY_BEADS`
  opt-in; priority is ORDER BY only. **`jleechan-ydr` closes on merge.**
- Exit codes are a real contract: `factory-overlay.sh die_code` (2/3/4/5/6/7/9) consumed by
  `factory-af-tick.sh:182-209` with distinct handling; only rc=0 increments `dispatched`.
- ZFC improved: stderr keyword-grepping (`*over capacity*`…) replaced by exit-code cases (exempt
  deterministic contract).
- Follow-ups (non-blocking): (1) record-before-spawn ordering unchanged —
  `factory-af-tick.sh:169` remediate/`ao spawn` still precedes the `:178` DISPATCHED record
  (bead `jleechan-6kwn`); (2) no positive regression test that an arbitrary QUEUED bead is
  selected with the filter unset — a re-added allowlist would pass all current tests
  (bead `jleechan-gfa6`). Dead codes `EX_NOT_FOUND=8`/`EX_NOOP=10` never emitted.

## #172 — REQUEST CHANGES (macOS-lane hygiene, not the scheduling blocker)

- 🔴 CRITICAL `plist:191-201`: `KeepAlive=true` + `StartInterval=240` on a short-lived tick script
  → relaunch on every exit, floored only by `ThrottleInterval=60`. Real cadence ≈60s; the
  configurable `AFD_TICK_INTERVAL_SEC` knob (its own acceptance criterion) is a no-op. Fix: drop
  KeepAlive, use StartInterval alone.
- 🟠 PATH fix diverges from the hermes pattern: wrapper sources ONE profile via elif
  (`.bash_profile` OR `.profile` OR `.bashrc`) instead of all three in sequence, and never adds
  `~/.local/bin` — where `br` actually lives. `FileNotFoundError: br` can recur.
- 🟠 `set -e` active while sourcing the profile (only `set +u` guarded) — a nonzero return in
  `.bash_profile` kills the wrapper pre-exec, retried every 60s forever.
- 🟠 ThrottleInterval guards the SHELL tick, not the Rust daemon whose `main.rs:277` exit-on-gh-error
  is the documented crash-loop hazard. Does not close it; a future KeepAlive plist pointed at the
  Rust daemon without per-tick isolation IS the rate-limit-burning loop.
- 7/8 claimed fixes real (F3 interval is the no-op); all untested on a real host (`bash -n` +
  `--dry-run` + plutil only); no install-time check that the ProgramArguments target exists.
- **100% macOS.** Does nothing for `jleechan-1m4` (Linux systemd unit for the Rust daemon) and the
  supervised target still had the hardcoded bead IDs until #174 merges.

## #173 — REQUEST CHANGES (rebase + fix circular probe)

- 🔴 Stale: real merge conflicts with current main (`ci.yml` daemon-tests block,
  `test_callpath_overlay_harness.sh`).
- 🔴 Daemon-fetch half duplicates `2a57f8c` already on main — drop the branch's step in rebase.
- 🟠 Vendored probe never runs against the real `daemon/factory-overlay.sh` — only generated stubs
  sharing the SAME hardcoded 19-subcommand list (probe `:78-98`, test `:146-166`, stub).
  Near-circular; a 20th subcommand added to the real overlay fails nothing. Same
  gate-self-certification anti-pattern as `setup-agent-hooks.sh --check`. Fix: one assertion
  running the probe against the real file expecting `ok/19`.
- ✅ Keep: CI wiring mechanism is correct (every-PR trigger, no path filter, `set +e` aggregation,
  `::group::` annotations); `test_factory_overlay.sh` is a genuine integration test. Coverage of
  `test_factory_af_tick.sh` is contingent on rebasing (absent from the stale branch, glob picks it
  up post-rebase).

## #163 — NEEDS-REBASE + REQUEST CHANGES (independence up, grounding still absent)

- 🔴 Grounded-gate rule fails: skeptic prompt (`tick.rs:407-413`) interpolates only bead_id + PR —
  no diff, no head SHA, no cwd. Stale verdicts from earlier commits still count.
- 🔴 "sign-off" subsystem self-certifiable (`tick.rs:497-511`): any non-bot comment containing
  `verdict: pass`/`/skeptic pass` flips signoff→pass; the implementing agent's own account
  qualifies. Combined with no SHA binding: spoofable.
- 🟡 3-subsystem gate effectively inert in prod (no automated GHA-skeptic producer wired; only
  green path is the insecure sign-off). Changes the park reason, not the outcome.
- 🟡 pqip `spawn_batch` is dead code masked by a new crate-level `#![allow(dead_code)]`;
  q525 (`prefer_adversarial`) not evidently implemented; 6 raw `eprintln!` in intake.
- ✅ Keep: reviewer-CLI-vendor ≠ coder-vendor independence (`tick.rs:420-441`), minimax default +
  `spawn_with_fallback` (t4g5), `reroll.rs` quote-escape bug fix, REST fallbacks for rate limits.
- Hard conflict on current main in `adapters.rs` `labeled_issues` — rebase first.
- **Does not close `jleechan-qqq`** (`tick.rs:532` still hardcodes `ErVerdict::Absent`) **or
  `jleechan-240`** (no code-standards/zfc anywhere in the diff).

## Cross-PR observations

1. **The self-certification anti-pattern keeps reappearing** (third + fourth instances today:
   #173's stub-only probe, #163's sign-off comment gate) — reinforces the metric-gaming check in
   the retrospective's harness fixes.
2. **Merge order matters:** #174 first (unblocks dispatch, closes ydr) → #173 rebase (its glob then
   runs main's af-tick tests against #174's refactor) → #172 only after the KeepAlive fix →
   #163 after rebase + SHA binding.
3. **Still nobody is building the Linux scheduler** (`jleechan-1m4`) — both scheduling PRs are
   macOS launchd. The primary factory host remains unsupervised.
4. **None of these PRs supersede the refined ratchet order:** #174 improves shell dispatch but
   dispatch confidence still depends on `jleechan-6kwn` and `jleechan-gfa6`; #172 is not the Linux
   scheduler; #163 does not close `/er` (`jleechan-qqq`) or `/code-standards`/`/zfc`
   (`jleechan-240`). The critical path remains `jleechan-qdw` before `jleechan-1m4`.
