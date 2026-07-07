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
| jleechan-1m4 | systemd user unit for Rust daemon (durable Linux trigger) | P0 |
| jleechan-g1k | fix router LLM fallback chain (claude flags passed as message text, adapters.rs:983) | P0 |
| jleechan-gib | automated HUMAN_HELD exit (Stage 2 or port recover-held into Rust tick) | P0 |
| jleechan-qqq | wire an automated /er runner (gate 6 permanently Unknown) | P0 |
| jleechan-240 | add /code-standards and /zfc gates to autonomous path | P1 |
| jleechan-ydr | un-hardcode or retire factory-af-tick.sh (3 dead bead IDs) | P2 |
| jleechan-s3c | schedule merge/ready step + resolve spec "merge never" vs cutover X4 | P1 |
| jleechan-qdw | reliability: per-tick error isolation + backoff + ETag cache | P1 |
| jleechan-3ff | fix setup-agent-hooks.sh: re-rotate 3 CLI templates + escape HOOK_PATH + harden --check | P1 |

## Work queue (minimal path to ONE zero-touch green PR, in order)

1. **jleechan-1m4** — systemd user unit, `Restart=always`; today one IDE close kills the factory.
2. **jleechan-g1k** — router fallback bug parked 6/8 beads before any coder spawned.
3. **jleechan-gib** — HUMAN_HELD terminal dead-end = the severed iterate loop; also stop autonomy clock during ci_pending.
4. **jleechan-qqq** — /er runner (independent reviewer, not implementing agent); without it all_green is structurally unreachable.
5. **jleechan-240** — /code-standards + /zfc lanes; fix `gate_cs → exit` (no fix loop) and universal-prompt ZFC misdefinition.
6. **jleechan-s3c** → **jleechan-ydr** → **jleechan-qdw** as follow-ons.

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
