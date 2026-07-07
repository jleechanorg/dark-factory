# Nextsteps — ultracode adversarial gap review: factory label → autonomous green PR — 2026-07-06

## Executive summary

- **53-agent ultracode workflow** (6 dimension reviewers → adversarial refutation per finding → synthesis): 46 raw findings, **42 confirmed / 4 refuted**. Full report: `docs/factory-goal-gap-review-2026-07-06.md`.
- **Verdict: ~2 of 6 stages functional.** Intake GREEN (label→bead idempotent, both lanes). Dispatch AMBER (2 real `ao spawn`s ever → PRs #8177/#8178; 6/8 beads parked at LLM-router parse failure). Everything downstream RED: no automated HUMAN_HELD exit (Stage 1 pinned), /er has no runner (hardcoded Absent), /code-standards + /zfc absent from autonomous path, merge step has no caller, only scheduler artifact is a macOS launchd plist on a Linux host.
- **Zero-touch label→green→merge has never happened once**; all merged factory PRs were merged manually by jleechan2015. Live steady state at review time: 8/8 overlay beads HUMAN_HELD, tick 2606 all-zero metrics, ~8h idle ticks — "intake work, then silently abandon it."
- **Correction to prior roadmap claims:** the Rust daemon poll loop WAS live (fresh ticks observed) — but hand-launched from an IDE terminal, unsupervised. Also "shell/Rust paths aligned" is **false** (shell lacks author permission gate, misses directly-labeled PRs, un-parks Rust's deliberate parks), and `docs/auto_factory_spec_gap_analysis.md` cited by the 2026-07-06 intake nextsteps **was never committed**.
- **Separate parallel review:** `scripts/setup-agent-hooks.sh` (uncommitted) — all 11 runtime claims verified true, but **3 of 4 CLI hook templates are rotated one CLI off** (only Codex correct; Cursor/Gemini/OpenCode hooks silently never fire) and `--check` self-certifies the broken state. Report: `docs/setup-agent-hooks-review-2026-07-06.md`.

## Bead index (this session)

| Bead | Title | Priority |
|------|-------|----------|
| jleechan-qdw | reliability: per-tick error isolation + backoff + ETag cache | P0 |
| jleechan-1m4 | systemd user unit for Rust daemon (durable Linux trigger) | P0 |
| jleechan-g1k | fix router LLM fallback chain (claude flags passed as message text, adapters.rs:983) | P0 |
| jleechan-gib | automated HUMAN_HELD exit (Stage 2 or port recover-held into Rust tick) | P0 |
| jleechan-qqq | wire an automated /er runner (gate 6 permanently Unknown) | P0 |
| jleechan-240 | add /code-standards and /zfc gates to autonomous path | P1 |
| jleechan-ydr | un-hardcode or retire factory-af-tick.sh (3 dead bead IDs) | P2 |
| jleechan-s3c | schedule merge/ready step + resolve spec "merge never" vs cutover X4 | P1 |
| jleechan-3ff | fix setup-agent-hooks.sh: re-rotate 3 CLI templates + escape HOOK_PATH + harden --check | P1 |
| jleechan-niq | self-hosting ratchet: factory drives its own blockers + canary smoke + watchdog | P1 |

## Work queue (minimal path to ONE zero-touch green PR, in order)

1. **jleechan-qdw** — per-tick error isolation/backoff/ETag cache. This must land before
   systemd restart supervision; otherwise one `gh` 403/timeout becomes a rate-limit-burning crash
   loop.
2. **jleechan-1m4** — Linux `systemd --user` unit for the Rust daemon. Start with durable process
   supervision; add `Type=notify`/`WatchdogSec` only after the binary emits a real heartbeat.
3. **jleechan-g1k** — router fallback bug parked 6/8 beads before any coder spawned.
4. **jleechan-gib** — HUMAN_HELD terminal dead-end = the severed iterate loop; stop autonomy clock
   during `ci_pending` and require a recovery/reroll/human-required verdict.
5. **Read-only watchdog slice of jleechan-niq** — observe only at first: out-of-band alerting,
   deduped incidents, state-specific dwell thresholds, and meaningful-progress metrics that
   exclude HUMAN_HELD→QUEUED churn and canary-only movement.
6. **Daily canary slice of jleechan-niq** — classify as liveness smoke until it proves the full PR
   lifecycle with the required evidence class. Require 3 consecutive successes plus one
   non-canary bead escaping HUMAN_HELD autonomously before self-hosting handoff.
7. **jleechan-qqq** — /er runner (independent reviewer, not implementing agent); without it
   `all_green=false` forever.
8. **jleechan-240** — /code-standards + /zfc lanes; fix `gate_cs → exit` (no fix loop) and
   universal-prompt ZFC misdefinition.
9. **jleechan-s3c** → **jleechan-ydr** as follow-ons. Keep watchdog/canary/supervisor/evidence
   rules write-locked against autonomous factory edits until the ratchet has promotion history.

## Self-hosting ratchet constraints

Authoritative correction: `docs/adversarial-review-miss-retrospective-2026-07-06.md`. The original
`/innov` ratchet direction still stands, but its sequencing and metrics are refined:

- **Read-only watchdog first:** watchdog/timer code may observe, alert out-of-band, and create
  deduped incidents only after alert-only burn-in. It must not silently route its own P0s back into
  the broken factory as the only notification path.
- **Canary is liveness smoke:** a trivial daily canary is not E2E until it proves a full PR
  lifecycle with the evidence class required by the evidence-standards matrix.
- **Promotion gate before self-hosting:** require 3 consecutive canary successes plus one
  non-canary bead autonomously escaping `HUMAN_HELD` before the factory receives its own low-risk
  blocker beads.
- **Oversight write-lock:** watchdog, canary definitions, supervisor units, evidence rules, and
  verifier prompts stay outside autonomous factory edits until the ratchet has promotion history.
- **Zero-touch ledger:** record `pure zero-touch`, `watchdog-assisted`, `human-assisted`, and
  `failed/stalled` buckets distinctly.
- **Correlation IDs:** every daemon/runner handoff records bead id, branch, PR, runner run id, head
  SHA, and evidence bundle hash so cutover claims can be audited.

## Open-PR sweep addendum

The limit-interrupted Claude review sweep is now captured in
`docs/pr-review-sweep-2026-07-06.md`:

- PR [#174](https://github.com/jleechanorg/dark-factory/pull/174) is APPROVE and closes
  `jleechan-ydr` only after merge; follow-ups are `jleechan-6kwn` (record-before-spawn) and
  `jleechan-gfa6` (arbitrary-QUEUED regression).
- PR [#172](https://github.com/jleechanorg/dark-factory/pull/172) is REQUEST CHANGES and is
  macOS-only; it does not move `jleechan-1m4`.
- PR [#173](https://github.com/jleechanorg/dark-factory/pull/173) is REQUEST CHANGES; keep CI
  bash wiring but replace the self-certifying vendored probe with a real-file probe.
- PR [#163](https://github.com/jleechanorg/dark-factory/pull/163) is NEEDS-REBASE +
  REQUEST CHANGES; `jleechan-seey` tracks the grounded SHA-bound skeptic fix. It does not close
  `jleechan-qqq` or `jleechan-240`.

## Key evidence pointers

- Full findings + verifier reasoning: `docs/factory-goal-gap-review-2026-07-06.md` (Appendix A: 42 findings; Appendix B: 44 verified-working items).
- Hooks installer review: `docs/setup-agent-hooks-review-2026-07-06.md`.
- Cutover exit criteria: **0/10 met** by the repo's own scorecard (all real-adapter tests `#[ignore]`).
- The one autonomous READY ever emitted was a false positive (fix landed, never validated live).
- Smoke issue [#8164](https://github.com/jleechanorg/worldarchitect.ai/issues/8164) still OPEN; completed work never reconciled back to GH issues (loop open at both ends).

## Learnings pointer

- `~/roadmap/learnings-2026-07.md` — 2026-07-06 entries: gate self-certification anti-pattern (`--check` greps its own template's sentinel), roadmap-overclaim pattern (docs asserting uncommitted artifacts), adversarial-workflow calibration (42/46 findings survived refutation).

## Roadmap pointer

- Prior: `roadmap/nextsteps-2026-07-06-auto-factory-intake-callpath.md` (intake + callpath), `roadmap/nextsteps-2026-07-05-auto-factory-ironclad.md`.
- This doc supersedes the "paths aligned" claim in the 2026-07-06 intake nextsteps.
