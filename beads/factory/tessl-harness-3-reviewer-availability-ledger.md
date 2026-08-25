---
bead_id: tessl-harness-3-reviewer-availability-ledger
title: "reviewer alias + ReviewerHealth: extend vendor_aliases + vendor_health, don't duplicate (R5 redesign, codex-revised)"
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
  prior_beads: jleechan-jsby (vendor waiver), jleechan-984e (cross-model), rev-9zrgs (vendor_aliases)
  plan_doc: docs/plans/tessl-harness-2026-08-25/invariant-redesigns.md#redesign-r5
  codex_review: ac2f46420d36a8511 (PLAUSIBLE; revised to extend vendor_aliases + vendor_health, not duplicate)
---

# reviewer alias + ReviewerHealth: extend existing substrate

## Problem (Tessl P1+P3 violation)

Two related gaps:

**G5 (P1):** Vendor identity is hardcoded in gate logic. `daemon/src/gates_compute.rs:138` `r.author.login.contains("coderabbit")`. A future bot rename requires code change.

**G6 (P3):** The vendor-waiver contract (`verifier.rs:1118-1147`) requires Skeptic::Pass + /er::Pass + cross-model, but NONE of the three compensating signals have availability invariants.

**Codex-review finding (incorporated):** Don't propose new infrastructure. The repo already has the canonical pattern:
- `daemon/src/vendor_aliases.rs` (174 LOC, `include_str! + OnceLock + serde::Deserialize` + exact-match)
- `config/vendor_aliases.json` (structured config)
- `daemon/src/vendor_health.rs` (428 LOC, 11 unit tests, `VendorHealthLedger`)
- `daemon/src/reviewer_priority.rs` (parallels vendor_aliases for reviewer priorities)

R5 should EXTEND these, not duplicate. AST-grep tooling doesn't exist in this repo — use `cargo clippy --workspace -- -D clippy::match_wildcard_for_single_variants` and existing unit-test patterns as substrate.

## Acceptance — 5 Green

### G1 — Extend `config/vendor_aliases.json` for reviewers
Add a parallel `reviewer_aliases` section mirroring the `vendor_aliases` structure (canonical-reviewer-name → aliases). Includes `skeptic`, `er`, `cold_reviewer`. Update `daemon/src/vendor_aliases.rs` (or fork to `reviewer_aliases.rs`) to add `canonical_reviewer(raw: &str) -> String` following the same exact-match pattern.

### G2 — Extend `VendorHealthLedger` for reviewers
Add `ReviewerHealth` variants to the existing `vendor_health.rs` enum (Healthy/Capped/Stalled). The existing `VendorHealthLedger` is extended with `compute_reviewer_health()` reading CXDB. **No new file.**

### G3 — Vendor-waiver contract extension
`verifier.rs::compensating_coverage_green` (line 1008-1012) extends:
```rust
skeptic_pass && er_pass && !review_degraded
    && vendor_health.all_reviewer_healthy(&[Reviewer::Skeptic, Reviewer::Er])
```
Uses extended ledger.

### G4 — Gate-level alias resolution
`gates_compute.rs` reads `config/vendor_aliases.json` for vendor matching (replace hardcoded `.contains("coderabbit")` at line 138). For reviewers, use the new `canonical_reviewer()` helper.

### G5 — Migration safety
- Phase 1 (this PR): extend `vendor_aliases.json` with `reviewer_aliases`; extend `vendor_health.rs` with `ReviewerHealth`. Gate logic UNCHANGED.
- Phase 2 (next PR): apply to vendor-waiver contract. Kill-switch `DARK_FACTORY_DISABLE_REVIEWER_HEALTH_LEDGER=1` reverts.
- Phase 3 (final PR): replace hardcoded `.contains("coderabbit")` with `canonical_vendor("coderabbit")`. Clippy coverage test prevents reintroduction.

## Migration contract (3 PRs)

Each PR passes `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` + 5 daemon-test runs. Each includes `test_reviewer_health_*` integration tests.

## Out of scope

R1 (BugBot structured), R4 (Tiered LOC), R3 (Skeptic::Warn telemetry) — separate beads. R3 folds into R5's G3 implementation.

## Verification

```
$ cargo build --workspace
$ cargo test --workspace -- --skip slow
$ cargo clippy --workspace -- -D warnings -D clippy::match_wildcard_for_single_variants
$ env DARK_FACTORY_DISABLE_REVIEWER_HEALTH_LEDGER=1 cargo run --bin dark-factory-daemon -- --smoke
$ cat config/vendor_aliases.json  # confirms reviewer_aliases section
```

## Why now

1. **Highest Tessl alignment** — article's "ladder" (P3) + "vendor in config" (P1).
2. **Direct incident** — MEMORY.md 2026-08-17 false-PASS at 7/7 PRs.
3. **Cutover charter R1+R3+R5+R6**: fail-closed alias misconfig, executable substrate (clippy), three-skeptics-distinct.
4. **Codex-verified** (PLAUSIBLE) with simplified fix extending existing alias + ledger substrate.