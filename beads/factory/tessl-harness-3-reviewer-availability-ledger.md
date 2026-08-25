---
bead_id: tessl-harness-3-reviewer-availability-ledger
title: "reviewer alias config + ReviewerHealth ledger (R5 redesign)"
target_repo: jleechanorg/dark-factory
labels: factory
priority: P1
risk_tier: HIGH
tessl_pillar: P1_systems_invariants+P3_risk_ladder
created: 2026-08-25
branch: research/tessl-harness-applied-to-dark-factory
head_sha: 422e86bc5e
refs:
  attacks: A-2.1, A-3.2, A-5.1, A-5.2, C-1
  prior_beads: jleechan-jsby (vendor waiver contract), jleechan-984e (cross-model)
  plan_doc: docs/plans/tessl-harness-2026-08-25/invariant-redesigns.md#redesign-r5
  memory: feedback_2026-08-17_ironclad_audit_results ("7/7 PASS" false claims)
---

# reviewer alias config + ReviewerHealth ledger

## Problem (Tessl P1+P3 violation)

Two related gaps:

**G5 (P1 violation):** Vendor identity is hardcoded in gate logic. `daemon/src/gates_compute.rs:138` `r.author.login.contains("coderabbit")`. A future bot rename requires code change, not config change. The `coderabbitai[bot]` rename risk is real; same for BugBot.

**G6 (P3 violation):** The vendor-waiver contract (`verifier.rs:1118-1147`) substitutes Skeptic::Pass + /er::Pass + cross-model for absent external reviewers. But NONE of the three compensating signals themselves have an availability invariant. They can fail silently.

Per MEMORY.md `feedback_2026-08-17_ironclad_audit_results`, the repo has been bitten: a 17-PR matrix was systematically overstated; "7/7 PASS" claims failed at CodeRabbit rate-limited + BugBot usage-limited + no Skeptic transcripts. **This is exactly the Tessl P3 risk-tier failure mode: a high-stakes claim awarded without verifying the supporting signals were live.**

## Acceptance — 5 Green

### G1 — `[reviewers.*]` config table
`config/reviewers.toml` defines vendor aliases (coderabbit, bugbot, skeptic, er, cold_reviewer — each with `aliases`, `display_name`, `weight`). `gates_compute.rs` reads the config; hardcoded `.contains("coderabbit")` is replaced with `config.reviewer_aliases_for("coderabbit")`.

### G2 — `ReviewerHealth` ledger
New module `daemon/src/reviewer_health.rs` parallel to `vendor_health.rs`:

```rust
pub enum ReviewerHealth {
    Healthy { last_seen_ts: u64, reviews_24h: u32 },
    Capped { reason: String, since_ts: u64 },
    Stalled { last_seen_ts: u64, stall_threshold_secs: u64 },
}
```

`compute_reviewer_health()` reads CXDB for the last 24h of activity per reviewer: 0 reviews in 24h (after first seen) → `Stalled`; explicit "rate-limited"/"quota" comment → `Capped`; else `Healthy`.

### G3 — Vendor-waiver contract extension
`verifier.rs::compensating_coverage_green` (line 1008-1012) extends to require `reviewer_health.all_healthy(&[Skeptic, Er, ColdReviewer])`. ANY compensating signal Capped/Stalled → vendor waiver INELIGIBLE → bead wedges with explicit operator message ("vendor waiver ineligible: Skeptic is Capped since 14:23 UTC").

### G4 — Telemetry
`gate_assessment.jsonl` emits `REVIEWER_HEALTH` events with current per-reviewer status. Daily `reviewer_health.jsonl` digest committed to `docs/heuristic/`. Alert: any reviewer `Capped` >2 hours → `REVIEWER_CAPPED_PROLONGED` event.

### G5 — Migration safety
- Phase 1 (this PR): add `[reviewers.*]` config + `ReviewerHealth` ledger + telemetry. Gate logic UNCHANGED (the alias config is read but the hardcoded substring stays as fallback).
- Phase 2 (next PR): apply `ReviewerHealth::all_healthy` to vendor-waiver contract. Kill-switch `DARK_FACTORY_DISABLE_REVIEWER_HEALTH_LEDGER=1` reverts to current behavior.
- Phase 3 (final PR): remove hardcoded `.contains("coderabbit")` strings; AST-grep rule prevents reintroduction.

## Migration contract (3 PRs)

This bead covers G1+G2+G4+G5 (Phase 1 + ledger + kill-switch infra). Each PR passes `cargo test --workspace` + 5 daemon-test runs; each includes `test_reviewer_health_*`.

## Out of scope

R1 (BugBot structured), R4 (Tiered LOC), R3 (Skeptic::Warn telemetry) — separate beads. R3 is small enough to fold into R5's G3 implementation.

## Verification

```
$ cargo build --workspace
$ cargo test --workspace -- --skip slow
$ env DARK_FACTORY_DISABLE_REVIEWER_HEALTH_LEDGER=1 cargo run --bin dark-factory-daemon -- --smoke
$ cat config/reviewers.toml  # confirms aliases load
```

## Why now

1. **Highest Tessl alignment** — article's "ladder" (P3) + "vendor in config" (P1).
2. **Direct incident** — MEMORY.md 2026-08-17 false-PASS at 7/7 PRs caused by this gap.
3. **Foundation for future invariants** — once `ReviewerHealth` exists, every reviewer change has a registration point. Today there is none.
4. **Cutover charter R1+R3+R5+R6**: fail-closed alias misconfig, executable AST-grep, three-skeptics-distinct.