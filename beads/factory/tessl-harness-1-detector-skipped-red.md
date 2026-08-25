---
bead_id: tessl-harness-1-detector-skipped-red
title: "vacuous_red_green: separate DetectorSkipped from BaselineFailed (R2 redesign)"
target_repo: jleechanorg/dark-factory
labels: factory
priority: P1
risk_tier: CRITICAL
tessl_pillar: P1_systems_invariants
created: 2026-08-25
branch: research/tessl-harness-applied-to-dark-factory
head_sha: 422e86bc5e
refs:
  attacks: A-8.1
  prior_beads: jleechan-ijod, jleechan-yoqy, jleechan-6xje, jleechan-sb4b
  prior_prs: "#387 (r5), #413 (2026-07-21 CI failure)"
  plan_doc: docs/plans/tessl-harness-2026-08-25/invariant-redesigns.md#redesign-r2
  memory: project_2026-08-25_factory_destructive_session_reap
---

# vacuous_red_green: separate DetectorSkipped from BaselineFailed

## Problem (Tessl P1 violation)

`daemon/src/verifier.rs:965` maps `VacuousRedGreenStatus::BaselineFailed(_reason) => GateResult::Green`. The vacuous-test detector (issue #387) cannot materialize the base worktree (no GH_TOKEN, gh pr view failed, rustup missing cargo) → the gate that EXISTS to catch vacuous tests PASSES.

Per MEMORY.md `project_2026-08-25_factory_destructive_session_reap`, this is a known design intent acknowledged in the comment at lines 946-954 (PR #413 2026-07-21). But the EFFECT is: any infra failure = universal vacuous-pass. A misconfigured runner means the entire vacuous-detection gate is nullified for that PR.

The Tessl invariant is "absence of finding ≠ absence of measurement". Today, those two are conflated.

## Acceptance — 5 Green

### G1 — Type system
`VacuousRedGreenStatus` (vacuous_red_green.rs:64-90) gains a new variant:
```rust
/// Detector subprocess could not start (GH_TOKEN missing, gh pr view failed,
/// cargo/rustup missing). Distinct from BaselineFailed (targeted test ran on
/// base tree and failed) and from Vacuous (targeted tests passed on reverted
/// production tree). Maps to Red.
DetectorSkipped(String),
```

### G2 — Gate verdict
`daemon/src/verifier.rs` `vacuous_red_green_gate` (line 955-986) maps `DetectorSkipped(reason) => GateResult::Red(format!("vacuous detector did not run: {reason}"))`. Per-bead `assess` aggregator (line 1162) propagates Red to `all_green=false`.

### G3 — Pre-detection trigger
`daemon/src/tick.rs` (around line 5523 where `verifier::assess` is called) inspects the detector's pre-flight conditions BEFORE mapping `BaselineFailed` to its current Green. Conditions checked: `GH_TOKEN` env present, `gh pr view --json mergeable` succeeds on the PR, `rustup which cargo` resolves. ANY failure → `DetectorSkipped` reason recorded → `vacuous_red_green_gate` returns Red.

### G4 — Telemetry + Healer
- `gate_assessment.jsonl` emits a new event type `DETECTOR_SKIPPED` with the reason.
- `df-healer` (daemon/src/healer.rs) clusters `DETECTOR_SKIPPED` events separately from `vacuous=true` events. Operator dashboard surfaces infra-fix-needed PRs vs real-vacuous PRs.
- New bead state `INFRA_VERIFICATION_NEEDED` separates these beads from Attested.

### G5 — Migration safety
- Phase 1 (this PR): add `DetectorSkipped` variant; keep `BaselineFailed => Green` for backward compat.
- Phase 2 (next PR): flip `BaselineFailed` to map to `DetectorSkipped`. Gate `DARK_FACTORY_DISABLE_DETECTOR_SKIP_RED=1` env var reverts to current Green behavior. Required kill-switch per bead authoring contract.
- Phase 3 (final PR): remove kill-switch after 2 weeks of stable operation.

## Migration contract (3 PRs)

This bead covers G1+G3+G5 (Phase 1 + kill-switch infra). Each PR passes `cargo test --workspace` + 5 daemon-test runs; each includes a `test_detector_skipped_*` integration test.

## Out of scope

R1 (BugBot structured), R5 (ReviewerHealth ledger), Healer dashboard — separate beads.

## Verification

```
$ cargo build --workspace
$ cargo test --workspace -- --skip slow
$ env DARK_FACTORY_DISABLE_DETECTOR_SKIP_RED=1 cargo run --bin dark-factory-daemon -- --smoke
```

Local-run proof: 5 consecutive daemon-test passes with and without kill-switch.

## Why now

1. **Critical risk-tier** per Tessl P1: gate's purpose negated by infra failure.
2. **Known incident** (PR #413 2026-07-21): design acknowledged gap, didn't fix.
3. **Cutover charter R3+R5**: explicit invariant + measurable enforcement.
4. **Direct Tessl alignment**: capture invariant, enforce via type system, surface via telemetry.