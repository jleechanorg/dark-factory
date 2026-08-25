---
bead_id: tessl-harness-1-detector-skipped-red
title: "vacuous_red_green: flip BaselineFailed from Green to Unknown/Red (R2 redesign, simplified per codex review)"
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
  codex_review: ac2f46420d36a8511 (PLAUSIBLE; revised to extend infra, not duplicate)
---

# vacuous_red_green: flip BaselineFailed from Green to Unknown/Red

## Problem (Tessl P1 violation)

`daemon/src/verifier.rs:965` maps `VacuousRedGreenStatus::BaselineFailed(_reason) => GateResult::Green`. The vacuous-test detector can't materialize the base worktree (no GH_TOKEN, gh pr view failed) → the gate that EXISTS to catch vacuous tests PASSES.

The other infra-fail variants (`CargoNotFound`, `PytestNotFound`, `ManifestMissing`, `GreenFailed`, `NoChangedTests`) correctly map to `Unknown`. Only `BaselineFailed` is anomalous.

**Codex-review finding (incorporated):** no new enum variant is needed. The existing `VacuousRedGreenStatus::BaselineFailed(String)` is fine; only the gate mapping needs to flip. This collapses 3 PRs into 1.

## Acceptance — 5 Green

### G1 — Single-line gate flip
`daemon/src/verifier.rs:965`: `BaselineFailed(_reason) => GateResult::Green` becomes `BaselineFailed(reason) => GateResult::Red(format!("vacuous detector baseline failed: {reason}"))`. Operator-visible Red.

### G2 — Aggregator propagation
The aggregator at `verifier.rs::GateReport::all_green` already treats Red as fail-closed (line 128-129). No aggregator change needed.

### G3 — Healer surface
`daemon/src/healer.rs` adds a cluster branch for `vacuous_red_green: BaselineFailed` events. Operator dashboard distinguishes "infra-baseline-failed" from "tests vacuous".

### G4 — Telemetry
`gate_assessment.jsonl` already serializes `verdict: "fail"` for Red (lines 161-164). No new telemetry shape; the existing `evidence: [reason]` carries the baseline-failure string.

### G5 — Migration safety
- Phase 1 (this PR): flip the mapping. Add kill-switch `DARK_FACTORY_LEGACY_BASELINE_FAILED_GREEN=1` reverts to current behavior. Default OFF.

## Migration contract (1 PR)

Single PR with kill-switch. Pass `cargo test --workspace` + 5 daemon-test runs locally. Includes `test_baseline_failed_to_red` integration test that flips kill-switch and asserts gate output.

## Out of scope

R1 (BugBot structured), R5 (ReviewerHealth ledger), Healer dashboard — separate beads.

## Verification

```
$ cargo build --workspace
$ cargo test --workspace -- --skip slow
$ env DARK_FACTORY_LEGACY_BASELINE_FAILED_GREEN=1 cargo run --bin dark-factory-daemon -- --smoke
```

Local-run proof: 5 consecutive daemon-test passes with kill-switch ON and OFF.

## Why now

1. **Critical risk-tier** per Tessl P1: gate's purpose negated by infra failure.
2. **Known incident** (PR #413 2026-07-21): design acknowledged gap, didn't fix.
3. **Cutover charter R3+R5**: explicit invariant + measurable enforcement.
4. **Direct Tessl alignment**: capture invariant, enforce via type system, surface via telemetry. Codex-verified attack vector (PLAUSIBLE) with simplified fix (extend existing infra, not duplicate).